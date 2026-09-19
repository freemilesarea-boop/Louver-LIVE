# Testing

## Principle

The things that would actually break a 24-hour broadcast are tested against
real FFmpeg and real processes. The things that are pure logic are tested
without them, on an injected clock, so the suite finishes in seconds.

Nothing in this suite asserts a number that was not measured. Where the
environment cannot support a check, the test **skips loudly** rather than
passing quietly.

## Running

```bash
npm run verify    # everything, in fail-fast order
```

| Command | What it covers |
| --- | --- |
| `npm run typecheck` | TypeScript in strict mode |
| `npm run lint` | ESLint, zero warnings allowed |
| `npm run test` | Frontend unit tests (formatting, day masks, status display) |
| `npm run test:e2e` | The whole UI journey against an in-memory backend |
| `npm run test:rust` | All Rust unit + integration tests |
| `npm run test:media` | Media pipeline, supervisor and runtime suites |
| `npm run soak -- --duration 1h` | Long-running stability |

## Layers

### 1. Rust unit tests — 223 tests, in-module

Fast, no I/O beyond temp directories. The interesting ones:

- **FFmpeg builder** — that stream-copy argv contains *no* video encoder; that
  Windows, macOS and Korean paths survive as single argv entries; that a stream
  key is masked in every form it can appear.
- **State machine** — every legal transition, and that an explicit stop is not
  undone by a non-zero exit code or a late exit event.
- **Scheduler** — ordinary windows, windows past midnight, the start day owning
  an overnight window, a day outside the mask, boundary inclusivity, and the
  §20 "app launched at 12:10 inside a 09:00–18:00 window" case.
- **Playlist** — order preservation, disabled items, empty playlists, and that
  shuffle never repeats a video back to back *including across the loop seam*
  (checked over 200 seeds with duplicated media ids).
- **Database** — migrations, CRUD, cascade deletes, and that a corrupt database
  file is moved aside so the app still starts.
- **Cache** — that a replaced source invalidates its cache, that a truncated
  encode reads as a miss, and that disk estimation refuses a job that would
  fill the disk.
- **Backoff** — the 2/5/10/20s schedule, monotonic, capped at 60s.

### 2. Media pipeline — 7 tests, real FFmpeg (`media_pipeline.rs`)

This is the suite that justifies the architecture (§74). Fixtures are
generated with FFmpeg at test time; no copyrighted media is committed.

| Test | What it proves |
| --- | --- |
| `full_pipeline_normalizes_concatenates_and_stream_copies` | Three mutually different sources (720p25/44.1k, 480p24/48k mono, 1080p60/32k) normalize to byte-identical stream parameters, concat, and stream-copy to FLV with 0 faults and an exact frame count |
| `infinite_loop_repeats_the_playlist_in_order` | `-stream_loop -1` produces A B C A B C A B C — verified by **sampling the pixel colour** at the middle of each 2s slot, not by trusting the duration |
| `concat_case_a_identical_sources` | §14 case A |
| `concat_case_b_and_c_different_sources_and_durations` | §14 cases B and C, including that no boundary stalls the video for >100ms |
| `concat_case_d_skewed_timestamps_are_repaired_by_normalization` | §14 case D — a source whose audio lags video by 700ms is repaired, and A/V start together afterwards |
| `looped_stream_copy_does_not_accumulate_av_drift` | Ten cycles, thirty boundaries: A/V skew stays <100ms and does **not grow** from early to late; zero stalls |
| `compatibility_mode_transcodes_the_same_playlist` | The fallback path still works |

The drift test is the important one. A 30ms error per seam would be invisible
in a short test and would put a 24-hour broadcast seconds out of sync.

### 3. Supervisor and recovery — 9 tests, real processes (`supervisor_recovery.rs`)

Spawns actual FFmpeg processes and kills them.

- A real FFmpeg is spawned, reports progress, reaches LIVE, and is recognised
  as ffmpeg by the orphan-cleanup check.
- Killing it out from under the supervisor produces RECONNECTING with a 2s
  backoff, and a respawn returns to LIVE with the backoff reset.
- An FFmpeg that fails to start is retried, not mistaken for a clean exit.
- A user stop terminates the process and no amount of polling restarts it.
- **FFmpeg stderr containing a publish URL with a stream key reaches the log
  callback masked.** This is the test that would catch a key leak.
- A simulated power cut: the state file is found, recovery resumes with the
  same `order_seed`, and the orphan pid is available for cleanup.
- Writing the state file 200 times never leaves a reader with a torn document.
- An orphan pid that is *not* ffmpeg is never killed.

### 4. Runtime — 16 tests, virtual clock (`runtime_scheduling.rs`)

The whole loop with a fake process and a `TestClock`, so the §64 success
scenario runs in milliseconds.

`the_scheduler_starts_and_stops_a_broadcast_without_any_user_action` walks
through: idle before 20:00 → auto-start at 20:00 → still LIVE at 23:59, 00:01,
03:00, 07:59 without relaunching → auto-stop at 08:00 → stays off at noon →
auto-starts again at 20:00 the next day.

Also covered: manual broadcasts are never stopped by the scheduler; stopping
by hand inside a window does not immediately restart, but scheduling resumes
at the next window; four consecutive crashes each recover; an explicit stop is
never undone; launching inside a window resumes immediately and launching
outside one does not; a crash whose window has passed cleans up without
broadcasting; a dangling database session is closed at startup.

### 5. Frontend — 16 unit + 16 e2e tests

Unit tests cover the pure helpers, including that the day-of-week bit layout
matches Rust's (Monday = bit 0) — a mismatch there would make every schedule
fire on the wrong day.

The e2e suite drives the real React components against `mockBackend.ts`:
first-run wizard → create playlist → add videos → see "optimization required"
→ read *why* → see the disk estimate → optimize → reorder by drag and drop →
schedule 20:00→08:00 and see "자정 넘김" → dry run shows TEST not LIVE →
uptime warnings → go live → confirm stop → save a stream key and see it masked
→ reveal only after confirmation → restart and find settings restored.

Two UI defects were found by writing these: a duplicated "add video" button in
the empty state, and a playlist total with no stable hook.

### 6. Soak — `npm run soak`

Not run in CI. Drives the real live pipeline on an operator's machine and
records memory, CPU, restarts and errors to CSV plus a JSON summary.

```bash
npm run soak -- --duration 1h
npm run soak -- --duration 24h --profile 1080p30
npm run soak -- --duration 10m --kill-every 2m   # exercise recovery
```

It paces with `-re`, the way a live broadcast is paced. An unpaced run reports
~145% CPU and 190 Mbps, which measures the disk rather than the product; the
summary includes a `paced_like_a_broadcast` verdict so a lost flag is caught.

The leak verdict uses **steady-state** growth (second half of the run), because
FFmpeg's buffer warm-up over the first ~30s otherwise looks like a leak.

## Release-candidate harness

These are `#[ignore]`d or script-driven: they take minutes to hours and need an
ingest endpoint, so they never run in `npm run verify`. They exist to answer
the questions a release has to answer, and they are the same harness whether
the endpoint is a local RTMP server or real YouTube.

### A local RTMP endpoint

```bash
npm run rc:sink -- --port 1935 --out rc-results/ingest
```

A persistent RTMP server that goes back to listening after each disconnect,
which is what makes reconnect testing possible. The app performs a real RTMP
handshake over a real socket, so the stream can be analysed at the *receiving*
end rather than trusting the sender's own account of itself.

### A broadcast run

```bash
# 30 minutes against the local endpoint
LOUVER_RC_DURATION_SECS=1800 \
  cargo test -p louver-core --test rc_live -- --ignored --nocapture rc_broadcast

# the same run against real YouTube, no code change (§4)
LOUVER_TEST_RTMPS_URL=rtmps://a.rtmps.youtube.com/live2 \
LOUVER_TEST_STREAM_KEY=… LOUVER_RC_DURATION_SECS=1800 \
  cargo test -p louver-core --test rc_live -- --ignored --nocapture rc_broadcast
```

It drives the real `BroadcastRuntime`, supervisor and playlist engine, samples
both processes, and writes a CSV plus a JSON summary. The stream key is read
from the environment into the in-memory secret store and is masked everywhere
it is reported — including in the harness's own console output.

### An unattended long run

```bash
npm run soak:overnight -- --end-at 2026-09-20T02:35:00Z --port 1940 --push
```

One command that starts an ingest, broadcasts against it until a wall-clock
end time, stops the way the Stop button does, checks for orphans and zombies,
analyses what the ingest received, and writes
`rc-results/overnight/FINAL_RESULT.md` with a PASS/FAIL/INCOMPLETE verdict. It
needs nobody watching it, which is the point.

Two things it does differently from a short run, both forced by length:

- **The CSV is written as it goes.** A thirteen-hour run that dies at hour ten
  must still leave ten hours of evidence behind.
- **The capture is a rolling window.** A full capture at 5 Mbps is about 2.4 GB
  an hour, so the ingest writes standalone FLV pieces and keeps only the first
  few and the last few. Comparing A/V skew in the last piece against the first
  is how drift across the whole run gets measured without storing the whole
  run.

If the run itself is interrupted — this repository's own container has been
reclaimed mid-run — the analysis is a separate program and can still produce
the verdict from whatever reached the disk:

```bash
npm run soak:report -- --out rc-results/overnight
```

A criterion it cannot measure is reported as NOT TESTED, which makes the
overall result INCOMPLETE. It is never a pass.

Other runs in the same file:

| Test | What it does |
| --- | --- |
| `rc_network_interruption` | Blackholes the ingest port with `iptables -j DROP` for 60s. A DROP, not a stopped server: dropped packets *block* FFmpeg rather than killing it, and only that reveals whether stall detection works. |
| `rc_ffmpeg_crash_recovery` | Kills the live FFmpeg three times and checks each recovery, that the backoff resets, and that an explicit Stop is still honoured. |
| `rc_application_restart_recovery` | Drops the owning process mid-broadcast, then starts a fresh runtime on the same data directory — inside the window it must resume with the same play order, outside it must not. |

### Boundary analysis

```bash
npm run rc:boundaries -- rc-results/ingest/session-001.flv
```

Takes a captured broadcast and measures, at every playlist transition, what a
viewer would actually experience: inter-frame gap, frame luminance (black
frames), audio peak level (dropouts), audio gap, and timestamp continuity —
plus whole-stream frame count, A/V skew drift and keyframe cadence.

### Long runs

```bash
npm run soak -- --duration 6h
npm run soak -- --duration 24h --profile 1080p30
npm run soak -- --duration 24h --destination rtmps://a.rtmps.youtube.com/live2 --stream-key <key>
```

One command: it stands up the endpoint, runs the real runtime, samples both
processes every five minutes, and writes everything to
`soak-results/<label>/` — `samples.csv`, `summary.json`, `runtime.log`,
`ffmpeg.log`, `console.log`, `ingest/sink-events.log`.

The leak verdict uses **steady-state** growth (second half of the run) because
FFmpeg's buffer warm-up over the first ~30s otherwise reads as a leak. The
summary also carries a `paced_like_a_broadcast` verdict: an early version of
this harness ran unpaced and reported 145 % CPU, which measured the disk rather
than the product.

### Supply-chain and secret checks

```bash
npm run secret-scan                    # working tree + full git history
npm run secret-scan -- --runtime       # also the app's data directory
npm run ffmpeg:manifest                # what the sidecars actually are
npm run ffmpeg:manifest -- --check     # fail if they are unfit to ship
```

`secret-scan` runs as part of `npm run verify`. It was proved to fire by
planting a stream key and a PEM block, both of which it caught.

## What is not tested here

- **Real YouTube ingest.** Every broadcast test runs against a real RTMP
  endpoint, but a local one. Set `LOUVER_TEST_RTMPS_URL` and
  `LOUVER_TEST_STREAM_KEY` to point the identical harness at YouTube.
- **Windows and macOS specifics.** The keychain, sleep prevention and autostart
  implementations are compiled for those targets but were not executed on them
  in this environment; the traits they implement are exercised through the
  no-op versions.
- **The packaged installer.** See IMPLEMENTATION_REPORT.md.
