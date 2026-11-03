#!/usr/bin/env bash
# Re-collect Korea's traffic cameras, once a day, because their addresses expire.
#
#   scripts/cctv-daily-kr.sh                 # the whole cycle
#   scripts/cctv-daily-kr.sh --dry-run       # fetch and show, write nothing
#
# Korea's ITS hands out a tokenised URL that stops working after about a day.
# A camera imported on Monday is a dead address by Wednesday, and the catalogue
# has no way to know: the row still says "checked, working", because it was —
# on Monday. Customers are then offered cameras that cannot be opened. This is
# the job that stops that happening.
#
# WHY THIS RUNS IN KOREA AND NOT ON THE SERVER
#
# `openapi.its.go.kr` refuses connections from outside Korea — measured, not
# assumed: the production server in Germany times out on ports 80 and 9443
# while the same request from Seoul answers 200. The *video* host
# (`cctvsec.ktict.co.kr`) has no such block, so the split is:
#
#   here (Korea)    fetch the list, import it
#   the server      open each camera and take its picture
#
# The second half is started over ssh and left running; it takes hours and
# there is no reason to hold this script open for it.
#
# WHAT IT NEEDS
#
#   --key-file      the ITS API key            (default ~/.its-key)
#   --cookie-file   an admin session cookie    (default ~/.louver-cookie)
#   --ssh           where the server is        (default root@2.28.225.85)
#   --import        cctv-import.py             (default beside this script)
#   --remote-check  cctv-check.py, on the server (default /root/cctv-check.py)
#
# Both secrets are files, never arguments: an argument is in the process list
# and in the shell's history, and this runs unattended where nobody would see
# it happen.
set -euo pipefail

KEY_FILE="${HOME}/.its-key"
COOKIE_FILE="${HOME}/.louver-cookie"
SSH_TARGET="root@2.28.225.85"
IMPORT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/cctv-import.py"
REMOTE_CHECK="/root/cctv-check.py"
WORK="${TMPDIR:-/tmp}/cctv-daily-kr"
DRY=0

while [ $# -gt 0 ]; do
  case "$1" in
    --key-file) KEY_FILE="${2:?--key-file needs a path}"; shift 2 ;;
    --cookie-file) COOKIE_FILE="${2:?--cookie-file needs a path}"; shift 2 ;;
    --ssh) SSH_TARGET="${2:?--ssh needs a target}"; shift 2 ;;
    --import) IMPORT="${2:?--import needs a path}"; shift 2 ;;
    --remote-check) REMOTE_CHECK="${2:?--remote-check needs a path}"; shift 2 ;;
    --work) WORK="${2:?--work needs a directory}"; shift 2 ;;
    --dry-run) DRY=1; shift ;;
    -h|--help) sed -n '2,36p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "모르는 옵션: $1" >&2; exit 2 ;;
  esac
done

say() { printf '%s  %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*"; }
die() { say "중단: $*" >&2; exit 1; }

# Which cameras are worth offering. The same words for the import and for the
# check, because they have to agree: a camera the import adds but the check
# never looks at stays invisible for ever, and one the check looks at but the
# import no longer refreshes is checked against yesterday's address.
#
# Tunnels are excluded because a tunnel wall is a working camera and a useless
# backdrop, and because the full eleven thousand cannot be re-checked inside
# the lifetime of their own URLs — 11,254 cameras at nine seconds each is
# twenty-eight hours, and the addresses die at twenty-four.
FILTER=(
  --exclude-name 터널
  --include-name 대교
  --include-name 해안
  --include-name 강
  --include-name 휴게소
  --include-name 고개
)

# One at a time. A second copy would re-import on top of the first's rows while
# the server is checking them, and every one of those checks would be against
# an address that had already moved. `mkdir` is the lock because it is atomic
# on every filesystem and needs no flock, which macOS does not ship.
LOCK="${WORK}.lock"
mkdir -p "$WORK"
if ! mkdir "$LOCK" 2>/dev/null; then
  die "이미 실행 중입니다 ($LOCK). 아니라면 그 디렉터리를 지우고 다시 실행하세요."
fi
trap 'rmdir "$LOCK" 2>/dev/null || true' EXIT

[ -r "$KEY_FILE" ] || die "ITS 인증키 파일을 읽을 수 없습니다: $KEY_FILE"
[ -r "$COOKIE_FILE" ] || die "세션 쿠키 파일을 읽을 수 없습니다: $COOKIE_FILE"
[ -r "$IMPORT" ] || die "적재 스크립트를 찾을 수 없습니다: $IMPORT"
command -v python3 >/dev/null || die "python3 가 없습니다."

KEY="$(tr -d '[:space:]' < "$KEY_FILE")"
[ -n "$KEY" ] || die "$KEY_FILE 이 비어 있습니다."

# --- 1. fetch ---------------------------------------------------------------
#
# Checked by how many cameras came back, not by HTTP status alone. A key that
# has been revoked answers 200 with an error document, and a 200 carrying four
# cameras is not Korea's road network; importing it would leave yesterday's
# rows un-refreshed while every line of this script reported success. The floor
# is deliberately far below the real counts (thousands per type) so that it
# catches a broken answer and never a quiet day.
MIN_ROWS=300

fetch() { # fetch <type> <dest>
  local kind="$1" dest="$2" code rows
  code="$(curl -sS -m 180 -o "$dest" -w '%{http_code}' \
    "https://openapi.its.go.kr:9443/cctvInfo?apiKey=${KEY}&type=${kind}&cctvType=4&getType=json&minX=124&maxX=132&minY=33&maxY=39" \
    || true)"
  [ "$code" = "200" ] || die "ITS ${kind} 응답이 HTTP ${code} 입니다. 000 이면 해외 IP 차단이거나 네트워크 문제입니다."
  rows="$(python3 -c '
import json, sys
try:
    d = json.load(open(sys.argv[1], encoding="utf-8"))
except Exception:
    print(0); raise SystemExit(0)
print(len((d.get("response") or {}).get("data") or []))
' "$dest")"
  [ "${rows:-0}" -ge "$MIN_ROWS" ] || die "ITS ${kind} 응답에 카메라가 ${rows:-0} 대뿐입니다. 인증키가 만료·거부됐거나 응답이 깨졌습니다."
  say "ITS ${kind} 받음 — ${rows} 대"
}

fetch ex "$WORK/kr-ex.json"
fetch its "$WORK/kr-its.json"

# --- 2. import --------------------------------------------------------------
#
# The addresses moved, so every row this touches has its check reset and drops
# out of the picker until the server has opened it again. That is the point:
# yesterday's success is not evidence about today's URL.
import_one() { # import_one <input> <provider>
  local extra=()
  [ "$DRY" = 1 ] && extra+=(--dry-run)
  python3 "$IMPORT" --source its \
    --input "$1" --provider "$2" \
    --out-dir "$WORK/out" \
    --cookie-file "$COOKIE_FILE" \
    "${FILTER[@]}" "${extra[@]}" \
    || die "등록 실패 ($2). HTTP 401 이면 세션 쿠키가 만료된 것이니 브라우저에서 다시 받아 $COOKIE_FILE 에 넣어 주세요."
}

import_one "$WORK/kr-ex.json" '국가교통정보센터 (고속도로)'
import_one "$WORK/kr-its.json" '국가교통정보센터 (국도)'

if [ "$DRY" = 1 ]; then
  say "--dry-run 이므로 여기서 멈춥니다. 확인은 시작하지 않았습니다."
  exit 0
fi

# --- 3. check, on the server ------------------------------------------------
#
# Detached on purpose: this is hours of work and the laptop that started it may
# close. Refused if one is already running, because two checkers racing over
# the same rows would each record a verdict the other has already replaced.
say "서버에서 확인을 시작합니다 ($SSH_TARGET)"

# The script's path is substituted into the text piped over ssh, not into any
# command line. That matters for the `pgrep` below: a path mentioned in an
# argument would appear in this very shell's own argv and the guard would match
# itself, so it would always report "already running" and never start anything.
# This exact mistake has been made in this project before.
ssh "$SSH_TARGET" bash -s <<REMOTE || die "서버에서 확인을 시작하지 못했습니다."
set -eu
if [ ! -r "$REMOTE_CHECK" ]; then
  echo "서버에 확인 스크립트가 없습니다: $REMOTE_CHECK" >&2
  exit 1
fi
if pgrep -f cctv-check >/dev/null 2>&1; then
  echo "이미 확인이 돌고 있습니다. 새로 시작하지 않습니다."
  exit 0
fi
nohup python3 "$REMOTE_CHECK" --region KR --sleep 3 \
  --exclude-name 터널 \
  --include-name 대교 --include-name 해안 --include-name 강 \
  --include-name 휴게소 --include-name 고개 \
  > /root/kr-check.log 2>&1 &
sleep 5
head -2 /root/kr-check.log
REMOTE

say "끝. 확인 진행은 다음으로 봅니다:"
say "  ssh $SSH_TARGET 'tail -3 /root/kr-check.log; python3 /root/cctv-report.py'"
