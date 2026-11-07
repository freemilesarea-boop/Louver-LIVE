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
# WHY THIS RUNS IN KOREA
#
# `openapi.its.go.kr` refuses connections from outside Korea — measured, not
# assumed: the production server in Germany times out on ports 80 and 9443
# while the same request from Seoul answers 200. So the machine that fetches
# the list has to be in Korea. Nothing else does.
#
# In particular the check does not: `cctv-check.py` only calls
# `POST /api/admin/cctv/sources/{id}/check` over HTTPS, and the server is what
# runs FFmpeg against the camera. So this one script does the whole cycle from
# wherever it is, and needs no ssh into production — no key on this box, no way
# from here into the machine that carries the live broadcasts. The only secret
# that leaves is an admin session cookie, over TLS, to the public API.
#
# It takes about three hours, nearly all of it waiting on the server to open
# cameras one at a time. The check is resumable, so a run that is killed
# half-way is continued by the next one rather than restarted.
#
# WHAT IT NEEDS
#
#   --key-file      the ITS API key            (default ~/.its-key)
#   --login-file    an admin e-mail and password, one per line, to mint a
#                   session for this run and revoke it at the end
#                   (default ~/.louver-login, used only if it exists)
#   --cookie-file   a session cookie, if no login file (default ~/.louver-cookie)
#   --base          the API                    (default https://247streams.kr)
#   --import        cctv-import.py             (default beside this script)
#   --check         cctv-check.py              (default beside this script)
#   --no-check      import only, check nothing
#
# Both secrets are files, never arguments: an argument is in the process list
# and in the shell's history, and this runs unattended where nobody would see
# it happen.
set -euo pipefail

KEY_FILE="${HOME}/.its-key"
LOGIN_FILE="${HOME}/.louver-login"
COOKIE_FILE="${HOME}/.louver-cookie"
MINTED=0
BASE="https://247streams.kr"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMPORT="$HERE/cctv-import.py"
CHECK="$HERE/cctv-check.py"
NO_CHECK=0
WORK="${TMPDIR:-/tmp}/cctv-daily-kr"
DRY=0

while [ $# -gt 0 ]; do
  case "$1" in
    --key-file) KEY_FILE="${2:?--key-file needs a path}"; shift 2 ;;
    --cookie-file) COOKIE_FILE="${2:?--cookie-file needs a path}"; shift 2 ;;
    --login-file) LOGIN_FILE="${2:?--login-file needs a path}"; shift 2 ;;
    --base) BASE="${2:?--base needs a URL}"; shift 2 ;;
    --import) IMPORT="${2:?--import needs a path}"; shift 2 ;;
    --check) CHECK="${2:?--check needs a path}"; shift 2 ;;
    --no-check) NO_CHECK=1; shift ;;
    --work) WORK="${2:?--work needs a directory}"; shift 2 ;;
    --dry-run) DRY=1; shift ;;
    -h|--help) sed -n '2,44p' "${BASH_SOURCE[0]}"; exit 0 ;;
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
cleanup() {
  # A session minted for this run is revoked even when the run dies half-way.
  # The alternative is a live admin token sitting on an unattended box until it
  # ages out thirty days later.
  if [ "$MINTED" = 1 ] && [ -r "$WORK/cookie" ]; then
    python3 "$CHECK" --base "$BASE" --cookie-file "$WORK/cookie" --logout \
      >/dev/null 2>&1 || true
  fi
  rm -f "$WORK/cookie" "$WORK/login.json" "$WORK/login.head"
  rmdir "$LOCK" 2>/dev/null || true
}
trap cleanup EXIT

[ -r "$KEY_FILE" ] || die "ITS 인증키 파일을 읽을 수 없습니다: $KEY_FILE"
[ -r "$IMPORT" ] || die "적재 스크립트를 찾을 수 없습니다: $IMPORT"
[ "$NO_CHECK" = 1 ] || [ -r "$CHECK" ] || die "확인 스크립트를 찾을 수 없습니다: $CHECK"
command -v python3 >/dev/null || die "python3 가 없습니다."

KEY="$(tr -d '[:space:]' < "$KEY_FILE")"
[ -n "$KEY" ] || die "$KEY_FILE 이 비어 있습니다."

# --- 0. the session ---------------------------------------------------------
#
# Two ways in, and the first is better on a machine nobody watches.
#
#   a login file   e-mail and password; a session is minted for this run and
#                  revoked when it ends. Nothing long-lived is left behind, and
#                  the job does not die once a month when a cookie ages out.
#   a cookie file  a session copied from a browser. Valid for thirty days, then
#                  the job stops — loudly, but it stops.
#
# Neither is a scoped credential, because the server has none to give: both are
# full admin access. The password's exposure is that it cannot be revoked
# without changing it; the cookie's is that it must be replaced by hand every
# month. On an unattended box the shorter-lived secret wins.
if [ "$DRY" = 1 ]; then
  # --dry-run fetches from ITS and transmits nothing, so it needs no session at
  # all: `cctv-import.py` reads its cookie only after the --dry-run return.
  #
  # Not a tidy-up. This is the preflight `cctv-vps-setup.sh` runs to answer one
  # question — does ITS answer from this box's address? — before any more money
  # is spent on the box. Asking for an admin password to answer it meant a
  # mistyped password failed the check and told you nothing about the address,
  # which is the one thing it exists to tell you.
  say "--dry-run 이므로 세션은 받지 않습니다 (전송하지 않습니다)."
elif [ -r "$LOGIN_FILE" ]; then
  [ -r "$CHECK" ] || die "로그인 방식은 세션을 되돌리기 위해 $CHECK 가 필요합니다."

  # The body is built by python from the file, so the password is never an
  # argument and never reaches the process list or the shell's history.
  ( umask 077
    python3 -c '
import json, sys
lines = [l.strip() for l in open(sys.argv[1], encoding="utf-8").read().splitlines() if l.strip()]
if len(lines) < 2:
    sys.exit("두 줄(메일, 비밀번호)이 필요합니다")
json.dump({"email": lines[0], "password": lines[1]}, open(sys.argv[2], "w"))
' "$LOGIN_FILE" "$WORK/login.json" ) || die "$LOGIN_FILE 을 읽을 수 없습니다. 첫 줄 메일, 둘째 줄 비밀번호."

  code="$(curl -sS -m 60 -o /dev/null -D "$WORK/login.head" -w '%{http_code}' \
    -H 'content-type: application/json' --data-binary "@$WORK/login.json" \
    "$BASE/api/auth/login" || true)"
  rm -f "$WORK/login.json"
  [ "$code" = "200" ] || die "로그인이 HTTP ${code} 입니다. 401 이면 메일·비밀번호가 틀렸고, 429 면 잠시 뒤 다시 하세요."

  ( umask 077
    sed -n 's/.*[Ll]ouver_session=\([^;]*\).*/louver_session=\1/p' "$WORK/login.head" \
      | head -1 > "$WORK/cookie" )
  rm -f "$WORK/login.head"
  [ -s "$WORK/cookie" ] || die "로그인은 됐지만 응답에서 세션 쿠키를 찾지 못했습니다."
  COOKIE_FILE="$WORK/cookie"
  MINTED=1
  say "이번 실행용 세션을 받았습니다. 끝나면 되돌립니다."
else
  [ -r "$COOKIE_FILE" ] || die "로그인 파일($LOGIN_FILE)도, 쿠키 파일($COOKIE_FILE)도 없습니다."
  say "쿠키 파일을 씁니다. 30일마다 손으로 갱신해야 합니다 — $LOGIN_FILE 을 쓰면 그 일이 없어집니다."
fi

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
    --input "$1" --provider "$2" --base "$BASE" \
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

# --- 3. check ---------------------------------------------------------------
#
# In the foreground, so this script's exit code is the whole cycle's verdict
# and the scheduler's log says what happened. Three hours of it is the server
# opening cameras; the lock above is what keeps tomorrow's run from starting on
# top of today's.
if [ "$NO_CHECK" = 1 ]; then
  say "--no-check 이므로 확인은 건너뜁니다. 갱신된 카메라는 아직 고객에게 안 보입니다."
  exit 0
fi

say "확인을 시작합니다. 약 세 시간 걸립니다."
python3 "$CHECK" --base "$BASE" --cookie-file "$COOKIE_FILE" \
  --region KR --sleep 3 \
  "${FILTER[@]}" \
  || die "확인이 끝까지 가지 못했습니다. 이어서 하려면 다시 실행하면 됩니다 — 확인은 안 된 것만 골라서 합니다."

say "끝. 현황:"
python3 "$HERE/cctv-report.py" --base "$BASE" --cookie-file "$COOKIE_FILE" 2>/dev/null \
  || say "  (현황 집계는 건너뜀 — cctv-report.py 를 찾지 못했습니다)"
