#!/usr/bin/env bash
# Put a full source tree on the server and rebuild the container.
#
#   서버에서:  bash deploy-backend.sh /root/louver-full-<sha>.tar.gz
#
# Replaces the whole of /root/louver-live rather than patching files into it.
# That directory has been updated by tarballs of *changed* files for months, so
# nobody can say which vintage any one file in it is — and a stale file there is
# not a cosmetic problem: the upload disk check was rewritten in September, and
# a server still carrying the old one demands three times the file size in free
# space and refuses uploads that should succeed.
#
# WHAT THIS WILL COST
#
# `docker compose up --build` recreates the container, which takes every live
# broadcast off air for about a minute. They come back by themselves:
# `desired_state` is untouched and recovery restarts them at boot. Do it when a
# minute of black is acceptable, not in the middle of a customer's event.
#
# THE ONE THING THAT MUST NOT GO WRONG
#
# Compose names its volume after the project, and the project after the
# directory. `/root/louver-live` gives `louver-live_louver-data`, which holds
# the database, every customer's video and the encryption of their stream keys.
# Building the same compose file from a directory with any other name creates a
# **new, empty** volume and the service comes up as a blank install with the
# real data orphaned beside it. So the directory name is restored before
# anything is built, and the volume is checked by name before and after.
set -euo pipefail

BUNDLE="${1:?사용법: deploy-backend.sh <번들.tar.gz>}"
LIVE=/root/louver-live
PROJECT=louver-live
VOLUME="${PROJECT}_louver-data"

say() { printf '\n== %s\n' "$*"; }
die() { printf '\n중단: %s\n' "$*" >&2; exit 1; }

[ -r "$BUNDLE" ] || die "번들을 읽을 수 없습니다: $BUNDLE"
[ -d "$LIVE" ] || die "$LIVE 이 없습니다. 이 서버가 맞습니까?"
[ -r "$LIVE/.env" ] || die "$LIVE/.env 이 없습니다. 이게 없으면 서버는 기동하지 않습니다."
command -v docker >/dev/null || die "docker 가 없습니다."

say "0/6  지금 상태"
docker volume inspect "$VOLUME" -f '{{.Name}} → {{.Mountpoint}}' \
  || die "데이터 볼륨 $VOLUME 을 찾을 수 없습니다. 프로젝트 이름이 다릅니다 — 멈춥니다."
BEFORE="$(docker volume inspect "$VOLUME" -f '{{.Mountpoint}}')"
docker ps --filter "name=$PROJECT" --format '  {{.Names}}  {{.Status}}'
echo "  송출 중인 FFmpeg: $(pgrep -c ffmpeg || true)"

say "1/6  풀어보기 (아직 아무것도 바꾸지 않습니다)"
NEW="/root/louver-live-new.$$"
mkdir -p "$NEW"
tar xzf "$BUNDLE" -C "$NEW"
# A bundle that is missing these is not a source tree and must not replace one.
for f in docker-compose.yml Dockerfile crates/louver-cloud/src/db.rs apps/server/src/main.rs; do
  [ -r "$NEW/$f" ] || die "번들에 $f 가 없습니다. 잘못된 파일입니다."
done
printf '  파일 %s개\n' "$(find "$NEW" -type f | wc -l | tr -d ' ')"

say "2/6  .env 가져오기"
# Never in the bundle: it holds LOUVER_MASTER_KEY, and that key is what decrypts
# every stored stream key. A bundle carrying it would put it in a tarball in
# /root and in whatever copied it there.
cp "$LIVE/.env" "$NEW/.env"
chmod 600 "$NEW/.env"
grep -q 'LOUVER_MASTER_KEY' "$NEW/.env" || die ".env 에 LOUVER_MASTER_KEY 가 없습니다."

say "3/6  자리 바꾸기"
OLD="/root/louver-live-old.$(date +%Y%m%d-%H%M%S)"
mv "$LIVE" "$OLD"
mv "$NEW" "$LIVE"
# The name is back to what it was, so the project and the volume are unchanged.
printf '  이전 트리는 %s 에 남겨 뒀습니다 (되돌릴 때 씁니다)\n' "$OLD"

say "4/6  빌드와 교체 — 여기서 약 1분 끊깁니다"
cd "$LIVE"
docker compose -p "$PROJECT" up -d --build

say "5/6  데이터가 그대로인지"
AFTER="$(docker volume inspect "$VOLUME" -f '{{.Mountpoint}}')"
[ "$BEFORE" = "$AFTER" ] || die "볼륨이 바뀌었습니다! $BEFORE → $AFTER — 즉시 롤백하세요."
echo "  볼륨 동일: $AFTER"
C="$(docker ps --format '{{.Names}}' | grep -i louver | head -1)"
[ -n "$C" ] || die "컨테이너가 올라오지 않았습니다. docker compose logs 를 보세요."
docker exec "$C" sqlite3 /var/lib/louver/cloud.db \
  'SELECT "회원 " || COUNT(*) FROM users;' 2>/dev/null \
  || echo "  (sqlite3 없음 — 관리자 화면에서 회원 수를 확인하세요)"

say "6/6  확인할 것"
cat <<'NEXT'
  요금제 한도는 아직 바뀌지 않았습니다. 코드의 한도는 기존 DB 행을 건드리지
  않기 때문입니다. 먼저 읽고, 그 다음에 적용합니다:

    docker exec <컨테이너> louver-server --audit-storage
    docker exec <컨테이너> louver-server --audit-storage --apply --yes

  되돌리려면:
    rm -rf /root/louver-live && mv <위에 남겨둔 old 경로> /root/louver-live
    cd /root/louver-live && docker compose -p louver-live up -d --build
NEXT
NEXT_HINT="docker logs $C --since 5m 2>&1 | tail -20"
echo
echo "  로그: $NEXT_HINT"
