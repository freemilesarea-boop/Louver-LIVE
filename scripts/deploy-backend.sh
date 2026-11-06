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
#
# THE SECOND THING THAT MUST NOT GO WRONG
#
# Caddy sits behind `profiles: ['https']`, and the app publishes on loopback
# only. A bare `docker compose up -d` leaves a profiled service alone, so a
# compose command written without the profile can take the site off the
# internet while leaving the app "healthy" and every broadcast on air — the
# failure is invisible from the server's own process list. On 2026-11-06 that
# is exactly what happened: a `stop` during the volume migration took Caddy
# down and three bare `up -d` calls could not bring it back. So the profile
# this install actually uses is read off the running containers at the start
# and passed to every compose call here, and Caddy is checked at the end.
#
# A sibling trap: `restart: unless-stopped` does NOT revive a container a
# person stopped, so nothing recovers this by itself.
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

# Which profiles does this install actually run? Asking the containers beats
# guessing: an IP-only server has no Caddy and must not be given the profile,
# and a server behind a domain must never lose it.
PROFILE_ARGS=()
if docker ps -a --filter "name=${PROJECT}-caddy" --format '{{.Names}}' | grep -q .; then
  PROFILE_ARGS=(--profile https)
  echo "  caddy 있음 → compose 에 --profile https 를 붙입니다"
else
  echo "  caddy 없음 (IP 전용 배포) → 프로필 없이 진행합니다"
fi

# Free space, before the build and not after it. A build pulls images and
# writes layers; on 2026-11-06 one run pushed the root filesystem under the
# app's own 5GB floor and the service came up "degraded" with uploads refused.
FREE_MB="$(df -Pm /var/lib/docker | awk 'NR==2 {print $4}')"
printf '  /var/lib/docker 여유: %s MB\n' "$FREE_MB"
if [ "${FREE_MB:-0}" -lt 8000 ]; then
  die "빌드에 쓸 여유가 ${FREE_MB}MB 뿐입니다 (8GB 이상 필요). 먼저 공간을 비우세요:
    docker builder prune -af      # 빌드 캐시
    docker image prune -af        # 안 쓰는 이미지
    rm -rf /root/louver-live-old.*  # 옛 소스 트리"
fi

say "1/6  풀어보기 (아직 아무것도 바꾸지 않습니다)"
NEW="/root/louver-live-new.$$"
mkdir -p "$NEW"
tar xzf "$BUNDLE" -C "$NEW"
# A bundle that is missing these is not a source tree and must not replace one.
for f in docker-compose.yml Dockerfile crates/louver-cloud/src/db.rs apps/server/src/main.rs; do
  [ -r "$NEW/$f" ] || die "번들에 $f 가 없습니다. 잘못된 파일입니다."
done
printf '  파일 %s개\n' "$(find "$NEW" -type f | wc -l | tr -d ' ')"
BUNDLE_VER="$(sed -n 's/^version = "\(.*\)"/\1/p' "$NEW/Cargo.toml" | head -1)"
printf '  번들 버전: %s\n' "${BUNDLE_VER:-알 수 없음}"
# The old upload formula demanded three times the file size in free space. A
# bundle still carrying it would deploy the very bug this is meant to remove.
if grep -q 'saturating_mul(3)' "$NEW/crates/louver-cloud/src/ingest.rs" 2>/dev/null; then
  die "이 번들에는 옛 디스크 공식(× 3)이 들어 있습니다. 잘못된 번들입니다."
fi
echo "  디스크 공식: 새 것 (여유 5GB 만 확인)"
# If this install keeps its data somewhere other than the default volume, the
# bundle's compose file has to honour that variable. One that does not would
# move every customer's video back onto the boot disk, quietly, and the only
# symptom would be uploads failing again once it filled. 2026-11-06: a bundle
# predating the compose fix did precisely this.
if grep -q '^LOUVER_DATA_PATH=' "$LIVE/.env" 2>/dev/null; then
  DATA_PATH="$(sed -n 's/^LOUVER_DATA_PATH=//p' "$LIVE/.env" | head -1)"
  grep -q 'LOUVER_DATA_PATH' "$NEW/docker-compose.yml" \
    || die "이 서버는 데이터를 $DATA_PATH 에 두고 있는데, 번들의 docker-compose.yml 은
그 설정을 읽지 않습니다. 이걸 배포하면 영상과 DB 가 조용히 부팅 디스크로
돌아갑니다. 더 새 번들을 쓰세요."
  printf '  데이터 경로: %s (번들이 이 설정을 읽습니다)\n' "$DATA_PATH"
fi

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
docker compose -p "$PROJECT" ${PROFILE_ARGS[@]+"${PROFILE_ARGS[@]}"} up -d --build

say "5/6  데이터가 그대로인지"
AFTER="$(docker volume inspect "$VOLUME" -f '{{.Mountpoint}}')"
[ "$BEFORE" = "$AFTER" ] || die "볼륨이 바뀌었습니다! $BEFORE → $AFTER — 즉시 롤백하세요."
echo "  볼륨 동일: $AFTER"
C="$(docker ps --format '{{.Names}}' | grep -i louver | head -1)"
[ -n "$C" ] || die "컨테이너가 올라오지 않았습니다. docker compose logs 를 보세요."
docker exec "$C" sqlite3 /var/lib/louver/cloud.db \
  'SELECT "회원 " || COUNT(*) FROM users;' 2>/dev/null \
  || echo "  (sqlite3 없음 — 관리자 화면에서 회원 수를 확인하세요)"
# The app can be healthy while the site is unreachable, so say it out loud.
if [ "${#PROFILE_ARGS[@]}" -gt 0 ]; then
  if docker ps --filter "name=${PROJECT}-caddy" --format '{{.Names}}' | grep -q .; then
    echo "  caddy 도 올라왔습니다 (사이트 접속 가능)"
  else
    die "앱은 올라왔지만 caddy 가 없습니다. 사이트는 밖에서 안 열립니다:
    cd $LIVE && docker compose -p $PROJECT --profile https up -d"
  fi
fi

say "6/6  확인할 것"
cat <<'NEXT'
  요금제 한도는 아직 바뀌지 않았습니다. 코드의 한도는 기존 DB 행을 건드리지
  않기 때문입니다. 아래를 서버에서 그대로 붙여넣으세요 (컨테이너 이름을
  직접 넣을 필요 없습니다):

    C=$(docker ps --format '{{.Names}}' | grep -i louver | head -1)
    docker exec "$C" louver-server --audit-storage                # 먼저 읽기
    docker exec "$C" louver-server --audit-storage --apply --yes   # 그 다음 적용

  되돌리려면:
    rm -rf /root/louver-live && mv <위에 남겨둔 old 경로> /root/louver-live
    cd /root/louver-live && docker compose -p louver-live --profile https up -d --build
                                  # ↑ caddy 가 있는 배포라면 이 프로필이 필요합니다
NEXT
NEXT_HINT="docker logs $C --since 5m 2>&1 | tail -20"
echo
echo "  로그: $NEXT_HINT"
