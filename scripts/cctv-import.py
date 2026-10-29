#!/usr/bin/env python3
"""Turn a traffic authority's published camera list into catalogue rows.

Why a script and not the `jq` one-liner the docs used to carry: the production
server has no `jq`, and a one-liner cannot do the three things that actually
matter here — drop the cameras the publisher itself marks as broken, refuse to
post a list it could not parse, and check the HTTP status of the POST. The last
one is not hypothetical: an expired session answers `401` with a body that looks
nothing like an error if you only read it with `grep`.

Standard library only, so it runs on the server with nothing installed.

    cat > /root/.louver-cookie     # paste the browser's louver_session value
    chmod 600 /root/.louver-cookie
    python3 cctv-import.py --source vdot --input /root/va-cams.json

The cookie comes from the browser (DevTools, Application, Cookies), not from a
password on a command line, and it arrives on stdin rather than in a variable
assignment — see `read_cookie`.

Adding a source means adding one function to SOURCES below. Each one is written
against a response that was actually fetched and read — never against a field
name taken from somebody's write-up, because a wrong guess here imports
hundreds of addresses that point nowhere.
"""

import argparse
import json
import os
import sys
import urllib.error
import urllib.request

# --- sources ---------------------------------------------------------------
#
# A source yields (name, url) pairs. It filters; it does not validate. The
# server validates — these addresses go through the same rules as one typed
# into the form, and a list is not a way around them.


def vdot(doc):
    """Virginia DOT, from the 511 map's public camera layer.

    Verified against the live response on 2026-10-29: a GeoJSON
    FeatureCollection whose properties carry `https_url` (the HLS playlist),
    `description` (a readable location), `jurisdiction`, `active` and
    `problem_stream`.

    `name` is the stream key — a random-looking string, no use to anybody
    choosing a camera — so the readable name is built from the description and
    the jurisdiction instead. `ios_url` is the same picture as fMP4; the plain
    playlist is the one FFmpeg handles without surprises.
    """
    for f in doc.get("features", []):
        p = f.get("properties") or {}
        if not p.get("active"):
            continue
        # The publisher's own word that a camera is broken. Honouring it saves
        # a connection attempt per camera and a wrong "failed" note on a row
        # that was never going to work.
        if p.get("problem_stream"):
            continue
        url = (p.get("https_url") or "").strip()
        if not url:
            continue
        where = (p.get("description") or "").strip()
        area = (p.get("jurisdiction") or "").strip()
        if where and area:
            name = f"{where} ({area})"
        else:
            name = where or area or str(p.get("id") or "").strip()
        if not name:
            continue
        yield name, url


def its(doc):
    """Korea's 국가교통정보센터, from the ITS open API.

    Verified against the live response on 2026-10-29: `response.data` is the
    array, and each row carries `cctvname` (a readable name, already prefixed
    with the road), `cctvurl`, `cctvtype` and `cctvformat`.

    `cctvtype` is the whole filter. 1 and 4 are live HLS (4 over HTTPS); 2 and
    5 are mp4 files and 3 is a still image, and none of those three can carry a
    broadcast that runs for days — FFmpeg reaches the end of a file and stops.

    Two things to know about these addresses. They are tokenised, so the URL is
    the grant rather than a location, and the token expires in about a day:
    this import has to be repeated, and a row whose URL moved has its check
    reset so it cannot be offered on the strength of yesterday's. And the list
    API refuses connections from outside Korea, so this import runs from a
    machine there, while the server that opens the streams does not have to be
    — `cctvsec.ktict.co.kr` answers from abroad.
    """
    data = (doc.get("response") or {}).get("data") or []
    for r in data:
        if str(r.get("cctvtype")) not in ("1", "4"):
            continue
        url = (r.get("cctvurl") or "").strip()
        name = (r.get("cctvname") or "").strip()
        if not url or not name:
            continue
        yield name, url


SOURCES = {
    "vdot": {
        "region": "US",
        "provider": "VDOT (Virginia)",
        "extract": vdot,
    },
    "its": {
        "region": "KR",
        # Overridden with --provider when the highways and the national roads
        # are imported separately, which is how the picker tells them apart.
        "provider": "국가교통정보센터",
        "extract": its,
    },
}


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


# --- posting ---------------------------------------------------------------


def post(base, cookie, rows):
    """One chunk. Returns the parsed outcome list, or raises with the status."""
    req = urllib.request.Request(
        f"{base}/api/admin/cctv/sources",
        data=json.dumps(rows).encode(),
        headers={"content-type": "application/json", "cookie": cookie},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=180) as r:
            if r.status != 200:
                raise SystemExit(f"[cctv] 예상하지 못한 응답: HTTP {r.status}")
            return json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        body = e.read().decode(errors="replace")[:400]
        if e.code == 401:
            raise SystemExit(
                "[cctv] HTTP 401 — 서버가 세션을 인정하지 않았습니다. 셋 중 하나입니다:\n"
                "       쿠키가 만료됐거나, 값이 잘못 들어갔거나, 관리자 계정이 아닙니다.\n"
                "       브라우저에서 louver_session 값을 다시 복사해 주세요."
            )
        if e.code == 403:
            raise SystemExit("[cctv] HTTP 403 — 이 계정은 관리자가 아닙니다.")
        raise SystemExit(f"[cctv] HTTP {e.code}: {body}")
    except urllib.error.URLError as e:
        raise SystemExit(f"[cctv] 서버에 연결할 수 없습니다: {e.reason}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--source", required=True, choices=sorted(SOURCES))
    ap.add_argument("--input", required=True, help="피드를 받아 저장한 JSON 파일")
    ap.add_argument("--base", default="https://247streams.kr")
    ap.add_argument(
        "--chunk",
        type=int,
        default=400,
        help="한 요청에 보낼 카메라 수. 서버 상한은 2000이지만, 나눠 보내면 "
        "실패한 구간만 다시 보낼 수 있고 요청이 오래 열려 있지 않는다.",
    )
    ap.add_argument("--out-dir", default="/root/cctv-import")
    ap.add_argument("--dry-run", action="store_true", help="만들어질 행만 보여주고 전송하지 않는다")
    ap.add_argument("--cookie-file", default=DEFAULT_COOKIE_FILE, help="관리자 세션 쿠키가 담긴 파일")
    ap.add_argument(
        "--provider",
        help="제공처 이름을 바꾼다. 같은 API를 쪼개 받을 때 쓴다 "
        "(예: '국가교통정보센터 (고속도로)'). 선택 화면에서 이것으로 구분된다",
    )
    args = ap.parse_args()

    spec = dict(SOURCES[args.source])
    if args.provider:
        spec["provider"] = args.provider
    try:
        with open(args.input, encoding="utf-8") as fh:
            doc = json.load(fh)
    except (OSError, ValueError) as e:
        raise SystemExit(f"[cctv] {args.input} 을 읽을 수 없습니다: {e}")

    # Deduplicated here as well as in the server, because a feed that lists one
    # camera twice would otherwise spend two requests on it and report it twice.
    seen, rows = set(), []
    for name, url in spec["extract"](doc):
        if url in seen:
            continue
        seen.add(url)
        rows.append(
            {
                "region": spec["region"],
                "provider": spec["provider"],
                "name": name[:200],
                "url": url,
            }
        )

    if not rows:
        raise SystemExit(
            f"[cctv] {args.input} 에서 쓸 수 있는 카메라를 찾지 못했습니다.\n"
            "       피드 구조가 바뀐 것일 수 있습니다. 응답 앞부분을 확인해 주세요."
        )

    print(f"[cctv] {spec['provider']} — 사용 가능 {len(rows)}대")
    for r in rows[:5]:
        print(f"         {r['name']}\n           {r['url']}")
    if len(rows) > 5:
        print(f"         … 외 {len(rows) - 5}대")

    os.makedirs(args.out_dir, exist_ok=True)
    chunks = [rows[i : i + args.chunk] for i in range(0, len(rows), args.chunk)]
    for n, chunk in enumerate(chunks):
        path = os.path.join(args.out_dir, f"{args.source}-{n:03d}.json")
        with open(path, "w", encoding="utf-8") as fh:
            json.dump(chunk, fh, ensure_ascii=False)
        print(f"[cctv] {path}  ({len(chunk)}대)")

    if args.dry_run:
        print("[cctv] --dry-run 이므로 전송하지 않았습니다.")
        return

    cookie = read_cookie(args.cookie_file)

    ok = bad = 0
    reasons = {}
    for n, chunk in enumerate(chunks):
        out = post(args.base, cookie, chunk)
        c_ok = sum(1 for o in out if o.get("ok"))
        ok += c_ok
        bad += len(out) - c_ok
        for o in out:
            if not o.get("ok"):
                reasons[o.get("message", "")] = reasons.get(o.get("message", ""), 0) + 1
        print(f"[cctv] {n + 1}/{len(chunks)} 전송 — 등록 {c_ok}  거부 {len(out) - c_ok}")

    print(f"[cctv] 합계 — 등록 {ok}  거부 {bad}")
    if reasons:
        print("[cctv] 거부 사유:")
        for msg, count in sorted(reasons.items(), key=lambda kv: -kv[1]):
            print(f"       {count:5d}  {msg}")
    print(
        "[cctv] 등록만으로는 고객에게 보이지 않습니다. "
        "cctv-check.py 로 서버가 직접 열어본 뒤에 보입니다."
    )


if __name__ == "__main__":
    sys.exit(main())
