#!/usr/bin/env bash
# 앱 데이터를 따로 사서 붙인 디스크로 옮긴다 — 송출을 거의 끊지 않고.
#
#   bash move-data-to-volume.sh             # 보기만 한다 (아무것도 바꾸지 않음)
#   bash move-data-to-volume.sh --presync   # 1차 복사: 컨테이너 켠 채로, 안 끊김
#   bash move-data-to-volume.sh --go        # 2차 복사와 전환: 1~2분 끊김
#
#   --dest /mnt/HC_Volume_123/louver-data   # 목적지를 직접 지정 (없으면 자동 탐색)
#
# 왜 필요한가
#
# 앱의 데이터는 도커 네임드 볼륨에 있고, 네임드 볼륨은 드라이버 옵션이 없으면
# 항상 /var/lib/docker/volumes 아래, 즉 **루트 디스크**에 만들어진다. 따로 사서
# 붙인 볼륨(/mnt/HC_Volume_... )에는 앱이 한 바이트도 쓰지 않는다. 그래서 큰
# 디스크를 샀는데도 업로드가 "서버 저장공간 부족"으로 거부된다 — 요금제에
# 여유가 남아 있어도 그렇다. 호스트 디스크 검사는 요금제와 별개로 걸린다.
#
# 왜 두 단계인가
#
# 영상 파일은 한 번 쓰면 바뀌지 않는다. 그래서 1차 복사는 컨테이너를 켠 채로
# 해도 안전하다 — 읽기는 원본, 쓰기는 목적지뿐이다. 수십 GB 가 여기서 넘어가고
# 송출은 안 끊긴다. 2차에서는 멈춘 뒤 차이분과 DB 만 넘기므로 1~2분이면 끝난다.
#
# DB 는 2차에서만 의미가 있다. SQLite 의 WAL 때문이다: 쓰는 중에 복사하면 DB 와
# WAL 의 시점이 어긋난 사본이 생기고, 그건 열리긴 하는데 내용이 틀리다 — 안
# 열리는 것보다 나쁘다. 그래서 1차가 만든 DB 사본은 쓰레기로 취급하고 2차에서
# 멈춘 상태로 새로 받는다.
#
# 반드시 틀리지 않아야 하는 것: 프로필
#
# Caddy 는 docker-compose.yml 에서 `profiles: ['https']` 안에 있고, 앱은
# 루프백에만 포트를 연다. 프로필 없는 `docker compose up -d` 는 프로필에 든
# 서비스를 **건드리지 않는다**. 그래서 프로필 없이 stop/up 을 하면 앱은
# "healthy", 송출 16개는 그대로 YouTube 로 나가는데 사이트만 밖에서 안 열린다
# — 서버 안에서는 아무 이상이 안 보인다. 2026-11-06 에 정확히 이걸로 3시간을
# 날렸다. `restart: unless-stopped` 도 사람이 멈춘 컨테이너는 되살리지 않는다.
# 그래서 이 스크립트는 지금 돌고 있는 컨테이너에서 프로필을 읽어 모든 compose
# 호출에 붙이고, 마지막에 caddy 가 돌아왔는지 확인한다.
#
# 원본은 지우지 않는다. 옛 자리를 되찾는 건 새 자리가 며칠 멀쩡하다고 확인한
# **다음**이고, 그건 이 스크립트가 하지 않는다.
set -euo pipefail

MODE=survey
DEST=""
while [ $# -gt 0 ]; do
  case "$1" in
    --presync) MODE=presync ;;
    --go)      MODE=go ;;
    --dest)    DEST="${2:?--dest 뒤에 경로가 필요합니다}"; shift ;;
    *)         printf '모르는 인자: %s\n' "$1" >&2; exit 1 ;;
  esac
  shift
done

LIVE=/root/louver-live
PROJECT=louver-live
VOLUME="${PROJECT}_louver-data"

say() { printf '\n== %s\n' "$*"; }
die() { printf '\n중단: %s\n' "$*" >&2; exit 1; }

[ "$(id -u)" = 0 ] || die "root 로 실행하세요."
[ -d "$LIVE" ] || die "$LIVE 이 없습니다."
[ -r "$LIVE/.env" ] || die "$LIVE/.env 이 없습니다."
command -v docker >/dev/null || die "docker 가 없습니다."
command -v rsync >/dev/null || die "rsync 가 없습니다:  apt-get install -y rsync"

SRC="$(docker volume inspect "$VOLUME" -f '{{.Mountpoint}}' 2>/dev/null)" \
  || die "볼륨 $VOLUME 을 찾을 수 없습니다. 프로젝트 이름이 다릅니다."
[ -d "$SRC" ] || die "볼륨 경로가 없습니다: $SRC"

say "0  지금 상태"
printf '  원본: %s\n' "$SRC"
df -hT "$SRC" | tail -1 | sed 's/^/        /'
SRC_KB="$(du -sk "$SRC" | awk '{print $1}')"
printf '  원본 크기: %s GB\n' "$((SRC_KB / 1024 / 1024))"

# 지금 이 설치가 쓰는 프로필을 컨테이너에서 읽는다. 추측하지 않는다: IP 전용
# 서버에는 caddy 가 없으니 프로필을 주면 안 되고, 도메인을 쓰는 서버는 프로필을
# 잃으면 사이트가 내려간다.
PROFILE_ARGS=()
if docker ps -a --filter "name=${PROJECT}-caddy" --format '{{.Names}}' | grep -q .; then
  PROFILE_ARGS=(--profile https)
  echo "  caddy 있음 → compose 에 --profile https 를 붙입니다"
else
  echo "  caddy 없음 (IP 전용 배포) → 프로필 없이 진행합니다"
fi
compose() { docker compose -p "$PROJECT" ${PROFILE_ARGS[@]+"${PROFILE_ARGS[@]}"} "$@"; }

# 목적지. 지정하지 않으면 /mnt 아래에서 루트가 아닌 파일시스템을 찾는다.
if [ -z "$DEST" ]; then
  say "1  목적지 찾기"
  SRC_DEV="$(df -P "$SRC" | awk 'NR==2 {print $1}')"
  CAND=""
  for m in /mnt/*; do
    [ -d "$m" ] || continue
    d="$(df -P "$m" 2>/dev/null | awk 'NR==2 {print $1}')"
    [ -n "$d" ] && [ "$d" != "$SRC_DEV" ] || continue
    avail="$(df -Pk "$m" | awk 'NR==2 {print $4}')"
    printf '  후보: %-34s 여유 %s GB\n' "$m" "$((avail / 1024 / 1024))"
    [ -z "$CAND" ] && CAND="$m"
  done
  [ -n "$CAND" ] || die "/mnt 아래에 쓸 수 있는 다른 디스크가 없습니다.
먼저 볼륨을 붙이고 마운트하세요. 또는 --dest 로 직접 지정하세요."
  DEST="$CAND/louver-data"
  printf '  고른 목적지: %s\n' "$DEST"
fi

DEST_PARENT="$(dirname "$DEST")"
[ -d "$DEST_PARENT" ] || die "목적지의 상위 폴더가 없습니다: $DEST_PARENT"
if [ "$(df -P "$DEST_PARENT" | awk 'NR==2 {print $1}')" = "$(df -P "$SRC" | awk 'NR==2 {print $1}')" ]; then
  die "목적지가 원본과 같은 디스크입니다. 옮기는 의미가 없습니다."
fi
DEST_AVAIL_KB="$(df -Pk "$DEST_PARENT" | awk 'NR==2 {print $4}')"
DEST_HAVE_KB=0
[ -d "$DEST" ] && DEST_HAVE_KB="$(du -sk "$DEST" | awk '{print $1}')"
if [ "$((DEST_AVAIL_KB + DEST_HAVE_KB))" -lt "$SRC_KB" ]; then
  die "목적지 여유가 모자랍니다: 필요 $((SRC_KB / 1024 / 1024))GB,
가능 $(((DEST_AVAIL_KB + DEST_HAVE_KB) / 1024 / 1024))GB"
fi

say "2  목적지 상태"
printf '  경로: %s\n' "$DEST"
df -hT "$DEST_PARENT" | tail -1 | sed 's/^/        /'
if [ -d "$DEST" ]; then
  printf '  이미 있음: %s GB\n' "$((DEST_HAVE_KB / 1024 / 1024))"
  ls -la "$DEST" | sed 's/^/        /'
  [ -s "$DEST/cloud.db" ] && echo "  (cloud.db 가 이미 있습니다 — 2차에서 덮어씁니다)"
else
  echo "  아직 없음 (만들 것입니다)"
fi

if [ "$MODE" = survey ]; then
  cat <<EOF

읽기만 했습니다. 아무것도 바꾸지 않았습니다.

  1차 (안 끊김, 몇 분~수십 분):
      bash $0 --presync --dest $DEST

  2차 (1~2분 끊김, 1차가 끝난 뒤에):
      bash $0 --go --dest $DEST

1차는 몇 번을 돌려도 됩니다 — 차이분만 넘어갑니다. 송출 중에 돌리세요.
EOF
  exit 0
fi

# ── 1차: 컨테이너를 켠 채로 영상만 복사 ─────────────────────────────────────
# 영상 파일은 한 번 쓰면 바뀌지 않는다. 읽기는 원본, 쓰기는 목적지뿐이므로
# 송출 중인 FFmpeg 은 영향을 받지 않는다. nice/ionice 로 우선순위를 낮춰
# 방송의 디스크 입출력을 빼앗지 않게 한다.
if [ "$MODE" = presync ]; then
  say "3  1차 복사 (컨테이너는 그대로 돕니다)"
  mkdir -p "$DEST/media"
  if [ -d "$SRC/media" ]; then
    nice -n 19 ionice -c3 rsync -aH --delete --info=progress2 \
      "$SRC/media/" "$DEST/media/"
  else
    echo "  원본에 media 폴더가 없습니다 — 넘어갑니다"
  fi
  cat <<EOF

1차 끝났습니다. 송출은 끊기지 않았습니다.

이제 2차를 하세요. 여기서 1~2분 끊기니 고객 행사 시간은 피하세요:
    bash $0 --go --dest $DEST
EOF
  exit 0
fi

# ── 2차: 멈추고, 차이분과 DB 를 넘기고, 전환 ────────────────────────────────
say "3  멈춥니다 — 여기서 송출이 끊깁니다"
compose stop
sleep 3

say "4  마지막 복사 (DB 포함)"
# 1차가 만든 DB 사본은 쓰는 중에 뜬 것이라 쓸 수 없다. 멈춘 지금 새로 받는다.
# 1차 기록(_migration-logs)은 제외한다 — 제외된 경로는 --delete 로도 지워지지
# 않으므로 옛 기록이 남는다.
rsync -aH --delete --exclude='/_migration-logs' --info=progress2 \
  "$SRC/" "$DEST/"

say "5  옮겨진 것이 쓸 수 있는지"
S_FILES="$(find "$SRC" -type f | wc -l | tr -d ' ')"
D_FILES="$(find "$DEST" -type f | wc -l | tr -d ' ')"
printf '  파일 수: 원본 %s → 목적지 %s\n' "$S_FILES" "$D_FILES"
if [ "$D_FILES" -lt "$S_FILES" ]; then
  die "파일 수가 모자랍니다. 원본은 그대로이니 그냥 다시 올리면 됩니다:
    cd $LIVE && docker compose -p $PROJECT ${PROFILE_ARGS[*]-} up -d"
fi
if [ -s "$SRC/cloud.db" ] && [ ! -s "$DEST/cloud.db" ]; then
  die "cloud.db 가 목적지에 없습니다. 전환하지 않았습니다. 원본은 그대로입니다:
    cd $LIVE && docker compose -p $PROJECT ${PROFILE_ARGS[*]-} up -d"
fi
ls -la "$DEST/cloud.db" 2>/dev/null | sed 's/^/        /' || true

say "6  .env 에 새 경로를 적고 올립니다"
# 이 변수를 compose 가 읽어야 한다. 읽지 않는 compose 파일이면 여기서 멈춘다 —
# 그대로 올리면 앱이 조용히 옛 디스크로 돌아가고, 증상은 나중에 그 디스크가
# 다시 찰 때에야 나타난다. 2026-11-06 에 이걸로 한 번 헛돌았다.
grep -q 'LOUVER_DATA_PATH' "$LIVE/docker-compose.yml" \
  || die "$LIVE/docker-compose.yml 이 LOUVER_DATA_PATH 를 읽지 않습니다.
이 compose 파일은 볼륨이 고정돼 있어 .env 를 넣어도 아무 일도 일어나지 않습니다.
더 새 소스 트리를 먼저 배포하세요. 원본 데이터는 그대로입니다:
    cd $LIVE && docker compose -p $PROJECT ${PROFILE_ARGS[*]-} up -d"
cd "$LIVE"
if grep -q '^LOUVER_DATA_PATH=' .env; then
  sed -i "s#^LOUVER_DATA_PATH=.*#LOUVER_DATA_PATH=$DEST#" .env
else
  printf '\nLOUVER_DATA_PATH=%s\n' "$DEST" >> .env
fi
grep '^LOUVER_DATA_PATH=' .env | sed 's/^/        /'
compose up -d
sleep 12

say "7  확인"
C="$(docker ps --format '{{.Names}}' | grep -i "${PROJECT}-louver" | head -1)"
[ -n "$C" ] || die "컨테이너가 올라오지 않았습니다:
    cd $LIVE && docker compose -p $PROJECT ${PROFILE_ARGS[*]-} logs --tail 50"
docker exec "$C" df -hT /var/lib/louver | tail -1 | sed 's/^/        /'
docker inspect "$C" -f '{{range .Mounts}}        {{.Type}} {{.Source}} -> {{.Destination}}{{"\n"}}{{end}}'
docker inspect "$C" -f '{{range .Mounts}}{{if eq .Destination "/var/lib/louver"}}{{.Source}}{{end}}{{end}}' \
  | grep -qx "$DEST" \
  || die "컨테이너가 아직 새 경로를 쓰지 않습니다. compose 가 .env 를 읽었는지 보세요."
echo "  데이터 경로 전환 확인"

# 앱이 healthy 인데 사이트가 안 열릴 수 있다. 소리내어 확인한다.
if [ "${#PROFILE_ARGS[@]}" -gt 0 ]; then
  if docker ps --filter "name=${PROJECT}-caddy" --format '{{.Names}}' | grep -q .; then
    echo "  caddy 도 올라왔습니다 (사이트 접속 가능)"
  else
    die "앱은 올라왔지만 caddy 가 없습니다. 사이트는 밖에서 안 열립니다:
    cd $LIVE && docker compose -p $PROJECT --profile https up -d"
  fi
fi

cat <<EOF

끝났습니다. 옛 데이터는 $SRC 에 그대로 있습니다.

되돌리려면:
    cd $LIVE && sed -i '/^LOUVER_DATA_PATH=/d' .env \\
      && docker compose -p $PROJECT ${PROFILE_ARGS[*]-} up -d

옛 자리(위 경로, $((SRC_KB / 1024 / 1024))GB)는 **며칠 돌려보고** 멀쩡하면 회수하세요:
    docker volume rm $VOLUME
지금 지우면 되돌릴 수 없습니다. 서두르지 마세요.
EOF
