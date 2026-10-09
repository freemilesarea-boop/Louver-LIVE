# louver-live-source (Beta)

Sends a **YouTube Live picture** with the **playlist's sound** to an RTMP(S)
destination, from its own process.

This crate exists to be isolated. It is not part of `louver-server`, it opens no
production database, and it shares no FFmpeg child, credential or thread with
the broadcasts running today. Nothing in it is wired into the production UI or
API yet — that is a separate design, deliberately.

## Why a separate process

A live video source cannot be stream-copied: its picture and the playlist's
sound are unrelated streams, so the output has to be encoded. Measured on a
4-core dev box with the production argv:

| path | CPU | RSS |
|---|---|---|
| existing playlist broadcast (stream copy) | 2–8 % | 56 MB |
| this worker, 1080p30 | 95–148 % | 388 MB |

About one core each, against two to eight percent for an existing broadcast.
Co-locating these with customer broadcasts would take CPU from people who are
on air, which is why [`limits`](src/limits.rs) caps concurrency from the
machine's core count and why the eventual home for this is a separate host.

## What it reuses, and what it adds

Reused verbatim, because two copies of either would be the bug:

- `louver_core::streaming::ffmpeg::build_live_video_stream_args` — the
  composition that maps `1:v:0` (live picture) and `0:a:0` (playlist sound).
  The source's own audio is dropped by not mapping it.
- `louver_cloud::cctv::validate` — SSRF, private ranges, URL credentials,
  ports, protocol allow-list.

Added here:

- `resolver` — a YouTube watch URL becomes a stream FFmpeg can open, via
  `yt-dlp`. Only `youtube.com` / `youtu.be` hosts with a real video id ever
  reach the binary, and the URL it hands back is re-validated before use.
- `watchdog` — notices that the picture has frozen while the sound carries on.
  The existing pipeline cannot: when a live source goes away FFmpeg does **not**
  exit (measured: picture stopped at 17.4 s, sound ran to 60.0 s, process alive
  at +60 s), so a supervisor waiting for an exit waits for ever.
- `args` — a 1080p output cap. The production argv has no scale filter, so
  without this a 2160p source is re-encoded at 2160p.
- `limits`, `state`, `worker` — concurrency ceiling, a JSON state file per
  worker, and one FFmpeg owned by handle with bounded restarts.

## Running it

`yt-dlp` and `ffmpeg` must be on `PATH` (or named with `--yt-dlp` / `--ffmpeg`).
**Do not install `yt-dlp` on the production server** — it is approved for
development environments only.

Start an RTMP receiver, in its own terminal:

```bash
./scripts/test-rtmp-sink.sh 1935 /tmp/received.flv 60
```

Build a concat manifest naming the playlist files the sound comes from:

```bash
printf "file '/path/a.mp4'\nfile '/path/b.mp4'\n" > /tmp/manifest.txt
```

Run the worker. The destination comes from the environment so a stream key does
not land in a shell history or a process listing:

```bash
export LOUVER_LIVE_SOURCE_DEST='rtmp://127.0.0.1:1935/live/test'
cargo run -p louver-live-source --bin live-source-worker -- \
  --source    'https://www.youtube.com/watch?v=VIDEO_ID' \
  --manifest  /tmp/manifest.txt \
  --state-dir /tmp/live-source-state \
  --worker-id w1 \
  --run-for   60
```

Then check what **arrived**, rather than trusting an exit code:

```bash
ffprobe -v error -show_entries stream=codec_type,codec_name,width,height \
        -of default=nw=1 /tmp/received.flv
cat /tmp/live-source-state/worker-w1.json
```

Options: `--max-restarts` (default 5), `--stall-after` (12 s), `--grace` (20 s),
`--run-for` (unbounded), `--worker-id`, `--yt-dlp`, `--ffmpeg`, `--ffprobe`.

## Secrets

Three things have no field, parameter or log line anywhere in this crate:

- the RTMP(S) **destination** — it carries the stream key;
- the **resolved manifest URL** — YouTube signs it, so it is closer to a
  credential than to an address;
- anything OAuth. This crate has no OAuth code and never reads a token.

The video id *is* logged and stored: it is in every share link, and without it
an operator cannot tell two failing workers apart. `tests/secrets.rs` asserts
all of this against the real binary's output and the state file it writes.

## Supported sources

A public or unlisted YouTube Live broadcast the user owns or has permission to
restream. `yt-dlp` is run with no cookies, no credentials and no browser
profile, so private, members-only, age-restricted and DRM-protected videos
simply fail to resolve — there is no code path that could be given a credential.

## Tests

```bash
cargo test -p louver-live-source
```

The integration suites run a real FFmpeg against a local HLS source and receive
the result over a real RTMP connection, then decode it: the fixtures are a
**green** picture with a **440 Hz** tone for the playlist and a **red** picture
with a **100 Hz** tone for the live source, so a correct output is red with
440 Hz and any wrong wiring is unmistakable. They skip, rather than fail, where
FFmpeg is not installed, and they find it on `PATH` (`LOUVER_TEST_FFMPEG`
overrides).

The seven heaviest are `#[ignore]`d, following this repository's convention for
tests that encode real video (`optimize_speed.rs`, `rc_live.rs`): together they
take about two minutes and bind real sockets. Run them explicitly:

```bash
cargo test -p louver-live-source -- --ignored
```

Everything else — the unit tests, the whole API surface, recovery and the
secret containment — runs in the default pass.

---

# The API and the beta page

Two more binaries' worth of surface, in the same crate and the same process
boundary: `live-source-api` serves both the HTTP API and the `/beta/` page, so
247streams needs no change to reach either.

## Authentication

247streams' session cookie is good for the **whole** production API. Handing it
to a worker on another machine on every request would make a compromise of that
machine a compromise of every beta user's entire account. So it is used **once**:

```
browser ──POST /api/live-source/session (cookie)──► worker
                                                     │ GET /api/me (that cookie, once)
                                                     ▼
                                                 247streams
worker ──{ token, destinations, limits }──────────► browser
browser ──Authorization: Bearer <token>───────────► worker   (cookie stripped by Caddy)
```

The token is HMAC-SHA-256 signed, audience-scoped (`live-source`, so it is
useless against production), carries only a user id and a plan, and expires in
30 minutes. It is **not** JWT: there is no `alg` field, so there is no
algorithm-confusion attack. The signature is compared in constant time.

Two alternatives were weighed:

| | production change | supplies identity |
|---|---|---|
| **A** — louver-server mints the token | **yes** (new image, container recreate) | yes |
| **B** — Caddy `forward_auth` alone | no (Caddyfile only) | **no** — `/api/me` answers with a body, and Caddy cannot read a body into a header |
| **C — implemented** — Caddy gate + one-shot handshake | no | yes |

A is the better design and the one to move to; it needs a container recreate,
which is the thing this project is organised around avoiding. B cannot tell the
API *who* is calling, and an API that cannot do that cannot keep one user's jobs
from another's. C is B as the outer gate plus one cookie use for identity.
Moving to A later changes `Identity::from_production` and nothing else.

## Routes

| | path | auth |
|---|---|---|
| `GET` | `/api/live-source/health` | none — says nothing about any user |
| `POST` | `/api/live-source/session` | session cookie (the only place it is used) |
| `POST` | `/api/live-source/check` | bearer |
| `GET` `POST` | `/api/live-source/jobs` | bearer |
| `GET` `DELETE` | `/api/live-source/jobs/{id}` | bearer |

`POST /jobs` is **idempotent on `broadcast_id`**: a repeat returns the existing
job rather than starting a second FFmpeg against the same destination, because
two senders on one ingest URL is worse for a viewer than none.

Somebody else's job reads as **404**, never 403 — a 403 would confirm the id
exists, which is the whole value of guessing ids.

## What a request may not contain

- **No user id.** The caller is whoever the token says. There is no field.
- **No file path.** Media is named, and the name is resolved inside one
  configured directory: a separator, a `..`, a dot-prefix, a colon or a NUL is
  refused before any join, and the joined path is canonicalised and
  re-checked — which also catches a symlink out of the root.
- **No destination URL.** The RTMP(S) URL carries the stream key. A client names
  a destination and the operator's configuration supplies the URL. A client that
  could post one could point someone else's picture and music at its own ingest.
- **No internal address.** `POST /jobs` and `POST /check` share one
  `validate_source`, so loopback, private ranges and metadata endpoints are
  refused at the boundary rather than later and out of sight.

## Running it

```bash
export LOUVER_LIVE_SOURCE_SECRET='…32+ bytes…'
export LOUVER_LIVE_SOURCE_DESTINATIONS='{"test-sink":"rtmp://127.0.0.1:1935/live/test"}'
cargo run -p louver-live-source --bin live-source-api -- \
  --listen 127.0.0.1:9080 \
  --origin https://247streams.kr \
  --media-dir /srv/live-source/media \
  --state-dir /var/lib/live-source
```

Both secrets come from the environment, never argv: a process listing is
readable by every user on the machine. The beta page is then at
`http://127.0.0.1:9080/beta/`.

## Recovery

State is one JSON file per job, and two writers share it with different fields:

- the **registry** owns `owner` (who may read it) and `desired` (whether it is
  meant to be running);
- the **worker** owns everything it observes (`phase`, `frames`, `restarts`).

The worker persists by read-modify-write and carries the registry's two fields
over, so a worker tick cannot blank an owner or undo a cancel. `cancel` writes
the intent before setting the stop flag *and* again after joining the thread,
which closes both the crash window and the stale-read race.

On start, `recover()` restarts the jobs whose `desired` is `Running` —
sequentially, because starting six x264 encodes in one instant is how a recovery
becomes an outage — and stops at the concurrency ceiling. A clean shutdown
deliberately leaves `desired` alone, so a planned restart resumes rather than
forgetting.
