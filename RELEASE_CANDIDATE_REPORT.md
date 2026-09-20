# Release Candidate Report — Louver Live v1.0.0

**Verdict: NOT a Release Candidate yet.** Four blockers remain, three of which
cannot be cleared from this machine. Details in *Release Blockers*.

Everything below that says PASS was executed and its output observed. Anything
not executed says **NOT TESTED**, never PASS.

## What would make this a Release Candidate

The rule, stated in advance so the verdict is not an opinion:

**A. Must be true — each one is a blocker on its own.**

| | Condition | Today |
| --- | --- | --- |
| A1 | `npm run verify` passes on the release commit | **PASS** |
| A2 | The FFmpeg sidecars are licence-cleared, static, and pass `ffmpeg-manifest.mjs --check` | **BLOCKER** — only the development fallback exists here |
| A3 | One real YouTube broadcast of 30 minutes or more, with a recovered interruption | **NOT TESTED** — needs a stream key |
| A4 | The packaged app runs on real macOS, through `MACOS_RELEASE_TEST.md` | **NOT TESTED** — no macOS machine |
| A5 | The packaged app runs on real Windows 10/11 x64 | **NOT TESTED** — no Windows machine |
| A6 | A production licence key exists, and a release build rejects a licence signed by any other key | **NOT TESTED** — no production key, by design (see `SIGNING.md`) |
| A7 | The stream key appears in no log, database, crash report or UI outside its own masked field | **PASS** |
| A8 | A long unattended run holds the broadcast with no leak, no unrecovered failure and no orphan process | **PASS** — 8h 52m, 76.9 playlist loops, 0 reconnects, FFmpeg memory unchanged, A/V skew 20 ms at both ends |

**B. Must be recorded, not necessarily cleared.**

Code signing and notarization, a 24-hour run, and hardware-encoder figures may
be absent at RC as long as they are stated as absent. They are not blockers;
pretending they were measured would be.

**C. The disqualifier.**

Any figure in this report that was not produced by running something. A single
estimated number invalidates the report, whatever the other rows say.

On that rule: **A2 and A3–A6 are open, so this is not yet a Release
Candidate.** What is open is open for lack of a machine, a key or a signature —
not for lack of a working product on the platform where it has been measured.

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
| 9 | Long unattended soak | **PASS** — 8h 52m continuous, 0 reconnects, 0 leak; see §10 |
| 10 | FFmpeg distribution / licence | **BLOCKER** — decision documented and gated in CI, not yet executed |
| 11 | Production licence verification | **PASS** — rules proved on four real cases; the production key itself is **NOT GENERATED** (see §16.4) |
| 12 | Stream key security | **PASS** |
| 13 | macOS installer | **BUILD READY** — `.github/workflows/release.yml`, never run on macOS hardware |
| 14 | Windows installer | **BUILD READY** — same workflow, never run on Windows hardware |
| 15 | YouTube metadata and live chat (V1 by decision, §17) | **NOT TESTED** against real YouTube — see §16 |

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

## 14b. Defects found in real use on macOS + real YouTube (2026-09-20)

Two release blockers reported from a real Mac against a real YouTube channel.
Both are fixed; each has a regression test that fails against the old code.

### A. A scheduled window was skipped, and the app reported tomorrow

Reported: a `17:14 → 17:40` daily schedule; at ~17:17 the app was not
broadcasting, the schedule read `다음 방송 2026-09-21 17:14`, and the dashboard
sat at `PREPARING · FFmpeg Stopped · playlist: night · 0개 영상`.

| | Cause |
| --- | --- |
| Skipped window | `BroadcastRuntime::start` called `supervisor.begin()` — which moves the machine to PREPARING — *before* `build_plan`, the stream-key lookup, `create_session` and `spawn_now`. Any failure after that point returned early and left the machine in PREPARING. `StreamState::is_active()` counts PREPARING as active, and `tick_scheduler` returns early while the runtime is active. One failed start therefore locked the scheduler out for the rest of the window |
| "tomorrow" | The schedule list rendered `next_occurrence` only, which by definition begins *after* now. Inside an open window that is the next day. The row had no way to say "this window is open" |
| `0개 영상` | The dashboard reads `status.item_count` whenever the state looks live, and PREPARING looks live. `item_count` comes from `self.plan`, which the failed start never set. The name came from the separately-held UI selection, so the two disagreed |
| `PREPARING` forever | Same single cause as the skipped window — nothing moved the machine out |

Not a cause: stale playlist ids. `schedules.playlist_id` is
`REFERENCES playlists(id) ON DELETE CASCADE` with `PRAGMA foreign_keys=ON`, so
deleting a playlist deletes its schedules; re-creating one with the same name
gets a new id and no schedule. There is no name matching anywhere in the
scheduler. `deleting_a_playlist_takes_its_schedules_with_it` pins this.

**Fixed by** rolling a failed start back to ERROR (`StreamSupervisor::abort`),
retrying the window once a minute rather than abandoning it, new codes
`LL-SCHED-003` / `LL-SCHED-004` / `LL-STREAM-009`, `last_start_error` on
`RuntimeStatus`, and an active-window row in the schedule list.

**Verified in the real app** under Xvfb: with the app closed across the start
of an `08:55 → 09:03` window, launching at 08:57 attempted the start at once,
failed on the missing stream key, showed `STATUS ERROR` with
`LL-STREAM-007` (not PREPARING), and the schedule row read
`지금 방송 시간입니다 · 2026-09-20 09:03에 종료`. Entering the key led to a retry
60 s later that launched FFmpeg with `예약 시작` and `예약 종료까지 00:03:52`.

### B. The broadcast went live under YouTube's own title

Reported: title, description, six tags, category 음악 and 일부공개 were set in
the app, and the live broadcast started as "Playlist".

| | Cause |
| --- | --- |
| Applied too late | Metadata was pushed from the one-second broadcast tick, gated on `rt.state() == Live`. By then FFmpeg had connected and YouTube was already live under whatever the resource said |
| Applied never, silently | The same tick returned early when no account was connected, while 방송 설정 promised "방송이 시작되면 … 적용합니다". A stream-key-only build can never change a title over RTMPS |
| The old title written back | `update_video_snippet` read the *video* snippet and wrote it back with only `tags` and `categoryId` replaced. The watch page and Studio read the video, not the broadcast, so this put the stale title — the channel's default, "Playlist" — back over the one `liveBroadcasts.update` had just set |
| Never checked | A 2xx was treated as success. Nothing read the resource back |

**Fixed by** a `PreStartHook` on `BroadcastRuntime::start`, which both the
manual button and the scheduler go through, running before `spawn_now`;
`merge_metadata_into_snippet`, which writes title and description as well as
tags and category while keeping `defaultLanguage` and everything else;
`verify_metadata`, which re-reads the video and the broadcast and compares
five fields; a start-time choice (`다시 시도` / `설정 없이 방송 시작`) instead of a
silent start; and a per-field panel on the dashboard.

| Requirement | Evidence |
| --- | --- |
| §B-1 stream and metadata reported separately | `MetadataApplyState` is independent of `SupervisorStatus`; seen on screen reading `not_connected` while the stream was RECONNECTING |
| §B-2 no false promise without an account | e2e *does not claim settings will be applied when no account is connected* |
| §B-3 the user chooses | e2e *asks before going live under whatever title YouTube already has* |
| §B-4 metadata before FFmpeg | `a_manual_start_runs_the_pre_start_work_before_ffmpeg`, and `the_stream_does_not_start_when_the_metadata_could_not_be_applied` (launch count 0) |
| §B-5 read-back | `a_two_hundred_is_not_evidence_that_the_title_changed` |
| §B-6 tags merge safety | `the_video_write_carries_the_new_title_and_keeps_what_the_user_did_not_choose` |
| §B-7 one lifecycle | `a_scheduled_start_runs_exactly_the_same_pre_start_work` |
| §B-8 fresh `activeLiveChatId` | `ChatBot::start` resolves it from the pinned broadcast id every time; `start_bot_for` stops a bot pinned to a different one |
| §B-9 no silent default | `the_user_can_choose_to_broadcast_without_the_youtube_settings` and the two schedule-policy tests |
| §B-10 per-field display | e2e *reports each field from what YouTube says afterwards* |

**A metadata refusal is not an engine failure.** The first cut of the fix
rolled every declined start back through the same path, so a YouTube account
that was not connected turned the whole dashboard red — a `PREPARING → ERROR`
that said the broadcast was broken when nothing about it was. `start` now
tells the two apart: a stream failure (no playlist, no key, FFmpeg would not
spawn, RTMPS refused) goes to ERROR as before, while the pre-start work
declining cancels the attempt and leaves the runtime IDLE. The YouTube half
carries its own state next to it — `조치 필요`, `적용 안 함`, `적용 실패` — and the
dialog offers [취소] [YouTube 연결] [설정 없이 방송 시작], because connecting an
account is what actually fixes it and pressing the same button again is not.

**Unattended windows.** A scheduled start has nobody to ask. The default is
the same as a cancelled manual start — hold, do not broadcast under settings
the user did not choose — and 방송 설정 carries a switch for a 24/7 channel that
would rather stay on air and have the failure recorded instead.

### The cost rule

Louver Live's YouTube integration must not put a bill on anyone's Google Cloud
account. Half of that is configuration and half is code, and the two halves
protect different things.

Configuration carries the guarantee: the project has **no billing account
linked**, so there is nothing to charge against and a request past the free
quota is refused rather than billed. No pay-as-you-go, no quota-increase
request, and YouTube Data API v3 is the only Google service enabled.
`YOUTUBE_OAUTH_PRODUCTION.md` §0 has the click path and the check.

Code protects the *broadcast*: a run that burns the day's 10,000 units in an
hour leaves the rest of the day with no metadata and no chat.
`youtube/quota.rs` counts the day's spending and stops before the end of it.

| Decision | Why |
| --- | --- |
| The meter is in the transport, not the call sites | `MeteredClient` wraps `HttpClient`, and it is the only path from this app to the Data API. The chat bot runs on its own thread and builds its own client — metering that separately is the kind of thing that gets missed |
| Costs are deliberate over-estimates | Google's per-method table could not be fetched from this machine (`developers.google.com` is unreachable here), so it is read 1 / write 50 / search 100 with **anything unrecognised charged as a write**. Guessing low ends in a 403 mid-apply; guessing high stops early. Only one of those is safe |
| 500 units held in reserve | The estimate is not trustworthy at the very end, so the app stops on its own terms with a message rather than on Google's 403 |
| Google's `quotaExceeded` latches the day shut | Whatever the local count says. The estimate can be wrong; Google's answer cannot |
| The ledger survives a restart | A relaunch that started the count at zero would spend an allowance already gone |
| Running out never stops the stream | Unlike every other refusal in the pre-start hook, quota returns `Ok`: the broadcast starts and the optional half waits for the reset. It is not something the user can fix, and it is not a reason to take a 24/7 channel off air |

A realistic day — one metadata apply plus a chat message every 20 minutes —
spends **3,704 units**, under 40% of the allowance
(`a_days_realistic_use_fits_comfortably`).

**NOT TESTED:** everything above was checked against a stand-in for the
YouTube API and in the real app under Xvfb. TEST 1–5 against a real Google
account and a real live broadcast — read-back of the real `videos.update`, the
real `activeLiveChatId` rotation, the real OAuth failure path, and the absence
of a billing account on the real Cloud project — have not been run here and
remain **NOT TESTED**. `rc-results/youtube-test.md` is where
those results go.

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

## 16. YouTube metadata and live chat — verification status

Everything below the line was executed. Everything above it needs a Google
account and a live broadcast, which this machine does not have.

| Check | Result |
| --- | --- |
| Metadata limits: 100-character title, 5000-character description, 500-character tag budget | **PASS** — unit tests, counting characters not bytes, so a 100-character Korean title is legal |
| A spaced tag costs two extra characters, as YouTube quotes it | **PASS** — unit test, and visible in the live UI (`lofi, work music, jazz` reads 22/500) |
| `videos.update` merges instead of replacing | **PASS** — asserted on the request body sent over a socket: title, description, categoryId, defaultLanguage and thumbnails all survive a tag change |
| YouTube error reasons map to user-facing codes | **PASS** — five failure bodies, each reaching the right `LL-CHAT-*` / `LL-YOUTUBE-*` code |
| Chat order, interval floor, no-repeat, rotation reset on a new broadcast | **PASS** — unit tests |
| OAuth token caching, refresh-token rotation, disconnect | **PASS** — unit tests against a stand-in token endpoint |
| Tokens absent from SQLite, logs and the UI | **PASS** — `rc_security`, the secret scanner, and a UI test asserting no token-shaped text renders |
| Broadcast engine unaffected | **PASS** — `npm run verify` green; the YouTube module is unreachable from the broadcast tick |
| — | — |
| **Real Google OAuth consent** | **NOT TESTED** |
| **Real title / description / tag / category change on a live broadcast** | **NOT TESTED** |
| **Real live chat messages, in order** | **NOT TESTED** |
| **Stop → Start with no stale `liveChatId`** | **NOT TESTED** |

`YOUTUBE_SETUP.md` has the Google Cloud setup and the twelve-step procedure.
Results go in `rc-results/youtube-test.md`; until they do, the four rows above
stay NOT TESTED and no claim is made about them.

---

## 17. Release blockers

Ordered by what retires the most risk.

1. **Run one real YouTube broadcast.** Enter a key in the app's settings
   screen, broadcast to a private or unlisted stream for 30 minutes, then pull
   the network cable. **No code change is needed** — the same harness that ran
   every figure in this report points at YouTube through two environment
   variables:
   ```bash
   LOUVER_TEST_RTMPS_URL=rtmps://a.rtmps.youtube.com/live2 \
   LOUVER_TEST_STREAM_KEY=<your key> \
   LOUVER_RC_DURATION_SECS=1800 \
     cargo test -p louver-core --test rc_live -- --ignored --nocapture rc_broadcast
   ```
   The key is read from the environment straight into the in-memory secret
   store; it is masked in every log line and never written to the CSV, the
   summary or the database. `crates/louver-core/tests/rc_security.rs` asserts
   that against a real broadcast attempt.
2. **Verify on macOS and Windows.** `MACOS_RELEASE_TEST.md` is the ordered
   eighteen-step path that decides whether a build ships; `MACOS_QA.md` and
   `WINDOWS_QA.md` are the wider checklists. The keychain paths are the highest
   risk: they have never run anywhere.
3. **Settle the FFmpeg distribution.** `LICENSES.md` documents both options
   with the trade-offs and recommends GPL v3 for direct download. It needs
   sign-off from someone qualified, then:
   ```bash
   node scripts/fetch-ffmpeg.mjs --require-download --force
   node scripts/ffmpeg-manifest.mjs --check   # must pass
   ```
   The check is wired into `.github/workflows/release.yml` ahead of the build,
   so an unfit binary cannot reach an artifact. It currently **fails** here, as
   it should: this machine has only the dynamically linked development
   fallback. Note GPL v3 is generally read as incompatible with the Mac App
   Store.
4. **Generate the production keys** — the Ed25519 licence keypair and the Tauri
   updater keypair — and add them to CI as secrets. `SIGNING.md` has the
   procedure. The private licence key is generated on your own machine and
   stays offline; this repository neither creates nor stores one, and
   `npm run secret-scan` fails the build if one appears. Until then
   `license-generator verify-build` correctly reports that the build carries
   the placeholder public key.

Not blockers, but do them before shipping: code-sign and notarize both
platforms, and run a 24-hour soak on target-class hardware.

---

## 18. Scope: what is in, and what stays out

The stream-copy architecture, scheduler, supervisor, reconnect and session
recovery are unchanged. Two additions were made during RC verification because
§13 and §18 asked for them: the live-command diagnostic panel, and the FFmpeg
error classifier.

### YouTube OAuth is in V1, but off the default path

§20 originally listed **YouTube OAuth, 썸네일 자동 변경 and 제목 자동 변경** as
excluded from V1. The product owner reversed that on 2026-09-20 and asked for
broadcast metadata and automatic live chat to be built. This report records the
decision rather than the original list.

Later the same day the owner set the shape that decision has to take: the
**default product is three steps — 영상 추가 → 스트림 키 입력 → 방송 시작** — and
OAuth is an optional extra behind a button, not a step. What that means in the
build, and what was checked:

| Requirement | Where it is enforced | Evidence |
| --- | --- | --- |
| The broadcast path contains no OAuth | `streaming/preflight.rs`, `runtime.rs`, `commands/streaming.rs` | No `youtube::` import and no OAuth symbol in any of the three; the only "youtube" string in preflight is the default RTMPS host |
| A first run never meets Google | First-run wizard steps are 소개 · 스트림 키 · 송출 품질 · 자동 시작 · 완료 | e2e `first run` |
| Settings leads with the stream key and marks YouTube optional | `SettingsPage.tsx` — `기본 송출` card, then `YouTube 고급 기능 (선택)` | e2e *puts the stream key first and marks the YouTube card optional*, which also asserts the DOM order |
| A broadcast goes LIVE with no account connected | — | e2e *goes live with nothing but a stream key, never connecting an account*: `youtube_status.connected` is `false` before and during LIVE |
| An API failure does not touch FFmpeg | `youtube_follow_broadcast` runs off the tick; `applyNow` reports and returns | e2e *keeps the broadcast running when the YouTube side fails*: apply fails with `LL-YOUTUBE-004`, the pill is still `LIVE` |
| No ordinary user is asked for a Client ID, Secret, API enablement or quota | Credentials are built in; the custom-client fields live in developer Advanced Mode only | e2e *never asks an ordinary user for a Client ID or Secret* |

What that decision admitted to V1:

| In V1 now | Why it is not a risk to the broadcast |
| --- | --- |
| YouTube OAuth (one scope, `youtube.force-ssl`) | Used only for metadata and chat. Video still reaches YouTube over RTMPS with a stream key, exactly as before |
| Title, description, tags, category, privacy | Applied through the API after the broadcast is up; a failure changes nothing about the stream |
| Metadata presets | Local data only |
| Automatic live chat messages | Runs on its own thread. It cannot start, stop or delay FFmpeg, and an API failure pauses the bot alone |

**Still excluded, and not begun:** AI of any kind, automatic comment replies,
viewer analytics, moderation, automatic thumbnail changes, automatic broadcast
creation, Twitch, TikTok, scenes, camera, overlay, multi-stream and cloud.

The rule the decision did not change: nothing in the YouTube module is reachable
from the broadcast tick. `youtube_follow_broadcast` hands every network call to
another thread, so the loop that keeps FFmpeg alive never waits on Google.
