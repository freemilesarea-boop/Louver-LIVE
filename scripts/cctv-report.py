#!/usr/bin/env python3
"""What is in the camera catalogue, per authority.

Three numbers per provider, because they answer three different questions and
only the third is about customers:

  등록    rows in the catalogue
  확인됨  rows this server has opened at least once
  보임    rows a customer can actually choose — checked, passed, and active

A gap between the first two is work left to do. A gap between the second and
third is cameras that failed, and `--failures` says why.

Reads only. Takes the admin session cookie the same way the other two scripts
do, from a file, so no token reaches a command line.
"""

import argparse
import json
import sys
import urllib.error
import urllib.request
from collections import Counter

DEFAULT_COOKIE_FILE = "/root/.louver-cookie"


def read_cookie(path):
    try:
        with open(path, encoding="utf-8") as fh:
            raw = fh.read().strip()
    except OSError as e:
        raise SystemExit(f"[cctv] 쿠키 파일을 읽을 수 없습니다 ({path}): {e}")
    if not raw:
        raise SystemExit(f"[cctv] {path} 이(가) 비어 있습니다.")
    return raw if "=" in raw else f"louver_session={raw}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="https://247streams.kr")
    ap.add_argument("--cookie-file", default=DEFAULT_COOKIE_FILE)
    ap.add_argument("--failures", action="store_true", help="실패 사유도 센다")
    args = ap.parse_args()

    req = urllib.request.Request(
        f"{args.base}/api/admin/cctv/sources",
        headers={"cookie": read_cookie(args.cookie_file)},
    )
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            if "json" not in r.headers.get("content-type", ""):
                raise SystemExit("[cctv] JSON 이 아닙니다. 이 경로가 배포됐는지 확인해 주세요.")
            rows = json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        raise SystemExit(f"[cctv] HTTP {e.code} — 쿠키나 관리자 권한을 확인해 주세요.")
    except urllib.error.URLError as e:
        raise SystemExit(f"[cctv] 서버에 연결할 수 없습니다: {e.reason}")

    total = Counter(r["provider"] for r in rows)
    checked = Counter(r["provider"] for r in rows if r.get("checked_at"))
    visible = Counter(
        r["provider"] for r in rows if r.get("checked_ok") and r.get("active")
    )

    print(f"{'등록':>7} {'확인됨':>7} {'보임':>7}  제공처")
    for p in sorted(total):
        print(f"{total[p]:7} {checked[p]:7} {visible[p]:7}  {p}")
    print(f"{sum(total.values()):7} {sum(checked.values()):7} {sum(visible.values()):7}  합계")

    if args.failures:
        why = Counter(
            r.get("check_note") or "(사유 없음)"
            for r in rows
            if r.get("checked_at") and not r.get("checked_ok")
        )
        if why:
            print("\n실패 사유:")
            for note, n in why.most_common():
                print(f"{n:7}  {note[:100]}")


if __name__ == "__main__":
    sys.exit(main())
