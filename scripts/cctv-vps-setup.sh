#!/usr/bin/env bash
# Turn a fresh Korean VPS into the box that re-collects the camera list daily.
#
#   scp -r scripts root@<VPS>:/tmp/louver-scripts
#   ssh root@<VPS> 'bash /tmp/louver-scripts/cctv-vps-setup.sh'
#
# Run as root on a fresh Debian 12 or Ubuntu 22.04/24.04 box. Idempotent: run
# it again after changing something and it will settle to the same state.
#
# WHY A BOX IN KOREA
#
# `openapi.its.go.kr` refuses foreign IPs, so the camera list cannot be fetched
# from the production server in Germany. Everything else can: the check is an
# HTTPS call to 247streams.kr, and the server is what runs FFmpeg. So this box
# needs a Korean IP, python3, and nothing else — no ssh key into production, no
# database, no FFmpeg, no inbound ports. 512 MB is plenty.
#
# WHAT IT ASKS FOR
#
#   the ITS API key          — openapi.its.go.kr
#   an admin e-mail/password — to mint one session per run and revoke it after
#
# Both are typed without echo and land in files only root and the service user
# can read. Neither is ever an argument to anything.
#
# THE ONE THING TO VERIFY BEFORE PAYING FOR THIS BOX
#
# ITS is known to block foreign IPs. Whether it also blocks a Korean *cloud*
# provider's range has not been measured. So the last thing this script does is
# fetch the list for real and tell you whether it worked. If it did not, the
# box is the wrong box and nothing else here matters.
set -euo pipefail

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEST=/opt/louver
STATE=/var/lib/louver
SVC_USER=louver
NO_PREFLIGHT=0

while [ $# -gt 0 ]; do
  case "$1" in
    --src) SRC="${2:?--src needs a directory}"; shift 2 ;;
    --no-preflight) NO_PREFLIGHT=1; shift ;;
    -h|--help) sed -n '2,30p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "모르는 옵션: $1" >&2; exit 2 ;;
  esac
done

say() { printf '\n== %s\n' "$*"; }
die() { printf '\n중단: %s\n' "$*" >&2; exit 1; }

[ "$(id -u)" = 0 ] || die "root 로 실행하세요."
for f in cctv-daily-kr.sh cctv-import.py cctv-check.py cctv-report.py; do
  [ -r "$SRC/$f" ] || die "$SRC/$f 가 없습니다. scripts/ 디렉터리를 통째로 올렸는지 보세요."
done
[ -r "$SRC/systemd/cctv-daily-kr.service" ] || die "$SRC/systemd/ 가 없습니다."

say "1/6  필요한 꾸러미"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq python3 curl ca-certificates tzdata >/dev/null
# The schedule is written in Korean local time, so the box's clock has to agree
# with it. A VPS defaults to UTC and 05:12 UTC is 14:12 in Seoul.
timedatectl set-timezone Asia/Seoul
printf '     시간대: %s\n' "$(timedatectl show -p Timezone --value)"

say "2/6  서비스 사용자"
# A system user: no login shell, no password, no home full of dotfiles. Its
# whole purpose is to own two files and run one script.
if ! id -u "$SVC_USER" >/dev/null 2>&1; then
  useradd --system --home-dir "$STATE" --create-home --shell /usr/sbin/nologin "$SVC_USER"
fi
install -d -m 700 -o "$SVC_USER" -g "$SVC_USER" "$STATE"

say "3/6  스크립트"
install -d -m 755 "$DEST"
for f in cctv-daily-kr.sh cctv-import.py cctv-check.py cctv-report.py; do
  install -m 755 "$SRC/$f" "$DEST/$f"
done
printf '     %s\n' "$DEST"

say "4/6  비밀 두 개"
# read -rs: not echoed, and not in the shell's history either, because it never
# becomes part of a command line. `cat >` would have printed the key as it was
# pasted — that happened once in this project already.
secret() { # secret <file> <prompt>
  local dest="$1" prompt="$2" v
  if [ -s "$dest" ]; then
    printf '     %s — 이미 있습니다. 바꾸려면 Enter 말고 새 값을 넣으세요.\n' "$dest"
  fi
  printf '     %s: ' "$prompt"
  read -rs v; printf '\n'
  if [ -z "$v" ]; then
    [ -s "$dest" ] || die "$dest 가 비어 있고 입력도 없습니다."
    printf '     그대로 둡니다.\n'; return
  fi
  ( umask 077; printf '%s' "$v" > "$dest" )
  chown "$SVC_USER:$SVC_USER" "$dest"
  unset v
}

secret "$STATE/.its-key" "ITS 인증키"

printf '     관리자 메일: '
read -r ADMIN_EMAIL
printf '     관리자 비밀번호: '
read -rs ADMIN_PASS; printf '\n'
if [ -n "$ADMIN_PASS" ]; then
  ( umask 077; printf '%s\n%s\n' "$ADMIN_EMAIL" "$ADMIN_PASS" > "$STATE/.louver-login" )
  chown "$SVC_USER:$SVC_USER" "$STATE/.louver-login"
  unset ADMIN_PASS
elif [ ! -s "$STATE/.louver-login" ]; then
  die "로그인 파일이 없습니다. 비밀번호 없이는 매달 쿠키를 손으로 넣어야 합니다."
fi
# Said plainly rather than left for someone to discover: this file is full
# admin access to the account, at rest, on a cheap box. The server has no
# scoped API key to offer instead. What it buys is that no *session* outlives a
# run — the script revokes the one it minted when it finishes.
printf '     %s 는 계정 전체 권한입니다. 서버에 범위 제한 키가 없어서 다른 방법이 없습니다.\n' "$STATE/.louver-login"
printf '     대신 세션은 실행이 끝날 때마다 되돌립니다.\n'

say "5/6  타이머"
install -m 644 "$SRC/systemd/cctv-daily-kr.service" /etc/systemd/system/
install -m 644 "$SRC/systemd/cctv-daily-kr.timer" /etc/systemd/system/
systemd-analyze verify /etc/systemd/system/cctv-daily-kr.timer \
  || die "타이머 파일이 systemd 검사를 통과하지 못했습니다."
systemctl daemon-reload
systemctl enable --now cctv-daily-kr.timer
systemctl list-timers cctv-daily-kr.timer --no-pager | sed -n '1,2p;$p'

if [ "$NO_PREFLIGHT" = 1 ]; then
  say "끝. --no-preflight 이므로 ITS 응답은 확인하지 않았습니다."
  exit 0
fi

say "6/6  이 IP 에서 ITS 가 응답하는지 — 이게 핵심입니다"
# --dry-run fetches for real and registers nothing. If this fails, the box's
# address is blocked and the box is useless, which is worth knowing in the
# first five minutes rather than after the first silent night.
if runuser -u "$SVC_USER" -- env HOME="$STATE" "$DEST/cctv-daily-kr.sh" --dry-run; then
  cat <<'DONE'

   ITS 가 이 IP 에 응답합니다. 이 박스는 쓸 수 있습니다.

   다음:
     systemctl start cctv-daily-kr      # 지금 한 바퀴 (약 세 시간)
     journalctl -u cctv-daily-kr -f     # 진행 보기
     systemctl list-timers cctv-daily-kr.timer
DONE
else
  cat <<'FAIL'

   ITS 가 응답하지 않았습니다. 위의 사유를 보세요.

     HTTP 000  이 IP 가 막혀 있습니다. 한국 안이라도 클라우드 대역은 막힐 수
               있습니다 — 다른 사업자(국내 호스팅)로 옮기는 게 답입니다.
     카메라 N 대뿐  인증키가 아직 승인 대기이거나 거부됐습니다.

   타이머는 설치돼 있으니, 원인을 고친 뒤 다시 확인하면 됩니다:
     runuser -u louver -- env HOME=/var/lib/louver /opt/louver/cctv-daily-kr.sh --dry-run
FAIL
  exit 1
fi
