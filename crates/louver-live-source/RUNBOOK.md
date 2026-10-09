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

In the **standalone configuration (§3.1), which is the one this test uses, there
is no link at all**: `/api/me` is answered on the test VPS itself, so not one
packet goes to production and not one production credential exists on the box.

The production-Caddy configuration (§3.2), for later, has exactly one link: the
worker asks production `GET /api/me` who a session cookie belongs to, once per
token. Nothing else, in either configuration.

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
# The asset name is architecture-specific. `yt-dlp_linux` is x86_64 ONLY.
case "$(uname -m)" in
  x86_64)  YTDLP_ASSET=yt-dlp_linux ;;
  aarch64) YTDLP_ASSET=yt-dlp_linux_aarch64 ;;
  armv7l)  YTDLP_ASSET=yt-dlp_linux_armv7l ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac
curl -fL -o /usr/local/bin/yt-dlp \
  "https://github.com/yt-dlp/yt-dlp/releases/download/2026.08.19/$YTDLP_ASSET"
chmod 755 /usr/local/bin/yt-dlp
yt-dlp --version    # expect 2026.08.19
```

Both assets exist at this tag — checked by HTTP HEAD: `yt-dlp_linux` → 200 and
`yt-dlp_linux_aarch64` → 200. Note `-f`, so a 404 fails the command instead of
writing an HTML error page to `/usr/local/bin/yt-dlp` and chmod-ing it 755.

**The architecture decides more than this one file.** `live-source-api` is a
compiled binary: one built on an x86_64 host does not run on an ARM VPS. Several
of the cheap plans with the large traffic allowances §1 requires are ARM
(Ampere, Graviton, Hetzner CAX). Decide the architecture **before** building, and
if it is ARM, the FFmpeg measurements in §1 do not transfer — they were taken on
x86_64 and the per-core encode cost will differ.

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
install -d -o louver -g louver -m 750 /var/lib/live-source/cache
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

# A FOURTH mandatory value. The worker EXITS at startup without it. It is not a
# secret-length check: it is `die("LOUVER_LIVE_SOURCE_DESTINATIONS 를 설정해 주세요")`.
# The placeholder below is replaced in §4.2 once the tester's user id is known.
printf 'LOUVER_LIVE_SOURCE_DESTINATIONS=%s\n' \
  '{"PLACEHOLDER-USER-ID":{"beta-test":"rtmp://127.0.0.1:1935/live/test"}}' \
  >> /etc/louver-live-source.env

chown root:louver /etc/louver-live-source.env
chmod 640 /etc/louver-live-source.env
```

**Ordering, found by reading the binary rather than by running it:**
`LOUVER_LIVE_SOURCE_DESTINATIONS` is **required**, and it is keyed by user id —
so the worker cannot start until a user id exists to key it by. The id comes from
whatever `/api/me` the worker is pointed at (§3). Without the placeholder above,
`systemctl enable --now live-source` in §2.7 exits immediately and the banner
never appears. With the placeholder it starts, and the only thing that does not
work is starting a broadcast, until §4.2 substitutes the real id and restarts the
service.

They answer three different questions — which user is this (signing), did this
come through Caddy (gate), is this the operator (admin) — and each is hashed
under its own domain-separation label, so **reusing one value across two
variables does not make either accept the other's header.** Use three anyway.

The binary refuses to start if any of the three is missing or under 32 bytes, and
refuses to start without a destination map. There is no flag that disables the
gate or the admin API, and no flag that supplies any of these on argv — a process
listing is world-readable and the map contains stream keys.

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
# yt-dlp writes a cache under $HOME, and ProtectSystem=strict makes
# /srv/live-source read-only. Point it at the one directory that is writable.
Environment=XDG_CACHE_HOME=/var/lib/live-source/cache
ExecStart=/usr/local/bin/live-source-api \
  --listen 127.0.0.1:9080 \
  --origin https://<TEST_HOST> \
  --allow-origin https://<TEST_HOST> \
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

**`--listen 127.0.0.1:9080` — loopback, never `0.0.0.0`.** In the standalone
configuration (§3.1) Caddy runs on the same box, so loopback is the whole of the
worker's exposure and the firewall never has to be the layer that saves it.
`<TEST_HOST>` is the test VPS's own hostname — never `247streams.kr`; §3.1 says
why that substitution is not optional.

For the later production-Caddy configuration (§3.2) this becomes the VPS's
**private** address, e.g. `--listen 10.0.0.2:9080`, because Caddy is then on a
different host. Do not use a private address on a provider where the box has no
private network: `10.0.0.2` pasted literally fails to bind with
`EADDRNOTAVAIL` and `Restart=always` turns that into a restart loop every 5 s.
Check with `ip -brief addr` first, and bind an address that command actually
prints.

Expect in the log:

```
[louver][live-source-api] cores=4 max_concurrent=2 max_per_user=1 예상 비용=3.0 core / 800 MB
[louver][live-source-api] gate=on admin=on allow-origin=https://<TEST_HOST> 송출 대상 사용자 N명
[louver][live-source-api] 송출 정지된 계정 0건
[louver][live-source-api] 복원된 작업 0건
[louver][live-source-api] listening on http://127.0.0.1:9080  (beta UI: /beta/)
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

**Read the SSH warning below before running any of this.**

```bash
# 1. Know your own address FIRST, from the machine you will SSH from.
#    Run this on your laptop, not on the VPS:
#      curl -s https://api.ipify.org; echo
OPERATOR_IP=<the address that command printed>

# 2. Rules BEFORE enabling. ufw applies them in order and `enable` is the
#    commit, so an SSH rule added after `enable` is added too late.
ufw default deny incoming
ufw default allow outgoing              # YouTube ingest out, the live source in
ufw allow from "$OPERATOR_IP"/32 to any port 22 proto tcp   # SSH, key only
ufw allow 443/tcp                       # Caddy, standalone configuration (§3.1)

# 3. The worker's port gets NO rule at all in the standalone configuration:
#    it is on loopback, so nothing off-box can reach it regardless of ufw.

ufw --force enable
ufw status numbered                     # read this before you log out
```

Then harden SSH: `PasswordAuthentication no`, `PermitRootLogin no`.

**Keeping SSH while the firewall goes up.** `ufw --force enable` does not drop
the connection you are typing on — ufw admits `ESTABLISHED,RELATED` — but the
*next* connection is governed by the rules above, so a mistake is only visible
once you have logged out. Three precautions, in order of how much grief they
save:

1. **Do not log out until a second session proves it works.** Open a new
   terminal and SSH in again *while the first one is still connected*. If it
   hangs, fix it from the session you still have.
2. **A dynamic address will lock you out later.** Most domestic ISPs rotate.
   If `$OPERATOR_IP` is not static, either use the provider's web console/VNC as
   the recovery path (confirm it works *before* enabling ufw), or widen the rule
   to `ufw limit 22/tcp` and rely on key-only auth instead of the address.
3. **IPv6.** `/etc/default/ufw` ships `IPV6=yes`, so the v6 default is deny too,
   and `allow from <v4>/32` grants nothing over v6. If you SSH to the box's AAAA
   record you are locked out by a rule that looks correct. Either connect over
   v4 explicitly (`ssh -4`) or add the v6 counterpart:
   `ufw allow from <OPERATOR_V6>/128 to any port 22 proto tcp`.

Verify from off-box, after enabling, that the worker's port is not reachable:

```bash
# From the operator's machine. Both must fail in the standalone configuration.
curl -m 5 -sS -o /dev/null -w '%{http_code}\n' http://<TEST_HOST>:9080/health
nc -z -w 5 <TEST_HOST> 9080 && echo "REACHABLE — STOP" || echo "closed, as intended"
```

Three layers in front of the worker's port. Only the third is the same in both
configurations of §3:

| | standalone (§3.1) | production Caddy (§3.2) |
|---|---|---|
| 1 | bound to loopback, so there is no off-box path to the port at all | bound to the private address only |
| 2 | ufw admits 22 and 443 from the operator's address only | ufw admits production's IP to 9080 |
| 3 | the gate secret — measured: a request reaching the port directly without the header answers **403** | identical |

**`/admin/*` is deliberately not reachable through any of this**, in either
configuration. The Caddy matchers cover only `/api/live-source/*` and `/beta/*`,
so an `/admin/…` request from the internet is answered by Caddy's own catch-all
and never proxied. The operator routes are reachable only from the VPS itself:

```bash
# On the VPS, over SSH. This address must match --listen exactly: in the
# standalone configuration that is 127.0.0.1, and in the production-Caddy
# configuration it is the private address the worker bound.
curl -s -H "X-Louver-Admin: $ADMIN" http://127.0.0.1:9080/admin/revoked
```

**In the standalone configuration `/admin/*` is kept off the internet by two
independent things, and only one of them is the admin secret:** the worker is on
loopback, and §3.1's Caddy routes only `/beta/*` and `/api/live-source/*`, so an
`/admin/…` request from outside is answered by Caddy — not proxied. Verify both,
per §8's authorization-boundary check. If you ever bind the worker to a public
address "just to test from your laptop", the admin secret becomes the *only*
thing between the internet and taking a broadcast off air.

---

## 3. Serving the beta

Two configurations. **§3.1 is the one to use for this test**: it runs entirely on
the test VPS and does not involve production's Caddy at all. §3.2 is the later
path, for the day the beta is actually offered to a customer, and it is a human's
action on the production host.

### 3.1 Standalone — the test VPS's own Caddy (use this one)

Nothing here touches production: not its Caddyfile, not its container, not its
network. The cost is one real limitation, stated at the end of this section.

**Why a reverse proxy is still needed even standalone.** `/beta/*` is served by
the worker from inside the gated router (`api.rs:658` nests it *before* the gate
layer is applied), so every request to it must carry `X-Louver-Gate`. A browser
cannot add that header. Something in front has to inject it — in production that
is production's Caddy, and here it is the test VPS's own.

**Why the production session cookie cannot be reused.** Verified in the source
rather than assumed:

- `/session` is the only route that reads a cookie, and it requires the gate
  header, an **exact** `Origin` match, and a `Cookie`, then calls
  `{--origin}/api/me` with it (`api.rs:202`, `auth.rs:132`).
- `app.js` sends the handshake with `credentials: "same-origin"`, so the browser
  attaches cookies belonging to **the page's own origin** — the test host's.
- production issues its cookie with `Path=/; HttpOnly; SameSite=Strict` and
  **no `Domain=`** (`apps/server/src/auth.rs:149`), making it host-only to
  `247streams.kr`. A browser will not send it to any other host, not even a
  `247streams.kr` subdomain.

That last fact is good news twice: the test VPS **cannot** receive a production
credential by accident, and a test subdomain is therefore not a leak risk. It
also means the standalone beta needs an identity of its own.

**The configuration.** `--origin` must be an `https://` host-only origin
(`ProductionMe::new` refuses `http://`, a path, a port-less bare host, or
credentials) and its TLS certificate must be one the system trusts, because
`ureq` verifies against the system roots. So: a real DNS name for the test VPS
and a real certificate.

```caddy
# /etc/caddy/Caddyfile on the TEST VPS. This is not production's file.
<TEST_HOST> {
	encode zstd gzip

	# TEST-ONLY IDENTITY. It vouches for a fixed id and checks nothing, so
	# whoever can reach this host IS the test user. Read the warning below
	# before opening port 443 to anything.
	handle /api/me {
		header Content-Type application/json
		respond `{"id":"beta-tester","plan_id":"basic"}` 200
	}

	handle /api/live-source/* {
		reverse_proxy 127.0.0.1:9080 {
			header_up X-Louver-Gate {env.LOUVER_GATE_SECRET}
		}
	}

	handle /beta/* {
		# The handshake sends credentials: "same-origin", so the browser needs
		# a cookie for THIS host. Any value will do — /api/me ignores it.
		header +Set-Cookie "louver_session=beta-tester; Path=/; Secure; HttpOnly; SameSite=Strict"
		reverse_proxy 127.0.0.1:9080 {
			header_up X-Louver-Gate {env.LOUVER_GATE_SECRET}
		}
	}

	# /admin/* is deliberately absent, so an /admin request from outside gets
	# this 404 from Caddy and never reaches the worker.
	handle {
		respond 404
	}
}
```

Never write `header_up -X-Louver-Gate` before the `header_up X-Louver-Gate`
line. Both land in one `HeaderOps` and Caddy applies `delete` after `set`, so
the header arrives **absent** and every request is 403. `caddy validate` passes
on the broken version — this was found by running it, not by reading it.

```bash
export LOUVER_GATE_SECRET=<same value as LOUVER_LIVE_SOURCE_GATE_SECRET>
caddy validate --config /etc/caddy/Caddyfile
systemctl restart caddy

# Point the host at itself, so the worker's own /api/me call goes over loopback
# instead of out and back through the provider's NAT.
echo "127.0.0.1 <TEST_HOST>" >> /etc/hosts

# Then set --origin and --allow-origin to https://<TEST_HOST> in §2.7 and:
systemctl restart live-source
```

Substitute `https://<TEST_HOST>` for `https://247streams.kr` everywhere in
§§4–9 — in the media upload (§4.3), the handshake (§6.1) and the soak sampler
(§9.2).

**Two warnings, both load-bearing.**

1. **Port 443 must not stay open to the internet.** With the stub identity,
   anyone who reaches this host is `beta-tester` and can start a broadcast to
   the tester's own stream key on the tester's own CPU. Let's Encrypt needs
   inbound 443 for the TLS-ALPN-01 challenge, so: open it, let Caddy issue the
   certificate, then narrow it.

   ```bash
   ufw allow 443/tcp                                  # for issuance only
   journalctl -u caddy -n 20 --no-pager | grep -i "certificate obtained"
   ufw delete allow 443/tcp
   ufw allow from "$OPERATOR_IP"/32 to any port 443 proto tcp
   ```

   A test box lives for days and Caddy renews about 30 days before expiry, so no
   renewal falls inside the test window. If one would, re-open 443 for it
   deliberately.

2. **Use a hostname that is not a production hostname.** A `247streams.kr`
   subdomain is safe as far as the cookie goes (host-only, proven above), but it
   puts a box with a stub identity under the service's own name. Prefer a
   separate domain the tester owns.

**What §3.1 does not test.** Composition, recovery, the admin API, the
concurrency ceilings, the media rules and the 24-hour soak are all exercised
exactly as in production — the worker's code path is identical. What is **not**
exercised is the real handshake: a production cookie, production's `/api/me`
answering for a real user id, and production's Caddy injecting the gate header
and stripping the cookie elsewhere. Those three stay unverified until §3.2 is
done, and the test report must say so rather than implying the beta is proven
end to end.

### 3.2 Production Caddy — a human's action, later, not for this test

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
# For the composition check (§6) ONLY: a sink that accepts one connection and
# records a short clip. `-t 60` and mpegts are both deliberate — see below.
ffmpeg -hide_banner -listen 1 -i "rtmp://127.0.0.1:1935/live/test" \
       -c copy -t 60 -f mpegts -y /tmp/received.ts
```

**Three limits of that one-liner, each of which breaks a later test if ignored:**

1. **`-listen 1` accepts exactly one connection and then exits.** Test 3 (§7)
   makes the worker reconnect after a stall; it will find nothing listening and
   fail for the wrong reason. Test 5 (§9) needs two broadcasts, and one listener
   cannot take two. For those, run a sink that stays up — a loop is enough:
   `while true; do ffmpeg -hide_banner -listen 1 -i "rtmp://127.0.0.1:1935/live/test" -f null - ; sleep 1; done`
   — or give each broadcast its own port (`:1935` and `:1936`) with its own
   listener, which is what two destinations in the map require anyway.
2. **Never record the soak.** Two broadcasts at ~6.4 Mbit/s each write about
   **138 GB in 24 hours**. That fills the 80 GB disk of §1 in roughly 7 hours and
   the first thing to fail will be the worker's own state write, not the
   recording. For the soak the sink must discard: `-f null -`.
3. **`-f mp4` to a file you will interrupt gives you an unplayable file**, because
   the moov atom is written at the end. `mpegts` is readable even if truncated,
   which is what a verification clip needs.

### 4.2a Test destination vs. real YouTube — keep them apart on purpose

The two options above are not interchangeable, and the difference matters more
than it looks:

| | **(a) local sink** | **(b) tester's own second channel** |
|---|---|---|
| URL shape | `rtmp://127.0.0.1:1935/live/test` | `rtmp://a.rtmp.youtube.com/live2/<KEY>` |
| leaves the box | no | yes — real egress, real ingest |
| proves | composition, recovery, admin stop, concurrency | that the real ingest path works |
| traffic cost | none | ~138 GB per 24 h for two (§1) |
| risk if confused | none | a stream key in a file, and real public video |

Rules that keep them distinguishable:

- **Use (a) for every test up to and including §8.** Composition, recovery and
  the forced-stop tests need nothing from YouTube's ingest, and a local sink
  cannot accidentally publish anything.
- **Use (b) only for the soak**, and only for the tester's own second channel.
  Never a customer's key, never production's, never the key from §4.1 that the
  *source* is publishing to — pointing the output at the input's own ingest makes
  a loop whose symptoms look like a product bug.
- **Name them for what they are.** `beta-test-local` and `beta-test-youtube`,
  not `beta-test`. The destination name is what the tester picks in the UI, and
  a wrong pick at hour 0 of a 24-hour run is discovered at hour 24.
- **Check before starting, not after.** `/session` returns the destination names
  for the signed-in user; the names tell you which map is loaded. To confirm the
  URL behind a name without printing it, start a broadcast and look at where the
  bytes went: `pgrep -x ffmpeg | wc -l` plus the receiving end. Never echo
  `LOUVER_LIVE_SOURCE_DESTINATIONS`, and never paste it into a terminal that
  scrolls back into a report.
- Set the YouTube key only when you reach §9, and remove it from
  `/etc/louver-live-source.env` when the soak ends.

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
curl -s -H "X-Louver-Admin: $ADMIN" http://127.0.0.1:9080/admin/revoked
#    {"users":[]}

# 2. Revoke the tester.
curl -s -X POST -H "X-Louver-Admin: $ADMIN" -H 'Content-Type: application/json' \
  -d '{"user_id":"<TESTER_USER_ID>"}' http://127.0.0.1:9080/admin/revoke
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
curl -s -H "X-Louver-Admin: $ADMIN" http://127.0.0.1:9080/admin/revoked
#    {"users":["<TESTER_USER_ID>"]}

# 6. Idempotency: revoking again is safe.
curl -s -X POST -H "X-Louver-Admin: $ADMIN" -H 'Content-Type: application/json' \
  -d '{"user_id":"<TESTER_USER_ID>"}' http://127.0.0.1:9080/admin/revoke
#    {"changed":false,"stopped":0,"jobs":[]}

# 7. The audit trail.
cat /var/lib/live-source/admin-audit.jsonl
#    one line per call: at, action, user_id, stopped, jobs[], changed

# 8. Lift it, and confirm broadcasting works again.
curl -s -X POST -H "X-Louver-Admin: $ADMIN" -H 'Content-Type: application/json' \
  -d '{"user_id":"<TESTER_USER_ID>"}' http://127.0.0.1:9080/admin/restore
```

**Also verify the authorization boundary** — each of these must answer 403:

```bash
for cred in "" "$GATE" "$SIGN" "$TOKEN" "wrong"; do
  printf '%-10s -> ' "${cred:0:8}"
  curl -s -o /dev/null -w '%{http_code}\n' \
    ${cred:+-H "X-Louver-Admin: $cred"} http://127.0.0.1:9080/admin/revoked
done
```

The gate secret in the admin header **must** fail, even though both are
32-byte secrets this machine holds — each is hashed under its own
domain-separation label. And confirm the admin route is **not** reachable
through Caddy:

```bash
# From anywhere on the internet. Must NOT be the worker's answer: expect this
# host's own 404, never 200 and never the worker's 403.
curl -s -o /dev/null -w '%{http_code}\n' https://<TEST_HOST>/admin/revoked
# And on the production-Caddy path, when that configuration is eventually used:
#   curl -s -o /dev/null -w '%{http_code}\n' https://247streams.kr/admin/revoked

# A 403 here would be its own finding: it would mean the request REACHED the
# worker and was refused by the admin secret, i.e. only one layer was left.
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

**On production: nothing.** In the standalone configuration there is no link to
production at all (§0), so there is no "before" reading to take and no reason to
log in. The isolation is established by the configuration, not by observing
production — and logging in to check would itself be the access this test is
supposed to avoid.

If a reading is ever wanted for a report, it is a human's own decision on a
session they already have, read-only and count-only
(`pgrep -x ffmpeg | wc -l`, never `pgrep -af`, which prints argv and therefore
stream keys). It is not a step of this runbook.

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

Rows 1–3 and 20 read differently in the two configurations of §3; the
**standalone** reading is given, with the production one after the slash.

| | requirement | how it is met | verified |
|---|---|---|---|
| 1 | worker not on a public address | `--listen 127.0.0.1` / private IP | runbook §2.7 |
| 2 | inbound default-deny | ufw; 22 and 443 to the operator's address only | §2.9 — **needs the VPS** |
| 3 | worker port unreachable from off-box | loopback bind, so no rule needed / ufw from production IP | §2.9 — **needs the VPS** |
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

| 20 | the beta identifies a real user | **standalone: NOT MET BY DESIGN** — §3.1's `/api/me` is a stub that vouches for a fixed id and checks nothing, so the firewall is the authentication. Production (§3.2): a real cookie production vouches for | §3.1 |

**Row 19 is the open blocker** for offering the beta to a customer. **Row 20 is
the price of standalone testing** and is acceptable only because the box holds
no customer data, carries only the tester's own stream key, and admits only the
operator's address — it must never be how a customer reaches `/beta/`. Rows 2
and 3 need the VPS. Everything else is measured.

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

### 11.1 Traffic overrun — the risk that is not in the table

The ~138 GB per 24 h of §1 is the **steady-state** figure for two healthy
broadcasts to a remote destination. Three things multiply it, and all three are
states this test deliberately provokes:

| cause | effect on traffic | guard |
|---|---|---|
| the soak runs past 24 h (the pass condition is that it is *still running*) | +~5.8 GB per hour per broadcast | decide the stop time in advance; it is the operator who ends it, by design |
| watchdog restart loop on a bad source | each restart re-resolves and re-pulls the source; a 4K rendition is the worst case | `restarts` is sampled hourly in §9.2 — a climbing count is a stop condition, not a curiosity |
| local sink mistaken for a remote one, or the reverse | 0 vs. full rate | §4.2a |

Ingress (pulling the live source) is metered by some providers and not others.
Assume it counts until the provider's own console says otherwise: that doubles
the figure to **~276 GB per 24 h**, still inside 5 TB but not inside 1 TB.

Set a hard stop that does not depend on anyone watching:

```bash
# Provider-side billing alert first — it is the only one that works when the
# box is wedged. Then a local tripwire, sampled with the §9.2 cron:
TX_GB=$(awk '/eth0|ens/ {print $10/1024/1024/1024}' /proc/net/dev)
# If TX_GB exceeds the budget, stop the broadcasts (admin §8) rather than the box:
# a destroyed VPS loses the evidence the soak exists to collect.
```

---

## 12. What this runbook does not cover

- **Buying anything.** No VPS has been purchased or created.
- **Touching production.** The standalone configuration (§3.1) never contacts
  it. The production-Caddy step (§3.2) is for a human on the production host,
  later, and this document cannot perform it.
- **Proving the real handshake.** §3.1 substitutes a stub `/api/me`, so a real
  cookie, a real user id and production's own gate injection stay untested until
  §3.2 is done — §3.1 says this in full, and the test report must repeat it.
- **Automatic suspension revocation** — REVOCATION.md §6; needs a production
  change whose deploy recreates the `louver` container.
- **A user-set stop time** — REVOCATION.md §9 option B, additive, not needed
  for this test, and it must default to "no end".
- **Scaling past two broadcasts.** The arithmetic extends linearly (1.5 cores,
  400 MB each) but nothing above four has been measured.

---

## 13. Static review of this runbook — what was wrong, and what is still unproven

Every command above was reviewed against the crate's own source on
`claude/live-source-hardening`. **Nothing was executed on a VPS, because no VPS
exists.** The review found six defects that would have stopped a tester, and
they are fixed in the text above. They are listed here because a runbook that
silently changed is a runbook nobody can trust.

| | defect | consequence had it not been found | evidence |
|---|---|---|---|
| 1 | `LOUVER_LIVE_SOURCE_DESTINATIONS` was not written until §4.2, but the binary **requires** it | §2.7's `systemctl enable --now` exits immediately; the expected banner never appears and the tester debugs systemd instead of reading one error | `bin/live-source-api.rs:116` — `die("…DESTINATIONS 를 설정해 주세요")` |
| 2 | `yt-dlp_linux` is x86_64-only, with no architecture check | silent on x86_64; on an ARM box the binary will not execute, and ARM is common among the high-traffic plans §1 needs | separate `yt-dlp_linux_aarch64` asset exists at the same tag (both HEAD → 200) |
| 3 | `--listen 10.0.0.2:9080` as the primary example | pasted literally on a provider with no private network it fails to bind `EADDRNOTAVAIL`, and `Restart=always` makes that a 5-second loop | `ip -brief addr` is now the check; standalone binds loopback |
| 4 | the admin example called `10.0.0.2` a "loopback address" | the operator curls an address the worker is not bound to and concludes the admin API is broken | plain contradiction in the text |
| 5 | `ProtectSystem=strict` leaves `$HOME` (`/srv/live-source`) read-only, where yt-dlp wants its cache | a warning per resolve, suppressed by `--no-warnings`, so it degrades invisibly rather than failing loudly | `resolver.rs:192` passes no `--no-cache-dir`; `XDG_CACHE_HOME` now points into a `ReadWritePaths` directory |
| 6 | the RTMP sink was `-listen 1` into an mp4 file, used for every test | one connection only (so §7's reconnect finds nothing), one stream only (so §9's two broadcasts cannot both land), ~138 GB into an 80 GB disk, and an unplayable file if interrupted | §4.2 |

**Checked and found correct, no change needed:**

- the `ufw` ordering — every `allow` precedes `--force enable`, which is the
  order that does not lock you out;
- the yt-dlp version pin `2026.08.19` matches the version the resolver was
  developed against, and the release URL resolves (HTTP 200);
- `EnvironmentFile` at `0640 root:louver` is readable by `User=louver`;
- `ReadWritePaths` covers both directories the worker writes;
- Ubuntu 24.04's FFmpeg is 6.1.1, the version every measurement used;
- no secret is ever passed on argv, so `ps` cannot leak one;
- the production session cookie is host-only, so it cannot reach a test host
  even by subdomain.

**Still unproven, and only a real VPS can prove it:**

| | what | why it cannot be checked here |
|---|---|---|
| 1 | that yt-dlp resolves a real YouTube Live URL | this environment has no YouTube egress (`youtube.com` → `000`) |
| 2 | that the composition is right at a real receiving end | needs §5 to succeed first |
| 3 | FFmpeg's sustained cost over 24 h on the chosen instance | measured here for minutes, on different hardware |
| 4 | that Caddy issues a certificate for `<TEST_HOST>` | needs DNS and inbound 443 |
| 5 | the provider's real price and included traffic | vendor pricing pages are 403 through this proxy (§11) |
| 6 | that ufw keeps SSH alive on the provider's network | §2.9's precautions are the mitigation, not a proof |

---

## 14. Teardown — stopping the work and stopping the bill

Run this in order. **Powering a VPS off does not stop billing at most
providers**; only destroying the instance does, and a detached volume, snapshot
or reserved IP keeps charging after the instance is gone.

```bash
# 1. End the broadcasts deliberately, so the logs show an operator stop rather
#    than a machine that vanished mid-stream.
curl -s -X POST -H "X-Louver-Admin: $ADMIN" \
  -H 'Content-Type: application/json' \
  -d '{"user_id":"beta-tester"}' http://127.0.0.1:9080/admin/revoke
pgrep -x ffmpeg | wc -l        # expect 0

# 2. Collect what the test was for, BEFORE the box goes away.
systemctl stop live-source
journalctl -u live-source --since "25 hours ago" --no-pager > /tmp/live-source.log
tar czf /tmp/soak-evidence.tgz /tmp/live-source.log /var/log/soak.log
#    Copy it off the box now: scp, not later.
```

Then, in this order:

1. **Revoke the test stream key** on the tester's own YouTube channel (Live
   Control Room → reset the key). It is the one credential that outlives the
   VPS. Also end the §4.1 source stream.
2. **Remove the destination map** from `/etc/louver-live-source.env` if the box
   survives for any reason.
3. **Destroy the instance** in the provider's console — destroy, not stop.
4. **Then hunt the leftovers**, each of which bills on its own: detached
   volumes, snapshots and backups, a reserved/floating IP, a private network, a
   load balancer, and the provider's own log retention.
5. **Delete the DNS record** for `<TEST_HOST>`.
6. **Confirm** on the billing page that the hourly charge has stopped, and check
   again the next day — the final invoice is where a forgotten floating IP shows
   up.
7. The three secrets die with the box. If any value was reused anywhere else,
   rotate it there; the whole point of §2.6 is that none of them was.

Nothing in this procedure touches production, and none of it requires the
production host to be reachable.
