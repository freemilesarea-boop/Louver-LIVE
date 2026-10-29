#!/usr/bin/env python3
"""Open every unchecked camera in the catalogue, one at a time.

An import is somebody else's claim about a URL. This is the server opening it
with FFmpeg and writing down what was there — and it is the only thing that
makes a camera visible to a customer. A row that has not been checked, or that
failed, stays in the catalogue and stays hidden.

One at a time and with a pause between, because every check is a real
connection out to a traffic authority's server, and because the box running
this also has live broadcasts on it.

Resumable: it asks the server which rows have no check yet, so a run that is
interrupted — a dropped ssh session, a Ctrl-C — is continued by running it
again. Nothing is lost and nothing is checked twice.

    nohup python3 cctv-check.py --provider 'VDOT (Virginia)' > /root/va-check.log 2>&1 &

`nohup` matters for a large list: 1,700 cameras take hours, and a laptop's ssh
session will not stay open that long.
"""

import argparse
import json
import sys
import time
import urllib.error
import urllib.request


# --- the cookie ------------------------------------------------------------

DEFAULT_COOKIE_FILE = "/root/.louver-cookie"


def read_cookie(path):
    """The admin session cookie, from a file.

    A file and not a shell variable. The variable form was tried and it failed
    in the way that matters: pasting a token into
    `export LOUVER_COOKIE="louver_session=$C"` let the paste land on the
    *name*, which produced a header the server could only answer with 401 —
    and put a live session token on a command line, where the shell keeps it.

    A file takes the token on stdin (`cat > file`), so nothing is in the
    history. The content may be the bare value or the whole `name=value`;
    both work, because whoever is pasting this should not have to know which.
    """
    try:
        with open(path, encoding="utf-8") as fh:
            raw = fh.read().strip()
    except OSError as e:
        raise SystemExit(
            f"[cctv] 쿠키 파일을 읽을 수 없습니다 ({path}): {e}\n"
            f"       브라우저에서 louver_session 값을 복사한 뒤:\n"
            f"         cat > {path}\n"
            f"         <값 붙여넣기, Enter, Ctrl-D>\n"
            f"         chmod 600 {path}"
        )
    if not raw:
        raise SystemExit(f"[cctv] {path} 이(가) 비어 있습니다.")
    if any(c.isspace() for c in raw):
        raise SystemExit(
            f"[cctv] {path} 에 공백이나 줄바꿈이 섞여 있습니다. 값만 들어 있어야 합니다."
        )
    return raw if "=" in raw else f"louver_session={raw}"


def get(base, cookie, path):
    req = urllib.request.Request(f"{base}{path}", headers={"cookie": cookie})
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            # Checked explicitly: the SPA answers an unknown path with 200 and
            # an HTML page, which `json.loads` would then fail on with a
            # message about character 0 instead of "that route is not there".
            ctype = r.headers.get("content-type", "")
            body = r.read().decode()
            if "json" not in ctype:
                raise SystemExit(
                    f"[cctv] {path} 이(가) JSON을 주지 않았습니다 (content-type: {ctype}).\n"
                    "       서버에 이 경로가 배포되지 않았을 수 있습니다."
                )
            return json.loads(body)
    except urllib.error.HTTPError as e:
        if e.code == 401:
            raise SystemExit(
                "[cctv] HTTP 401 — 서버가 세션을 인정하지 않았습니다.\n"
                "       만료됐거나, 값이 잘못 들어갔거나, 관리자 계정이 아닙니다."
            )
        if e.code == 403:
            raise SystemExit("[cctv] HTTP 403 — 이 계정은 관리자가 아닙니다.")
        raise SystemExit(f"[cctv] HTTP {e.code}: {e.read().decode(errors='replace')[:300]}")
    except urllib.error.URLError as e:
        raise SystemExit(f"[cctv] 서버에 연결할 수 없습니다: {e.reason}")


def check_one(base, cookie, source_id):
    """One camera. A failure here is data, not a reason to stop the run."""
    req = urllib.request.Request(
        f"{base}/api/admin/cctv/sources/{source_id}/check",
        data=b"",
        headers={"cookie": cookie},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=180) as r:
            d = json.loads(r.read().decode())
            return bool(d.get("ok")), d.get("message", "")
    except urllib.error.HTTPError as e:
        if e.code in (401, 403):
            # The one failure that is not about this camera. Stop, rather than
            # burn through the rest of the list recording the same wrong reason.
            raise SystemExit(f"[cctv] HTTP {e.code} — 세션이 끊겼습니다. 쿠키를 다시 받아 재실행해 주세요.")
        return False, f"HTTP {e.code}: {e.read().decode(errors='replace')[:160]}"
    except urllib.error.URLError as e:
        return False, f"연결 실패: {e.reason}"
    except (ValueError, TimeoutError) as e:
        return False, f"응답을 읽을 수 없습니다: {e}"


def summarise(done, ok, reasons, started):
    print(f"[cctv] 확인 {done}  성공 {ok}  실패 {done - ok}  경과 {int(time.time() - started)}s")
    if reasons:
        print("[cctv] 실패 사유:")
        for msg, count in sorted(reasons.items(), key=lambda kv: -kv[1]):
            print(f"       {count:5d}  {msg}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="https://247streams.kr")
    ap.add_argument("--cookie-file", default=DEFAULT_COOKIE_FILE, help="관리자 세션 쿠키가 담긴 파일")
    ap.add_argument("--provider", help="이 제공처만 확인한다 (예: 'VDOT (Virginia)')")
    ap.add_argument("--region", help="이 지역 코드만 확인한다 (예: US)")
    ap.add_argument("--sleep", type=float, default=2.0, help="카메라 사이 간격(초)")
    ap.add_argument("--limit", type=int, help="이번 실행에서 확인할 최대 대수")
    ap.add_argument(
        "--recheck-failed",
        action="store_true",
        help="확인은 됐지만 실패한 카메라를 다시 열어본다 (소스 쪽 복구 후)",
    )
    args = ap.parse_args()

    cookie = read_cookie(args.cookie_file)

    rows = get(args.base, cookie, "/api/admin/cctv/sources")
    if args.provider:
        rows = [r for r in rows if r.get("provider") == args.provider]
    if args.region:
        rows = [r for r in rows if r.get("region") == args.region.upper()]

    if args.recheck_failed:
        todo = [r for r in rows if r.get("checked_at") and not r.get("checked_ok")]
        what = "실패했던"
    else:
        todo = [r for r in rows if not r.get("checked_at")]
        what = "미확인"

    visible = sum(1 for r in rows if r.get("checked_ok") and r.get("active"))
    print(f"[cctv] 대상 {len(rows)}  고객에게 보이는 {visible}  {what} {len(todo)}")
    if args.limit:
        todo = todo[: args.limit]
    if not todo:
        print("[cctv] 할 일이 없습니다.")
        return

    started, ok, reasons = time.time(), 0, {}
    done = 0
    try:
        for n, r in enumerate(todo, 1):
            good, msg = check_one(args.base, cookie, r["id"])
            done += 1
            if good:
                ok += 1
            else:
                reasons[msg] = reasons.get(msg, 0) + 1
            # Flushed: this is read through `tail -f` on a log, and a buffered
            # line that arrives an hour later cannot tell anybody it is alive.
            print(
                f"  {n}/{len(todo)}  {'OK  ' if good else 'FAIL'}  {r['name'][:60]}"
                + ("" if good else f"  — {msg[:90]}"),
                flush=True,
            )
            if n < len(todo):
                time.sleep(args.sleep)
    except KeyboardInterrupt:
        print("\n[cctv] 중단했습니다. 다시 실행하면 남은 것만 이어서 확인합니다.")

    summarise(done, ok, reasons, started)
    after = get(args.base, cookie, "/api/admin/cctv/sources")
    if args.provider:
        after = [r for r in after if r.get("provider") == args.provider]
    print(
        f"[cctv] 지금 고객에게 보이는 카메라: "
        f"{sum(1 for r in after if r.get('checked_ok') and r.get('active'))}대"
    )


if __name__ == "__main__":
    sys.exit(main())
