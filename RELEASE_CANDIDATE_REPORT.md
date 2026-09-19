# Release Candidate Report — Louver Live v1.0.0

**Verdict: NOT a Release Candidate yet.** Four blockers remain, three of which
cannot be cleared from this machine. Details in *Release Blockers*.

Everything below that says PASS was executed and its output observed. Anything
not executed says **NOT TESTED**, never PASS.

---

## 1. Environment

| | |
| --- | --- |
| Build version | 1.0.0 (`9b545a1`) |
| Host OS | **Ubuntu 24.04 (Linux 6.18) — not macOS, not Windows** |
| CPU | Intel Xeon @ 2.10 GHz, 4 cores |
| RAM | 15 GiB |
| Node / Rust | 22.22.2 / 1.94.1 |
| FFmpeg | 6.1.1-3ubuntu5, GPL-2.0-or-later, **dynamically linked (215 libs)** |
| Ingest endpoint | Local RTMP server (real protocol, real socket) — **not YouTube** |
| Date | 2026-09-19 |

The two facts that shape this whole report: **the development machine is
Linux**, so every macOS and Windows item is untested; and **no YouTube stream
key was available**, so every broadcast test ran against a local RTMP endpoint
instead.

The local endpoint is a real RTMP server, not a file: the app performs the
full RTMP handshake over a TCP socket and the stream is analysed at the
*receiving* end. That is much closer to YouTube than a file sink, but it is
not YouTube, and this report never claims otherwise.

---

## 2. PASS / FAIL summary

| # | Condition (§21) | Result |
| --- | --- | --- |
| 1 | `npm run verify` | **PASS** |
| 2 | Real macOS packaged application | **NOT TESTED** — no macOS machine |
| 3 | Real YouTube Live, 30 minutes | **NOT TESTED** — no stream key. 30 min over real RTMP: PASS |
| 4 | Playlist loop | **PASS** — 21 boundaries over 4.3 loops, all clean |
| 5 | Network failure recovery | **PASS** — 60s packet blackhole |
| 6 | FFmpeg crash recovery | **PASS** — 3 kills, 3 recoveries |
| 7 | Application restart recovery | **PASS** — real processes, real RTMP |
| 8 | Machine restart + autostart + schedule recovery | **PARTIAL** — scheduler fires at launch; OS autostart and reboot NOT TESTED |
| 9 | 6-hour soak | *(see §9 below)* |
| 10 | FFmpeg distribution / licence | **BLOCKER** — decision documented, not executed |
| 11 | Production licence verification | **PASS** |
| 12 | Stream key security | **PASS** |

---

## 3. Build and static verification

`npm run verify`, 10 of 10 steps:

```
PASS  ffmpeg sidecar               PASS  rust fmt check
PASS  secret scan                  PASS  rust clippy
PASS  frontend typecheck           PASS  rust tests
PASS  frontend lint                PASS  frontend build
PASS  frontend tests               PASS  UI e2e tests
```

| Suite | Tests | Uses |
| --- | --- | --- |
| `louver-core` unit | 244 | — |
| `louver-desktop` unit | 3 | — |
| media pipeline | 9 | real FFmpeg |
| supervisor / recovery | 9 | real processes |
| runtime scheduling | 16 | virtual clock |
| runtime live | 2 | real FFmpeg |
| RC security | 7 | real FFmpeg |
| RC licence | 10 | real Ed25519 keypair |
| docs sync | 5 | — |
| frontend unit | 16 | jsdom |
| UI e2e | 18 | jsdom + in-memory backend |
| **Total, automatic** | **339** | |
| RC harness (`--ignored`, run by hand) | 4 | real RTMP endpoint |

Release build: `louver-desktop` 7.5 MB stripped; Linux `.deb` bundles with
both sidecars. macOS and Windows bundles **NOT BUILT** — Tauri does not
cross-compile.

---

## 4. YouTube / RTMP broadcast test (§5)

**Real YouTube: NOT TESTED.** The harness is ready and needs only a key.

Against a real local RTMP endpoint, 5 source videos of deliberately different
lengths and formats (95s/1280×720/25fps, 62s/1920×1080/30fps,
128s/854×480/24fps/mono, 47s/1920×1080/60fps, 83s/640×360/15fps), Sequential,
1080p30:

**30 minutes, completed 13:01.** `rc-results/run2/rc-30min-summary.json`.

| Metric | Measured |
| --- | --- |
| Duration | 1800 s |
| Playlist loops completed | **4.3** |
| Publisher sessions at the ingest | **1** — one unbroken RTMP connection for the whole 30 minutes |
| Connect time | 1.0 s |
| Reconnects / restarts | **0 / 0** |
| Ticks not LIVE | **0** |
| Data sent | 1.18 GB |
| Throughput | 5.25 Mbps |
| FFmpeg CPU | mean **0.57 %**, peak **0.63 %** |
| FFmpeg memory | 59.7 → 68.1 MB; steady-state growth **0.00 %** |
| Runtime memory | 10.86 → 10.89 MB; steady-state growth **0.00 %** |
| FFmpeg errors | **0** |
| State transitions | `CONNECTING → LIVE (1s) → STOPPED` — nothing else |

Throughput is 5.25 Mbps rather than the 10 Mbps profile cap because the
fixtures are synthetic test patterns, which compress far below it. Real music
video would sit near the cap. This affects network throughput only: stream
copy does the same work per byte whatever the bytes contain.

---

## 5. Playlist boundary test (§6)

Measured on the stream the ingest **received**, not on the sender's own view.
**21 boundaries across 4.3 cycles** of the 30-minute run, covering every
transition including the loop seam (rc_05 → rc_01). An earlier 24.4-minute
capture gave the same result across 17 boundaries.

| Check | Result |
| --- | --- |
| Freeze (화면 멈춤) | **0** — worst inter-frame gap at a seam 55 ms (< 2 frames) |
| Black frame (검은 프레임) | **0** — luminance 92–131 at every seam |
| Audio dropout (순간적인 음소거) | **0** — peak −20.7 to −21.1 dB at every seam |
| Audio gap | 22 ms at every seam — exactly one AAC frame, inaudible |
| Timestamp jump | **0** duplicate, **0** backwards |
| Buffering / stream restart | **0** — one RTMP session for all 30 minutes |
| Frame count | 54 028 vs 54 040 expected (0.02 %) |
| A/V sync | 6 ms → 4 ms over 30 minutes, max 16 ms — **not accumulating** |
| Keyframe interval | max 2.02 s (YouTube requires ≤ 4 s) |

Every boundary was clean. Raw data: `rc-results/boundary-analysis.json`.
Reproduce with `npm run rc:boundaries -- <captured.flv>`.

---

## 6. Network failure test (§7)

A genuine 60-second packet blackhole (`iptables -j DROP` on the ingest port),
not a stopped server. This distinction found a real bug — see *Defects*.

| Observation | Result |
| --- | --- |
| LIVE → RECONNECTING | at 35 s (30 s stall timeout + tick) |
| Reconnect attempts during the outage | 3, spaced **2 s / 5 s / 10 s** — the documented backoff |
| App crash | none |
| UI answerable during the outage | yes, 114 consecutive status reads |
| Recovery after the network returned | **4 s**, on a new FFmpeg process |
| Zombie FFmpeg | none |
| User stop still honoured afterwards | yes — 20 ticks, no restart |

Transitions: `LIVE → RECONNECTING(35s) → CONNECTING(+2s) → RECONNECTING →
CONNECTING(+5s) → RECONNECTING → CONNECTING(+10s) → LIVE`.

---

## 7. FFmpeg crash test (§8)

Three consecutive `SIGKILL`s of the live FFmpeg during a real RTMP broadcast:

| Round | Killed | Recovered on |
| --- | --- | --- |
| 1 | 18383 | 18395 |
| 2 | 18395 | 18404 |
| 3 | 18404 | 18416 |

- Every kill produced `LIVE → RECONNECTING → LIVE`
- `restart_count` reached 3; `reconnect_count` returned to 0 after each success,
  so the backoff does not escalate across unrelated faults
- An explicit Stop afterwards was **not** undone (40 ticks, no restart)
- No zombie process from any round

---

## 8. Application restart recovery (§9)

A broadcast running over real RTMP, the owning process dropped without a clean
stop, then a fresh runtime started on the same data directory.

| Stage | Result |
| --- | --- |
| Crash evidence left behind | `session.json` still `LIVE`, dangling DB row, orphan FFmpeg 17145 alive |
| Orphan cleanup | FFmpeg 17145 killed after a name check, not on pid alone |
| Crash detected and explained | `이전 방송이 비정상 종료되었습니다. 예약 시간 내이므로 방송을 재개합니다.` |
| Resumed inside the window | yes, on FFmpeg 17161 |
| Play order replayed | yes — seed 4242 preserved |
| Restarted **outside** the window | did **not** broadcast; stale state cleared |

---

## 9. Machine restart (§10)

**PARTIAL — the reboot itself was NOT TESTED**, and cannot be from this
container.

What was verified, using the real packaged binary: with a schedule covering
the current time, launching the application fresh causes the scheduler to fire
at startup and attempt the broadcast. It stopped at the only remaining step —
the stream key — logging:

```
[LL-STREAM-007] 스트림 키가 없습니다. 설정에서 YouTube 스트림 키를 입력해주세요.
```

So the chain *app launch → scheduler evaluates → inside window → broadcast
start attempted* is proven in the packaged application. What remains untested
is the OS-level autostart entry and an actual reboot. Both are in
`MACOS_QA.md` and `WINDOWS_QA.md`.

---

## 10. Long-run soak (§11)

<!-- LONG-RUN:BEGIN -->
*(No long run has completed yet. Written here from the measurement when one
does; NOT TESTED until then.)*
<!-- LONG-RUN:END -->

---

## 11. Stream key security (§12, §4) — PASS

Seven automated checks, run against a **real broadcast attempt** with a
realistic YouTube-shaped key (`a1b2-c3d4-e5f6-g7h8-i9j0`), auditing every
surface afterwards:

| Surface | Result |
| --- | --- |
| Log files (`app`/`stream`/`ffmpeg`) | clean — FFmpeg's own stderr containing the publish URL arrives masked |
| SQLite database | clean; no settings row holds a key by design |
| `stream_events` table | clean |
| `session.json` | clean |
| Status payloads sent to the UI | clean |
| Error message and its technical detail | clean |
| Serialized / `Display` / `Debug` error forms (crash reports) | clean |
| UI display | masked; the real value only after an explicit confirmation |

`npm run secret-scan` scans the working tree **and the full git history**, and
is wired into `npm run verify`. It was proved to fire by planting a key and a
PEM block, which it caught; the repository itself is clean.

---

## 12. Production licence verification (§16) — PASS

| Case | Result |
| --- | --- |
| Valid licence | accepted |
| Modified licence (7 fields, individually) | rejected, `LL-LICENSE-002` |
| Expired licence | rejected, `LL-LICENSE-004`; a future expiry and a perpetual licence both pass |
| Wrong signature (forged key, corrupted bytes, malformed base64) | rejected |
| Missing licence | `LL-LICENSE-001`, broadcasting blocked |
| Device binding | enforced only when configured; `LL-LICENSE-005` on mismatch |
| Corrupt licence file | never falls back to a development licence |
| DEV_LICENSE in a release build | compiled out (`cargo test --release` asserts it) |
| Public key in source | still the placeholder; a real key must come from `LOUVER_LICENSE_PUBLIC_KEY` |

End to end with a real keypair: `keygen` → `issue` → build with the public key
embedded → `verify-build` accepted the genuine licence and rejected a
hand-edited one. The private key is written `0600` and never enters the
repository.

---

## 13. CPU and memory

All from the local RTMP runs; see §10 for the long-run figures.

| Metric | Measured |
| --- | --- |
| FFmpeg CPU, 1080p30 stream copy | mean **0.57 %**, peak **0.63 %** over 30 minutes |
| FFmpeg memory | 59.7 → 68.1 MB; steady-state growth 0.00 % |
| Broadcast runtime memory | 10.86 → 10.89 MB; steady-state growth 0.00 % |
| Desktop shell (Tauri + webview), idle | **173.6 MB, 0.1 % CPU, 30 threads — completely flat over 40 minutes** |
| Normalization | 3.4× realtime (libx264 veryfast, 4 cores, software only) |

`STREAM COPY` is now verifiable rather than assumed: Developer Mode reports the
**actual argv** of the running process, flags any mismatch with the configured
mode, and shows the masked command on request (§13).

---

## 14. Defects found and fixed during RC verification

| # | Defect | Found by | Severity |
| --- | --- | --- | --- |
| 1 | **Stall detection was dead code.** The runtime refreshed the timer from a sticky "has ever produced output" flag, so `is_stalled()` could never fire. A packet blackhole blocks FFmpeg rather than killing it, so the app stayed `LIVE` with nothing reaching YouTube and never reconnected. | 60 s iptables blackhole | **Critical** |
| 2 | Every FFmpeg failure reported one generic code. Now classified from real FFmpeg 6.1 output into network / key-rejected / storage / missing-file codes, including a new `LL-STREAM-008`. | §18 review | Major |
| 3 | `is_hardware_encoder` was `!= "libx264"`, so any other software encoder would be labelled hardware-accelerated in Settings. | LGPL feasibility analysis | Minor |
| 4 | The software encoder fallback hard-coded `libx264`, so an LGPL FFmpeg build would have had no encoder at all. | LGPL feasibility analysis | Major (blocks Option B) |

Test-harness defects, fixed so the results can be trusted: an RTMP sink that
respawned in a tight loop when the port was held; a sink that leaked its child
listener on shutdown; an outage test that produced a **false PASS** because the
server came straight back; a soak that ran unpaced and measured the disk.

---

## 15. Known issues

1. **Real YouTube broadcasting is unverified.** Everything up to the socket is
   proven over real RTMP, but nothing has been sent to YouTube.
2. **macOS and Windows are unverified at runtime.** Keychain, Credential
   Manager, sleep prevention, autostart and tray have never executed.
3. **The bundled FFmpeg is a development fallback** — GPL and dynamically
   linked against 215 libraries. It would not run on a user's machine.
4. **The licence public key is a placeholder.**
5. **The updater is designed, not deployed** (`updater.active: false`).
6. No video preview (§43, optional). No speed test (§29 forbids inventing one).
7. Dev-tooling npm advisories (esbuild/vite) reachable only through the dev
   server and test runner; no shipped code is affected.
8. The minimum hardware spec in README is a starting point, not a measurement.

---

## 16. Release blockers

Ordered by what retires the most risk.

1. **Run one real YouTube broadcast.** Enter a key, broadcast to a private or
   unlisted stream for 30 minutes, then pull the network cable. The harness
   needs no code change:
   ```bash
   npm run soak -- --duration 30m \
     --destination rtmps://a.rtmps.youtube.com/live2 --stream-key <key>
   ```
2. **Verify on macOS and Windows.** Work through `MACOS_QA.md` and
   `WINDOWS_QA.md` on real hardware. The keychain paths are the highest risk:
   they have never run anywhere.
3. **Settle the FFmpeg distribution.** `LICENSES.md` documents both options
   with the trade-offs and recommends GPL v3 for direct download. It needs
   sign-off from someone qualified, then:
   ```bash
   node scripts/fetch-ffmpeg.mjs --require-download --force
   node scripts/ffmpeg-manifest.mjs --check   # must pass
   ```
   Note GPL v3 is generally read as incompatible with the Mac App Store.
4. **Generate the production keys** — the Ed25519 licence keypair and the Tauri
   updater keypair — and add them to CI as secrets. The private licence key
   stays offline.

Not blockers, but do them before shipping: code-sign and notarize both
platforms, and run a 24-hour soak on target-class hardware.

---

## 17. What was deliberately not done

No new features were added during this phase. The stream-copy architecture,
scheduler, supervisor, reconnect and session recovery are unchanged except for
the four bug fixes above. Nothing from the §20 exclusion list — OAuth, AI,
thumbnails, titles, other platforms, scenes, camera, overlay, multi-stream,
cloud — was implemented or begun.

Two additions were made because §13 and §18 asked for them: the live-command
diagnostic panel, and the FFmpeg error classifier.
