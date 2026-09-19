# Architecture

## The decision everything else follows from

Louver Live broadcasts finished music videos on a loop, for hours at a time, on
whatever computer the user already owns. That makes CPU cost the dominant
constraint, and it rules out the obvious design — decode, compose, re-encode in
real time the way OBS does.

Instead the expensive work happens **once, at import time**, and the live path
does no encoding at all:

```
  import time (once per file)           broadcast time (continuous)
  ──────────────────────────────        ─────────────────────────────────
  source.mp4                            cache/<hash>/normalized.mp4  ─┐
    │ ffprobe                           cache/<hash>/normalized.mp4  ─┼─ concat
    │ compatibility check                cache/<hash>/normalized.mp4  ─┘   demuxer
    │ normalize (H.264 encode)                      │
    ▼                                               │ -c copy  (no encoder)
  cache/<hash>/normalized.mp4                       ▼
                                          FLV ──► RTMPS ──► YouTube
```

A single FFmpeg process runs for the whole broadcast. `-stream_loop -1` makes
the concat input repeat forever, so a file boundary is not a process boundary:
FFmpeg never restarts between videos and the RTMPS connection is never
re-established.

### Why this needs normalization to be strict

The concat demuxer can only stream-copy inputs that agree on codec, geometry,
frame rate, pixel format, time base and audio layout. "Close enough" produces
timestamp faults at every seam. So `check_compatibility` refuses anything that
is not an exact match — 29.97fps instead of 30, or a `1/15360` time base
instead of `1/30000`, both send the file to the normalizer.

Normalized output is pinned to:

| Property | 1080p30 | 720p30 |
| --- | --- | --- |
| Resolution | 1920×1080 | 1280×720 |
| Frame rate | 30 CFR | 30 CFR |
| Video | H.264 High @ 4.2, yuv420p | same |
| GOP | 60 frames (2s keyframes) | same |
| Video time base | 1/30000 | 1/30000 |
| Audio | AAC-LC, 48 kHz, stereo, 192k | same |
| Bitrate | 10 Mbps | 4 Mbps |

Every normalized file is also cut to a **whole number of video frames**
(`-t`, rounded down). Rounding down rather than to nearest matters: asking
FFmpeg for a frame the source does not have makes it hold the last frame, which
shows up as a stutter at every loop seam.

### Evidence

This was measured before the architecture was settled, and the measurement
lives in `crates/louver-core/tests/media_pipeline.rs`. Over ten loop cycles
(thirty file boundaries) of three mutually different sources:

- 0 timestamp faults, 0 DTS warnings, exit code 0
- 3600 video frames for 120.000s — no dropped or duplicated frames
- A/V skew flat at <1ms, start to finish (`looped_stream_copy_does_not_accumulate_av_drift`)

A hybrid mode (video copy, audio re-encoded with `aresample=async=1`) was also
measured and was **worse** — 29 boundary discontinuities against 20 — so full
`-c copy` is the primary path and real-time transcoding exists only as the
"호환 모드" fallback.

## Crate layout

The product's behaviour lives in `louver-core`, which has no Tauri dependency.
That is what lets the engine — including scheduling and crash recovery — be
tested on a CI machine with no GUI stack.

```
crates/louver-core/           no Tauri, no UI, fully testable
  error.rs                    LL-XXX-NNN codes + Korean messages
  config.rs                   output profiles, stream modes
  clock.rs                    Clock trait; TestClock drives the scheduler tests
  database/                   SQLite + versioned migrations
  media/probe.rs              ffprobe JSON -> MediaInfo, compatibility check
  media/normalize.rs          the one place encoding happens
  media/cache.rs              content-addressed cache, disk estimation
  streaming/ffmpeg.rs         FfmpegCommandBuilder, encoder detection, masking
  streaming/manifest.rs       concat demuxer manifest
  streaming/playlist.rs       PlaylistOrderEngine (sequential / shuffle-once)
  streaming/state.rs          StreamState machine with validated transitions
  streaming/supervisor.rs     process health, exit handling, reconnect backoff
  streaming/engine.rs         playlist -> SessionPlan -> manifest
  streaming/preflight.rs      CHECK 1-7
  scheduler/                  occurrence maths, including past midnight
  runtime.rs                  the tick loop that ties it all together
  session/                    crash-safe state file + recovery decisions
  security/                   SecretStore trait, masking, ingest URL assembly
  license/                    Ed25519 verification
  logging/                    rotating logs that mask on the way in
  system/                     metrics, disk, sleep/autostart traits

apps/desktop/src-tauri/       windows, tray, IPC, OS integration only
  state.rs                    wires core dependencies together
  commands/                   thin translation layer
  platform/                   keychain, sleep prevention, autostart

apps/desktop/src/             React UI
  services/ipc.ts             the only place that talks to Rust
  test/mockBackend.ts         in-memory backend for dev + e2e
```

## The runtime loop

`BroadcastRuntime::tick()` runs about once a second on a background thread, so
it keeps working while the window is hidden:

```
tick()
 ├── tick_supervisor()
 │     ├─ restart due?          -> respawn FFmpeg
 │     ├─ CONNECTING + output?  -> LIVE, reset backoff
 │     ├─ stalled (no output)?  -> kill, RECONNECTING
 │     └─ poll(): exited?       -> RECONNECTING (2s,5s,10s,20s… capped 60s)
 │                                 unless the user asked to stop
 ├── tick_scheduler()
 │     ├─ ShouldBroadcast  -> start, unless this window was stopped by hand
 │     ├─ ShouldStop       -> stop (never stops a manual broadcast)
 │     └─ Idle             -> clear the suppression once the window passes
 ├── persist()   session.json (atomic rename) + stream_sessions row
 └── publish()   -> louver://status -> the dashboard
```

Its three dependencies — `Clock`, `StreamLauncher`, `RuntimeEvents` — are all
injected, which is why `tests/runtime_scheduling.rs` can run a full
20:00→08:00 overnight broadcast, a mid-broadcast power cut and four crash
recoveries in 60 milliseconds.

## State machine

```
IDLE ──► PREPARING ──► CONNECTING ──► LIVE
              │             │  ▲        │
              │             ▼  │        ▼
              │        RECONNECTING ◄───┘
              ▼             │
           STOPPING ◄───────┘
              │
              ▼
           STOPPED ──► (PREPARING)
```

Illegal moves are rejected with `LL-STREAM-006` rather than silently applied.
The `user_requested_stop` latch is what separates "FFmpeg died, reconnect" from
"the user pressed Stop, stay stopped" — the supervisor consults it before every
restart, and a late exit event arriving after a completed stop is a no-op.

## Recovery

Two independent mechanisms, because they fail differently:

1. **Within a run** — the supervisor notices a dead or stalled FFmpeg and
   restarts it with backoff. The RTMPS connection is re-established; the user
   sees RECONNECTING.
2. **Across runs** — `session.json` is rewritten (temp file + atomic rename) on
   every tick. On launch, a file whose state is not terminal means the previous
   process died. Recovery then resumes **only if the current time is inside a
   schedule window**; outside one, the state is cleared and nothing broadcasts.
   The saved `order_seed` is replayed so a shuffled playlist resumes in the
   same order.

Orphaned FFmpeg processes are killed by pid *and* name check — pids are reused,
and killing the wrong process would be worse than leaving one behind.

## Security boundaries

- The stream key goes to the OS keychain (macOS Keychain / Windows Credential
  Manager) and never to SQLite, JSON or a log file.
- `mask_secrets()` runs on the way *into* the logger, the event table and every
  error detail, so no call site has to remember to mask.
- FFmpeg is invoked as an argv vector, never through a shell. Paths with
  spaces or Korean characters are handled by the exec layer, not by quoting.
- The Tauri capability set grants file selection, folder reveal and autostart.
  The shell scope is empty: the frontend cannot execute anything.

## What V1 deliberately does not have

No camera, microphone, browser or game capture, no scenes, no overlay or
subtitle editor, no platforms other than YouTube, no streaming server of its
own, no OAuth, no cloud sync, no AI. The V2 seams that exist are
`PlaylistOrderEngine` (for dynamic shuffle) and `SpeedTestProvider` (for a real
bandwidth measurement) — both traits with one implementation today.
