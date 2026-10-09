#!/usr/bin/env bash
# Validate Caddyfile.sample's logic against a real Caddy, a real worker and a
# stand-in for the `louver` container — all on loopback, nothing in production.
#
# This is the script that found the defect documented in Caddyfile.sample: a
# `header_up -X-Louver-Gate` before `header_up X-Louver-Gate <value>` cancels
# the line that adds it, because Caddy applies `delete` after `set` within one
# HeaderOps. Written down as a script so that claim stays checkable and so the
# next change to the proxy rules is tested rather than reasoned about.
#
#   ./caddy-validate.sh            # needs a `caddy` on PATH, or CADDY=/path/to/caddy
#
# It asserts, against live traffic:
#   1. /beta/* and /api/live-source/* reach the worker
#   2. the gate header is injected, so the worker answers at all
#   3. a client-forged gate header is overwritten, not trusted
#   4. Cookie is stripped everywhere but the handshake
#   5. the handshake does receive the cookie
#   6. the catch-all still reaches `louver`, keeping Cookie and without the gate
#   7. a broken config is refused and the running one keeps serving
#   8. a reload does not break an in-flight long-lived response
set -uo pipefail

CADDY=${CADDY:-caddy}
command -v "$CADDY" >/dev/null || { echo "SKIP: no caddy binary (set CADDY=...)"; exit 0; }

CRATE=$(cd "$(dirname "$0")" && pwd)
WORKER_BIN=${WORKER_BIN:-$CRATE/../../target/debug/live-source-api}
[ -x "$WORKER_BIN" ] || { echo "SKIP: build it first - cargo build -p louver-live-source"; exit 0; }

GATE="a-local-caddy-validation-gate-secret"
SIGN="a-local-caddy-validation-sign-secret"
WORK=$(mktemp -d); trap 'cleanup' EXIT
PIDS=()
cleanup() { for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null; done; rm -rf "$WORK"; }

port() { python3 -c "import socket;s=socket.socket();s.bind(('127.0.0.1',0));print(s.getsockname()[1]);s.close()"; }
P_CADDY=$(port); P_WORKER=$(port); P_LOUVER=$(port); P_ADMIN=$(port); P_SSE=$(port)

fail=0
ok()   { printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail+1)); }
is()   { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (got '$2', want '$3')"; fi; }

# --- the stand-in louver, and a long-lived stream --------------------------
cat > "$WORK/up.py" <<'PY'
import json, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def do_GET(self):
        if self.path.startswith("/stream"):
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            try:
                for i in range(20):
                    c = f"data: {i}\n\n".encode()
                    self.wfile.write(b"%x\r\n" % len(c) + c + b"\r\n"); self.wfile.flush()
                    time.sleep(0.4)
                self.wfile.write(b"0\r\n\r\n")
            except Exception:
                pass
            return
        self._j()
    do_POST = do_DELETE = do_PUT = lambda s: s._j()
    def _j(self):
        b = json.dumps({"upstream": "louver", "path": self.path,
                        "cookie": self.headers.get("Cookie"),
                        "gate": self.headers.get("X-Louver-Gate")}).encode()
        self.send_response(200); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def log_message(self, *a): pass
ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
PY
python3 "$WORK/up.py" "$P_LOUVER" & PIDS+=($!)

mkdir -p "$WORK/media" "$WORK/state"
LOUVER_LIVE_SOURCE_SECRET="$SIGN" \
LOUVER_LIVE_SOURCE_GATE_SECRET="$GATE" \
LOUVER_LIVE_SOURCE_DESTINATIONS='{"u":{"sink":"rtmp://127.0.0.1:1935/live/x"}}' \
"$WORKER_BIN" --listen "127.0.0.1:$P_WORKER" --origin https://247streams.kr \
  --allow-origin "http://127.0.0.1:$P_CADDY" \
  --media-dir "$WORK/media" --state-dir "$WORK/state" > "$WORK/worker.log" 2>&1 & PIDS+=($!)

# --- the config under test, mirroring Caddyfile.sample --------------------
printf '%s\n' \
'{' \
"	admin 127.0.0.1:$P_ADMIN" \
'	auto_https off' \
'}' \
'' \
":$P_CADDY {" \
'	@live_source_session path /api/live-source/session' \
'	@live_source_other path /api/live-source/* /beta /beta/*' \
'' \
'	handle @live_source_session {' \
"		reverse_proxy 127.0.0.1:$P_WORKER {" \
'			header_up X-Louver-Gate {env.LOUVER_GATE_SECRET}' \
'		}' \
'	}' \
'' \
'	handle @live_source_other {' \
"		reverse_proxy 127.0.0.1:$P_WORKER {" \
'			header_up -Cookie' \
'			header_up X-Louver-Gate {env.LOUVER_GATE_SECRET}' \
'		}' \
'	}' \
'' \
"	reverse_proxy 127.0.0.1:$P_LOUVER" \
'}' > "$WORK/Caddyfile"

export LOUVER_GATE_SECRET="$GATE"
echo "== caddy validate =="
if "$CADDY" validate --config "$WORK/Caddyfile" --adapter caddyfile >/dev/null 2>&1; then
  ok "the configuration adapts and validates"
else
  bad "caddy validate rejected the configuration"; exit 1
fi

"$CADDY" run --config "$WORK/Caddyfile" --adapter caddyfile > "$WORK/caddy.log" 2>&1 & PIDS+=($!)
B="http://127.0.0.1:$P_CADDY"
for _ in $(seq 1 40); do curl -sf -m 2 -o /dev/null "$B/" && break; sleep 0.25; done

code() { curl -s -m 10 -o /dev/null -w '%{http_code}' "$@"; }
body() { curl -s -m 10 "$@"; }
jget() { python3 -c "import json,sys;print(json.load(sys.stdin).get('$1'))"; }

echo "== 1-2. routing and gate injection =="
is "/beta/ reaches the worker"              "$(code "$B/beta/")" "200"
is "/beta/app.js reaches the worker"        "$(code "$B/beta/app.js")" "200"
is "/api/live-source/health via the proxy"  "$(code "$B/api/live-source/health")" "200"

echo "== 3. a client-forged gate header is overwritten =="
is "forged gate does not win"               "$(code -H 'X-Louver-Gate: CLIENT-FORGED' "$B/api/live-source/health")" "200"
is "direct access without the gate is 403"  "$(code "http://127.0.0.1:$P_WORKER/api/live-source/health")" "403"

echo "== 4-5. the cookie =="
# 401 means the worker saw no cookie, i.e. Caddy stripped it. 403 would mean it
# arrived and the worker's own refusal fired.
is "Cookie stripped on /jobs"               "$(code -H 'Cookie: louver_session=x' "$B/api/live-source/jobs")" "401"
with=$(body -X POST -H 'Cookie: louver_session=x' -H "Origin: $B" "$B/api/live-source/session")
without=$(body -X POST -H "Origin: $B" "$B/api/live-source/session")
if [ "$with" != "$without" ]; then ok "the handshake does receive the cookie"
else bad "the handshake got the same answer with and without a cookie"; fi

echo "== 6. the catch-all is unchanged =="
is "catch-all reaches louver"               "$(body -H 'Cookie: louver_session=x' "$B/api/me" | jget upstream)" "louver"
is "catch-all keeps the cookie"             "$(body -H 'Cookie: louver_session=x' "$B/api/me" | jget cookie)" "louver_session=x"
is "catch-all is not given the gate secret" "$(body -H 'Cookie: louver_session=x' "$B/api/me" | jget gate)" "None"
is "the SPA path still reaches louver"      "$(body "$B/" | jget upstream)" "louver"

echo "== 7. a broken config is refused, the running one keeps serving =="
printf '%s\n' '{' "	admin 127.0.0.1:$P_ADMIN" '}' '' ":$P_CADDY {" '	not_a_directive' '}' > "$WORK/Caddyfile.broken"
"$CADDY" reload --config "$WORK/Caddyfile.broken" --adapter caddyfile --address "127.0.0.1:$P_ADMIN" >/dev/null 2>&1 \
  && bad "a broken config was accepted" || ok "a broken config is refused"
is "the old config still serves the worker" "$(code "$B/api/live-source/health")" "200"
is "the old config still serves louver"     "$(body "$B/api/me" | jget upstream)" "louver"

echo "== 8. a reload does not break an in-flight response =="
sed "s|reverse_proxy 127.0.0.1:$P_LOUVER|reverse_proxy 127.0.0.1:$P_SSE|" "$WORK/Caddyfile" > "$WORK/Caddyfile.sse"
python3 "$WORK/up.py" "$P_SSE" & PIDS+=($!)
sleep 1
"$CADDY" reload --config "$WORK/Caddyfile.sse" --adapter caddyfile --address "127.0.0.1:$P_ADMIN" >/dev/null 2>&1
( curl -s -N -m 20 "$B/stream" > "$WORK/sse.txt" 2>&1 ) & SSE_PID=$!
sleep 2
"$CADDY" reload --config "$WORK/Caddyfile.sse" --adapter caddyfile --address "127.0.0.1:$P_ADMIN" >/dev/null 2>&1
wait $SSE_PID
is "an in-flight stream survives a reload"  "$(grep -c '^data:' "$WORK/sse.txt")" "20"

echo
if [ "$fail" -eq 0 ]; then echo "ALL CHECKS PASSED"; else echo "$fail CHECK(S) FAILED"; fi
exit "$fail"
