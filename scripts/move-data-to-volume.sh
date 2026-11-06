#!/usr/bin/env bash
# 데이터를 500GB 볼륨으로 옮긴다.
#
#   bash move-data-to-volume.sh          # 보기만 한다 (아무것도 바꾸지 않음)
#   bash move-data-to-volume.sh --go     # 실제로 옮긴다
#
# 왜: 앱의 데이터는 도커 네임드 볼륨에 있고, 그건 항상 루트 디스크
# (/dev/sda1, 75GB, 지금 96%)에 만들어진다. 따로 사서 붙인 500GB
# (/dev/sdb → /mnt/HC_Volume_...)에는 앱이 한 바이트도 쓰지 않는다. 그래서
# 500GB 를 샀는데도 업로드가 "서버 저장공간 부족"으로 거부된다.
#
# 무엇을 하는가
#   1. 양쪽을 보고, 목적지에 이미 뭐가 있는지 보여준다
#   2. 컨테이너를 멈춘다 (송출이 끊긴다 — 복사가 끝나야 돌아온다)
#   3. 데이터를 복사한다 (원본은 지우지 않는다)
#   4. .env 에 LOUVER_DATA_PATH 를 넣고 컨테이너를 올린다
#   5. 확인한다. 옛 데이터는 그대로 남아 있어 언제든 되돌릴 수 있다
#
# 멈춘 상태에서 복사하는 이유: SQLite 의 WAL 이다. 쓰는 중에 복사하면 DB 와
# WAL 의 시점이 어긋난 사본이 생긴다 — 열리긴 하는데 내용이 틀린 쪽이 더
# 나쁘다.
#
# 원본은 지우지 않는다. 57GB 를 되찾는 건 새 자리가 멀쩡하다고 확인한
# **다음**이고, 그건 이 스크립트가 하지 않는다.
set -euo pipefail

GO=0
[ "${1:-}" = "--go" ] && GO=1

LIVE=/root/louver-live
PROJECT=louver-live
VOLUME="${PROJECT}_louver-data"

say() { printf '\n== %s\n' "$*"; }
die() { printf '\n중단: %s\n' "$*" >&2; exit 1; }

[ "$(id -u)" = 0 ] || die "root 로 실행하세요."
[ -d "$LIVE" ] || die "$LIVE 이 없습니다."
[ -r "$LIVE/.env" ] || die "$LIVE/.env 이 없습니다."

SRC="$(docker volume inspect "$VOLUME" -f '{{.Mountpoint}}' 2>/dev/null)" \
  || die "볼륨 $VOLUME 을 찾을 수 없습니다."

# 붙어 있는 데이터 볼륨을 스스로 찾는다. 루트가 아니고, 가장 큰 것.
MNT="$(findmnt -rno TARGET,SIZE -t ext4,xfs \
       | awk '$1 ~ /^\/mnt\// {print $1}' | head -1)"
[ -n "$MNT" ] || die "/mnt 아래에 붙은 데이터 볼륨을 찾지 못했습니다."
DEST="$MNT/louver"

say "원본 (루트 디스크)"
df -hT "$SRC" | tail -1
du -sh "$SRC" 2>/dev/null | sed 's/^/  합계 /'

say "목적지 ($MNT)"
df -hT "$MNT" | tail -1
echo "  이미 들어 있는 것:"
du -sh "$MNT"/* 2>/dev/null | sort -rh | head -10 | sed 's/^/    /'

if [ -e "$DEST" ]; then
  say "주의: $DEST 이 이미 있습니다"
  echo "  안에 있는 것:"
  du -sh "$DEST"/* 2>/dev/null | sort -rh | head -10 | sed 's/^/    /'
  echo
  echo "  무엇인지 모르는 상태로 덮어쓰지 않습니다. 위 내용을 확인하고,"
  echo "  버려도 되는 과거 복사본이면 지운 뒤 다시 실행하세요:"
  echo "    rm -rf $DEST"
  [ "$GO" = 1 ] && die "목적지가 비어 있지 않습니다."
fi

NEED_KB="$(du -sk "$SRC" | cut -f1)"
FREE_KB="$(df -Pk "$MNT" | awk 'NR==2 {print $4}')"
say "공간"
printf '  필요 %sGB / 여유 %sGB\n' "$((NEED_KB/1048576))" "$((FREE_KB/1048576))"
[ "$FREE_KB" -gt $((NEED_KB + 10485760)) ] \
  || die "목적지 여유가 부족합니다 (원본 + 10GB 는 있어야 합니다)."

if [ "$GO" != 1 ]; then
  cat <<'DRY'

  여기까지가 확인입니다. 아무것도 바뀌지 않았습니다.

  실제로 옮기려면:
    bash move-data-to-volume.sh --go

  그때 송출이 끊깁니다. 57GB 복사는 같은 기계 안의 디스크 간 복사라
  대개 10~20분입니다. 그동안 방송은 내려갑니다.
DRY
  exit 0
fi

say "1/5  컨테이너를 멈춥니다 (여기서 송출이 끊깁니다)"
cd "$LIVE"
docker compose -p "$PROJECT" stop
sleep 3
pgrep -c ffmpeg >/dev/null 2>&1 && echo "  남은 ffmpeg: $(pgrep -c ffmpeg)" || echo "  ffmpeg 전부 종료됨"

say "2/5  복사 (원본은 그대로 둡니다)"
mkdir -p "$DEST"
if command -v rsync >/dev/null; then
  rsync -aH --info=progress2 "$SRC/" "$DEST/"
else
  # rsync 가 없으면 cp -a. 권한과 소유자를 유지한다.
  cp -a "$SRC/." "$DEST/"
fi

say "3/5  옮긴 것이 맞는지"
S_FILES="$(find "$SRC" -type f | wc -l)"; D_FILES="$(find "$DEST" -type f | wc -l)"
S_KB="$(du -sk "$SRC" | cut -f1)"; D_KB="$(du -sk "$DEST" | cut -f1)"
printf '  파일 %s → %s\n' "$S_FILES" "$D_FILES"
printf '  용량 %sGB → %sGB\n' "$((S_KB/1048576))" "$((D_KB/1048576))"
[ "$D_FILES" -ge "$S_FILES" ] || die "파일 수가 모자랍니다. 원본은 그대로이니 컨테이너를 그냥 다시 올리면 됩니다: docker compose -p $PROJECT up -d"
[ -s "$DEST/cloud.db" ] || die "$DEST/cloud.db 이 없습니다. 위와 같이 되돌리세요."

say "4/5  .env 에 새 경로를 넣고 올립니다"
# 이미 있으면 바꾸고, 없으면 붙인다.
if grep -q '^LOUVER_DATA_PATH=' "$LIVE/.env"; then
  sed -i "s#^LOUVER_DATA_PATH=.*#LOUVER_DATA_PATH=$DEST#" "$LIVE/.env"
else
  printf '\nLOUVER_DATA_PATH=%s\n' "$DEST" >> "$LIVE/.env"
fi
grep '^LOUVER_DATA_PATH=' "$LIVE/.env" | sed 's/^/  /'
docker compose -p "$PROJECT" up -d

say "5/5  확인"
sleep 10
C="$(docker ps --format '{{.Names}}' | grep -i louver | head -1)"
[ -n "$C" ] || die "컨테이너가 올라오지 않았습니다. docker compose -p $PROJECT logs --tail 50 을 보세요."
echo "  컨테이너가 보는 데이터 디스크:"
docker exec "$C" df -hT /var/lib/louver | tail -1 | sed 's/^/    /'
echo "  서비스 상태:"
curl -sS -m 20 http://127.0.0.1:8080/api/health 2>/dev/null | head -c 300 || echo "    (로컬 포트로 못 물어봤습니다)"
echo
cat <<DONE

  됐으면 컨테이너가 보는 여유가 400GB 대여야 합니다.

  옛 데이터(57GB)는 아직 $SRC 에 그대로 있습니다. 며칠 돌려보고 멀쩡하면
  그때 되찾으세요 — 지금 지우면 되돌릴 수 없습니다:
    docker volume rm $VOLUME     # 확인된 뒤에만

  되돌리려면 .env 에서 LOUVER_DATA_PATH 줄을 지우고 다시 올리면 됩니다:
    sed -i '/^LOUVER_DATA_PATH=/d' $LIVE/.env
    cd $LIVE && docker compose -p $PROJECT up -d
DONE
