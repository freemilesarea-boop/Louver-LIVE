#!/usr/bin/env bash
# Is the front-end split actually in front right now?
#
#   scripts/web-split-verify.sh https://247streams.kr
#   scripts/web-split-verify.sh http://127.0.0.1:9700
#
# Run this after ANY `docker compose` command on the server. It exists because
# the split can come off without anything failing:
# `scripts/deploy-vps.sh` ends in a bare `docker compose --profile https up -d
# --build`, which resolves `docker-compose.yml` alone — so Caddy is recreated
# with the root Caddyfile and no `/srv/web` mount, the dashboard keeps being
# served out of the server image, and nobody is told. The next
# `deploy-web.sh release` then changes nothing anyone can see.
#
# The probe that settles it: ask for a hashed file that cannot exist.
#
#   split in front   -> 404. `handle /assets/*` has no fallback, on purpose.
#   split off        -> 200 and a page of HTML, because the server's
#                       `ServeDir::new(dir).fallback(ServeFile::new(index))`
#                       (apps/server/src/lib.rs) answers every unknown path
#                       with index.html.
#
# Read-only: it sends GETs and nothing else. It cannot change a release, a
# container or a broadcast.
set -uo pipefail

BASE="${1:-}"
[ -n "$BASE" ] || { echo "usage: $0 <base-url>" >&2; exit 2; }
BASE="${BASE%/}"
PROBE="/assets/__web_split_probe__$(date +%s).js"
PASS=0; FAIL=0
pass() { printf '  ok    %s\n' "$1"; PASS=$((PASS + 1)); }
fail() { printf '  FAIL  %s\n          %s\n' "$1" "$2"; FAIL=$((FAIL + 1)); }
code() { curl -sS -o /dev/null -w '%{http_code}' --max-time 15 "$BASE$1" 2>/dev/null; }
cc()   { curl -sSI --max-time 15 "$BASE$1" 2>/dev/null | tr -d '\r' \
           | awk 'tolower($1) == "cache-control:" { $1 = ""; sub(/^ /, ""); print }'; }

echo "probing $BASE"
echo

# --- the application half, which must be untouched either way --------------
c="$(code /api/health)"
[ "$c" = "200" ] && pass "/api/health is 200 — the server is answering" \
                 || fail "/api/health is 200" "got $c; the API is not reachable, stop here"
c="$(code /health)"
[ "$c" = "200" ] && pass "/health is 200" || fail "/health is 200" "got $c"

# --- the discriminator -----------------------------------------------------
c="$(code "$PROBE")"
case "$c" in
  404) pass "a file that cannot exist is 404 — the split IS in front" ;;
  200) fail "a file that cannot exist is 404" \
            "got 200: the split is NOT in front. Caddy is proxying everything to louver and serving the bundle baked into the image. Re-apply the overlay (docs/WEB_DEPLOY.md §3) — and add COMPOSE_FILE to .env so the next deploy keeps it." ;;
  *)   fail "a file that cannot exist is 404" "got $c, which is neither; look at the Caddy logs" ;;
esac

# --- the cache policy, which only the split sets ---------------------------
h="$(cc /)"
[ "$h" = "no-cache" ] && pass "index.html says Cache-Control: no-cache" \
  || fail "index.html says Cache-Control: no-cache" "got '${h:-<none>}' — the split sets this; the server's ServeDir does not"

# --- a real hashed file, taken from the page itself ------------------------
asset="$(curl -sS --max-time 15 "$BASE/" 2>/dev/null | grep -oE '/assets/[A-Za-z0-9._-]+\.js' | head -1)"
if [ -n "$asset" ]; then
  c="$(code "$asset")"
  [ "$c" = "200" ] && pass "the page's own chunk $asset is 200" \
                   || fail "the page's own chunk is 200" "$asset gave $c — this release is broken"
  h="$(cc "$asset")"
  case "$h" in
    *immutable*) pass "it is immutable for a year" ;;
    *)           fail "the chunk is immutable for a year" "got '${h:-<none>}'" ;;
  esac
else
  fail "the page names a chunk" "no /assets/*.js in the HTML — is this the 247streams dashboard?"
fi

# --- the public pages, which must keep their own prerendered HTML ----------
for p in /pricing/ /terms/; do
  c="$(code "$p")"
  [ "$c" = "200" ] && pass "$p is 200" || fail "$p is 200" "got $c"
done

echo
if [ "$FAIL" -eq 0 ]; then
  printf 'ARMED — the split is in front and serving (%d checks)\n' "$PASS"
  exit 0
fi
printf 'NOT ARMED — %d of %d checks failed. See docs/WEB_DEPLOY.md\n' "$FAIL" "$((PASS + FAIL))"
exit 1
