#!/usr/bin/env bash
# Everything to check BEFORE pointing Caddy at the host directory — and nothing
# else. Run it on the server, in the deployment directory.
#
#   scripts/web-split-preflight.sh
#   LOUVER_WEB_RELEASES=/srv/louver/web scripts/web-split-preflight.sh
#
# READ-ONLY BY CONSTRUCTION. The only docker commands here are `compose config`,
# `compose ps` and `compose up --dry-run`, none of which start, stop, build or
# remove anything. There is no un-flagged `up` in this file, and no `restart`,
# `stop`, `rm`, `kill`, `exec` or `build`. It cannot touch a broadcast.
#
# What it establishes, in order:
#   1. a release is staged and ready, so the switch cannot blank the dashboard
#   2. the overlay changes the `caddy` service and leaves `louver` byte-identical
#   3. the apply's own dry run acts on `caddy` and not on `louver`
#   4. the `louver` container's identity now, to compare with afterwards
#   5. whether a future bare `docker compose` would silently drop the overlay
#   6. whether a future bare `up -d` would now RECREATE louver — the trap the
#      step 5 pin leaves behind, because `.env` is louver's own `env_file`
set -uo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
ROOT="${LOUVER_WEB_RELEASES:-/srv/louver/web}"
BASE="-f docker-compose.yml"
OVER="-f docker-compose.yml -f docker-compose.web.yml"
FAIL=0
WARN=0
step() { printf '\n── %s\n' "$1"; }
pass() { printf '  ok    %s\n' "$1"; }
fail() { printf '  FAIL  %s\n          %s\n' "$1" "${2:-}"; FAIL=$((FAIL + 1)); }
# Not every hazard is a reason to refuse the switch. Check 6 finds one that the
# switch neither causes nor fixes: refusing would leave the operator with no
# move at all, so it is said loudly and counted separately.
warn() { printf '  WARN  %s\n          %s\n' "$1" "${2:-}"; WARN=$((WARN + 1)); }

# The louver service's container, by the name Compose gave it. Matching output
# on the service word instead would match the caddy container too: the project
# is `louver-live`, so `louver-live-caddy-1` contains `louver` with a word
# boundary on each side. Checked: that spelling made check 3 fail a correct
# apply, because the dry run says `Container louver-live-caddy-1  Recreate`.
louver_container() {
  # shellcheck disable=SC2086
  docker compose $BASE ps -a --format '{{.Name}}' louver 2>/dev/null | head -1
}

cd "$HERE" || exit 2
[ -f docker-compose.yml ] || { echo "run this from the deployment directory (no docker-compose.yml here)" >&2; exit 2; }

step "1. is a release ready to serve?  (deploy-web.sh check)"
if scripts/deploy-web.sh check --root "$ROOT"; then
  pass "a release is staged and current points at it"
else
  fail "no release is ready" "publish one FIRST: switching Caddy now would answer every static path with 404 while the API kept working — a blank dashboard that looks like a Caddy fault"
fi

step "2. does the overlay touch anything but caddy?"
# The `louver` block of the resolved configuration, with and without the
# overlay. Compose decides what to recreate from this, so comparing it is
# comparing the thing that matters.
only_louver() { awk '/^  louver:/ { on = 1; print; next } on && /^  [a-z]/ { exit } on { print }'; }
# shellcheck disable=SC2086
if ! docker compose $BASE --profile https config > /tmp/.wsp-base.yml 2>/tmp/.wsp-base.err; then
  fail "docker compose config (without the overlay)" "$(tail -2 /tmp/.wsp-base.err)"
# shellcheck disable=SC2086
elif ! docker compose $OVER --profile https config > /tmp/.wsp-over.yml 2>/tmp/.wsp-over.err; then
  fail "docker compose config (with the overlay)" "$(tail -2 /tmp/.wsp-over.err)"
else
  only_louver < /tmp/.wsp-base.yml > /tmp/.wsp-base-louver.yml
  only_louver < /tmp/.wsp-over.yml > /tmp/.wsp-over-louver.yml
  if diff -q /tmp/.wsp-base-louver.yml /tmp/.wsp-over-louver.yml > /dev/null; then
    pass "the resolved louver service is byte-identical, so Compose has nothing to recreate there"
  else
    fail "the louver service changed" "$(diff -u /tmp/.wsp-base-louver.yml /tmp/.wsp-over-louver.yml | head -20)"
  fi
  printf '  note  what the overlay does change:\n'
  diff -u /tmp/.wsp-base.yml /tmp/.wsp-over.yml | grep -E '^[+-][^+-]' | sed 's/^/          /'
fi

step "3. what would the apply actually do?  (up --dry-run, which runs nothing)"
LOUVER_CT="$(louver_container)"
# shellcheck disable=SC2086
out="$(docker compose $OVER --profile https up -d --no-deps --dry-run caddy 2>&1)"
printf '%s\n' "$out" | sed 's/^/          /'
if [ -z "$LOUVER_CT" ]; then
  printf '  note  no louver container on this host, so there is nothing it could act on\n'
elif printf '%s' "$out" | grep -F "$LOUVER_CT" | grep -qiE 'recreate|create|start|stop|remove|kill'; then
  fail "the dry run acts on $LOUVER_CT" "this command is supposed to be caddy-only — do not apply it"
else
  pass "the dry run does not act on $LOUVER_CT"
fi

step "4. the louver container as it stands (compare this after the apply)"
docker compose $BASE ps --format '          {{.Name}}  {{.Service}}  created={{.CreatedAt}}  state={{.State}}' 2>/dev/null \
  || docker compose $BASE ps 2>/dev/null | sed 's/^/          /'
printf '  note  after the apply, louver'"'"'s created= must be unchanged. That is the\n'
printf '        acceptance test: if it moved, the broadcasts were restarted.\n'

step "5. would a later bare \`docker compose\` keep the overlay?"
if [ -f .env ] && grep -qE '^COMPOSE_FILE=.*docker-compose\.web\.yml' .env; then
  pass ".env sets COMPOSE_FILE, so even scripts/deploy-vps.sh keeps the split"
  printf '          %s\n' "$(grep -E '^COMPOSE_FILE=' .env)"
else
  fail "nothing pins the overlay" \
"add this line to .env — scripts/deploy-vps.sh ends in a BARE \`docker compose
          --profile https up -d --build\`, which would recreate caddy from
          docker-compose.yml alone and take the split off without telling anyone:

            COMPOSE_FILE=docker-compose.yml:docker-compose.web.yml

          .env is excluded from the deploy tarball and set_env only rewrites
          LOUVER_* keys, so the line survives every redeploy."
fi

step "6. after the step-5 pin, would a bare \`up -d\` recreate louver?"
# A different question from check 3, and the one check 3 cannot answer: that
# dry run is given `--no-deps caddy`, so it could not name louver whatever the
# state of things. This one names no service and passes no --no-deps, which is
# what an operator types when they mean "bring the stack up".
#
# It matters because of the fix for F1. `COMPOSE_FILE=` goes into `.env`, and
# `.env` is louver's own `env_file` (docker-compose.yml). If Compose resolves
# env_file into the service's environment, that edit changes louver's config
# hash, the running container no longer matches it, and the next bare `up -d`
# recreates the one container that is the parent of every FFmpeg on the box.
#
# Asked rather than reasoned about, because whether env_file lands in the hash
# depends on the Compose version, and the dry run is the same machinery the
# real command would use. It runs nothing.
if [ -z "$LOUVER_CT" ]; then
  printf '  note  no louver container to compare against; nothing to warn about\n'
else
  # shellcheck disable=SC2086
  bare="$(docker compose $OVER --profile https up -d --dry-run 2>&1)"
  if printf '%s' "$bare" | grep -F "$LOUVER_CT" | grep -qiE 'recreate|stop|remove|kill'; then
    printf '%s\n' "$bare" | grep -F "$LOUVER_CT" | sed 's/^/          /'
    warn "a bare \`docker compose up -d\` would now recreate $LOUVER_CT" \
"THE CADDY-ONLY APPLY BELOW IS STILL SAFE — it passes --no-deps and acts on
          caddy alone, and check 3 just confirmed that. What this says is that
          FROM NOW ON, \`docker compose up -d\` without --no-deps ends every
          broadcast on this machine, where before this it may have been a no-op.

          There is nothing to fix here: a running container cannot be made to
          match a changed configuration without being recreated, and removing
          the COMPOSE_FILE pin would hand F1 back. Know it, and reach for
          --no-deps:

            docker compose up -d --no-deps caddy      # the front door only

          The next deliberate louver deployment recreates it anyway, which is
          why that belongs in a maintenance window and not in a web release."
  else
    pass "a bare \`up -d\` leaves $LOUVER_CT alone"
  fi
fi

rm -f /tmp/.wsp-base.yml /tmp/.wsp-over.yml /tmp/.wsp-base.err /tmp/.wsp-over.err \
      /tmp/.wsp-base-louver.yml /tmp/.wsp-over-louver.yml

printf '\n'
if [ "$FAIL" -eq 0 ]; then
  if [ "$WARN" -gt 0 ]; then
    printf 'PREFLIGHT OK with %d warning(s) — read them above. They do not block the\n' "$WARN"
    printf 'apply; they are about what other commands do after it.\n\n'
  fi
  printf 'PREFLIGHT OK. The apply command is:\n\n'
  printf '  docker compose -f docker-compose.yml -f docker-compose.web.yml \\\n'
  printf '      --profile https up -d --no-deps caddy\n\n'
  printf 'Then, in this order:\n'
  printf '  docker compose -f docker-compose.yml ps        # louver created= unchanged\n'
  printf '  scripts/web-split-verify.sh https://<domain>   # ARMED\n'
  exit 0
fi
printf 'PREFLIGHT FAILED (%d). Do not apply.\n' "$FAIL"
exit 1
