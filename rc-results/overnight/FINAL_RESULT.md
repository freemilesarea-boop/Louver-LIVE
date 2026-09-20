# Overnight stability test — result

**OVERALL RESULT: PASS**

Every criterion below was measured and met.

| | |
| --- | --- |
| Test start | 2026-09-20 02:34:37 KST |
| Test end | 2026-09-20 11:26:47 KST |
| Actual runtime | **8h 52m** (31930s of 31930s planned) |
| Average CPU | FFmpeg **0.61 %**, runtime 0.04 % |
| Peak CPU | FFmpeg **0.93 %**, runtime 0.34 % |
| Memory growth | FFmpeg **0 %**, runtime **0.11 %** (steady state) |
| Reconnects | **0** |
| Errors | 0 FFmpeg error line(s), 0 restart(s), 0 incident(s) |
| Playlist loops | **76.9** (380 media boundaries) |
| Release impact | No long-run blocker found on this platform. Remaining release blockers are unchanged and listed in RELEASE_CANDIDATE_REPORT.md. |

## Criteria

| Criterion | Verdict | Measured |
| --- | --- | --- |
| broadcast held until the scheduled end | **PASS** | 31930s of 31930s planned |
| LIVE at every sample | **PASS** | 0 of 107 samples not LIVE |
| no unexpected process restarts | **PASS** | 0 restart(s) |
| no FFmpeg stream errors | **PASS** | 0 error line(s) |
| no FFmpeg memory leak | **PASS** | 0% steady-state growth |
| no runtime memory leak | **PASS** | 0.11% steady-state growth |
| playlist looped | **PASS** | 76.9 cycles, 380 media boundaries |
| no zombie processes | **PASS** | peak 0, after shutdown 0 |
| no orphan FFmpeg after Stop | **PASS** | 0 left |
| one unbroken RTMP session | **PASS** | 1 publisher session(s) |
| reconnects | **PASS** | 0 reconnect(s) |
| clean media boundaries in both windows | **PASS** | 28 boundaries examined across 4 window(s), 0 finding(s) |
| timestamps monotonic | **PASS** | 0 backwards, 0 duplicate across 4 window(s) |
| A/V drift did not accumulate | **PASS** | head max 20ms at 0s, tail max 20ms at 31200.07s |
| keyframe interval within YouTube limits | **PASS** | tail max 2.02s |

## Measured figures

| Metric | Value |
| --- | --- |
| Samples taken | 107 (every 300s) |
| FFmpeg RSS | 56.0 MB → 62.2 MB, peak 62.2 MB |
| Runtime RSS | 10.3 MB → 10.4 MB, peak 10.4 MB |
| Desktop shell RSS | 172.0 MB → 172.0 MB (0% steady growth) |
| Data sent | 20.38 GB at 5.11 Mbps |
| Data received by ingest | 20.38 GB |
| RTMP publisher sessions | 1 |
| Longest gap without progress | 0s |
| Samples with ingest stalled | 0 |
| Samples with network down | 0 |
| Zombie processes | peak 0, after shutdown 0 |
| Orphan FFmpeg after Stop | 0 |

## Stream received by the ingest

Capture is a rolling window: keeping all of this run would have taken
20.4 GB, so the ingest wrote standalone pieces and kept only the first
and last few. Comparing the last against the first is how drift across the
whole run is measured without storing the whole run.

| Window | Starts at | Boundaries | Freezes | Dup ts | Backwards ts | A/V skew max | Keyframe max |
| --- | --- | --- | --- | --- | --- | --- | --- |
| head | 0s | 7 | 0 | 0 | 0 | 20 ms | 2.02 s |
| head-2 | 600.2s | 7 | 0 | 0 | 0 | 20 ms | 2.02 s |
| tail | 30600.95s | 7 | 0 | 0 | 0 | 20 ms | 2.02 s |
| tail-2 | 31200.07s | 7 | 0 | 0 | 0 | 20 ms | 2.02 s |



## Earlier attempts at this run

**Attempt 1** — started 2026-09-19T13:51:13Z, died 2026-09-19T13:58:00Z after 300s of a planned 45226s.

- Cause: The execution container was reclaimed while the session was idle. `uptime` read 0 minutes when the session resumed at 17:30Z, and every process — orchestrator, ingest, FFmpeg, shell observer — was gone. The working directory survived.
- Application at fault: **no**
- Evidence: rc-results/overnight-aborted-1/ — runtime log shows a clean start and no error; the last sample at 300s was LIVE, 0 reconnects, 0 restarts, FFmpeg 0.62% CPU / 62.6 MB.
- Response: Restarted for the window that remained, and the analysis stage was split into `scripts/soak-report.mjs` so a report can still be produced from whatever reaches the disk if it happens again.

## Incidents

None recorded.


## What this run does and does not establish

- Measured on **Linux (container)** against a **local RTMP ingest** — a real
  socket and a real RTMP handshake, but not YouTube. macOS, Windows and
  YouTube ingest remain **NOT TESTED**.
- Sleep prevention: inhibited via `systemd-inhibit`.
- The test runs inside this container. If the container itself is destroyed,
  the run dies with it; that is a property of the environment, not of the app.
- Figures are what was measured over 8h 52m. Nothing here is
  extrapolated to 24 hours or to other hardware.

## Files

| | |
| --- | --- |
| Per-sample metrics | `rc-results/overnight/overnight.csv` |
| Harness summary | `overnight-summary.json` |
| Computed analysis | `analysis.json` |
| Runtime event log | `overnight-runtime.log` |
| FFmpeg log | `overnight-ffmpeg.log` |
| Orchestrator log | `orchestrator.log` |
| Ingest event log | `ingest/sink-events.log` |
| Boundary analysis | `boundaries-head.json`, `boundaries-head-2.json`, `boundaries-tail.json`, `boundaries-tail-2.json` |
