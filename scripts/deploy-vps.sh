#!/usr/bin/env bash
# Put Louver Live Cloud on a Linux server, from this machine, in one command.
#
#   scripts/deploy-vps.sh root@203.0.113.10
#   scripts/deploy-vps.sh root@203.0.113.10 --domain live.example.com --email me@you.com
#   scripts/deploy-vps.sh root@203.0.113.10 --user me@example.com --plan business
#
# What it does, in order: check the server is reachable, install Docker if it is
# missing, copy this working tree over SSH, write a .env whose master key is
# generated ON THE SERVER (so the key never travels and never lands in a shell
# history), build and start the stack, wait for /health, optionally create an
# account, and print how to reach it.
#
# Re-running it is an update: the code is replaced, the .env's master key is left
# alone, and the containers are rebuilt. Nothing here prints a secret.
set -euo pipefail

TARGET=""
DOMAIN=""
EMAIL=""
ACCOUNT=""
PLAN="business"
REMOTE_DIR="louver-live"

while [ $# -gt 0 ]; do
  case "$1" in
    --domain) DOMAIN="${2:?--domain needs a hostname}"; shift 2 ;;
    --email) EMAIL="${2:?--email needs an address}"; shift 2 ;;
    --user) ACCOUNT="${2:?--user needs an address}"; shift 2 ;;
    --plan) PLAN="${2:?--plan needs a plan id}"; shift 2 ;;
    --dir) REMOTE_DIR="${2:?--dir needs a path}"; shift 2 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) TARGET="$1"; shift ;;
  esac
done

if [ -z "$TARGET" ]; then
  echo "사용법: scripts/deploy-vps.sh user@host [--domain live.example.com --email me@you.com] [--user me@example.com]" >&2
  exit 1
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
say() { printf '\n\033[1m▸ %s\033[0m\n' "$1"; }

say "서버 확인: $TARGET"
ssh -o BatchMode=yes -o ConnectTimeout=10 "$TARGET" 'echo "  $(uname -srm)"; echo "  $(. /etc/os-release 2>/dev/null; echo "${PRETTY_NAME:-unknown}")"'

# Two things a broadcast server runs out of, said before an hour of building.
ssh "$TARGET" 'free -m 2>/dev/null | awk "/^Mem:/ {printf \"  메모리 %d MB\n\", \$2}"; df -BG --output=avail / 2>/dev/null | tail -1 | awk "{printf \"  디스크 여유 %s\n\", \$1}"'

say "Docker 확인"
ssh "$TARGET" 'if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
    echo "  이미 설치됨: $(docker --version)"
  else
    echo "  Docker를 설치합니다…"
    curl -fsSL https://get.docker.com | sh
    systemctl enable --now docker 2>/dev/null || true
    docker compose version
  fi'

say "코드 전송 → ~/$REMOTE_DIR"
# The build context the image actually needs. target/ alone is tens of
# gigabytes, and the sidecar FFmpeg binaries are replaced by the distro's.
tar czf - -C "$ROOT" \
  --exclude='./.git' \
  --exclude='./target' \
  --exclude='./node_modules' \
  --exclude='./.louver-dev' \
  --exclude='./apps/web/dist' \
  --exclude='./apps/desktop/dist' \
  --exclude='./apps/desktop/src-tauri/target' \
  --exclude='./apps/desktop/src-tauri/binaries/ffmpeg*' \
  --exclude='./apps/desktop/src-tauri/binaries/ffprobe*' \
  --exclude='./tests/fixtures' \
  --exclude='./soak-results' \
  --exclude='./rc-results' \
  --exclude='./.env' \
  . | ssh "$TARGET" "mkdir -p '$REMOTE_DIR' && tar xzf - -C '$REMOTE_DIR'"

say "환경 설정"
ssh "$TARGET" "cd '$REMOTE_DIR' && \
  DOMAIN='$DOMAIN' EMAIL='$EMAIL' bash -s" <<'REMOTE'
set -eu
if [ ! -f .env ]; then
  cp .env.example .env
  echo "  .env 를 만들었습니다"
else
  echo "  기존 .env 를 사용합니다"
fi
chmod 600 .env

set_env() {
  key="$1"
  value="$2"
  # Delete the line whether it is set, commented out or absent, then append. One
  # line per key afterwards, which matters because .env.example ships an empty
  # LOUVER_MASTER_KEY= and two definitions of the same key is a trap.
  sed -i "/^#\? *${key}=/d" .env
  echo "${key}=${value}" >> .env
}

# The master key is made here and stays here. Losing it means re-entering every
# saved stream key, so it is generated once — and also when the file carries the
# example's empty value, which is the mistake this is most likely to meet.
if grep -qE '^LOUVER_MASTER_KEY=.+' .env; then
  echo "  기존 master key를 유지합니다"
else
  set_env LOUVER_MASTER_KEY "$(openssl rand -hex 32)"
  echo "  master key를 생성했습니다 (.env 를 백업해 두세요)"
fi

set_env LOUVER_DEPLOYMENT cloud
if [ -n "${DOMAIN:-}" ]; then
  set_env LOUVER_DOMAIN "$DOMAIN"
  [ -n "${EMAIL:-}" ] && set_env LOUVER_TLS_EMAIL "$EMAIL"
  # TLS in front, so the app is published on loopback only and the cookie is
  # always Secure.
  set_env LOUVER_COOKIE_SECURE always
  set_env LOUVER_PORT 127.0.0.1:8080
else
  # Reached directly over http://<ip>:8080 or through an SSH tunnel. `auto`
  # keeps the cookie usable in both.
  set_env LOUVER_COOKIE_SECURE auto
  set_env LOUVER_PORT 8080
fi
grep -c . .env > /dev/null
REMOTE

say "빌드 및 실행 (처음에는 몇 분 걸립니다)"
if [ -n "$DOMAIN" ]; then
  # Compose interpolates the whole file before it filters profiles, so the
  # `https` service can no longer declare its variable as required — that marker
  # broke every deployment that did not use it. The check moved here, where it
  # only applies to the run that actually needs it.
  ssh "$TARGET" "cd '$REMOTE_DIR' && grep -qE '^LOUVER_DOMAIN=.+' .env" || {
    echo "  .env 에 LOUVER_DOMAIN 이 없습니다. --domain 없이 다시 실행하면 IP로 접속합니다." >&2
    exit 1
  }
  ssh "$TARGET" "cd '$REMOTE_DIR' && docker compose --profile https up -d --build"
else
  ssh "$TARGET" "cd '$REMOTE_DIR' && docker compose up -d --build"
fi

say "상태 확인"
# `--health-check` prints the body it got even when a check failed, so this can
# tell a server that is still starting from one that is up but missing
# something — and only the first is a reason to stop.
set +e
ssh "$TARGET" "cd '$REMOTE_DIR' && for i in \$(seq 1 90); do
    out=\$(docker compose exec -T louver louver-server --health-check 2>/dev/null)
    if [ -n \"\$out\" ]; then
      echo \"  \$out\"
      case \"\$out\" in
        *'\"status\":\"ok\"'*) exit 0 ;;
        *) exit 3 ;;
      esac
    fi
    sleep 2
  done
  echo '  서버가 응답하지 않습니다. 최근 로그:'
  docker compose logs --tail 40 louver
  exit 1"
HEALTH_RC=$?
set -e
case "$HEALTH_RC" in
  0) ;;
  3) echo "  경고: 위 JSON에서 false인 항목이 있습니다. 서버는 응답하므로 계속 진행합니다." ;;
  *) exit 1 ;;
esac

if [ -n "$ACCOUNT" ]; then
  say "계정 만들기: $ACCOUNT ($PLAN)"
  # Read here, piped there. The password is never an argument on either side, so
  # it cannot be read out of `ps` or a shell history file.
  printf '비밀번호 (10자 이상, 화면에 표시되지 않습니다): '
  read -r -s PASSWORD
  printf '\n'
  printf '%s\n' "$PASSWORD" | ssh "$TARGET" "cd '$REMOTE_DIR' && docker compose exec -T louver louver-server --create-user '$ACCOUNT' --plan '$PLAN'"
  unset PASSWORD
fi

HOST_ONLY="${TARGET#*@}"

say "밖에서 접속되는지 확인"
if [ -n "$DOMAIN" ]; then
  PROBE="https://$DOMAIN/health"
else
  PROBE="http://$HOST_ONLY:8080/health"
fi
if curl -fsS --max-time 8 "$PROBE" 2>/dev/null; then
  printf '\n  %s 로 접속됩니다\n' "$PROBE"
else
  cat <<EOF
  이 컴퓨터에서 $PROBE 에 닿지 않습니다. 서버 안에서는 정상이므로 방화벽입니다.

  - Hetzner Cloud Firewall: 콘솔 → Firewalls → 인바운드 TCP $( [ -n "$DOMAIN" ] && echo '80, 443' || echo '8080' ) 허용
  - 서버 안의 ufw 를 쓰고 있다면:
EOF
  ssh "$TARGET" 'if command -v ufw >/dev/null 2>&1 && ufw status 2>/dev/null | grep -q "Status: active"; then
      echo "      ufw 가 켜져 있습니다. 서버에서:  ufw allow 8080/tcp   (도메인 사용 시 80,443)"
    else
      echo "      ufw 는 꺼져 있습니다 — 클라우드 방화벽 쪽을 보세요."
    fi'
  echo "  또는 방화벽을 건드리지 않고 SSH 터널로 접속하세요 (아래)."
fi

say "완료"
if [ -n "$DOMAIN" ]; then
  cat <<EOF
  접속:   https://$DOMAIN
          (DNS A 레코드가 이 서버를 가리키고 있어야 인증서가 발급됩니다)
EOF
else
  cat <<EOF
  접속 방법 두 가지:

  1) SSH 터널 — 권장. 통신이 SSH로 암호화되고 브라우저는 localhost로 보므로
     쿠키 문제도 없습니다:

       ssh -N -L 8080:127.0.0.1:8080 $TARGET
       그 다음 브라우저에서  http://localhost:8080

     터널을 닫아도(노트북을 꺼도) 방송은 서버에서 계속됩니다.

  2) 직접 접속 — http://$HOST_ONLY:8080
     암호화되지 않습니다. 로그인 토큰이 평문으로 지나갑니다. 공개 서비스로
     쓸 거라면 --domain 으로 다시 배포해서 HTTPS를 붙이세요.
EOF
fi
cat <<EOF

  화면 맨 위에 REMOTE CLOUD SERVER 가 보이면 제대로 올라간 것입니다.

  상태:   ssh $TARGET "cd $REMOTE_DIR && docker compose ps"
  로그:   ssh $TARGET "cd $REMOTE_DIR && docker compose logs -f louver"
  계정:   ssh $TARGET "cd $REMOTE_DIR && docker compose exec -T louver louver-server --create-user you@example.com --plan business"
  백업:   ~/$REMOTE_DIR/.env (master key) 와 louver-data 볼륨
EOF
