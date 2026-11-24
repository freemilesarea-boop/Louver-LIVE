#!/usr/bin/env bash
#
# Diagnostic deploy — PR #6 (YouTube redirect diagnostics). **Mac에서, repo clone 안에서** 실행.
#
#   cd ~/Louver-LIVE && bash deploy-22dddc6.sh
#
# 대상 commit: 22dddc6fe72cccd22ac5aad1b08ce4ba9723ba22  (parent e692dd8)
#
#   A. local repo 검증 (clean / HEAD / origin / parent)
#   B. predeploy (health / ffmpeg baseline / DB baseline / backup / integrity / 서버 밖 복사)
#   C. scripts/deploy-vps.sh 로 배포 (스크립트 무수정, 공식 명령 그대로)
#   D. postdeploy (health / source SHA / ffmpeg / DB 보존 / redirect instrumentation / panic 스캔)
#
# deploy-e692dd8.sh 에서 바뀐 것은 세 가지뿐입니다:
#   1) WANT / PARENT / 백업 파일명
#   2) D5 — 이번 commit 의 redirect instrumentation 5개 문자열로 교체
#   3) VERIFY_ONLY 에서 D2/D5 를 치명적 실패가 아니라 "배포됨 / 아직 아님" 판정으로
#      바꿨습니다. 배포 **전에** read-only 로 돌리면 production source 는 당연히
#      아직 e692dd8 이고, 그것은 오류가 아니라 사실이기 때문입니다.
# 제품 source code, scripts/deploy-vps.sh, streaming architecture 는 손대지 않습니다.
#
# 모든 필수 단계는 실패하면 즉시 멈춥니다. 이번 배포는 container recreate 로 인한
# 일시 중단을 허용합니다. 배포 후 YouTube 방송 생성은 하지 않습니다 — 이 스크립트도
# 만들지 않습니다.
#
# ---------------------------------------------------------------------------
# `set -e` 감사
# ---------------------------------------------------------------------------
# `set -Eeuo pipefail` 을 씁니다. 빠뜨린 검사가 조용히 통과하는 것보다 멈추는 쪽이
# 안전하기 때문입니다. 대신 nonzero 가 정상인 자리를 전수 확인했습니다:
#
#   * `grep -c` / `grep` 로그 스캔 — 매치가 0이면 1을 반환합니다. 전부 `|| true`
#     로 감싸고, 값을 따로 판정합니다. `pipefail` 때문에 파이프 중간의 nonzero 도
#     파이프라인을 실패시키므로 로그 파이프라인에도 `|| true` 를 붙였습니다.
#   * `X=$(cmd)` — 명령 치환 안의 실패가 대입을 실패시켜 abort 합니다. 값이 빈
#     경우를 스스로 판정하는 자리는 `X=$(cmd || true)` 로 받고 그 다음 줄에서
#     `[ -n "$X" ] || die ...` 로 끊습니다.
#   * `[ ... ] || die` / `if ! cmd` / `a && b || c` — 조건 문맥에서는 `-e` 가
#     비활성화되므로 의도대로 동작합니다. 그래서 판정은 전부 이 형태입니다.
#   * 한글이 바로 뒤따르는 변수는 전부 `${n}건` 형태로 중괄호를 씁니다. 중괄호가
#     없으면 로케일에 따라 한글 바이트가 변수명으로 흡수되어 `set -u` 가
#     "unbound variable" 로 스크립트를 끊습니다 (deploy-e692dd8.sh D5 에서 실제로
#     발생했던 고장입니다).
#   * `die` 는 항상 `||` 뒤에 있어 `-e` 와 충돌하지 않습니다.
#   * `trap ... ERR` + `set -E` — 위 감사를 빠져나간 실패가 있어도 어느 줄에서
#     멈췄는지 찍고 종료합니다. 조용한 성공은 없습니다.
# ---------------------------------------------------------------------------
set -Eeuo pipefail
trap 'printf "\n\033[31m■ STOP: 예상치 못한 실패 — line %s, exit %s\033[0m\n" "$LINENO" "$?" >&2' ERR

# ---------------------------------------------------------------------------
# VERIFY_ONLY — production 을 읽기만 하는 모드
# ---------------------------------------------------------------------------
#   VERIFY_ONLY=1 bash deploy-22dddc6.sh      (또는 --verify-only)
#
# 배포(C)와 백업 생성(B5~B8)을 건너뛰고 A1/A2/A4 + D1~D9 만 돕니다. 쓰는 명령이
# 하나도 실행되지 않습니다: 배포 없음 / 백업 생성 없음 / DB write 없음 /
# OAuth·token refresh 없음 / 방송 생성·삭제 없음 / FFmpeg 조작 없음. DB 질의는
# 전부 `mode=ro` 로 열고 SELECT 와 PRAGMA table_info 만 씁니다.
#
# 두 가지 용도가 있습니다:
#   * 배포 **전** pre-flight — 현재 production 상태를 확인합니다. 이때 D2/D5 는
#     "아직 22dddc6 아님" 으로 나오는 것이 정상이며, 그래서 이 모드에서는 둘을
#     치명적 실패로 보지 않고 판정만 출력합니다.
#   * 배포 **후** 검증 마무리 — 스크립트를 그냥 다시 돌리면 재배포가 되므로
#     그것을 막기 위한 모드입니다. 이때는 배포 전 실측치를 환경변수로 주입하세요:
#       BASE_FF=.. BASE_B=.. BASE_U=.. BASE_M=.. BASE_C=.. BASE_WANT_RUN=.. \
#         VERIFY_ONLY=1 bash deploy-22dddc6.sh
#     주입하지 않으면 B4 에서 **실측한 현재 값** 을 기준으로 삼습니다. 오래된
#     기본값을 박아 두면 엉뚱한 비교로 거짓 실패를 낼 수 있어서 기본값을 두지
#     않았습니다.
VERIFY_ONLY=${VERIFY_ONLY:-0}
case "${1:-}" in --verify-only) VERIFY_ONLY=1 ;; esac

SRV=${SRV:-root@2.28.225.85}
WANT=22dddc6fe72cccd22ac5aad1b08ce4ba9723ba22
PARENT=e692dd8146340d1ae77d8a7ff4b056895bd9c452
BRANCH=claude/intelligent-lamport-gansqi
SELF_NAME=$(basename "$0")
TS=$(date +%Y%m%d-%H%M%S)
BK="/root/backups/cloud-pre-22dddc6-$TS.db"
LOCAL_BK="$HOME/247streams-backup"
HEALTH_URL="https://247streams.kr/health"

step() { printf '\n\033[1m▸ %s\033[0m\n' "$1"; }
die()  { printf '\n\033[31m■ STOP: %s\033[0m\n' "$1" >&2; exit 1; }
ok()   { printf '  \033[32mok\033[0m   %s\n' "$1"; }
bad()  { printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
warn() { printf '  \033[33m··\033[0m   %s\n' "$1"; }

# ---------------------------------------------------------------------------
# 민감정보 차단
# ---------------------------------------------------------------------------
# 서버에서 온 모든 텍스트는 이 필터를 통과합니다. 치환만으로는 마크업·따옴표에
# 붙은 키를 놓칠 수 있으므로, 먼저 **ingest/키 관련 줄 자체를 버리고** 그 다음
# 치환합니다. 진단 정보를 조금 잃는 편이 키를 흘리는 것보다 낫습니다.
#   - 1단계: rtmp(s) URL, streamName/stream_name/streamKey/ingestion* 을 포함한
#            줄, FFmpeg argv 로 보이는 줄(`-f flv`, `ffmpeg -`) 을 삭제
#   - 2단계: 남은 줄에서 rtmp URL, key=..., ya29.(access), 1//(refresh),
#            GOCSPX-(client secret), Bearer <...>, 그리고 YouTube 스트림 키 모양
#            (4자 이상 그룹이 하이픈으로 4개 이상) 을 치환
scrub() {
  sed -E \
    -e '/rtmps?:\/\//d' \
    -e '/[Ss]tream[_]?[NnKk]/d' \
    -e '/ingestion/Id' \
    -e '/-f +flv/d' \
    -e '/ffmpeg +-/d' \
  | sed -E \
    -e 's#rtmps?://[^[:space:]"]*#rtmps://[REDACTED]#g' \
    -e 's/(key|KEY|secret|SECRET|token|TOKEN)=[^[:space:]&"]*/\1=[REDACTED]/g' \
    -e 's/[Bb]earer +[A-Za-z0-9._~+\/-]{10,}=*/Bearer [REDACTED]/g' \
    -e 's#ya29\.[A-Za-z0-9._-]+#[ACCESS_TOKEN_REDACTED]#g' \
    -e 's#1//[A-Za-z0-9._-]{20,}#[REFRESH_TOKEN_REDACTED]#g' \
    -e 's#GOCSPX-[A-Za-z0-9._-]+#[CLIENT_SECRET_REDACTED]#g' \
    -e 's#[A-Za-z0-9]{4,}(-[A-Za-z0-9]{4,}){3,}#[KEY_REDACTED]#g'
}

rssh() { ssh -o BatchMode=yes -o ConnectTimeout=15 "$SRV" "$@"; }

# `key=value` 줄 묶음에서 하나를 꺼냅니다. 없으면 빈 문자열.
#
# `… | sed | head -1` 로 쓰지 않는 이유: `head` 가 먼저 끝나면 앞 단계가 SIGPIPE
# 로 죽고, `pipefail` + `set -e` 아래서는 그 nonzero 가 명령 치환을 실패시켜
# 스크립트를 멈춥니다. 읽기만 하는 함수가 그런 식으로 배포를 끊어서는 안 됩니다.
val() {
  local line
  while IFS= read -r line; do
    case "$line" in
      "$2="*) printf '%s' "${line#*=}" | tr -d ' \r'; return 0 ;;
    esac
  done <<<"$1"
  return 0
}

# 정수인지. 비어 있거나 숫자가 아니면 실패 — "조회 실패" 를 0 으로 오해하지
# 않기 위한 장치입니다.
# 원격 출력에서 스칼라 하나를 읽을 때 `| tail -n1 |` 을 거치는 이유: 로그인
# 셸이 프로필에서 한 줄이라도 찍으면 `tr -d` 가 그것을 값에 붙여 버리고,
# 그러면 멀쩡한 조회가 "조회 실패" 로 판정됩니다. 값은 전부 한 줄이므로
# 마지막 줄만 취하는 것이 정확하며, 깨끗한 출력에서는 결과가 같습니다.
is_int() { case "${1:-}" in ''|*[!0-9]*) return 1 ;; *) return 0 ;; esac; }

# 배포 전/후 DB 요약을 뽑습니다. 값이 아니라 개수와 스키마만 읽고, token /
# credential / cctv_url 값 자체는 어떤 질의에도 들어가지 않습니다.
db_summary() {
  rssh "python3 - <<'PY'
import sqlite3, glob, sys
g = sorted(glob.glob('/var/lib/docker/volumes/*louver-data/_data/cloud.db'))
if not g:
    print('db_found=0'); sys.exit(0)
c = sqlite3.connect('file:%s?mode=ro' % g[0], uri=True)
print('db_found=1')
def n(q):
    try: return c.execute(q).fetchone()[0]
    except sqlite3.Error: return -1
print('broadcasts=%d' % n('SELECT COUNT(*) FROM broadcasts'))
print('wanting_run=%d' % n(\"SELECT COUNT(*) FROM broadcasts WHERE desired_state='running'\"))
print('users=%d' % n('SELECT COUNT(*) FROM users'))
print('media=%d' % n('SELECT COUNT(*) FROM media'))
cols = [r[1] for r in c.execute('PRAGMA table_info(broadcasts)')]
print('cctv_url_column=%d' % (1 if 'cctv_url' in cols else 0))
if 'cctv_url' in cols:
    print('cctv_configured=%d' % n(\"SELECT COUNT(*) FROM broadcasts WHERE cctv_url IS NOT NULL AND TRIM(cctv_url)<>''\"))
else:
    print('cctv_configured=-1')
PY"
}

# =======================================================================
# A. local repo
# =======================================================================
step "A1. git 저장소 / 배포 스크립트"
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || die "git 저장소 안에서 실행해 주세요."
[ -x scripts/deploy-vps.sh ] || die "scripts/deploy-vps.sh 가 없습니다 (저장소 루트에서 실행)."
ok "저장소"

step "A2. origin 최신화 (default branch)"
git fetch origin "$BRANCH" || die "fetch 실패"
git checkout "$BRANCH" || die "checkout 실패"
git pull --ff-only origin "$BRANCH" || die "fast-forward pull 실패 (로컬 분기 커밋 확인)"
ok "fetch + ff-only pull"

step "A3. working tree clean (이 스크립트 자신만 예외)"
# 이 스크립트가 untracked 인 것은 허용하되 그 외 tracked/untracked 변경은 전부
# 차단합니다. git add / commit 도, .gitignore 수정도 하지 않습니다 — 허용은 이
# 검사 안에서만 이뤄집니다. 참고: deploy-vps.sh 는 working tree 를 그대로
# 전송하므로 이 파일도 서버로 복사됩니다(동작에 영향 없음).
#
# `git status --porcelain | grep … || true` 로 한 번에 쓰지 않는 이유: `git status`
# 자체가 실패해도 `|| true` 가 그 nonzero 를 먹어 치워 빈 결과가 되고, 그러면
# dirty tree 를 clean 으로 **오판**하게 됩니다. 그래서 상태를 먼저 받아 끊습니다.
ST=$(git status --porcelain) || die "git status 실행에 실패했습니다. 상태를 확인할 수 없으니 배포하지 않습니다."
DIRT=$(printf '%s\n' "$ST" | grep -v -x -F "?? $SELF_NAME" || true)
DIRT=$(printf '%s' "$DIRT" | sed '/^$/d' || true)
if [ -n "$DIRT" ]; then
  printf '%s\n' "$DIRT" | sed 's/^/    /'
  die "이 스크립트 외의 변경이 있습니다. 배포 전 정리해 주세요 (배포는 working tree 를 그대로 전송합니다)."
fi
SELFLINE=$(printf '%s\n' "$ST" | grep -x -F "?? $SELF_NAME" || true)
if [ -n "$SELFLINE" ]; then ok "clean — 예외 1건: '$SELF_NAME' (untracked, 허용)"
else ok "clean — 예외 없음 ('$SELF_NAME' 은 tracked 이거나 ignore 상태)"; fi

step "A4. HEAD / origin / parent"
H=$(git rev-parse HEAD); O=$(git rev-parse "origin/$BRANCH"); P=$(git rev-parse HEAD^)
echo "  HEAD   = $H"
echo "  origin = $O"
echo "  HEAD^  = $P"
[ "$H" = "$WANT" ]   || die "HEAD 가 배포 대상($WANT)과 다릅니다."
[ "$O" = "$WANT" ]   || die "origin default branch 가 배포 대상과 다릅니다."
[ "$P" = "$PARENT" ] || die "parent 가 $PARENT 가 아닙니다."
ok "세 값 모두 일치 (target 22dddc6 / parent e692dd8)"

# =======================================================================
# B. predeploy   (VERIFY_ONLY 에서는 B1~B4 만, 백업·배포는 건너뜀)
# =======================================================================
if [ "$VERIFY_ONLY" = "1" ]; then
  step "VERIFY_ONLY — 배포와 백업 생성을 건너뜁니다"
  echo "  이 모드는 production 을 읽기만 합니다. 재배포하지 않고, 쓰는 명령이 없습니다."
  echo "  (배포 없음 / 백업 생성 없음 / DB write 없음 / OAuth·refresh 없음 /"
  echo "   방송 생성·삭제 없음 / FFmpeg 조작 없음)"
fi

step "B1. production HTTPS /health"
PRE_BODY=$(mktemp)
PRE_CODE=$(curl -s -m 25 -o "$PRE_BODY" -w '%{http_code}' "$HEALTH_URL" || true)
echo "  http_code = ${PRE_CODE:-000}"
printf '  body      = %s\n' "$(head -c 300 "$PRE_BODY" 2>/dev/null | scrub | tr '\n' ' ' || true)"
rm -f "$PRE_BODY"
[ "${PRE_CODE:-000}" = "200" ] || die "predeploy /health 가 200 이 아닙니다 (${PRE_CODE:-000})."
ok "/health 200"

step "B2. SSH 도달 + FFmpeg baseline"
# 개수만 셉니다. `pgrep -af ffmpeg` 는 argv 를 찍어 stream key / RTMP URL 이
# 그대로 노출되므로 이 스크립트 어디에서도 쓰지 않습니다.
rssh 'true' || die "$SRV 에 SSH 접속할 수 없습니다."
PRE_FF=$(rssh 'pgrep -x ffmpeg | wc -l' 2>/dev/null | tail -n1 | tr -d ' \r\n' || true)
echo "  pgrep -x ffmpeg | wc -l = ${PRE_FF:-(조회실패)}"
is_int "${PRE_FF:-}" || die "FFmpeg 개수를 숫자로 조회하지 못했습니다 (빈 값을 0 으로 간주하지 않습니다)."
ok "baseline = $PRE_FF"

step "B3. cloud.db 경로"
DBPATH=$(rssh "python3 -c \"import glob; g=sorted(glob.glob('/var/lib/docker/volumes/*louver-data/_data/cloud.db')); print(g[0] if g else '')\"" 2>/dev/null | tail -n1 | tr -d ' \r\n' || true)
echo "  cloud.db = ${DBPATH:-(조회실패)}"
case "${DBPATH:-}" in
  /var/lib/docker/volumes/*louver-data/_data/cloud.db) ok "경로 확인" ;;
  *) die "cloud.db 를 찾지 못했습니다." ;;
esac

step "B4. 배포 전 DB baseline (SELECT only, 값 미출력)"
PRE_DB=$(db_summary || true)
[ -n "$PRE_DB" ] || die "배포 전 DB 요약을 얻지 못했습니다."
printf '%s\n' "$PRE_DB" | scrub | sed 's/^/    /'
[ "$(val "$PRE_DB" db_found)" = "1" ] || die "배포 전 cloud.db 를 열지 못했습니다."
PRE_B=$(val "$PRE_DB" broadcasts);   PRE_U=$(val "$PRE_DB" users)
PRE_M=$(val "$PRE_DB" media);        PRE_C=$(val "$PRE_DB" cctv_configured)
PRE_COL=$(val "$PRE_DB" cctv_url_column)
PRE_WR=$(val "$PRE_DB" wanting_run)
for v in "$PRE_B" "$PRE_U" "$PRE_M"; do
  is_int "$v" || die "배포 전 DB 카운트를 읽지 못했습니다 (broadcasts/users/media)."
done
[ "$PRE_COL" = "1" ] || die "배포 전 broadcasts.cctv_url column 이 없습니다. 스키마를 확인해 주세요."
ok "broadcasts=$PRE_B wanting_run=$PRE_WR users=$PRE_U media=$PRE_M cctv_url_column=$PRE_COL cctv_configured=$PRE_C"

if [ "$VERIFY_ONLY" = "1" ]; then
  # 배포가 없으므로 백업(B5~B8)과 배포(C)는 건너뜁니다. 기준치는 환경변수로
  # 주입된 값이 있으면 그것을, 없으면 방금 B2/B4 에서 **실측한 값** 을 씁니다.
  PRE_FF=${BASE_FF:-$PRE_FF}
  PRE_B=${BASE_B:-$PRE_B}
  PRE_U=${BASE_U:-$PRE_U}
  PRE_M=${BASE_M:-$PRE_M}
  PRE_C=${BASE_C:-$PRE_C}
  echo "  기준치: FFmpeg=$PRE_FF broadcasts=$PRE_B users=$PRE_U media=$PRE_M cctv_configured=$PRE_C"
  ok "B5~B8(백업 생성) 및 C(배포) 건너뜀 — VERIFY_ONLY"
else

step "B5. cloud.db online backup -> $BK"
# sqlite3 online backup API (`s.backup(d)`): 송출 중에도 일관된 사본. 파일 cp 아님.
# 호스트의 python3 표준 sqlite3 모듈만 사용합니다 — third-party 이미지를 끌어오지
# 않는 쪽이 공급망 측면에서 안전합니다. (호스트에 python3 가 없을 때의 대안인
# `keinos/sqlite3:latest` 는 Anthropic·이 프로젝트와 무관한 THIRD-PARTY 이미지이며
# 이 스크립트는 사용하지 않습니다.)
BK_OUT=$(rssh "mkdir -p /root/backups && python3 - <<'PY'
import sqlite3, glob, os
src = sorted(glob.glob('/var/lib/docker/volumes/*louver-data/_data/cloud.db'))[0]
dst = '$BK'
s = sqlite3.connect('file:%s?mode=ro' % src, uri=True)
d = sqlite3.connect(dst)
with d: s.backup(d)
d.close(); s.close()
print('backup_bytes=%d' % os.path.getsize(dst))
PY" 2>&1 || true)
printf '%s\n' "$BK_OUT" | scrub | sed 's/^/  /'
printf '%s' "$BK_OUT" | grep -q 'backup_bytes=' || die "backup 생성에 실패했습니다."
ok "backup 생성"

step "B6. backup / live 크기"
BK_SIZE=$(rssh "stat -c %s '$BK'" 2>/dev/null | tail -n1 | tr -d ' \r\n' || true)
LIVE_SIZE=$(rssh "stat -c %s '$DBPATH'" 2>/dev/null | tail -n1 | tr -d ' \r\n' || true)
echo "  backup : $BK  (${BK_SIZE:-?} bytes)"
echo "  live   : $DBPATH  (${LIVE_SIZE:-?} bytes)"
is_int "${BK_SIZE:-}" || die "backup 크기를 조회하지 못했습니다."
[ "$BK_SIZE" -gt 0 ] || die "backup 크기가 0 입니다."
ok "크기 확인"

step "B7. integrity_check (live + backup, 둘 다 읽기 전용)"
LIVE_CHK=$(rssh "python3 -c \"import sqlite3;print(sqlite3.connect('file:$DBPATH?mode=ro', uri=True).execute('PRAGMA integrity_check').fetchone()[0])\"" 2>/dev/null | tail -n1 | tr -d ' \r\n' || true)
BK_CHK=$(rssh "python3 -c \"import sqlite3;print(sqlite3.connect('file:$BK?mode=ro', uri=True).execute('PRAGMA integrity_check').fetchone()[0])\"" 2>/dev/null | tail -n1 | tr -d ' \r\n' || true)
echo "  live   integrity_check = ${LIVE_CHK:-(없음)}"
echo "  backup integrity_check = ${BK_CHK:-(없음)}"
[ "${LIVE_CHK:-}" = "ok" ] || die "live DB integrity_check 가 ok 가 아닙니다."
[ "${BK_CHK:-}" = "ok" ]   || die "backup integrity_check 가 ok 가 아닙니다."
ok "둘 다 ok"

step "B8. backup 을 서버 밖으로 복사 (실패 시 배포 중단)"
# 서버 안에만 있는 백업은 서버가 문제일 때 쓸 수 없습니다.
# 참고: credentials 테이블의 secret 은 sealed 상태이고 master key 는 서버를
# 떠나지 않으므로, 이 사본만으로 secret 을 풀 수는 없습니다.
mkdir -p "$LOCAL_BK" || die "로컬 backup 디렉터리를 만들 수 없습니다: $LOCAL_BK"
scp -q "$SRV:$BK" "$LOCAL_BK/" || die "backup 을 서버 밖으로 복사하지 못했습니다. 배포를 진행하지 않습니다."
LOCAL_FILE="$LOCAL_BK/$(basename "$BK")"
[ -f "$LOCAL_FILE" ] || die "복사된 backup 파일이 없습니다: $LOCAL_FILE"
LOCAL_SIZE=$(wc -c < "$LOCAL_FILE" | tr -d ' ')
echo "  local  : $LOCAL_FILE  ($LOCAL_SIZE bytes)"
[ "$LOCAL_SIZE" = "$BK_SIZE" ] || die "복사된 backup 크기가 서버와 다릅니다 (server=$BK_SIZE local=$LOCAL_SIZE)."
ok "서버 밖 복사 + 크기 일치"
ok "predeploy 전체 통과"

# =======================================================================
# C. 배포 — 공식 명령 그대로. deploy-vps.sh 는 수정하지 않습니다.
# =======================================================================
step "C. production 배포 (scripts/deploy-vps.sh, 무수정)"
echo "  container recreate 로 실행 중 방송이 일시 중단되고 boot recovery 됩니다 (이번 배포에서 허용)."
scripts/deploy-vps.sh "$SRV" \
  --domain 247streams.kr \
  --email freemilesarea@gmail.com \
  --user freemilesarea@gmail.com || die "배포 실패 — 위 출력의 마지막 오류를 그대로 보고해 주세요 (임의 수정 금지)."
ok "배포 명령 완료"
fi   # VERIFY_ONLY 분기 끝

# =======================================================================
# D. postdeploy
# =======================================================================
step "D1. HTTPS health (200 아니면 실패)"
POST_CODE=000
POST_BODY=$(mktemp)
for _ in $(seq 1 60); do
  POST_CODE=$(curl -s -m 10 -o "$POST_BODY" -w '%{http_code}' "$HEALTH_URL" || true)
  [ "${POST_CODE:-000}" = "200" ] && break
  sleep 5
done
echo "  external http_code = ${POST_CODE:-000}"
printf '  body = %s\n' "$(head -c 300 "$POST_BODY" 2>/dev/null | scrub | tr '\n' ' ' || true)"
rm -f "$POST_BODY"
[ "${POST_CODE:-000}" = "200" ] || die "배포 후 외부 /health 가 200 이 아닙니다 (${POST_CODE:-000}). 여기서 멈춥니다."
ok "외부 /health 200"
echo "  --- internal health ---"
rssh 'cd louver-live && curl -s localhost:8080/health' 2>/dev/null | scrub | sed 's/^/  /' || true
echo

step "D2. production source SHA (목표: 22dddc6)"
# 서버의 /root/louver-live 는 git 저장소가 아니므로 파일 지문으로 비교합니다.
# VERIFY_ONLY 에서는 배포 전일 수 있으므로 치명적 실패가 아니라 판정만 냅니다.
MIS=0
for f in crates/louver-core/src/youtube/http.rs \
         crates/louver-cloud/src/youtube.rs \
         crates/louver-cloud/src/cctv.rs \
         crates/louver-core/src/streaming/ffmpeg.rs; do
  L=$(git show "$WANT:$f" | shasum -a 256 | awk '{print $1}')
  R=$(rssh "sha256sum louver-live/$f 2>/dev/null | awk '{print \$1}'" 2>/dev/null | tail -n1 | tr -d ' \r\n' || true)
  if [ -n "$R" ] && [ "$L" = "$R" ]; then
    ok "$f"
  else
    bad "$f"; echo "    local(22dddc6) $L"; echo "    server         ${R:-(조회실패)}"; MIS=$((MIS+1))
  fi
done
if [ "$MIS" -eq 0 ]; then
  ok "4개 파일 모두 일치 — PRODUCTION SOURCE = 22dddc6 YES"
elif [ "$VERIFY_ONLY" = "1" ]; then
  warn "PRODUCTION SOURCE = 22dddc6 NO (불일치 ${MIS}건) — 배포 전 read-only 점검이면 정상입니다."
else
  die "production source 지문 불일치 ${MIS}건. 배포된 코드가 $WANT 가 아닙니다."
fi

step "D3. FFmpeg process count  (before: $PRE_FF)"
# 단순 출력이 아니라 판정. before>0 이었다면 recovery 로 다시 떠야 합니다(최대
# 4분 대기). before==0 이면 강제하지 않습니다 — 띄울 방송이 없었던 것이고,
# 스케줄이 그사이 시작했을 수도 있습니다. 여기서도 개수만 셉니다.
POST_FF=""
for _ in $(seq 1 24); do
  POST_FF=$(rssh 'pgrep -x ffmpeg | wc -l' 2>/dev/null | tail -n1 | tr -d ' \r\n' || true)
  if is_int "${POST_FF:-}"; then
    if [ "$PRE_FF" -eq 0 ] || [ "$POST_FF" -gt 0 ]; then break; fi
  fi
  sleep 10
done
echo "  before -> after : $PRE_FF -> ${POST_FF:-(조회실패)}"
is_int "${POST_FF:-}" || die "배포 후 FFmpeg 개수를 숫자로 조회하지 못했습니다."
if [ "$PRE_FF" -gt 0 ]; then
  [ "$POST_FF" -gt 0 ] || die "배포 전 ${PRE_FF}개가 돌고 있었는데 4분 안에 하나도 복구되지 않았습니다. 방송 생성 단계로 넘어가지 말고 멈춥니다."
  ok "recovery 확인 (before>0, after=$POST_FF)"
else
  ok "before=0 — 복구 대상 없음 (after=$POST_FF, 판정 없음)"
fi

step "D4. DB 보존 / cctv_url column / CCTV configured (판정 포함)"
POST_DB=$(db_summary || true)
[ -n "$POST_DB" ] || die "배포 후 DB 요약을 얻지 못했습니다."
printf '%s\n' "$POST_DB" | scrub | sed 's/^/    /'
[ "$(val "$POST_DB" db_found)" = "1" ] || die "배포 후 cloud.db 를 열지 못했습니다."
POST_B=$(val "$POST_DB" broadcasts); POST_U=$(val "$POST_DB" users)
POST_M=$(val "$POST_DB" media);      POST_C=$(val "$POST_DB" cctv_configured)
POST_COL=$(val "$POST_DB" cctv_url_column)
for v in "$POST_B" "$POST_U" "$POST_M"; do
  is_int "$v" || die "배포 후 DB 카운트를 읽지 못했습니다."
done
echo "  broadcasts $PRE_B -> $POST_B | users $PRE_U -> $POST_U | media $PRE_M -> $POST_M | cctv_configured $PRE_C -> $POST_C"
# 동일 수를 강제하지 않습니다(방송은 정상적으로 늘거나 줄 수 있음). 다만 **감소**는
# 데이터 유실 신호로 보고 멈춥니다. 배포 중 사용자가 실제로 삭제했다면 이 검사도
# 걸리므로, 그때는 무시하지 말고 손으로 확인한 뒤 판단해 주세요.
[ "$POST_COL" = "1" ] || die "broadcasts.cctv_url column 이 없습니다. 스키마가 변경됐습니다."
[ "$POST_U" -ge "$PRE_U" ] || die "users 수가 줄었습니다 ($PRE_U -> $POST_U). 데이터 유실 가능 — 멈춥니다."
[ "$POST_M" -ge "$PRE_M" ] || die "media 수가 줄었습니다 ($PRE_M -> $POST_M). 데이터 유실 가능 — 멈춥니다."
[ "$POST_B" -ge "$PRE_B" ] || die "broadcasts 수가 줄었습니다 ($PRE_B -> $POST_B). 데이터 유실 가능 — 멈춥니다."
if is_int "$PRE_C" && is_int "$POST_C"; then
  [ "$POST_C" -ge "$PRE_C" ] || die "cctv_url 이 설정된 방송 수가 줄었습니다 ($PRE_C -> $POST_C)."
fi
ok "column 유지 + 감소 없음"

step "D5. redirect instrumentation 반영 (source 5건 + binary, 판정 포함)"
# 이번 commit 이 production 에 실제로 들어갔는지를 가리는 유일한 근거입니다.
# **개수만** 읽습니다 — `grep -c` 는 일치한 줄을 출력하지 않으므로 주변 문자열이
# 함께 흘러나올 일이 없습니다. Authorization / token / secret 은 어떤 패턴에도
# 들어가지 않습니다.
INS_FAIL=0
check_src() { # $1=설명 $2=파일 $3=고정 문자열
  local n
  # `grep -cF` — 괄호와 `=` 를 정규식이 아니라 그대로 찾습니다.
  n=$(rssh "grep -cF -- '$3' louver-live/$2 2>/dev/null || true" 2>/dev/null | tail -n1 | tr -d ' \r\n' || true)
  # 한글이 바로 뒤따르는 변수는 반드시 중괄호를 씁니다. 중괄호가 없으면 뒤따르는
  # 한글 바이트가 변수명으로 흡수될 수 있습니다. 로케일에 따라 `isalnum()` 이 상위
  # 바이트를 영숫자로 분류하는 환경(macOS bash 3.2 등)에서는 변수명이
  # `n<EA><B1><B4>` 가 되고, `set -u` 가 "unbound variable" 로 스크립트를 끊습니다.
  # deploy-e692dd8.sh 의 D5 가 실제로 이 자리에서 중단됐습니다.
  if is_int "${n:-}" && [ "$n" -ge 1 ]; then ok "$1 (source, ${n}건)"
  else bad "$1 (source, ${n:-조회실패}건)"; INS_FAIL=$((INS_FAIL+1)); fi
}
HTTP_RS=crates/louver-core/src/youtube/http.rs
check_src "save_redirect_history(true)" "$HTTP_RS" 'save_redirect_history(true)'
check_src "get_redirect_history"        "$HTTP_RS" 'get_redirect_history'
check_src "redirects= 로그 필드"         "$HTTP_RS" 'redirects='
check_src "redirect_host= 로그 필드"     "$HTTP_RS" 'redirect_host='
check_src "redirect_auth_policy=NEVER"  "$HTTP_RS" 'redirect_auth_policy=NEVER'

# 실행 바이너리 쪽. `grep -ac` 로 바이너리를 텍스트처럼 읽고 **개수만** 받습니다.
# strings 전체 dump 는 하지 않습니다 — 그러면 바이너리에 박힌 다른 문자열까지
# 쏟아지고, 거기에 민감한 것이 섞여 있을 여지가 생깁니다.
#
# 찾는 대상은 `format!` 이 .rodata 에 남기는 조각뿐입니다. `save_redirect_history`
# 와 `get_redirect_history` 는 **코드**이고 문자열이 아니므로 release 바이너리에서
# 안전하게 관측할 수 없습니다 — 그래서 추측하지 않고 확인 대상에서 뺐습니다.
check_bin() { # $1=설명 $2=고정 문자열(grep BRE 에서 특수문자 없는 것만)
  local n
  n=$(rssh "cd louver-live && docker compose exec -T louver sh -lc 'b=\$(command -v louver-server); [ -n \"\$b\" ] && grep -ac \"$2\" \"\$b\" || echo 0'" 2>/dev/null | tail -n1 | tr -d ' \r\n' || true)
  if is_int "${n:-}" && [ "$n" -ge 1 ]; then ok "$1 (binary, ${n}건)"
  else bad "$1 (binary, ${n:-조회실패}건)"; INS_FAIL=$((INS_FAIL+1)); fi
}
check_bin "로그 접두사"                  '\[louver\]\[youtube\]\[http\]'
check_bin "redirect_auth_policy=NEVER"  'redirect_auth_policy=NEVER'
check_bin "redirects= 로그 필드"         'redirects='

[ "$INS_FAIL" -eq 0 ] || {
  if [ "$VERIFY_ONLY" = "1" ]; then
    warn "REDIRECT INSTRUMENTATION = NO (실패 ${INS_FAIL}건) — 배포 전 read-only 점검이면 정상입니다."
  else
    die "instrumentation 확인 실패 ${INS_FAIL}건. 배포된 바이너리에 redirect 진단 코드가 없을 수 있습니다 — 방송 생성 단계로 넘어가지 말고 멈춥니다."
  fi
}
[ "$INS_FAIL" -ne 0 ] || ok "REDIRECT INSTRUMENTATION = YES (source 5건 + binary 3건)"

step "D6. 예상 외 panic / crash / recovery failure 스캔"
BAD_LOG=$(rssh 'cd louver-live && docker compose logs --since 20m louver' 2>/dev/null \
  | grep -iE "panicked at|SIGSEGV|SIGABRT|fatal runtime|thread .* panicked|복구 실패|recovery failed" \
  | scrub | tail -20 || true)
if [ -n "$BAD_LOG" ]; then
  printf '%s\n' "$BAD_LOG" | sed 's/^/    /'
  die "로그에 panic/crash/recovery 실패가 있습니다. 방송 생성 단계로 넘어가지 말고 위 내용을 보고해 주세요."
fi
ok "panic/crash/recovery 실패 없음"

step "D7. 최근 louver logs (참고) — redirect 필드 포함"
rssh 'cd louver-live && docker compose logs --since 20m louver' 2>/dev/null \
  | grep -iE 'recover|boot|복구|START|STOP|PREPARING|error|오류|youtube|oauth|redirects=' \
  | scrub | tail -40 | sed 's/^/  /' || true

step "D8. louver-server --diagnose (참고)"
# --diagnose 는 제품이 스스로 redact 한 형태로만 출력합니다
# (refresh_token=[REDACTED len=N sha256:8hex], access_token=[REDACTED]).
# 그 위에 scrub 가 한 번 더 걸립니다.
rssh 'cd louver-live && docker compose exec -T louver louver-server --diagnose' 2>/dev/null \
  | scrub | head -60 | sed 's/^/  /' || echo "  (--diagnose 경로 사용 불가)"

step "D9. FFmpeg 수 변화의 출처 (비민감 메타데이터 + redacted error 만)"
# `pgrep -x ffmpeg | wc -l` 은 개수만 줍니다. 어느 방송이 안 떴는지는 DB 의
# 상태 컬럼과 요약된 오류 문구로만 좁힙니다. argv / stream key / RTMP URL /
# token 은 어떤 질의에도 들어가지 않습니다.
POST_WR=$(val "$POST_DB" wanting_run)
echo "  wanting_run(desired_state=running) = ${POST_WR:-?}   /   FFmpeg = ${POST_FF}"
if is_int "${POST_WR:-}" && [ "$POST_FF" -lt "$POST_WR" ]; then
  printf '  \033[33m차이 %s개\033[0m — 아래에서 desired=running 인데 runtime 이 running 이 아닌 방송을 찾습니다.\n' "$((POST_WR - POST_FF))"
fi
rssh "python3 - <<'PY'
import sqlite3, glob
g = sorted(glob.glob('/var/lib/docker/volumes/*louver-data/_data/cloud.db'))
c = sqlite3.connect('file:%s?mode=ro' % g[0], uri=True)
cols = [r[1] for r in c.execute('PRAGMA table_info(broadcasts)')]
def has(x): return x in cols
sel = ['substr(id,1,8)', 'substr(name,1,16)', 'desired_state', 'runtime_state']
for x in ('restart_count','youtube_account_id','youtube_status'):
    sel.append(x if has(x) else \"''\")
# 오류 문구는 200자로 자릅니다. 여기 담기는 것은 엔진이 이미 mask_secrets 를
# 거쳐 기록한 문장이고, 바깥에서 scrub 가 한 번 더 걸립니다.
for x in ('last_error','youtube_error'):
    sel.append(\"substr(COALESCE(%s,''),1,200)\" % x if has(x) else \"''\")
print('--- desired=running 인데 runtime 이 running 이 아닌 방송 ---')
rows = list(c.execute(\"SELECT %s FROM broadcasts WHERE desired_state='running' AND COALESCE(runtime_state,'') <> 'RUNNING' ORDER BY id\" % ','.join(sel)))
print('count=%d' % len(rows))
for r in rows:
    print('  id=%s name=%s desired=%s runtime=%s restarts=%s yt_account=%s yt_status=%s' % (
        r[0], r[1], r[2], r[3], r[4], ('yes' if r[5] else 'no'), r[6]))
    if r[7]: print('    last_error: %s' % r[7])
    if r[8]: print('    youtube_error: %s' % r[8])
print('--- 전체 상태 분포 ---')
for r in c.execute('SELECT desired_state, COALESCE(runtime_state,\\'\\'), COUNT(*) FROM broadcasts GROUP BY 1,2 ORDER BY 1,2'):
    print('  desired=%s runtime=%s count=%d' % r)
PY" 2>/dev/null | scrub | sed 's/^/  /' || echo "  (조회 실패)"

printf '\n\033[1m모든 필수 검사를 통과했습니다. 위 출력을 그대로 붙여 주세요.\033[0m\n'
printf 'YouTube 방송 생성은 아직 하지 마세요 — 이 단계에서는 금지입니다.\n'
printf 'CCTV URL 입력/변경, OAuth 재인증, 계정 연결/해제, 수동 token refresh, DB write 도 하지 마세요.\n'
