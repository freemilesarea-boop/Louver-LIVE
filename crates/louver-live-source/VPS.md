# Beta test VPS — specification and 24-hour live test plan

Sizing target: **two concurrent live-source broadcasts.** Nothing here has been
bought or created.

Every resource number below comes from a measurement taken on the development
box, not from a guess. Every price is **unverified** and marked as such — see
§8 for why.

## 1. Where the numbers come from

Measured with the product's own argv, against a real RTMP sink:

| path | CPU | RSS |
|---|---|---|
| an existing playlist broadcast (stream copy) | 2–8% of one core | 56 MB |
| **this worker, 1080p source** | **95–148%** | **384–388 MB** |
| this worker, 4K source capped to 1080p | 175% | 492 MB |
| worker supervision (the watchdog, no encode) | 0.0% | 3.9 MB |

A live video source cannot be stream-copied — the picture and the playlist's
sound are unrelated streams, so `runtime.rs:671` forces
`StreamMode::CompatibilityEncode`. Every one of these broadcasts is a real x264
encode costing about a core. That single fact is what makes a separate machine
a requirement rather than a preference, and it is what sizes everything below.

The code already budgets with these numbers (`limits.rs`):
`RESERVED_CORES = 1`, `CORES_PER_WORKER = 1.5`, `MB_PER_WORKER = 400`.

## 2. CPU

`Limits::for_machine(cores, max)` computes `(cores - 1) / 1.5`, so:

| vCPU | `max_concurrent` the code will allow |
|---|---|
| 2 | 1 |
| **4** | **2** ← the target |
| 6 | 3 |
| 8 | 4 |

**4 dedicated vCPU minimum.** That yields exactly the two concurrent
broadcasts asked for: 2 × 1.5 = 3 cores budgeted, 1 reserved for the OS, Caddy
and the supervisor.

Two cautions, both from the measurements rather than theory:

- **Shared/burstable vCPU is not adequate.** At 95–148% sustained per encode
  for 24 hours, a burstable instance exhausts its credit and then throttles —
  which presents as the frozen picture the watchdog is built to catch, on every
  broadcast at once. Pay for dedicated cores.
- **4K sources need more.** Two 4K→1080p encodes measured 3.5 cores, plus 1
  reserved = 4.5, which does not fit in 4. If the beta will accept 4K sources,
  **6 vCPU**. For 1080p sources 4 is correct.

## 3. RAM

| item | measured / known |
|---|---|
| 2 × 1080p encode | ~0.78 GB |
| 2 × 4K→1080p encode (worst case) | ~0.98 GB |
| worker supervision | 4 MB |
| Caddy | ~50 MB (if the worker terminates TLS itself — see §6) |
| Ubuntu 24.04 + systemd | ~0.5–0.7 GB |

**4 GB minimum, 8 GB recommended.** 4 GB covers the measured working set with
headroom; 8 GB is what makes `ffprobe` on a 512 MB upload, a 24-hour journal
and two encodes coexist without the kernel having to choose. The code's own
budget line prints `800 MB` for `max_concurrent = 2`, which is the encodes
alone.

## 4. Network transfer — the number most likely to be underestimated

Measured egress is **~6.4 Mbps per broadcast**.

| window | egress | ingress (pulling the source) | both |
|---|---|---|---|
| 1 broadcast, 1 hour | 2.9 GB | ~2.9 GB | ~5.8 GB |
| **2 broadcasts, 24 hours** (the test) | **~138 GB** | **~138 GB** | **~276 GB** |
| 2 broadcasts, 30 days continuous | ~4.2 TB | ~4.2 TB | ~8.3 TB |

Arithmetic, so it can be checked: 12.8 Mbit/s × 86 400 s = 1 105 920 Mbit
= 138 240 MB ≈ 138 GB.

Consequences for choosing a provider:

- A **1 TB/month** allowance is exhausted by roughly **7 days** of two
  continuous broadcasts. Several US VPS tiers include exactly that.
- Ingress is usually not metered, but it is not free of capacity — the NIC
  carries ~26 Mbps steady in both directions for two broadcasts.
- **Require ≥ 5 TB/month included**, or accept an overage bill. Overage is
  commonly ~USD 0.01/GB, which on 4.2 TB past an allowance is ~USD 42/month
  *(unverified — see §8)*.
- A 1 Gbps port is ample; the constraint is the monthly allowance, not the
  line rate.

## 5. Storage

| item | size |
|---|---|
| Ubuntu 24.04 + packages | ~5 GB |
| ffmpeg, ffprobe, yt-dlp, the worker binary | ~0.5 GB |
| media, at `MAX_USER_BYTES` = 4 GiB per user | 4 GiB × beta users |
| job state files (`state_dir`) | kilobytes |
| journal, capped (§10) | 2 GB |

**40 GB SSD minimum, 80 GB recommended.** 40 GB holds the OS plus three beta
users' media quota; 80 GB is what stops a quota being reached from also
stopping the journal, and leaves room to grow the beta without a resize.

NVMe is not required: the encode reads a local playlist and writes to a socket,
so this is not a disk-bound workload. Uploads are the only burst, at 512 MB.

## 6. Operating system

**Ubuntu 24.04 LTS**, and for a specific reason rather than familiarity: its
distribution FFmpeg is **6.1.1**, which is the exact version every measurement
and the 1080p cap filter were verified against on the development box
(`ffmpeg version 6.1.1-3ubuntu5`, Ubuntu 24.04). Debian 12 ships FFmpeg 5.1,
where the `force_divisible_by` behaviour of the cap filter has not been
checked here. Matching the tested version removes a variable from the first
real test.

## 7. FFmpeg and yt-dlp

**FFmpeg** — the distribution package:

```
apt-get install -y ffmpeg
ffmpeg -version      # expect 6.1.1
```

**yt-dlp** — deliberately *not* the distribution package, which is always too
old: YouTube changes break extraction, and a stale yt-dlp fails to resolve.
Install the official standalone binary at a **pinned** version:

```
curl -L -o /usr/local/bin/yt-dlp \
  https://github.com/yt-dlp/yt-dlp/releases/download/2026.08.19/yt-dlp_linux
chmod +x /usr/local/bin/yt-dlp
yt-dlp --version     # expect 2026.08.19
```

`2026.08.19` is the version the resolver was developed and tested against here.
Pin it, and update it as a deliberate step with a re-run of the resolver tests
— never with an unattended `yt-dlp -U`, because an update that changes the JSON
shape breaks resolution for every broadcast at once.

The worker never runs yt-dlp as root and never passes it a cookie file,
`--exec`, or an output template: see the argv assertions in `resolver.rs`.

## 8. Provider and price — NOT VERIFIED

**I could not verify a single price from a primary source.** Outbound HTTPS to
vendor pricing pages is refused by this environment's proxy
(`CONNECT tunnel failed, response 403` for `hetzner.com`; the same for
`digitalocean.com`). Everything in this section is therefore an **estimate**,
and two independent reasons say not to budget from it:

1. **Hetzner raised cloud prices twice in 2026** — a broad adjustment on
   1 April and a second round on 15 June that applies to new orders. Third-party
   listings and any remembered figure predate these. One report has a US CPX31
   moving from 20.99 to 62.49, and another suggests the 8 GB tier has been
   renamed (CPX31 → CPX32), so even the plan name may be wrong.
2. **Third-party aggregators disagree with each other** on both price and the
   included-traffic figure for the same plan — and traffic is the number that
   matters most here (§4).

Indicative only, to be replaced with console figures before any purchase:

| provider | plan shape | traffic | price/month | status |
|---|---|---|---|---|
| Hetzner Cloud | 4 dedicated vCPU / 8 GB (CPX3x) | EU listings cite 20 TB; US lower | unknown after the 2026 increases | **ESTIMATE — plan name and price both uncertain** |
| DigitalOcean | 4 vCPU / 8 GB Droplet | ~5 TB cited, but sources also say 2–4 TB | ~USD 48 | **ESTIMATE** |
| Vultr | 4 vCPU / 8 GB | ~5 TB cited | ~USD 40–48 | **ESTIMATE** |

**What to check in each provider's own console before buying**, in this order:

1. **Included traffic.** Below 5 TB/month, two continuous broadcasts will
   overrun (§4). This disqualifies plans faster than price does.
2. **Dedicated vs shared vCPU** (§2) — a burstable 4 vCPU is not a 4 vCPU here.
3. Current price for the current plan name, post-June-2026.
4. Region: choose for latency to YouTube's ingest and to the operator, and note
   that traffic allowances differ by region at the same price.

## 9. TLS

**The worker needs no certificate of its own.** The design serves `/beta/` and
`/api/live-source/*` under `247streams.kr` through production's existing Caddy,
which already terminates TLS — validated end-to-end in §4 of the Caddy work.
So:

- the worker listens on **plaintext HTTP**, bound to the interface Caddy
  reaches it on, and is never published to the internet;
- no ACME, no certificate renewal, no second TLS surface to keep patched;
- the browser's connection is TLS to Caddy exactly as it is today.

If the worker is ever given its own hostname, Caddy on the VPS obtains a
certificate automatically — but that is a second deployment shape, not this one.

## 10. Firewall

Default-deny inbound. Three rules:

```
# SSH, key only, no password auth, no root login.
ufw allow from <operator-ip>/32 to any port 22 proto tcp

# The worker's port, reachable ONLY from production's Caddy.
ufw allow from <production-ip>/32 to any port 9080 proto tcp

ufw default deny incoming
ufw default allow outgoing      # YouTube ingest + the source pull
ufw enable
```

The worker's `--listen` must be the private or Caddy-facing address, never
`0.0.0.0`, so the firewall is the second layer rather than the only one. The
gate secret is the third: §4 of the Caddy validation measured that a request
reaching the worker's port directly, without the header, answers 403.

Outbound cannot be narrowed usefully — YouTube's ingest and `googlevideo`
address ranges are large and change.

## 11. Service auto-recovery

Two layers, both already built and tested.

**systemd restarts the process:**

```ini
[Service]
Restart=always
RestartSec=5
EnvironmentFile=/etc/louver-live-source.env   # chmod 600 — holds three secrets
ExecStart=/usr/local/bin/live-source-api \
  --listen 10.0.0.2:9080 \
  --origin https://247streams.kr \
  --allow-origin https://247streams.kr \
  --media-dir /srv/live-source/media \
  --state-dir /var/lib/live-source
# A 512MB upload plus ffprobe needs room; the encodes need the CPU.
LimitNOFILE=8192
```

**The worker restarts the broadcasts:** `Registry::recover()` reads every state
file and restarts the jobs whose `desired` is `Running` — the instruction, not
the last observed phase — sequentially, stopping at the ceiling. Eight tests in
`tests/recovery.rs` cover this, including that a cancelled job is *not*
resurrected and that ownership survives.

The three secrets go in the `EnvironmentFile`, never in `ExecStart`: a process
listing is world-readable.

## 12. Logs and monitoring

**Cap the journal first.** The compose file already notes that Docker's
`json-file` driver has no ceiling and that a server broadcasting around the
clock writes all day. The same applies to journald:

```
# /etc/systemd/journald.conf
SystemMaxUse=2G
MaxRetentionSec=14day
```

**What to watch, and what each answers:**

| signal | where | what it tells you |
|---|---|---|
| `GET /api/live-source/health` | gated HTTP | the process is alive; `running` vs `max_concurrent` |
| `phase` per job | `state_dir/*.json` | `sending` / `reconnecting` / `gave_up` |
| `frames` per job | same | rising = picture moving; frozen = the watchdog's `VideoStalled` |
| `restarts`, `last_verdict` | same | how often the source is dropping |
| `du -s /srv/live-source/media` | disk | approach to the per-user quota |
| vCPU steal time | `top` | a "dedicated" instance that is not (§2) |

A `gave_up` phase or a climbing `restarts` is the signal that matters: it means
the source went away and the worker could not get it back. Health alone is not
enough — the process stays up with every broadcast dead, which is exactly the
failure the watchdog exists to make visible.

Nothing in this crate logs a stream key, a resolved manifest URL, or a token;
eight tests in `tests/secrets.rs` assert it, including over the binary's own
startup banner.

## 13. The 24-hour real YouTube Live test

The first test that uses a real YouTube source. **Production is not involved at
any point**: no production OAuth token, no customer stream key, no production
database, no shared Docker socket.

### Before starting

1. **Accounts and material.** A YouTube channel the tester owns, with a live
   stream running that they own or have documented permission to re-transmit.
   No DRM, no members-only, no age-restricted, no private source — the resolver
   refuses those and the feature does not work around them.
2. **A destination that is not a customer's.** A second YouTube channel owned
   by the tester, or a local RTMP sink. Configure it under the tester's user id
   only:
   `LOUVER_LIVE_SOURCE_DESTINATIONS={"<tester-id>":{"beta-test":"rtmps://…"}}`
3. **Three secrets generated on the VPS**, each ≥32 bytes
   (`openssl rand -hex 32`): the signing secret, the gate secret (shared with
   Caddy), and — if §5 of REVOCATION.md is implemented — the admin secret.
4. **Baseline recorded before any traffic:** `nproc`, `free -m`,
   `df -h`, `pgrep -x ffmpeg | wc -l` (count only — never `pgrep -af`, which
   prints argv), and the egress counter from `/proc/net/dev`.
5. **Confirm production is untouched:** the production Caddy still answers on
   every existing path, and `pgrep -x ffmpeg | wc -l` on the *production* host
   is unchanged. This is the check that must pass before and after.

### Hours 0–1: does it work at all

6. Sign in at `247streams.kr`, open `/beta/`. The handshake should succeed and
   the page should list exactly the tester's own destination and no one else's.
7. `POST /check` with the watch URL. Expect `ok: true` and the source's real
   dimensions.
8. Upload two short audio files. Confirm they appear, and that their sizes and
   probed kind are right.
9. Start one broadcast. Then **verify on the receiving end, not here**: open
   the destination and confirm **the picture is the live source** and **the
   sound is the playlist**, and that the source's own audio is absent. An HTTP
   200 and a running FFmpeg are *not* a pass — that was explicit from PHASE 1
   and it is still the acceptance criterion.

### Hours 1–3: the second broadcast and the ceilings

10. Start a second broadcast. Both must stay healthy; record CPU and RSS and
    compare against §1. A sustained figure materially above 1.5 cores per
    encode means the instance is not giving the cores it sells.
11. Attempt a third. Expect **429**, and expect the message to say whether it
    was the per-user or the machine ceiling.
12. From a second account, confirm it cannot see or name the first account's
    destination, media, or jobs.

### Hours 3–24: the part that is actually new

13. **Close the browser entirely.** Both broadcasts must continue. This is the
    requirement the five-minute token is designed not to break: renewal is
    liveness for the UI, never authority over a job (REVOCATION.md §3).
14. **Log out of 247streams** in a fresh browser. Both broadcasts must
    *continue*; the page must say signed out. Then confirm a new broadcast
    cannot be created within five minutes (REVOCATION.md §2).
15. **Interrupt the source deliberately** — stop the upstream YouTube live for
    ~60 s, then resume. Expect `reconnecting`, then `sending` again, with
    `restarts` incremented and the picture recovered. This is the defect
    measured in PHASE 1 (FFmpeg does **not** exit when a live source
    disappears, because the playlist input is `-stream_loop -1`) and the
    watchdog is the only thing that notices.
16. **Let it run overnight.** Sample hourly: `frames` rising, `phase` =
    `sending`, RSS flat (a climbing RSS over 24 h is a leak), egress tracking
    ~6.4 Mbps per broadcast, free disk steady.
17. **Restart the service** (`systemctl restart`) around hour 12. Both
    broadcasts must come back by themselves via `recover()`, with the same
    owners. Note the gap — this is a real interruption to the beta broadcast,
    and measuring it is the point.
18. At hour 24, confirm the `MAX_JOB_SECS` cap behaves as REVOCATION.md §3
    specifies — **if** it has been implemented. It has not been yet, so on
    today's code the broadcast simply continues; record that rather than
    reporting a pass.

### Finishing

19. Stop both broadcasts from the UI. Confirm no `ffmpeg` remains
    (`pgrep -x ffmpeg | wc -l` → 0) and `desired` is `stopped` for both.
20. Record the totals: peak CPU and RSS, total egress, restart count, every
    `last_verdict` seen, and any entry in the journal that was not expected.
21. **Confirm production is still untouched** — the same check as step 5. If
    the production FFmpeg count or any existing path changed during this test,
    that is the finding, whatever else passed.

### What makes the test a failure

- The destination shows the wrong picture, the wrong sound, or the source's
  original audio.
- A broadcast stops when the browser closes or the user logs out.
- A frozen picture that `frames` does not catch, or a `gave_up` with no
  recorded reason.
- RSS climbing across the 24 hours.
- Any production path, container or FFmpeg process affected at all.
- A stream key, token or resolved manifest URL appearing in any log.

## 14. What this specification does not cover

- **Prices.** §8 — not verifiable here.
- **A second region, or any redundancy.** One box, for a test.
- **Automatic suspension revocation.** REVOCATION.md §6; it needs either a
  manual operator step or a production change that interrupts broadcasts.
- **Scaling past two broadcasts.** The arithmetic in §2 extends linearly
  (1.5 cores and 400 MB each), but nothing above four has been measured.
