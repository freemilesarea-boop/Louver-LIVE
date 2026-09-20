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
| Ingest | Local RTMP server — real handshake, real socket. **Not YouTube.** |
| Commands | `npm run soak`, the RC harness in `crates/louver-core/tests/rc_live.rs`, `npm run rc:boundaries` |

This is a modest cloud VM, not the low-spec desktop the product targets. It is
a reasonable stand-in for a low-end machine's *per-core* performance, but the
macOS and Windows figures below are **not measured** — see Not Measured.

---

## 1. Live broadcast cost — the number that matters

A **30-minute broadcast over real RTMP**, 1080p30 stream copy, driven by the
real `BroadcastRuntime` with five source videos of differing lengths and
formats. One unbroken publisher connection for the whole run.

| Metric | Measured |
| --- | --- |
| FFmpeg CPU | mean **0.57 %**, peak **0.63 %** |
| FFmpeg RSS | 59.7 MB → 68.1 MB, then flat |
| FFmpeg steady-state memory growth | **0.00 %** |
| Broadcast runtime RSS | 10.86 MB → 10.89 MB |
| Runtime steady-state memory growth | **0.00 %** |
| Reconnects / restarts | **0 / 0** |
| Ticks not LIVE | **0** |
| FFmpeg errors | **0** |
| Playlist loops completed | 4.3 |
| Data sent | 1.18 GB |
| Throughput | 5.25 Mbps |
| Connect time | 1.0 s |

An earlier, shorter measurement against a **file sink** reported 1.5–1.7 %.
The figure against a real socket is lower still, because the pacing is
governed by the connection rather than by the disk.

Throughput is 5.25 Mbps against a 10 Mbps profile cap because the fixtures are
synthetic test patterns, which compress far below it. Real music video would
sit near the cap. It affects network throughput only, not CPU: stream copy does
the same work per byte whatever the bytes contain.

### Desktop shell, separately

The figures above are the broadcast engine. The Tauri shell — window, webview
and the once-a-second runtime tick — was observed separately for 40 minutes:

| Metric | Measured |
| --- | --- |
| RSS | **173.6 MB, completely flat** |
| CPU | 0.1 % |
| Threads | 30, stable |

**Why it is this low:** the live argv contains no encoder at all. A unit test
asserts exactly that (`stream_copy_argv_contains_no_video_encoder`), and the
app can verify it at runtime from the process's real argv (Settings →
Developer Mode → Streaming Mode), so the property cannot regress unnoticed.

### Two lessons about measuring this

- **Pacing matters enormously.** An early version of the soak harness ran
  unpaced and reported **145 % CPU and 190 Mbps** — it was measuring how fast
  the disk could absorb data, not what a broadcast costs. A live broadcast is
  paced by the connection. The summary now carries a
  `paced_like_a_broadcast` verdict so a lost `-re` cannot quietly invalidate a
  future measurement.

- **Total memory growth is the wrong metric.** FFmpeg allocates its muxer and
  I/O buffers over the first ~30 seconds, which reads as roughly 10 % growth
  and looks like a leak. The leak verdict therefore measures the **second half
  of the run only**. Over the 30-minute run that figure is 0.00 % for both
  processes.

A longer run on the operator's own machine is what settles the 24-hour
question:

```bash
npm run soak -- --duration 24h --profile 1080p30
```

---

## 2. Concat and loop integrity

Measured on the stream the **ingest received**, not on the sender's own
account of itself: `npm run rc:boundaries` decodes the captured broadcast and
measures each transition. 21 boundaries across 4.3 playlist cycles of the
30-minute run.

| Metric | Measured |
| --- | --- |
| Inter-frame gap at a boundary | 41 – 55 ms (< 2 frames at 30 fps) |
| Frames stalled > 100 ms | **0** |
| Black frames at a boundary | **0** (luminance 92 – 131) |
| Audio dropouts at a boundary | **0** (peak −20.7 to −21.1 dB) |
| Audio gap at a boundary | 22 ms — exactly one AAC frame, inaudible |
| Duplicate timestamps | **0** |
| Backwards timestamps | **0** |
| Frames received | 54 028 vs 54 040 expected (0.02 %) |
| A/V skew | 6 ms → 4 ms over 30 minutes, max 16 ms |
| Keyframe interval | max 2.02 s (YouTube requires ≤ 4 s) |
| Publisher reconnections | **0** — one RTMP session throughout |

The A/V row is the one a 24/7 channel depends on. Even 30 ms of error per seam
would compound to seconds of desync over a day; measured, it does not grow.

The unit-level equivalent (`media_pipeline`, ten cycles of three sources over a
file sink) reports the same: 3600 frames for 120.000 s, 0 duplicates, 0 faults.

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

Against a real RTMP endpoint.

**Network outage** — a genuine 60-second packet blackhole (`iptables -j DROP`),
not a stopped server:

| Metric | Measured |
| --- | --- |
| LIVE → RECONNECTING | 35 s (30 s stall timeout + tick) |
| Reconnect attempts in 60 s | 3, spaced **2 s / 5 s / 10 s** |
| Recovery once the network returned | **4 s** |
| App crash | none; UI answered 114 consecutive status reads |
| Orphan processes | 0 |

A stopped server is *not* equivalent: dropped packets block FFmpeg on the
socket instead of killing it, and only stall detection notices. Measuring with
a stopped server produced a false PASS and hid a real bug.

**FFmpeg killed** — three consecutive `SIGKILL`s during a live broadcast:

| Metric | Measured |
| --- | --- |
| Recoveries | 3 / 3, each on a new process |
| `reconnect_count` after each success | reset to 0 — the backoff does not escalate across unrelated faults |
| Explicit Stop honoured afterwards | yes, 40 ticks with no restart |
| Orphan processes | 0 |

Backoff timing is asserted separately in unit tests: 2 s, 5 s, 10 s, 20 s,
monotonic, capped at 60 s, reset after a successful reconnect.

## 6. Long-run stability

<!-- LONG-RUN:BEGIN -->
### 8h 52m, 2026-09-20 02:34:37 KST → 2026-09-20 11:26:47 KST

Real RTMP ingest, 1080p30 stream copy, one continuous FFmpeg session, run
unattended by `npm run soak:overnight`. **Result: PASS.**

| Metric | Measured |
| --- | --- |
| Runtime | 8h 52m (31930s) |
| FFmpeg CPU | mean **0.61 %**, peak **0.93 %** |
| Runtime CPU | mean 0.04 %, peak 0.34 % |
| FFmpeg RSS | 56.0 MB → 62.2 MB, peak 62.2 MB |
| FFmpeg steady-state growth | **0 %** |
| Runtime RSS | 10.3 MB → 10.4 MB, peak 10.4 MB |
| Runtime steady-state growth | **0.11 %** |
| Reconnects / restarts | **0 / 0** |
| Samples not LIVE | 0 of 107 |
| FFmpeg errors | 0 |
| Playlist loops | **76.9** (380 media boundaries) |
| RTMP publisher sessions | 1 |
| Longest gap without progress | 0s |
| Zombie / orphan processes | 0 / 0 |
| Data sent | 20.38 GB at 5.11 Mbps |
| A/V skew, first window (0s) | max 20 ms |
| A/V skew, last window (31200.07s) | max 20 ms |

Capture is a rolling window of standalone FLV pieces; a full capture of this
run would have been ~20 GB. The first and last pieces are kept, which is
what makes the two A/V rows above a before-and-after rather than a single
reading.

Full detail, per-sample CSV and the criteria table: `rc-results/overnight/FINAL_RESULT.md`.

Still **NOT TESTED**: macOS, Windows, YouTube ingest, and any run longer than
this one. Nothing above is extrapolated.
<!-- LONG-RUN:END -->

## 7. Test suite runtime

| Suite | Tests | Time |
| --- | --- | --- |
| Rust unit | 244 | 1.1 s |
| Media pipeline (real FFmpeg) | 9 | 14.0 s |
| Supervisor / recovery (real processes) | 9 | 1.0 s |
| Runtime / scheduling (virtual clock) | 16 | 0.05 s |
| Runtime live (real FFmpeg) | 2 | 11.9 s |
| RC security (real FFmpeg) | 7 | 4.1 s |
| RC licence | 10 | 0.06 s |
| Docs sync | 5 | <0.1 s |
| Frontend unit | 16 | 1.5 s |
| UI e2e | 18 | 7.5 s |
| **Total** | **339** | ~55 s |

The runtime suite covers a full overnight broadcast, a power cut and four
crash recoveries in 60 milliseconds, because the clock is injected.

---

## Not measured

Stated plainly rather than estimated:

- **macOS and Windows.** No figure on either platform. The keychain, sleep
  prevention and autostart code paths compile for those targets but were not
  executed there.
- **Hardware encoders.** No NVENC/QSV/AMF/VideoToolbox device was present.
- **Real YouTube ingest.** No stream key was available. Every broadcast figure
  here is against a real RTMP endpoint — real handshake, real socket, measured
  at the receiving end — but a local one. Set `LOUVER_TEST_RTMPS_URL` and
  `LOUVER_TEST_STREAM_KEY` to point the identical harness at YouTube.
- **Runs longer than those stated in §6.** No 24-hour figure has been observed.
  Run `npm run soak -- --duration 24h` on target-class hardware to obtain one;
  nothing here is extrapolated to 24 hours.
- **The minimum hardware spec.** README lists i5 / 8 GB as a starting point.
  It has not been validated on such a machine and should be revised once it is.
- **Real-world bitrate.** All throughput figures use synthetic test patterns,
  which compress far below the profile cap. Measure again with actual music
  video before quoting a bandwidth requirement to users.

## Reproducing

```bash
npm install && npm run sidecar && npm run rc:fixtures

# 30-minute broadcast over a real RTMP endpoint
npm run rc:sink -- --port 1935 --out rc-results/ingest &
LOUVER_RC_DURATION_SECS=1800 \
  cargo test -p louver-core --test rc_live -- --ignored --nocapture rc_broadcast

# boundary analysis of what the ingest received
npm run rc:boundaries -- rc-results/ingest/session-001.flv

# the unit-level pipeline checks
cargo test -p louver-core --test media_pipeline -- --nocapture
```

The soak harness writes a per-sample CSV and a `soak-summary.json` carrying the
verdicts, so a future run can be compared against this one directly.
