# Benchmark

Every number here was measured on the machine and in the conditions described
below. Nothing is a target, a goal, or an estimate. Where a figure has a
caveat, the caveat is stated.

## Environment

| | |
| --- | --- |
| CPU | Intel Xeon @ 2.10 GHz, 4 cores |
| RAM | 15 GiB |
| OS | Linux 6.18 (container) |
| FFmpeg | 6.1.1 (Ubuntu build) |
| Date | 2026-09-19 |
| Commands | `npm run soak -- --duration 180s --sample 10s --profile 1080p30`, `cargo test --workspace` |

This is a modest cloud VM, not the low-spec desktop the product targets. It is
a reasonable stand-in for a low-end machine's *per-core* performance, but the
macOS and Windows figures below are **not measured** — see Not Measured.

---

## 1. Live broadcast cost — the number that matters

1080p30, stream copy, paced with `-re` exactly as the app paces it.
180 seconds, 18 samples.

| Metric | Measured |
| --- | --- |
| FFmpeg CPU | **1.50 % min / 2.30 % max / 1.68 % mean** |
| FFmpeg RSS | 56.3 MB → 61.7 MB, then flat |
| Steady-state memory growth | **0.00 %** |
| Projected 24 h memory growth | **0 %** |
| FFmpeg restarts | 0 |
| Timestamp / DTS errors | 0 |
| Output rate | 4.34 Mbps |

The 2.30 % peak is the first sample, while FFmpeg is opening files and filling
buffers. From 40s onward it sits at 1.5–1.7 %.

**Why it is this low:** the live argv contains no encoder at all. There is a
unit test asserting exactly that (`stream_copy_argv_contains_no_video_encoder`),
so the property cannot regress silently.

Two caveats on these figures:

- **Output rate is 4.34 Mbps, not 10.** The profile caps at 10 Mbps but the
  soak fixtures are synthetic test patterns, which compress far below the cap.
  A real music video would push closer to 10 Mbps. This affects network
  throughput, not CPU: stream copy does the same work per byte regardless of
  what the bytes contain.
- **Pacing matters enormously.** An earlier unpaced run of the same pipeline
  reported **145 % CPU and 190 Mbps** — it was measuring how fast the disk
  could absorb data, not what a broadcast costs. `-re` was added to the soak
  harness, and the summary now carries a `paced_like_a_broadcast` verdict so a
  lost flag cannot quietly invalidate a future measurement.

### Memory: how the leak check works

Total RSS growth over the whole run reads 9.52 %, which looks alarming and is
not. FFmpeg allocates its muxer and I/O buffers over the first ~30 seconds;
after that memory is completely flat.

The leak verdict therefore measures the **second half** of the run only. Over
90 seconds of steady state, growth was 0.00 % — 61.7 MB at 90s and 61.7 MB at
180s. Extrapolated to 24 hours: 0 %.

A longer run on the operator's own machine is what actually settles this:

```bash
npm run soak -- --duration 24h --profile 1080p30
```

---

## 2. Concat and loop integrity

From `cargo test -p louver-core --test media_pipeline`, ten loop cycles of
three mutually different normalized sources (thirty file boundaries):

| Metric | Measured |
| --- | --- |
| Video frames produced | 3600 for 120.000 s — exact, none dropped or duplicated |
| Duplicate video timestamps | 0 |
| FFmpeg warnings / DTS faults | 0 |
| Video stalls >100 ms at a boundary | 0 |
| A/V skew, early in the run | < 1 ms |
| A/V skew, late in the run | < 1 ms |
| Skew growth across 30 boundaries | none measurable |

The last row is the one a 24/7 channel depends on. Even 30 ms of error per
seam would compound to seconds of desync over a day.

### A rejected alternative, for the record

A hybrid mode — video `-c:v copy`, audio re-encoded through
`aresample=async=1` — was measured against full stream copy on the same
fixtures:

| | Full `-c copy` | Hybrid (audio re-encode) |
| --- | --- | --- |
| Boundary discontinuities >50 ms | **20** | 29 |
| A/V skew along the timeline | **0.0 ms** | 3–19 ms |

Full stream copy won on both counts, so it is the primary path and the hybrid
was dropped. The only fallback is full real-time transcoding, shown in the UI
as "호환 모드".

---

## 3. Normalization cost (import time, not broadcast time)

60 seconds of 720p25 source → 1080p30, `libx264 -preset veryfast`, 4 cores,
software encoding:

| Metric | Measured |
| --- | --- |
| Wall time | 17.7 s |
| Throughput | **3.4× realtime** |
| Output size | 77.4 MB for 60 s |

So a 1-hour video takes roughly **18 minutes** to optimize on this machine,
once, and then costs nothing for every subsequent broadcast.

A hardware encoder (NVENC, QuickSync, AMF, VideoToolbox) is selected
automatically when available and is typically several times faster. None was
available here, so **no hardware-encoder figure is reported.**

---

## 4. Storage

Derived from the measured output size above, not from the nominal bitrate:

| Profile | Measured | Per hour | 10 × 1-hour videos |
| --- | --- | --- | --- |
| 1080p30 | 77.4 MB / 60 s | **4.64 GB** | ~46 GB |
| 720p30 | (nominal 4 Mbps) | ~1.9 GB | ~19 GB |

The app shows this estimate before optimization starts and refuses to begin if
it would leave less than 2 GB free.

---

## 5. Recovery

From `npm run soak -- --duration 40s --kill-every 15s`, with FFmpeg killed by
`SIGKILL` twice mid-run:

| Metric | Measured |
| --- | --- |
| Restarts after a kill | 2 / 2 — every kill recovered |
| Errors logged | 0 |
| Memory growth across restarts | 0.16 % |
| Orphan processes left behind | 0 |

Backoff timing is asserted separately in unit tests: 2 s, 5 s, 10 s, 20 s,
monotonic, capped at 60 s, and reset to 2 s after a successful reconnect.

## 6. Test suite runtime

| Suite | Tests | Time |
| --- | --- | --- |
| Rust unit | 223 | 1.2 s |
| Media pipeline (real FFmpeg) | 7 | 12.9 s |
| Supervisor / recovery (real processes) | 9 | 0.9 s |
| Runtime / scheduling (virtual clock) | 16 | 0.06 s |
| Frontend unit | 16 | 0.9 s |
| UI e2e | 16 | ~5 s |

The runtime suite covers a full overnight broadcast, a power cut and four
crash recoveries in 60 milliseconds, because the clock is injected.

---

## Not measured

Stated plainly rather than estimated:

- **macOS and Windows.** No figure on either platform. The keychain, sleep
  prevention and autostart code paths compile for those targets but were not
  executed there.
- **Hardware encoders.** No NVENC/QSV/AMF/VideoToolbox device was present.
- **Real YouTube ingest.** No stream key was available, so no end-to-end RTMPS
  measurement exists. Set `LOUVER_TEST_RTMPS_URL` to enable that path.
- **Runs longer than 3 minutes.** The 24-hour claim in §65 is *projected* from
  a flat 90-second steady state, not observed. Run `npm run soak -- --duration
  24h` to settle it.
- **The minimum hardware spec.** README lists i5 / 8 GB as a starting point.
  It has not been validated on such a machine and should be revised once it is.
- **Idle app CPU/RAM.** The Tauri shell was never run with a display attached
  in this environment, so only FFmpeg's cost is reported.

## Reproducing

```bash
npm install && npm run sidecar
npm run soak -- --duration 180s --sample 10s --profile 1080p30
cargo test -p louver-core --test media_pipeline -- --nocapture
```

The soak harness writes a per-sample CSV and a `soak-summary.json` carrying the
verdicts, so a future run can be compared against this one directly.
