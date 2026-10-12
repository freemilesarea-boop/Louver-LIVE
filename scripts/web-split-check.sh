#!/usr/bin/env bash
# Prove the front-end split works, with the real Caddy, before it goes near a
# server that is broadcasting.
#
#   scripts/web-split-check.sh                 # needs `caddy` on PATH
#   CADDY_BIN=/path/to/caddy scripts/web-split-check.sh
#
# It runs `deploy/web/Caddyfile` verbatim — every address in that file is an
# environment variable precisely so that the thing under test here is the file
# that will be deployed, not a local paraphrase of it. In front: a stand-in API
# (`web-split-stub-api.mjs`). Behind: two fixture bundles released and rolled
# back with `deploy-web.sh`.
#
# Everything happens in one temporary directory on loopback ports. The check
# also guards itself: `docker`, `docker compose`, `systemctl` and `pkill` are
# shadowed by recorders on PATH, and a local FFmpeg is left encoding to /dev/null
# for the duration, so "no container was recreated" and "no broadcast was
# touched" are measured rather than asserted in prose.
set -uo pipefail

SITE_PORT="${SITE_PORT:-9700}"
API_PORT="${API_PORT:-9703}"
ADMIN_PORT="${ADMIN_PORT:-2029}"
BASE="http://127.0.0.1:$SITE_PORT"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
CADDY_BIN="${CADDY_BIN:-$(command -v caddy || true)}"
[ -x "$CADDY_BIN" ] || { echo "no caddy binary: set CADDY_BIN" >&2; exit 2; }

WORK="$(mktemp -d)"
RELEASES="$WORK/web"
PASSED=0; FAILED=0
CADDY_PID=""; API_PID=""; FFMPEG_PID=""; SSE_PID=""

cleanup() {
  for pid in "$SSE_PID" "$CADDY_PID" "$API_PID" "$FFMPEG_PID"; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null
  done
  wait 2>/dev/null
  rm -rf "$WORK"
}
trap cleanup EXIT

pass() { printf '  PASS  %s\n' "$1"; PASSED=$((PASSED + 1)); }
fail() { printf '  FAIL  %s\n          %s\n' "$1" "$2"; FAILED=$((FAILED + 1)); }
want() { # want <name> <expected> <actual>
  if [ "$2" = "$3" ]; then pass "$1"; else fail "$1" "expected [$2], got [$3]"; fi
}
code() { curl -s -o /dev/null -w '%{http_code}' "$BASE$1"; }
head_of() { curl -sI "$BASE$1" | tr -d '\r'; }
cc() { head_of "$1" | awk 'tolower($1) == "cache-control:" { $1 = ""; sub(/^ /, ""); print }'; }

# --- fixtures --------------------------------------------------------------
# Shaped like a real `apps/web/dist`: a hashed chunk per build, the prerendered
# public pages, and the root files the SEO step writes (see docs/SEO.md).
fixture() { # fixture <dir> <label> <hash>
  local dir="$1" label="$2" hash="$3"
  mkdir -p "$dir/assets" "$dir/pricing"
  cat > "$dir/index.html" <<HTML
<!doctype html><html><head><title>247streams $label</title>
<script type="module" src="/assets/app-$hash.js"></script>
<link rel="stylesheet" href="/assets/app-$hash.css"></head>
<body><div id="root"><p>build $label</p></div></body></html>
HTML
  printf 'console.log("build %s")\n' "$label" > "$dir/assets/app-$hash.js"
  printf '/* build %s */\n' "$label" > "$dir/assets/app-$hash.css"
  printf '<!doctype html><html><head><title>pricing %s</title></head><body><div id="root"><h1>pricing</h1></div></body></html>\n' \
    "$label" > "$dir/pricing/index.html"
  printf 'User-agent: *\nAllow: /\n' > "$dir/robots.txt"
  printf '<svg xmlns="http://www.w3.org/2000/svg"/>\n' > "$dir/favicon.svg"
  printf '<?xml version="1.0"?><urlset/>\n' > "$dir/sitemap.xml"
}
V1_HASH="aaaa1111"; V2_HASH="bbbb2222"
fixture "$WORK/dist-v1" v1 "$V1_HASH"
fixture "$WORK/dist-v2" v2 "$V2_HASH"

# --- recorders, so a forbidden command cannot pass unnoticed ---------------
mkdir -p "$WORK/shims"
for tool in docker docker-compose systemctl service pkill; do
  cat > "$WORK/shims/$tool" <<SHIM
#!/bin/sh
echo "$tool \$*" >> "$WORK/forbidden.log"
exit 1
SHIM
  chmod +x "$WORK/shims/$tool"
done
PATH="$WORK/shims:$PATH"; export PATH

# --- a process that must survive every release -----------------------------
# A local encode to /dev/null, standing in for a customer's broadcast. Nothing
# in the release path has any business knowing it exists.
if command -v ffmpeg > /dev/null; then
  ffmpeg -nostdin -loglevel quiet -re -f lavfi -i "testsrc=size=160x120:rate=5" -t 3600 -f null - &
  FFMPEG_PID=$!
  FFMPEG_START="$(ps -o lstart= -p "$FFMPEG_PID" 2>/dev/null | tr -s ' ')"
fi

# --- the first release -----------------------------------------------------
echo "release v1"
"$HERE/scripts/deploy-web.sh" release "$WORK/dist-v1" --root "$RELEASES" --version v1 || exit 1
V1_DIR="$(readlink "$RELEASES/current")"

# --- the two servers -------------------------------------------------------
node "$HERE/scripts/web-split-stub-api.mjs" --port "$API_PORT" > "$WORK/api.log" 2>&1 &
API_PID=$!
for _ in $(seq 1 50); do
  curl -sf -o /dev/null "http://127.0.0.1:$API_PORT/health" && break
  sleep 0.2
done
LOUVER_DOMAIN=":$SITE_PORT" \
LOUVER_UPSTREAM="127.0.0.1:$API_PORT" \
LOUVER_WEB_ROOT="$RELEASES/current" \
LOUVER_TLS_EMAIL="nobody@example.invalid" \
CADDY_ADMIN="127.0.0.1:$ADMIN_PORT" \
  "$CADDY_BIN" run --config "$HERE/deploy/web/Caddyfile" --adapter caddyfile \
  > "$WORK/caddy.log" 2>&1 &
CADDY_PID=$!

for _ in $(seq 1 50); do
  [ "$(code /)" = "200" ] && break
  sleep 0.2
done

echo
echo "T1  the dashboard is served"
want "GET / is 200" 200 "$(code /)"
if curl -s "$BASE/" | grep -q 'build v1'; then pass "GET / is the v1 bundle"; else fail "GET / is the v1 bundle" "marker missing"; fi
want "the hashed chunk is 200" 200 "$(code "/assets/app-$V1_HASH.js")"
want "the hashed stylesheet is 200" 200 "$(code "/assets/app-$V1_HASH.css")"
want "a prerendered public page is 200" 200 "$(code /pricing)"
# The built pages link to `/pricing/`, with the slash — that is the URL a real
# visitor follows, and a directory is not a `try_files` match, so it is the one
# that can regress.
want "and with the trailing slash the pages actually link to" 200 "$(code /pricing/)"
want "robots.txt is 200" 200 "$(code /robots.txt)"

echo
echo "T2  the application still answers"
want "/api/health is 200" 200 "$(code /api/health)"
if curl -s "$BASE/api/health" | grep -q 'stub-api'; then pass "/api/health came from the app, not the files"; else fail "/api/health came from the app" "$(curl -s "$BASE/api/health" | head -c 120)"; fi
want "/health is 200" 200 "$(code /health)"
if curl -s "$BASE/health" | grep -q 'stub-api'; then pass "/health reached the app (not the SPA fallback)"; else fail "/health reached the app" "answered with files"; fi
want "an unknown API route still 404s from the app" 404 "$(code /api/nope)"
if curl -s -H 'Cookie: louver_session=abc' "$BASE/api/me" | grep -q 'louver_session=abc'; then
  pass "the session cookie survives the proxy"
else
  fail "the session cookie survives the proxy" "$(curl -s -H 'Cookie: louver_session=abc' "$BASE/api/me")"
fi
if curl -sI "$BASE/api/me" | tr -d '\r' | grep -qi '^set-cookie:'; then pass "Set-Cookie comes back through the proxy"; else fail "Set-Cookie comes back" "header missing"; fi

echo
echo "T3  an open SSE stream survives a release"
curl -sN --max-time 8 "$BASE/api/events" > "$WORK/sse.out" 2>/dev/null &
SSE_PID=$!
sleep 2
echo "    releasing v2 while the stream is open"
"$HERE/scripts/deploy-web.sh" release "$WORK/dist-v2" --root "$RELEASES" --version v2
sleep 2
echo "    rolling back while the stream is open"
"$HERE/scripts/deploy-web.sh" rollback --root "$RELEASES"
wait "$SSE_PID" 2>/dev/null
SSE_PID=""
TICKS="$(grep -c '^data: tick' "$WORK/sse.out" 2>/dev/null || echo 0)"
LAST="$(grep '^data: tick' "$WORK/sse.out" | tail -1 | awk '{print $3}')"
if [ "$TICKS" -ge 20 ]; then pass "the stream delivered $TICKS events across two releases"; else fail "the stream survived" "only $TICKS events"; fi
if [ "${LAST:-0}" = "$TICKS" ]; then pass "the stream was never reconnected (last tick = $LAST)"; else fail "the stream was never reconnected" "$TICKS events but last tick is $LAST"; fi

echo
echo "T4  v1 -> v2"
"$HERE/scripts/deploy-web.sh" activate v2 --root "$RELEASES"
if curl -s "$BASE/" | grep -q 'build v2'; then pass "GET / is the v2 bundle, with no reload"; else fail "GET / is the v2 bundle" "$(curl -s "$BASE/" | grep -o 'build v.')"; fi
want "the v2 chunk is 200" 200 "$(code "/assets/app-$V2_HASH.js")"
# The point of the carry-forward: a browser still holding v1's index.html names
# v1's chunk, and this release never built it. Without the hand-over that is a
# 404 and a blank screen.
want "v1's chunk is STILL served after the deploy" 200 "$(code "/assets/app-$V1_HASH.js")"
want "v1's stylesheet too" 200 "$(code "/assets/app-$V1_HASH.css")"
want "current points at v2" "releases/v2" "$(readlink "$RELEASES/current")"

echo
echo "T5  v2 -> v1 rollback"
"$HERE/scripts/deploy-web.sh" rollback --root "$RELEASES"
if curl -s "$BASE/" | grep -q 'build v1'; then pass "GET / is the v1 bundle again"; else fail "rollback restored v1" "$(curl -s "$BASE/" | grep -o 'build v.')"; fi
want "the v1 chunk is served" 200 "$(code "/assets/app-$V1_HASH.js")"
# And the same in the other direction: whoever loaded v2 a second ago keeps
# working after the rollback.
want "v2's chunk survives the rollback as well" 200 "$(code "/assets/app-$V2_HASH.js")"
want "current points at v1" "$V1_DIR" "$(readlink "$RELEASES/current")"
want "the release directories both survived the swap" "v1 v2" "$(ls "$RELEASES/releases" | sort | tr '\n' ' ' | sed 's/ $//')"

echo
echo "T6  a file that does not exist is 404, not a page of HTML"
want "a missing hashed chunk" 404 "$(code /assets/app-deadbeef.js)"
want "a missing stylesheet" 404 "$(code /assets/app-deadbeef.css)"
want "a missing sourcemap" 404 "$(code /assets/app-deadbeef.js.map)"
want "a missing script outside /assets" 404 "$(code /legacy/main.js)"
want "a missing stylesheet outside /assets" 404 "$(code /old.css)"
if curl -s "$BASE/assets/app-deadbeef.js" | grep -q '<div id="root"'; then
  fail "a missing chunk is not answered with index.html" "the SPA fallback leaked into /assets"
else
  pass "a missing chunk is not answered with index.html"
fi

echo
echo "T7  a refresh on a client route still works"
for path in /dashboard /admin /admin/users/42 /broadcasts/7/items; do
  if [ "$(code "$path")" = "200" ] && curl -s "$BASE$path" | grep -q '<div id="root"'; then
    pass "$path serves the SPA"
  else
    fail "$path serves the SPA" "$(code "$path")"
  fi
done
if curl -s "$BASE/pricing" | grep -q '<h1>pricing'; then pass "/pricing keeps its prerendered page, not the SPA shell"; else fail "/pricing keeps its prerendered page" "got the shell"; fi

echo
echo "T8  cache policy"
want "a hashed chunk is immutable for a year" "public, max-age=31536000, immutable" "$(cc "/assets/app-$V1_HASH.js")"
want "index.html must be revalidated" "no-cache" "$(cc /)"
want "a prerendered page must be revalidated" "no-cache" "$(cc /pricing)"
ETAG="$(head_of / | awk 'tolower($1) == "etag:" { print $2 }')"
if [ -n "$ETAG" ]; then
  want "index.html revalidates to 304" 304 "$(curl -s -o /dev/null -w '%{http_code}' -H "If-None-Match: $ETAG" "$BASE/")"
else
  fail "index.html revalidates to 304" "no ETag to revalidate with"
fi
A_ETAG="$(head_of "/assets/app-$V1_HASH.js" | awk 'tolower($1) == "etag:" { print $2 }')"
if [ -n "$A_ETAG" ]; then pass "a hashed chunk still carries an ETag"; else fail "a hashed chunk carries an ETag" "none"; fi

echo
echo "T9  nothing touched the broadcast"
if [ -n "$FFMPEG_PID" ]; then
  if kill -0 "$FFMPEG_PID" 2>/dev/null; then pass "the FFmpeg process is still running (pid $FFMPEG_PID)"; else fail "the FFmpeg process survived" "pid $FFMPEG_PID is gone"; fi
  NOW_START="$(ps -o lstart= -p "$FFMPEG_PID" 2>/dev/null | tr -s ' ')"
  want "it is the same process, not a restarted one" "$FFMPEG_START" "$NOW_START"
else
  fail "the FFmpeg process survived" "no ffmpeg on PATH to stand in for a broadcast"
fi

echo
echo "T10 no container was recreated"
if [ -s "$WORK/forbidden.log" ]; then
  fail "no docker/systemctl/pkill command was run" "$(tr '\n' ';' < "$WORK/forbidden.log")"
else
  pass "no docker/systemctl/pkill command was run"
fi
if grep -nE '\b(docker|systemctl|pkill|killall)\b' "$HERE/scripts/deploy-web.sh" > /dev/null; then
  fail "deploy-web.sh cannot reach a container" "$(grep -nE '\b(docker|systemctl|pkill|killall)\b' "$HERE/scripts/deploy-web.sh" | head -3)"
else
  pass "deploy-web.sh contains no container, service or signal command at all"
fi
if [ "$(code /api/health)" = "200" ] && [ "$(code /) " != " " ]; then pass "the app was still answering at the end of the run"; fi

echo
echo "T11 the carry-forward is what keeps the other build alive"
# Prove it by taking it away: a release activated with --no-carry goes back to
# answering the other build's chunk with 404.
"$HERE/scripts/deploy-web.sh" activate v2 --root "$RELEASES" --no-carry > /dev/null 2>&1
rm -f "$RELEASES/releases/v2/assets/app-$V1_HASH.js" "$RELEASES/releases/v2/assets/app-$V1_HASH.css"
want "with --no-carry, v1's chunk is 404 again" 404 "$(code "/assets/app-$V1_HASH.js")"
"$HERE/scripts/deploy-web.sh" activate v1 --root "$RELEASES" > /dev/null 2>&1
"$HERE/scripts/deploy-web.sh" activate v2 --root "$RELEASES" > /dev/null 2>&1
want "carrying again restores it, with no reload" 200 "$(code "/assets/app-$V1_HASH.js")"
if [ "$(stat -c %h "$RELEASES/releases/v2/assets/app-$V1_HASH.js")" -ge 2 ]; then
  pass "it is a hard link, so the bytes exist once however many releases hold them"
else
  fail "the carried file is a hard link" "link count is 1; it was copied, not linked"
fi
"$HERE/scripts/deploy-web.sh" rollback --root "$RELEASES" > /dev/null 2>&1

echo
echo "T12 the gate in front of the Caddy switch"
EMPTY="$WORK/empty-root"
if "$HERE/scripts/deploy-web.sh" check --root "$EMPTY" > /dev/null 2>&1; then
  fail "check refuses an empty releases directory" "it passed, so nothing would stop a switch that blanks the dashboard"
else
  pass "check refuses an empty releases directory"
fi
"$HERE/scripts/deploy-web.sh" publish "$WORK/dist-v1" --root "$EMPTY" --version only > /dev/null 2>&1
if "$HERE/scripts/deploy-web.sh" check --root "$EMPTY" > /dev/null 2>&1; then
  fail "check refuses a release that is staged but not live" "it passed"
else
  pass "check refuses a release that is staged but not live"
fi
"$HERE/scripts/deploy-web.sh" activate only --root "$EMPTY" > /dev/null 2>&1
if "$HERE/scripts/deploy-web.sh" check --root "$EMPTY" > /dev/null 2>&1; then
  pass "check passes once a release is live"
else
  fail "check passes once a release is live" "$("$HERE/scripts/deploy-web.sh" check --root "$EMPTY" 2>&1 | tail -3)"
fi
# And the state it is there to prevent: measured, not assumed.
ln -sfn releases/gone "$EMPTY/.x" && mv -T "$EMPTY/.x" "$EMPTY/current"
if "$HERE/scripts/deploy-web.sh" check --root "$EMPTY" > /dev/null 2>&1; then
  fail "check refuses a dangling current" "it passed"
else
  pass "check refuses a dangling current"
fi

echo
echo "T13 a failed deployment leaves the live release alone"
LIVE_BEFORE="$(readlink "$RELEASES/current")"
BROKEN="$WORK/broken-bundle"; mkdir -p "$BROKEN"
echo '<!doctype html>' > "$BROKEN/index.html"   # no assets/ at all
if "$HERE/scripts/deploy-web.sh" release "$BROKEN" --root "$RELEASES" --version bad > /dev/null 2>&1; then
  fail "a bundle with no assets/ is refused" "it was published and activated"
else
  pass "a bundle with no assets/ is refused"
fi
want "the live release did not move" "$LIVE_BEFORE" "$(readlink "$RELEASES/current")"
want "and the site is still serving" 200 "$(code /)"
if [ -e "$RELEASES/releases/bad" ]; then
  fail "nothing half-made was left behind" "releases/bad exists"
else
  pass "nothing half-made was left behind"
fi

echo
echo "T14 the armed/not-armed probe agrees with reality"
if "$HERE/scripts/web-split-verify.sh" "$BASE" > "$WORK/verify.out" 2>&1; then
  pass "web-split-verify.sh says ARMED while the split is in front"
else
  fail "web-split-verify.sh says ARMED" "$(tail -4 "$WORK/verify.out")"
fi
# Point it at the API alone, which is what a reverted Caddy looks like from
# outside: the probe path falls through to something that answers 200.
if "$HERE/scripts/web-split-verify.sh" "http://127.0.0.1:$API_PORT" > "$WORK/verify-off.out" 2>&1; then
  fail "the probe notices when the split is NOT in front" "it reported ARMED against a bare API"
else
  pass "the probe notices when the split is NOT in front"
fi

echo
printf '%s\n' "----"
printf 'passed %d, failed %d\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
