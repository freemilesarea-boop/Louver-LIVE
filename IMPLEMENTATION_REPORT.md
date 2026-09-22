# Implementation Report

Everything below was executed on this machine. Nothing is marked done because
the code exists; it is marked done because a command was run and its output
observed. Where something was not run, it is listed under Failed / Skipped or
Known Limitations rather than being glossed over.

Environment: Linux 6.18, Intel Xeon 2.10 GHz ×4, 15 GiB RAM, Node 22.22.2,
Rust 1.94.1, FFmpeg 6.1.1. Date 2026-09-19.

> **This document covers the implementation phase.** A subsequent
> release-candidate verification phase tested the built application against a
> real RTMP endpoint and found four further defects, including one critical
> one (stall detection was dead code, so a network outage that blocked rather
> than killed FFmpeg left the app showing LIVE forever). Current status, the
> measured results and the remaining release blockers are in
> **[RELEASE_CANDIDATE_REPORT.md](RELEASE_CANDIDATE_REPORT.md)**, which
> supersedes the performance figures below.

---

## 1. Implemented

### The four features the brief requires to actually work

| # | Feature | Status | Evidence |
| --- | --- | --- | --- |
| 1 | Multiple videos as one continuous playlist | Working | `media_pipeline.rs` — three heterogeneous sources normalized and concatenated; 3600 frames for exactly 120.000s |
| 2 | FFmpeg stream-copy broadcast pipeline | Working | `runtime_live.rs` produces a real playable FLV; live argv contains no encoder (asserted) |
| 3 | Scheduled start / stop | Working | `runtime_scheduling.rs` runs a full 20:00→08:00 overnight cycle including auto-restart the next day |
| 4 | Automatic recovery after abnormal termination | Working | `runtime_live.rs` kills real FFmpeg and recovers; `supervisor_recovery.rs` covers crash + power cut |

### Phase by phase

| Phase | Status | Notes |
| --- | --- | --- |
| 1 Project bootstrap | Done | Tauri 2 + React 18 + TS strict + Vite + Zustand + Tailwind |
| 2 FFmpeg sidecar | Done | `scripts/fetch-ffmpeg.mjs`, per-triple, download with documented dev fallback |
| 3 Media probe | Done | ffprobe JSON → `MediaInfo`, HDR/rotation/time-base detection |
| 4 Normalize / cache | Done | Content-addressed cache, disk estimation, cancellable, progress via `-progress pipe:1` |
| 5 Playlist / SQLite | Done | 7 tables + versioned migrations, corrupt-DB recovery |
| 6 Concat + local output | Done | Dry Run writes a real FLV |
| 7 Stream-copy engine | Done | Primary path; validated before the architecture was fixed |
| 8 RTMPS engine | Code complete, **unverified against YouTube** | No stream key was available — see Failed / Skipped |
| 9 Supervisor / reconnect | Done | Real-process tests, 2/5/10/20s backoff capped at 60s |
| 10 Scheduler | Done | Including past-midnight windows and startup recovery |
| 11 Autostart / tray / sleep | Code complete, **Linux only at runtime** | Tray built; Windows/macOS paths compiled but not executed |
| 12 Security / keyring | Code complete, **keychain not exercised** | macOS/Windows backends compile; Linux falls back to memory and says so |
| 13 UI polish | Done | Five pages + first-run wizard; screenshots in `docs/screenshots/` |
| 14 Automated tests | Done | 301 tests total |
| 15 Soak tooling | Done | `npm run soak`, ran up to 3 minutes here |
| 16 Packaging | Done for Linux | `.deb` built and inspected; Windows/macOS bundles not buildable here |

### Feature checklist (§75)

Working and tested: MP4/MOV/MKV import, automatic analysis, automatic
optimization, Sequential playlist, Shuffle Once, infinite loop, manual
start/stop, schedule, daily repeat, stream copy, reconnect, crash recovery,
startup recovery, system tray, sleep prevention (Linux/Windows/macOS code
paths), secure stream key (trait + platform backends), logs, dry run, tests.

Implemented, not verifiable here: RTMPS to YouTube, auto launch at login
(Tauri plugin wired but the OS-level entry was not created), Windows/macOS.

---

## 2. Passed tests

All figures are from actual runs.

### `npm run verify` — 9/9 steps

```
PASS  ffmpeg sidecar               0.1s
PASS  frontend typecheck           2.2s
PASS  frontend lint                1.4s
PASS  frontend tests               5.8s
PASS  UI e2e tests                 5.7s
PASS  rust fmt check               0.2s
PASS  rust clippy                  1.1s
PASS  rust tests                  37.2s
PASS  frontend build               4.7s

All checks passed.
```

### Test counts

| Suite | Tests | Time | Uses |
| --- | --- | --- | --- |
| `louver-core` unit | 227 | 1.2 s | — |
| `louver-desktop` unit | 3 | <0.1 s | — |
| `media_pipeline` | 7 | 12.5 s | real FFmpeg |
| `supervisor_recovery` | 9 | 0.9 s | real processes |
| `runtime_scheduling` | 16 | 0.05 s | virtual clock |
| `runtime_live` | 2 | 11.7 s | real FFmpeg + real runtime |
| `docs_sync` | 5 | <0.1 s | keeps README's error table honest |
| Frontend unit (Vitest) | 16 | 5.8 s | jsdom |
| UI e2e (Vitest) | 16 | 5.7 s | jsdom + mock backend |
| **Total** | **301** | | |

### The load-bearing measurement (§74)

Ten loop cycles, thirty file boundaries, three mutually different sources
normalized to one profile and stream-copied through one FFmpeg session:

- 0 timestamp faults, 0 DTS warnings, exit code 0
- 3600 video frames for 120.000s — none dropped or duplicated
- 0 duplicate timestamps
- 0 video stalls >100ms at any boundary
- A/V skew <1ms, and **not growing** from early to late

A hybrid mode (video copy + audio re-encode) was measured as an alternative and
was worse — 29 boundary discontinuities against 20 — so full `-c copy` is the
primary path and real-time transcoding remains only as the "호환 모드" fallback.

### Built and run

- `cargo build --release -p louver-desktop` → 7.5 MB stripped binary
- `npx tauri build --bundles deb` → `Louver Live_1.0.0_amd64.deb` (3.2 MB),
  containing `usr/bin/louver-desktop`, `usr/bin/ffmpeg`, `usr/bin/ffprobe`
- The release binary was **launched under Xvfb** and ran without crashing: it
  created its data directory, ran migrations (all 7 tables, schema version 1),
  wrote `app.log`, detected an encoder and rendered its UI.
  Screenshots: `docs/screenshots/first-run.png`, `docs/screenshots/dashboard.png`

### Licence tooling (§46)

Executed end to end: `keygen` → `issue` → `verify` returns VALID; a
hand-edited `edition` field returns INVALID (exit 1); a licence signed with a
different key returns INVALID; the private key is written `0600`; malformed
input produces a one-line message, not a panic.

---

## 3. Failed / skipped tests

Nothing is currently failing. These were **not run**, and why:

| Not run | Reason | How to run it |
| --- | --- | --- |
| Real YouTube RTMPS broadcast | No stream key available | Set `LOUVER_TEST_RTMPS_URL`, or enter a key in the app |
| Windows build and tests | No Windows machine | The CI workflow runs `verify` on `windows-latest` |
| macOS build and tests | No macOS machine | Same, on `macos-latest` |
| macOS Keychain / Windows Credential Manager | Those OSes only | Run the app on either platform |
| Sleep prevention, actually preventing sleep | Headless Linux container | Manual check on a laptop |
| Autostart entry creation | Needs a real login session | Toggle it in Settings on Windows/macOS |
| Hardware encoders (NVENC/QSV/AMF/VideoToolbox) | No such device present | Runs automatically when one exists |
| Soak longer than 3 minutes | Session time | `npm run soak -- --duration 24h` |
| `.msi` / `.dmg` bundles | Cross-compilation is not supported by Tauri | CI's `bundle` job |

The media tests **skip loudly** (printing `SKIP: no ffmpeg…`) rather than
passing when FFmpeg is absent.

---

## 4. Defects found and fixed during implementation

Recorded because each one is a case where writing the test changed the product.

| # | Defect | Found by | Fix |
| --- | --- | --- | --- |
| 1 | `apad` + `-shortest` in `filter_complex` generates audio forever and fills the disk (25 MB for a 3s clip before it was killed) | Manual pipeline validation | Audio is bounded by an explicit `-t` on a whole-frame boundary |
| 2 | `[0:a?]` is rejected inside a filtergraph label, so every source without audio failed to normalize | `media_pipeline` | Two separate graphs, chosen from the probe result |
| 3 | The live command had no `-y`; on reconnect FFmpeg hit the overwrite prompt and, with `-nostdin`, died instantly — **every retry failed, so a dry run could never recover** | `runtime_live` | `-y` added to `build_stream_args`, with a regression test |
| 4 | Encoder selection trusted `ffmpeg -encoders`, which lists compiled-in encoders; the packaged app picked `h264_nvenc` on a machine with no NVIDIA GPU | Running the release binary | `detect_encoder` proves each candidate by encoding one frame |
| 5 | Stream copy emits no `frame=` field, so a frame-based health check reads a healthy broadcast as dead | `runtime_live` | Liveness is byte-based; documented and tested |
| 6 | `snapped_duration` rounded to nearest, so it could request a frame the source lacks — a held frame at every loop seam | Unit test | Rounds down |
| 7 | Dashboard showed "0개 영상" — `??` does not fall back on `0` | Running the app | Reads the playlist when idle |
| 8 | The last-used playlist was never restored; the store read a field the backend never sent | Running the app | `active_playlist` added to `SettingsView` |
| 9 | Playlist page rendered two "add video" buttons when empty | UI e2e | Footer hidden when the list is empty |
| 10 | A stale FFmpeg exit arriving after a completed stop was treated as a failure | Unit test | Already-stopped is a no-op |
| 11 | Soak harness ran unpaced, reporting 145% CPU and 190 Mbps — measuring the disk, not a broadcast | Reviewing the numbers | `-re` added, plus a `paced_like_a_broadcast` verdict |
| 12 | Leak detection counted FFmpeg's buffer warm-up as growth (9.5%) | Reviewing the numbers | Steady-state (second half) growth, plus a 24h projection |
| 13 | `license-generator` panicked with a stack trace on bad input, and its positional argument picked up the subcommand | Manual CLI run | Typed `CliResult`, one-line messages |

---

## 5. Performance measurements

Full detail and method in [BENCHMARK.md](BENCHMARK.md). Headlines:

| Metric | Measured | Condition |
| --- | --- | --- |
| FFmpeg CPU, live | **1.50 % min / 1.68 % mean / 2.30 % peak** | 1080p30 stream copy, paced with `-re`, 180s |
| FFmpeg RSS | 61.7 MB, flat after warm-up | same |
| Steady-state memory growth | **0.00 %** over 90s → 0 % projected at 24h | same |
| Timestamp errors | 0 | same |
| Normalization throughput | 3.4× realtime (libx264 veryfast, 4 cores) | 60s of 720p25 → 1080p30 |
| Cache size | 4.64 GB/hour | 1080p30, measured output |
| Recovery after `SIGKILL` | 2/2 recovered, 0 errors | soak with `--kill-every` |
| Release binary | 7.5 MB stripped | |
| Linux bundle | 3.2 MB `.deb` | |

**Not measured:** macOS and Windows, hardware encoders, real YouTube ingest,
runs longer than 3 minutes, idle app CPU/RAM with a display attached, and the
minimum hardware spec. The 24-hour claim in §65 is *projected* from a flat
90-second steady state, not observed.

---

## 6. Known limitations

1. **Real YouTube broadcasting is unverified.** The RTMPS path is implemented
   and the identical pipeline is proven against a file sink, but nothing has
   been sent to YouTube. This is the single largest gap.
2. **Windows and macOS are unverified at runtime.** They compile; the keychain,
   sleep prevention, autostart and tray behaviour were not executed there.
3. **The bundled FFmpeg here is a development fallback.** The download hosts
   were unreachable from this environment, so `fetch-ffmpeg.mjs` copied the
   system FFmpeg (dynamically linked, 342 KB). A release must use
   `--require-download`. See [LICENSES.md](LICENSES.md), which also flags that
   the intended builds are **GPL v3** and that this needs legal review before
   commercial distribution.
4. **There is no in-app licence check, by design.** The Ed25519 gate this
   report described was removed: it refused a broadcast unless a signed
   `license.json` was installed, and no release build ever carried a real
   public key, so it refused every broadcast. Who may use Louver Live is
   decided before the installer changes hands.
5. **The updater is designed, not deployed.** `tauri.conf.json` has
   `updater.active: false` and a placeholder pubkey. §48 asks for the structure
   only, which exists.
6. **Video preview is not implemented.** §43 makes it optional; it was left out
   rather than added late and untested.
7. **No speed test.** §29 forbids inventing an Mbps figure, so CHECK 7 reports
   reachability only. `SpeedTestProvider` is the seam for a real one.
8. **Device binding is off by default.** Implemented and configurable; not
   enforced unless `enforce_device_binding` is set.
9. **Dev-tooling npm advisories.** `npm audit` reports issues in esbuild/vite
   reachable only through the dev server and test runner. No shipped code is
   affected; upgrading means a Vitest major bump.
10. **`start_minimized` hides the window but the tray needs a session bus.**
    On the headless container the tray warned about D-Bus; on a real desktop it
    is fine, but this was not confirmed on Windows/macOS.
11. **Intel macOS is untested.** The architecture resolver handles
    `x86_64-apple-darwin`, but no Intel Mac was available.

---

## 7. Next steps

In the order that retires the most risk:

1. **Run one real broadcast.** Enter a stream key, start a broadcast, watch it
   on YouTube for an hour. Then pull the network cable and confirm the
   reconnect. This is the only remaining unknown in the core product.
2. **Run `verify` on Windows and macOS.** The CI workflow already does this;
   pushing to a repository with Actions enabled is the whole step.
3. **Soak for 24 hours** on a target-class machine and replace the projected
   figure in BENCHMARK.md with an observed one.
4. **Settle the FFmpeg licence.** Either accept GPL v3 with the obligations in
   LICENSES.md, or build an LGPL FFmpeg without libx264 and re-measure
   normalization throughput.
5. **Generate the production signing keys** — the licence keypair and the Tauri
   updater keypair — and wire them into CI as secrets.
6. **Validate the minimum spec** on an actual i5 / 8 GB machine and correct the
   README, which currently states a starting point rather than a measurement.
7. **Code-sign and notarize** for macOS and Windows, or users will see
   warnings on first launch.
