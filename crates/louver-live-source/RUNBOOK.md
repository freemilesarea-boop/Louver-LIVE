# Beta VPS runbook — install, configure, and prove it works

Preparation only. **No VPS has been bought or created, and nothing here has
been run against one.** `VPS.md` sizes the machine and `REVOCATION.md` explains
the suspension design; this is the step-by-step for a human at a terminal.

Every command below is to be run **on the beta VPS**. None of them touches
247streams' production server, its Caddyfile, its database, its FFmpeg
processes, or any customer's broadcast or stream key. Where a step could be
confused for a production action, it says so.

---

## 0. The isolation rule, first

This is the condition everything else is subordinate to, and it is worth
stating before any command:

| | beta VPS | production |
|---|---|---|
| machine | separate | untouched |
| FFmpeg processes | its own, owned by handle from `spawn` | **never signalled** |
| database | none at all (JSON state files) | untouched |
| OAuth tokens | none — this service has no OAuth code | untouched |
| stream keys | the tester's own, in this VPS's environment | **never copied here** |
| Docker socket | not shared | not mounted |
| media volume | its own | not mounted |

The only link between the two is one HTTP call: the worker asks production
`GET /api/me` who a session cookie belongs to, once per token. Nothing else.

Three source-scanned tests enforce the left column: this crate cannot name
`pkill`, `pgrep`, `/proc/`, `libc::kill`, `sysinfo`, `from_pid`; cannot name
`rusqlite`, `cloud.db` or SQL (and does not declare the dependency); and cannot
name `refresh_token`, `access_token`, `liveBroadcasts`, `oauth2` or
`client_secret`. If a future change reaches for any of them, a test fails
rather than a review catching it.

---

## 1. VPS specification — confirmed

Sized from measurement, not taste. The numbers come from running the product's
own argv against a real RTMP sink (`VPS.md` §1).

| | **confirmed requirement** | why this number |
|---|---|---|
| vCPU | **4 dedicated** (6 if 4K sources) | one encode measured 95–148% of a core; `Limits::for_machine` computes `(4-1)/1.5 = 2` concurrent, which is the target. Two 4K→1080p measured 3.5 cores, which does not fit in 4. |
| vCPU type | **dedicated, not burstable** | 95–148% *sustained for 24 h* exhausts burst credit and then throttles — which presents as the frozen picture the watchdog exists to catch, on every broadcast at once. |
| RAM | **8 GB** (4 GB floor) | 2 × encode measured ~0.98 GB worst case; +OS, +Caddy, +`ffprobe` over a 512 MB upload, +a 24 h journal. 4 GB works; 8 GB means the kernel never has to choose. |
| disk | **80 GB SSD** (40 GB floor) | OS+packages ~5 GB, tools ~0.5 GB, media 4 GiB × beta users, journal capped at 2 GB. 80 GB leaves room to add a beta user without a resize. |
| transfer | **≥ 5 TB/month included** | **~138 GB egress per 24 h for two broadcasts** (12.8 Mbit/s × 86 400 s). A 1 TB allowance is gone in ~7 days. This disqualifies plans faster than price does. |
| port | 1 Gbps | the constraint is the monthly allowance, not line rate (~26 Mbps steady both ways). |
| OS | **Ubuntu 24.04 LTS** | its distro FFmpeg is **6.1.1** — the exact version every measurement and the 1080p cap filter were verified against. Debian 12 ships 5.1, where `force_divisible_by` has not been checked here. |
| region | near YouTube ingest and the operator | allowances differ by region at the same price. |

**Not required**: NVMe (this is not disk-bound — it reads a local playlist and
writes to a socket), a second region, redundancy, or a load balancer. One box,
for a test.

---

## 2. Installation runbook

### 2.1 Before the first command

```bash
# Who you are and what you are on. If this says anything other than the beta
# VPS you just created, STOP.
hostname; cat /etc/os-release | grep -E '^(NAME|VERSION)='
nproc; free -m | head -2; df -h / | tail -1
```

Expect Ubuntu 24.04, 4+ cores, 8 GB, 80 GB. If `nproc` is 2, the worker will
allow only one concurrent broadcast and the two-broadcast test cannot run.

### 2.2 System packages and FFmpeg

```bash
apt-get update
apt-get install -y ffmpeg curl ca-certificates ufw

# The version the measurements were taken against.
ffmpeg  -version | head -1   # expect 6.1.1
ffprobe -version | head -1   # expect 6.1.1
```

If FFmpeg is not 6.1.1, record the actual version in the test report. The
1080p cap filter (`scale=min(iw\,1920):min(ih\,1080):force_original_aspect_ratio=decrease:force_divisible_by=2`)
was verified empirically against 6.1.1 and nothing else.

### 2.3 yt-dlp — pinned, not the distro package

```bash
# NOT `apt-get install yt-dlp`: the distro version is always too old, and a
# stale yt-dlp fails to extract as YouTube changes.
curl -L -o /usr/local/bin/yt-dlp \
  https://github.com/yt-dlp/yt-dlp/releases/download/2026.08.19/yt-dlp_linux
chmod 755 /usr/local/bin/yt-dlp
yt-dlp --version    # expect 2026.08.19
```

`2026.08.19` is the version the resolver was developed and tested against.
**Pin it.** Update it as a deliberate step with a re-run of the resolver tests
— never with an unattended `yt-dlp -U`, because an update that changes the JSON
shape breaks resolution for every broadcast at once.

The worker never passes yt-dlp a cookie file, `--exec`, `--username` or an
output template; `resolver.rs` asserts that in a test.

### 2.4 A dedicated unprivileged user

```bash
useradd --system --create-home --home-dir /srv/live-source --shell /usr/sbin/nologin louver
install -d -o louver -g louver -m 750 /srv/live-source/media
install -d -o louver -g louver -m 750 /var/lib/live-source
```

The worker runs as `louver`, not root. It spawns FFmpeg, writes media and
writes state; none of that needs privilege.

### 2.5 The binary

Built from this branch — `cargo build --release -p louver-live-source` on a
build host, or on the VPS if a Rust toolchain is installed there.

```bash
install -o root -g root -m 755 live-source-api /usr/local/bin/live-source-api
install -d -o louver -g louver -m 750 /srv/live-source/beta
# the three files the beta page is made of
install -o louver -g louver -m 640 beta/index.html beta/app.js beta/app.css /srv/live-source/beta/
```

### 2.6 The three secrets

```bash
# Three different values. Generate them here; do not reuse anything.
printf 'LOUVER_LIVE_SOURCE_SECRET=%s\n'       "$(openssl rand -hex 32)" >  /etc/louver-live-source.env
printf 'LOUVER_LIVE_SOURCE_GATE_SECRET=%s\n'  "$(openssl rand -hex 32)" >> /etc/louver-live-source.env
printf 'LOUVER_LIVE_SOURCE_ADMIN_SECRET=%s\n' "$(openssl rand -hex 32)" >> /etc/louver-live-source.env
chown root:louver /etc/louver-live-source.env
chmod 640 /etc/louver-live-source.env
```

They answer three different questions — which user is this (signing), did this
come through Caddy (gate), is this the operator (admin) — and each is hashed
under its own domain-separation label, so **reusing one value across two
variables does not make either accept the other's header.** Use three anyway.

The binary refuses to start if any is missing or under 32 bytes. There is no
flag that disables the gate or the admin API.

**The gate secret must also be given to Caddy** (§3). Nothing else leaves this
file.

### 2.7 systemd

```ini
# /etc/systemd/system/live-source.service
[Unit]
Description=247streams live-source worker (beta)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=louver
Group=louver
EnvironmentFile=/etc/louver-live-source.env
ExecStart=/usr/local/bin/live-source-api \
  --listen 10.0.0.2:9080 \
  --origin https://247streams.kr \
  --allow-origin https://247streams.kr \
  --beta-dir /srv/live-source/beta \
  --media-dir /srv/live-source/media \
  --state-dir /var/lib/live-source \
  --max-concurrent 2
Restart=always
RestartSec=5
# A 512MB upload plus ffprobe over it.
LimitNOFILE=8192
# It needs none of these.
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/srv/live-source/media /var/lib/live-source

[Install]
WantedBy=multi-user.target
```

```bash
systemctl daemon-reload
systemctl enable --now live-source
systemctl status live-source --no-pager | head -5
journalctl -u live-source -n 20 --no-pager
```

**`--listen 10.0.0.2:9080` — the private address, never `0.0.0.0`.** Substitute
the VPS's actual private/Caddy-facing IP. Binding to all interfaces would make
the firewall the only layer instead of the second.

Expect in the log:

```
[louver][live-source-api] cores=4 max_concurrent=2 max_per_user=1 예상 비용=3.0 core / 800 MB
[louver][live-source-api] gate=on admin=on allow-origin=https://247streams.kr 송출 대상 사용자 N명
[louver][live-source-api] 송출 정지된 계정 0건
[louver][live-source-api] 복원된 작업 0건
[louver][live-source-api] listening on http://10.0.0.2:9080  (beta UI: /beta/)
```

No secret, no destination name and no URL appears in that banner — eight tests
assert it, including over the binary's own startup output.

### 2.8 Journal caps

```bash
# A server broadcasting around the clock writes all day, and journald has no
# ceiling by default.
printf 'SystemMaxUse=2G\nMaxRetentionSec=14day\n' >> /etc/systemd/journald.conf
systemctl restart systemd-journald
```

### 2.9 Firewall — default deny

```bash
ufw default deny incoming
ufw default allow outgoing          # YouTube ingest out, the live source in
ufw allow from <OPERATOR_IP>/32 to any port 22   proto tcp   # SSH, key only
ufw allow from <PRODUCTION_IP>/32 to any port 9080 proto tcp # Caddy → worker
ufw --force enable
ufw status numbered
```

Then harden SSH: `PasswordAuthentication no`, `PermitRootLogin no`.

Three layers in front of the worker's port, and this is the second:

1. it listens on the private address only;
2. the firewall admits only production's IP;
3. the gate secret — measured: a request reaching the port directly without
   the header answers **403**.

**`/admin/*` is deliberately not reachable through any of this.** Caddy's
matchers cover only `/api/live-source/*` and `/beta/*`, so the operator routes
fall to production's catch-all and never arrive from the internet. They are
reachable only from the VPS itself:

```bash
# On the VPS, over SSH. Note the loopback address: this is why --listen must
# also accept local connections, or use the private IP here.
curl -s -H "X-Louver-Admin: $ADMIN" http://10.0.0.2:9080/admin/revoked
```

---

## 3. Caddy — production side, by a human, not by this runbook

**This runbook does not change production.** `Caddyfile.sample` holds the two
blocks to copy into production's `./Caddyfile`, and they have been validated
against a real Caddy v2.10.0 (`caddy-validate.sh`, 15/15).

What a human does, once, on the production host:

1. add the two `handle` blocks from `Caddyfile.sample` **before** the existing
   catch-all `reverse_proxy louver:8080`;
2. put `LOUVER_GATE_SECRET` into Caddy's environment with the same value as the
   worker's `LOUVER_LIVE_SOURCE_GATE_SECRET`;
3. `caddy validate --config Caddyfile` — **then** `caddy reload`.

`caddy reload` replaces the running configuration without restarting the Caddy
container and without touching the `louver` container, so **no customer
broadcast is interrupted.** Measured: a long-lived streamed response through
Caddy survived a mid-flight reload intact (30/30 chunks, exit 0), which matters
because production's `/api/events` is SSE behind the same Caddy. And FFmpeg
sends to YouTube directly, never through Caddy at all.

A bad config is refused at adapt time and the running one keeps serving — also
measured. So the rollback is automatic.

Two cautions found by running it:

- **Do not add an `admin` directive** to production's Caddyfile. It has none,
  so Caddy listens on its default `localhost:2019` and `caddy reload` works.
  Setting `admin` to a random port is what makes reload unreachable.
- **Do not write `header_up -X-Louver-Gate` before `header_up X-Louver-Gate …`.**
  Both land in one `HeaderOps` and Caddy applies `delete` after `set`, so the
  header arrives **absent** and every beta request is a 403. `header_up` is
  already a `set` that replaces a client's value. `caddy validate` passes on
  the broken version, so only live traffic catches it.

---

## 4. Test material — the tester's own, never a customer's

### 4.1 The YouTube Live source

Required:

- a YouTube channel **the tester owns**, with a live stream running; or a
  stream they have **documented permission** to re-transmit;
- public, and **not** members-only, age-restricted, private or DRM-protected —
  the resolver refuses those and this feature does not work around them.

Produce one if needed, from any machine with a camera or a test pattern:

```bash
# On the tester's own machine, to the tester's own channel.
ffmpeg -re -f lavfi -i "testsrc2=size=1920x1080:rate=30" \
       -f lavfi -i "sine=frequency=1000" \
       -c:v libx264 -preset veryfast -b:v 4500k -g 60 \
       -c:a aac -b:a 128k \
       -f flv "rtmps://a.rtmps.youtube.com/live2/<TESTER_OWN_KEY>"
```

Note the watch URL: `https://www.youtube.com/watch?v=<VIDEO_ID>`.

### 4.2 The RTMP destination

**Never a customer's stream key, and never production's.** Two options:

**(a) A local RTMP sink on the VPS** — preferred for the first runs, because
nothing leaves the machine:

```bash
# A sink that accepts and discards, so composition can be checked by recording.
ffmpeg -hide_banner -listen 1 -i "rtmp://127.0.0.1:1935/live/test" \
       -c copy -f mp4 -y /tmp/received.mp4
```

**(b) A second YouTube channel the tester owns** — needed to prove the real
ingest path works.

Configure it under the tester's user id only:

```bash
# Appended to /etc/louver-live-source.env, then `systemctl restart live-source`.
LOUVER_LIVE_SOURCE_DESTINATIONS={"<TESTER_USER_ID>":{"beta-test":"rtmp://127.0.0.1:1935/live/test"}}
```

The shape is keyed by **user id first**. A flat `{name: url}` map is refused at
startup — that was the isolation defect this design exists to prevent. The
worker validates every URL at startup: not `rtmp://`/`rtmps://`, or containing
whitespace or a control character, is a refusal to start.

`<TESTER_USER_ID>` is the id 247streams' `/api/me` returns for the tester's own
account. Find it by signing in as the tester and reading the `id` field — not
by looking in production's database.

### 4.3 Playlist media

```bash
# Two short audio files, on the tester's machine, from material they own.
ffmpeg -f lavfi -i "sine=frequency=440:duration=120" -c:a aac song-a.m4a
ffmpeg -f lavfi -i "sine=frequency=880:duration=120" -c:a aac song-b.m4a
```

Upload through the beta page, or directly:

```bash
curl -X PUT --data-binary @song-a.m4a \
  -H "X-Louver-Gate: $GATE" -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/octet-stream" \
  https://247streams.kr/api/live-source/media/song-a.m4a
```

Accepted: `mp4 mov m4v mkv webm m4a mp3 aac wav`, ≤ **512 MB** per file,
≤ **4 GiB** per user, ≤ **200** playlist items. An extension is a claim —
`ffprobe` decides, and a file that is not media is refused with nothing left
behind.

---

## 5. Test 1 — resolve a YouTube Live URL, on its own

**Run this first, before any broadcast.** It is the one step this development
environment could never do: egress to YouTube is blocked here
(`youtube.com → 000`), so the resolver has only ever been exercised against
fakes. If this fails, nothing downstream is worth attempting.

### 5.1 yt-dlp directly, the way the worker calls it

```bash
# Exactly the worker's argv (resolver.rs::argv), so a difference here is the
# environment and not the product.
yt-dlp --ignore-config --no-warnings --no-progress --no-playlist \
       --dump-single-json --socket-timeout 20 \
       -f 'best[protocol^=m3u8]/best' \
       -- 'https://www.youtube.com/watch?v=<VIDEO_ID>' \
  | python3 -c '
import json,sys
d = json.load(sys.stdin)
print("is_live :", d.get("is_live"))
print("live    :", d.get("live_status"))
print("w x h   :", d.get("width"), "x", d.get("height"))
print("proto   :", d.get("protocol"))
print("url set :", bool(d.get("url")))   # the URL itself is NOT printed
'
```

**Pass**: `is_live: True`, a width and height, an m3u8 protocol, `url set: True`.

Do **not** print the resolved URL. YouTube signs it, and the worker treats it
as a secret — no log line, error message or API response in this crate ever
carries it.

**If it fails**, the message tells you which:

| symptom | meaning |
|---|---|
| `is_live: False` / `live_status: was_live` | not live now. The worker returns `not_live` (502). |
| `Private video` / `members-only` / `age-restricted` | unsupported by design, not a bug. |
| `Sign in to confirm you're not a bot` | YouTube is rate-limiting this IP. Try later; do **not** add cookies — the worker refuses `--cookies` by design. |
| timeout | egress blocked, or the 20 s `--socket-timeout` is too short for this link. |

### 5.2 Through the worker

```bash
# Via Caddy, as a browser would. TOKEN comes from the handshake (§6.1).
curl -s -X POST https://247streams.kr/api/live-source/check \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"source_url":"https://www.youtube.com/watch?v=<VIDEO_ID>"}'
```

**Pass**: `{"ok":true,"message":"확인됨 · 원본 1920×1080","video_id":"…"}`.

The response carries the video id (useful, not secret) and the dimensions, and
**not** the resolved manifest URL.

---

## 6. Test 2 — the composition: live picture, playlist sound

The acceptance criterion from the start of this project, unchanged: **look at
the destination.** An HTTP 200 and a running FFmpeg are *not* a pass.

### 6.1 Sign in and start

```bash
# Open https://247streams.kr/beta/ as the tester. The page handshakes with the
# session cookie and holds a 5-minute token in memory only.
# For curl, get a token the same way:
TOKEN=$(curl -s -X POST https://247streams.kr/api/live-source/session \
  -H "Origin: https://247streams.kr" -b "louver_session=<TESTER_COOKIE>" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["token"])')

curl -s -X POST https://247streams.kr/api/live-source/jobs \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"broadcast_id":"beta-1",
       "source_url":"https://www.youtube.com/watch?v=<VIDEO_ID>",
       "playlist":["song-a.m4a","song-b.m4a"],
       "destination":"beta-test"}'
```

Expect `201` and `{"broadcast_id":"beta-1","desired":"running","phase":"resolving",…}`.

### 6.2 Verify at the receiving end

With the local sink (§4.2a), the sink wrote `/tmp/received.mp4`. Check the
**decoded output**, not the logs:

```bash
# Streams present: one video, one audio.
ffprobe -v error -show_entries stream=index,codec_type,codec_name,width,height \
        -of csv=p=0 /tmp/received.mp4

# The picture must be the LIVE SOURCE. Pull a frame and look at it.
ffmpeg -v error -ss 10 -i /tmp/received.mp4 -frames:v 1 -y /tmp/frame.png
# Then view /tmp/frame.png — it must show the live stream's content, not a
# still from the playlist files.

# The sound must be the PLAYLIST. 440 Hz or 880 Hz from §4.3, and NOT the
# 1000 Hz tone the live source carries.
ffmpeg -v error -i /tmp/received.mp4 -t 5 -ac 1 -ar 8000 -f s16le - \
  | python3 -c '
import sys, struct
raw = sys.stdin.buffer.read()
s = struct.unpack(f"<{len(raw)//2}h", raw[:len(raw)//2*2])
# zero crossings → dominant frequency
z = sum(1 for a,b in zip(s, s[1:]) if (a<0) != (b<0))
print("≈", round(z / 2 / (len(s)/8000)), "Hz")
'
```

**Pass**, all four:

1. exactly one video and one audio stream;
2. the frame shows the **live source's** picture;
3. the tone is **440 or 880 Hz** (the playlist), not 1000 Hz (the source);
4. output resolution ≤ 1920×1080 — a 4K source must come out capped.

**Fail** if the picture is from a playlist file, if the audio is the source's
own, or if both audio streams are present. The composition maps `-map 1:v:0`
(live picture) and `-map 0:a:0` (playlist sound); the source's audio is dropped
by *not* being mapped.

With a second YouTube channel (§4.2b), do the same by eye and ear on that
channel's watch page, plus confirm YouTube Studio reports a healthy ingest.

---

## 7. Test 3 — automatic recovery when the source drops

This exercises the defect measured in PHASE 1: **FFmpeg does not exit when a
live source disappears**, because the playlist input is `-stream_loop -1`.
Measured, picture froze 17.4 s while sound ran to 60.0 s. The watchdog is the
only thing that notices.

```bash
# 1. Baseline, while healthy.
curl -s -H "Authorization: Bearer $TOKEN" \
  https://247streams.kr/api/live-source/jobs | python3 -m json.tool
#    expect phase "sending", frames rising, restarts 0

# 2. Stop the upstream YouTube live (§4.1) for ~60 seconds.

# 3. Watch the phase change. Sample every 5 s.
for i in $(seq 1 24); do
  curl -s -H "Authorization: Bearer $TOKEN" https://247streams.kr/api/live-source/jobs \
    | python3 -c 'import json,sys; j=json.load(sys.stdin)["jobs"][0]; print(j["phase"], j["frames"], j["restarts"], j["last_verdict"])'
  sleep 5
done

# 4. Restart the upstream live.
```

**Pass**: phase goes `sending` → `reconnecting` → `sending`; `restarts`
increments; `last_verdict` shows `video_stalled` at least once; `frames`
resumes rising; the picture recovers at the destination.

`frames` frozen while `out_time_ms` advances is `VideoStalled`; both frozen is
`ProcessStalled`. Defaults: stall after 12 s, 20 s grace, backoff
2→4→8→16→32→60 s, capped by `--max-restarts`.

**Also verify the blast radius is one job.** With two broadcasts running, drop
only one source: the other's `frames` must keep rising and its `restarts` must
stay unchanged. The worker signals only its own child, by handle from `spawn` —
never by pid or name.

**Service restart recovery:**

```bash
systemctl restart live-source
journalctl -u live-source -n 5 --no-pager | grep 복원
#    expect "복원된 작업 N건" where N is what was running
```

`recover()` reads `desired`, not the last observed `phase`, and restarts
sequentially. Note the gap — it is a real interruption to the beta broadcast,
and measuring it is the point.

---

## 8. Test 4 — operator forced stop, and no restart afterwards

```bash
# On the VPS. ADMIN from /etc/louver-live-source.env.
ADMIN=$(grep ADMIN_SECRET /etc/louver-live-source.env | cut -d= -f2)

# 1. Nobody revoked yet.
curl -s -H "X-Louver-Admin: $ADMIN" http://10.0.0.2:9080/admin/revoked
#    {"users":[]}

# 2. Revoke the tester.
curl -s -X POST -H "X-Louver-Admin: $ADMIN" -H 'Content-Type: application/json' \
  -d '{"user_id":"<TESTER_USER_ID>"}' http://10.0.0.2:9080/admin/revoke
#    {"changed":true,"stopped":1,"jobs":["beta-1"]}

# 3. The broadcast is off air. Check the destination, not just the API.

# 4. A new job is refused — even though the token is still valid.
curl -s -o /dev/null -w '%{http_code}\n' -X POST \
  https://247streams.kr/api/live-source/jobs \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"broadcast_id":"beta-2","source_url":"https://www.youtube.com/watch?v=<VIDEO_ID>","playlist":["song-a.m4a"],"destination":"beta-test"}'
#    403

# 5. A restart must NOT bring it back.
systemctl restart live-source
journalctl -u live-source -n 10 --no-pager | grep -E "정지된|복원"
#    expect "송출 정지된 계정 1건" and "복원된 작업 0건"
curl -s -H "X-Louver-Admin: $ADMIN" http://10.0.0.2:9080/admin/revoked
#    {"users":["<TESTER_USER_ID>"]}

# 6. Idempotency: revoking again is safe.
curl -s -X POST -H "X-Louver-Admin: $ADMIN" -H 'Content-Type: application/json' \
  -d '{"user_id":"<TESTER_USER_ID>"}' http://10.0.0.2:9080/admin/revoke
#    {"changed":false,"stopped":0,"jobs":[]}

# 7. The audit trail.
cat /var/lib/live-source/admin-audit.jsonl
#    one line per call: at, action, user_id, stopped, jobs[], changed

# 8. Lift it, and confirm broadcasting works again.
curl -s -X POST -H "X-Louver-Admin: $ADMIN" -H 'Content-Type: application/json' \
  -d '{"user_id":"<TESTER_USER_ID>"}' http://10.0.0.2:9080/admin/restore
```

**Also verify the authorization boundary** — each of these must answer 403:

```bash
for cred in "" "$GATE" "$SIGN" "$TOKEN" "wrong"; do
  printf '%-10s -> ' "${cred:0:8}"
  curl -s -o /dev/null -w '%{http_code}\n' \
    ${cred:+-H "X-Louver-Admin: $cred"} http://10.0.0.2:9080/admin/revoked
done
```

The gate secret in the admin header **must** fail, even though both are
32-byte secrets this machine holds — each is hashed under its own
domain-separation label. And confirm the admin route is **not** reachable
through Caddy:

```bash
# From anywhere on the internet. Must NOT be the worker's answer.
curl -s -o /dev/null -w '%{http_code}\n' https://247streams.kr/admin/revoked
```

**With two users**, confirm revoking one leaves the other's broadcast running
and its `restarts` unchanged.

---

## 9. Test 5 — 24-hour continuous soak

**There is no maximum broadcast duration, and there must not be one.**
247streams sells 24/7 unattended broadcasting, and `Schedule.stop_at` is
already `Option` in production — a broadcast with none set runs until something
ends it. The 24 hours here is **how long this test runs**, not a limit.

**A broadcast that stops by itself at hour 24 is a FAILURE, not a pass.**

### 9.1 Before starting

```bash
# Baseline. Count-only on FFmpeg: never `pgrep -af`, which prints argv.
date -u; nproc; free -m | head -2; df -h / | tail -1
pgrep -x ffmpeg | wc -l
cat /proc/net/dev | grep -E "eth0|ens" 
```

**And on production, to prove the test cannot have touched it** — read-only,
by a human:

```
pgrep -x ffmpeg | wc -l     # record; must be unchanged at the end
```

### 9.2 Start two broadcasts, then sample hourly

```bash
# /usr/local/bin/soak-sample.sh — run from cron or a tmux loop.
#!/usr/bin/env bash
TS=$(date -u +%FT%TZ)
JOBS=$(curl -s -H "Authorization: Bearer $TOKEN" https://247streams.kr/api/live-source/jobs)
RSS=$(ps -o rss= -C ffmpeg | awk '{s+=$1} END {print s/1024"MB"}')
CPU=$(ps -o pcpu= -C ffmpeg | awk '{s+=$1} END {print s"%"}')
TX=$(awk '/eth0|ens/ {print $10}' /proc/net/dev)
printf '%s ffmpeg=%s cpu=%s rss=%s tx=%s disk=%s\n  %s\n' \
  "$TS" "$(pgrep -x ffmpeg | wc -l)" "$CPU" "$RSS" "$TX" \
  "$(df -h / | awk 'NR==2{print $4}')" \
  "$(echo "$JOBS" | python3 -c 'import json,sys; [print(j["broadcast_id"], j["phase"], j["frames"], j["restarts"], j["last_verdict"]) for j in json.load(sys.stdin)["jobs"]]' | tr '\n' ' ')" \
  >> /var/log/soak.log
```

### 9.3 Scheduled events during the run

| hour | action | expected |
|---|---|---|
| 0 | both broadcasts started, verified at the destination (§6) | picture = live, sound = playlist |
| 0.5 | **close the browser entirely** | both continue — renewal is liveness for the UI, never authority over a job |
| 1 | **log out of 247streams** in a fresh browser | both continue; page says signed out; a new job is refused within 5 min |
| 2 | attempt a third broadcast | **429**, naming whether it was the per-user or machine ceiling |
| 3 | drop one source for 60 s (§7) | that one recovers; **the other is untouched** |
| 12 | `systemctl restart live-source` | both return via `recover()`, same owners; record the gap |
| 18 | upload a file while on air | succeeds; the running broadcast does not tear (a rename swaps the directory entry, not the inode the open fd holds) |
| 24 | **confirm both are STILL RUNNING** | a self-stop is a failure |

### 9.4 Finishing

```bash
# Stop from the UI, then:
pgrep -x ffmpeg | wc -l      # expect 0
curl -s -H "Authorization: Bearer $TOKEN" https://247streams.kr/api/live-source/jobs \
  | python3 -c 'import json,sys; [print(j["broadcast_id"], j["desired"], j["restarts"]) for j in json.load(sys.stdin)["jobs"]]'
#    expect desired "stopped" for both

# Totals for the report.
awk '{print}' /var/log/soak.log | tail -30
grep -ciE "gave_up|stalled" /var/log/soak.log
journalctl -u live-source --since "24 hours ago" | grep -ciE "error|warn"
```

Record: peak CPU and RSS, total egress (expect **~138 GB**), restart count,
every `last_verdict` seen, the hour-12 recovery gap, and anything in the
journal that was not expected.

### 9.5 What makes the soak a failure

- the destination shows the wrong picture, the wrong sound, or the source's
  own audio;
- a broadcast stops when the browser closes or the user logs out;
- **a broadcast stops by itself at any point, including hour 24**;
- RSS climbing across the 24 hours (a leak);
- a frozen picture that `frames` does not catch, or a `gave_up` with no reason;
- one broadcast's failure disturbing the other;
- **any** change to a production path, container, or FFmpeg count;
- a stream key, token or resolved manifest URL appearing in any log.

---

## 10. Security requirements, as a pre-flight checklist

Tick every row before a customer sees `/beta/`.

| | requirement | how it is met | verified |
|---|---|---|---|
| 1 | worker not on a public address | `--listen` private IP | runbook §2.7 |
| 2 | inbound default-deny | ufw, two rules | §2.9 — **needs the VPS** |
| 3 | worker port reachable only from Caddy | ufw from production IP | §2.9 — **needs the VPS** |
| 4 | direct port access without the gate refused | 403 | measured locally |
| 5 | admin API not on a public path | `/admin/*` outside Caddy's matchers | tested; §8 re-checks live |
| 6 | admin secret ≠ gate secret, enforced | per-secret domain labels | tested |
| 7 | admin API fail-closed | binary refuses to start without it | tested |
| 8 | three secrets, env-var only, 0640 root:louver | §2.6 | — |
| 9 | no secret in any log or banner | 8 secret tests + real-binary probe | measured |
| 10 | session cookie crosses on one route only | Caddy strips it elsewhere; worker refuses it | measured 15/15 |
| 11 | exact `Origin` on the handshake | missing/`null`/mismatch refused | tested |
| 12 | per-user destination isolation | keyed by user id; others read as unregistered | tested |
| 13 | per-user media isolation | `<root>/<user>/`, canonicalise-then-prefix | tested |
| 14 | upload is really media | `ffprobe`, not the extension | tested |
| 15 | per-user and machine concurrency ceilings | both, distinct messages | tested |
| 16 | strict CSP, no inline script | `app.js`/`app.css` split out | tested |
| 17 | no production DB, OAuth, socket or volume | source-scanned | tested |
| 18 | FFmpeg owned by handle, never by pid | source-scanned for `pkill`/`pgrep`/`/proc/` | tested |
| 19 | **suspension stops a running beta job automatically** | **NOT MET** — manual §8, or a production change that interrupts customers | REVOCATION.md §6 |

**Row 19 is the open blocker.** Rows 2 and 3 need the VPS. Everything else is
measured.

---

## 11. Cost estimate — UNVERIFIED

**No price here has been verified from a primary source.** Outbound HTTPS to
vendor pricing pages is refused by this development environment's proxy
(`CONNECT tunnel failed, response 403` for `hetzner.com` and
`digitalocean.com`), so every figure below is an **estimate** to be replaced
with console numbers before anything is bought.

Two independent reasons not to budget from them:

1. **Hetzner raised cloud prices twice in 2026** — a broad adjustment on
   1 April and a second round on 15 June for new orders. Third-party listings
   predate both, and the 8 GB tier may have been renamed (CPX31 → CPX32), so
   even the plan name may be wrong.
2. Third-party aggregators disagree with each other on both price **and**
   included traffic for the same plan — and traffic is the figure that decides
   here.

| provider | plan shape | traffic | est. /month | status |
|---|---|---|---|---|
| Hetzner Cloud | 4 dedicated vCPU / 8 GB (CPX3x) | EU listings cite 20 TB; US lower | unknown post-2026 increases | **ESTIMATE — plan name and price both uncertain** |
| DigitalOcean | 4 vCPU / 8 GB Droplet | ~5 TB cited (sources also say 2–4 TB) | ~USD 48 | **ESTIMATE** |
| Vultr | 4 vCPU / 8 GB | ~5 TB cited | ~USD 40–48 | **ESTIMATE** |
| overage, any | — | — | ~USD 0.01/GB → ~USD 42 on 4.2 TB over | **ESTIMATE** |

**Check in the provider's own console, in this order:**

1. **included traffic** — below 5 TB/month two continuous broadcasts overrun
   (§1). This disqualifies plans faster than price.
2. **dedicated vs shared vCPU** — a burstable 4 vCPU is not a 4 vCPU here.
3. current price for the current plan name, post-June-2026.
4. region, for latency and because allowances differ by region at one price.

A test does not need a year's commitment: an hourly instance for the soak, then
destroy, is the cheapest way to answer the open questions.

---

## 12. What this runbook does not cover

- **Buying anything.** No VPS has been purchased or created.
- **Touching production.** The Caddy step (§3) is for a human on the production
  host, and this document cannot perform it.
- **Automatic suspension revocation** — REVOCATION.md §6; needs a production
  change whose deploy recreates the `louver` container.
- **A user-set stop time** — REVOCATION.md §9 option B, additive, not needed
  for this test, and it must default to "no end".
- **Scaling past two broadcasts.** The arithmetic extends linearly (1.5 cores,
  400 MB each) but nothing above four has been measured.
