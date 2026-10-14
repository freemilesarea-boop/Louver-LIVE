#!/usr/bin/env bash
# Run the read-only YouTube status audit against the production server.
#
# WHAT THIS DOES NOT DO, by construction:
#   - it does not restart, recreate or signal the louver container
#   - it does not run docker compose up/down/restart/build
#   - it does not send a signal to any FFmpeg process
#   - it does not write to the database (SQLite is opened read-only and
#     enforces it; `scripts/` cannot promise that, SQLite can)
#   - it does not call a YouTube write API
#   - it does not print stream keys, tokens, or (by default) customer ids
#
# The important idea: the audit is a SEPARATE ONE-SHOT PROCESS that reads the
# database out of the Docker volume. It does not have to be deployed, and
# nothing in the running service is replaced or interrupted. Copying a binary
# onto the server is a file copy, not a deployment.
#
# Usage, from a Mac:
#   scripts/youtube-audit.sh --host root@2.28.225.85 --binary ./louver-server
#   scripts/youtube-audit.sh --host root@2.28.225.85 --binary ./louver-server --show-ids
#   scripts/youtube-audit.sh --dry-run --host ...          # print the plan only
#
# Usage, already on the server:
#   scripts/youtube-audit.sh --local --binary /root/louver-server-audit
#
# The binary must be a louver-server built from a commit that has
# `--youtube-audit` (PR #12 + the follow-up). Build it with:
#   cargo build --release -p louver-server --bin louver-server
# on a Linux box, or let CI produce it.

set -euo pipefail

HOST=""
BINARY=""
LOCAL=0
DRY_RUN=0
SHOW_IDS=0
ALLOW_REFRESH=0
PROJECT_DIR="/root/louver-live"
REMOTE_BINARY="/tmp/louver-server-audit"
CONTAINER="louver-live-louver-1"
# Set to bypass Docker volume discovery. For a test server, and for proving
# this script works without a Docker daemon to ask.
DATA_DIR=""
ENV_FILE_OVERRIDE=""

die() { printf '[audit] %s\n' "$*" >&2; exit 1; }
note() { printf '[audit] %s\n' "$*"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --host) HOST="${2:?--host needs a value}"; shift 2 ;;
    --binary) BINARY="${2:?--binary needs a path}"; shift 2 ;;
    --project-dir) PROJECT_DIR="${2:?}"; shift 2 ;;
    --container) CONTAINER="${2:?}"; shift 2 ;;
    --data-dir) DATA_DIR="${2:?--data-dir needs a path}"; shift 2 ;;
    --env-file) ENV_FILE_OVERRIDE="${2:?--env-file needs a path}"; shift 2 ;;
    --local) LOCAL=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --show-ids) SHOW_IDS=1; shift ;;
    --allow-token-refresh) ALLOW_REFRESH=1; shift ;;
    -h|--help) sed -n '2,32p' "$0"; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

AUDIT_FLAGS="--youtube-audit"
[ "$SHOW_IDS" = 1 ] && AUDIT_FLAGS="$AUDIT_FLAGS --show-ids"
[ "$ALLOW_REFRESH" = 1 ] && AUDIT_FLAGS="$AUDIT_FLAGS --allow-token-refresh"

# ---------------------------------------------------------------------------
# The part that runs on the server. Emitted as a script so the same text runs
# whether it arrives over ssh or is executed with --local.
# ---------------------------------------------------------------------------
remote_script() {
  cat <<REMOTE
set -euo pipefail
note() { printf '[audit] %s\n' "\$*"; }
die() { printf '[audit] %s\n' "\$*" >&2; exit 1; }

BIN="${REMOTE_BINARY}"
PROJECT_DIR="${PROJECT_DIR}"
CONTAINER="${CONTAINER}"
DATA_OVERRIDE="${DATA_DIR}"
ENV_OVERRIDE="${ENV_FILE_OVERRIDE}"

[ -x "\$BIN" ] || die "\$BIN is not executable"

# --- is this binary runnable on this host at all? -------------------------
# The image is debian:bookworm-slim; the host may be an older Ubuntu with an
# older glibc, in which case the binary cannot start. Checked rather than
# discovered, because the error otherwise reads like a corrupt download.
need=\$(objdump -T "\$BIN" 2>/dev/null | grep -oE 'GLIBC_[0-9]+\.[0-9]+' | sort -V | tail -1 || true)
have=\$(ldd --version 2>/dev/null | head -1 | grep -oE '[0-9]+\.[0-9]+' | tail -1 || true)
if [ -n "\$need" ] && [ -n "\$have" ]; then
  want=\${need#GLIBC_}
  if [ "\$(printf '%s\n%s\n' "\$want" "\$have" | sort -V | tail -1)" != "\$have" ]; then
    die "this binary needs glibc \$want and the host has \$have.
      Build it against this host's distribution, or build a musl target.
      Do NOT rebuild the production image to work around this."
  fi
  note "glibc ok (binary needs \$want, host has \$have)"
fi

# --- where the database actually is --------------------------------------
# Read-only docker queries. No container is created, started or stopped.
if [ -n "\$DATA_OVERRIDE" ]; then
  DATA="\$DATA_OVERRIDE"
  note "data dir given explicitly, skipping docker discovery"
else
  VOLUME=\$(docker volume ls --format '{{.Name}}' | grep -E 'louver.?data$' | head -1 || true)
  [ -n "\$VOLUME" ] || die "could not find the louver-data volume"
  DATA=\$(docker volume inspect "\$VOLUME" --format '{{.Mountpoint}}')
fi
[ -d "\$DATA" ] || die "data dir \$DATA is not a directory"
[ -f "\$DATA/cloud.db" ] || die "no cloud.db in \$DATA"
note "database: \$DATA/cloud.db"

# Record what is running, so the operator can compare afterwards. Read-only.
if [ -z "\$DATA_OVERRIDE" ]; then
  note "container: \$(docker ps --filter "name=\$CONTAINER" --format '{{.ID}} {{.Status}}' || echo 'not found')"
fi
note "ffmpeg pids before: \$(pgrep -d, -f '[f]fmpeg' || echo none)"

# --- the settings the audit needs, read without printing them ------------
# Only these four. The file is not echoed and nothing is exported globally.
ENV_FILE="\${ENV_OVERRIDE:-\$PROJECT_DIR/.env}"
[ -f "\$ENV_FILE" ] || die "no \$ENV_FILE"
get() { grep -E "^\$1=" "\$ENV_FILE" | tail -1 | cut -d= -f2- | sed 's/^"//;s/"\$//'; }
MK=\$(get LOUVER_MASTER_KEY)
CID=\$(get YOUTUBE_CLIENT_ID)
CSEC=\$(get YOUTUBE_CLIENT_SECRET)
RURI=\$(get YOUTUBE_OAUTH_REDIRECT_URI)
[ -n "\$MK" ] || die "LOUVER_MASTER_KEY is not in \$ENV_FILE"
[ -n "\$CID" ] || die "YOUTUBE_CLIENT_ID is not in \$ENV_FILE"
note "settings loaded (values not printed)"

# --- the audit ------------------------------------------------------------
# One process, reading. It exits on its own; nothing is left behind.
set +e
LOUVER_DATA_DIR="\$DATA" \
LOUVER_MASTER_KEY="\$MK" \
YOUTUBE_CLIENT_ID="\$CID" \
YOUTUBE_CLIENT_SECRET="\$CSEC" \
YOUTUBE_OAUTH_REDIRECT_URI="\$RURI" \
  "\$BIN" ${AUDIT_FLAGS}
rc=\$?
set -e

note "ffmpeg pids after : \$(pgrep -d, -f '[f]fmpeg' || echo none)"
if [ -z "\$DATA_OVERRIDE" ]; then
  note "container: \$(docker ps --filter "name=\$CONTAINER" --format '{{.ID}} {{.Status}}' || echo 'not found')"
fi
note "audit exit=\$rc  (a non-zero code means the audit could not look, not that a broadcast is broken)"
exit \$rc
REMOTE
}

if [ "$DRY_RUN" = 1 ]; then
  note "DRY RUN — nothing is copied and nothing is run."
  note "would copy : ${BINARY:-<--binary not given>} → ${HOST:-<--host not given>}:${REMOTE_BINARY}"
  note "would run  : louver-server ${AUDIT_FLAGS}"
  note "would read : the louver-data volume's cloud.db, read-only"
  echo
  echo "--- the script that would run on the server ---"
  remote_script
  exit 0
fi

[ -n "$BINARY" ] || die "--binary is required (a louver-server with --youtube-audit)"

if [ "$LOCAL" = 1 ]; then
  [ -x "$BINARY" ] || die "$BINARY is not executable"
  REMOTE_BINARY="$BINARY"
  note "running locally against $BINARY"
  remote_script | bash
  exit $?
fi

[ -n "$HOST" ] || die "--host is required unless --local"
[ -f "$BINARY" ] || die "$BINARY does not exist"

note "copying the audit binary to $HOST:$REMOTE_BINARY"
note "this replaces no part of the running service — it is a new file in /tmp"
scp -q "$BINARY" "$HOST:$REMOTE_BINARY"
ssh "$HOST" "chmod 0700 $REMOTE_BINARY"

note "running the audit on $HOST"
remote_script | ssh "$HOST" "bash -s"
rc=$?

# Leaving a copy of the server binary in /tmp on a production box is untidy,
# and it is also a thing somebody could run by accident. Removed.
ssh "$HOST" "rm -f $REMOTE_BINARY" || note "could not remove $REMOTE_BINARY — do it by hand"
exit $rc
