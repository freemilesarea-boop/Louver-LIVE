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
FFmpeg is not installed.
