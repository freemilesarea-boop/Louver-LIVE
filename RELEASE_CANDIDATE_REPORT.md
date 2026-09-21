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
| Costs come from a per-method table, not a read/write rule of thumb | `COSTS` in `youtube/quota.rs` is the 2026 table, supplied by the product owner from Google's official quota calculator. It could not be checked from this machine (`developers.google.com` is unreachable here), so it is **single-sourced** and that table is the one place to correct it. Anything unrecognised is charged as a write, because guessing low ends in a 403 mid-apply |
| Two allowances, counted separately | Most methods draw on one pool of 10,000 units a day. `search.list` and `videos.insert` draw on **their own allowance of 100 calls a day at 1 unit each**. Pricing those as 100-unit writes is wrong twice over: it overstates what they take from the pool and says nothing about the limit that actually stops them. Neither is called today, and a test keeps it that way |
| 200 units held in reserve | Enough to finish one metadata apply (103) and one chat message (50), so the app stops on its own terms rather than partway through changing a title |
| Google's `quotaExceeded` latches the allowance shut | Whatever the local count says. A refusal on a bucketed method shuts that bucket alone; the shared pool is untouched and the rest of the app carries on |
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

## 14c. A scheduled window that never started the stream (2026-09-20)

Reported from a real Mac: a `20:58 → 21:00` daily schedule. At 20:59 the
schedule page read `지금 방송 시간입니다 · 2026-09-20 21:00에 종료`, and the
dashboard read `OFFLINE · STATUS IDLE · FFmpeg Stopped` with
`진행 중인 YouTube 라이브를 찾지 못했습니다. YouTube에서 라이브를 먼저 만들어주세요.`

Not a timing bug. The scheduler fired correctly; the startup pipeline stopped.

**Cause.** `apply_metadata` opened with `api.active_broadcast(&token)?`, which
lists `active` then `upcoming` broadcasts and returns `LL-YOUTUBE-003` when
there are none. A scheduled window arrives with nothing on the channel — that
is the normal case, not an edge one — so the pre-start hook returned an error,
the scheduled start was held under the default policy, and FFmpeg was never
launched. The product required a user who is asleep to have opened YouTube
Studio in advance.

**Fixed** by provisioning rather than requiring. `youtube/provision.rs` decides,
and the decisions are pure so they are testable without a network:

| Step | Rule |
| --- | --- |
| Find or create | Reuse a broadcast scheduled within 15 minutes of this window — so a retry does not leave two behind — and otherwise `liveBroadcasts.insert` with the saved title, description, privacy, `scheduledStartTime`, `scheduledEndTime` and auto start/stop. An arbitrary "upcoming" broadcast is never taken over: it may be the user's own, for another time |
| Bind | `liveStreams.list` and match the **saved stream key** against `cdn.ingestionInfo.streamName`, then `liveBroadcasts.bind`. Never "the first stream", which binds to an endpoint nothing is publishing to and leaves YouTube waiting for video with nothing to say about why. The key is compared in memory and dropped — not logged, not stored, not in a URL or a body |
| Confirm | `contentDetails.boundStreamId` must name the stream that was asked for. A 200 is not the answer |
| Metadata | The same three calls the manual button uses, then read-back |
| FFmpeg | Only now, because a broadcast that goes live first is live under whatever title it had |
| Go live | Only once `liveStream.status.streamStatus` is `active`. YouTube refuses a transition while the stream is silent, with an error that reads like a permissions problem. `enableAutoStart` broadcasts are left to YouTube |
| End | FFmpeg stops, the broadcast is transitioned to `complete`, the chat bot stops. A broadcast left `live` with nothing publishing to it is exactly the stale state the next window would try to reuse |

Manual and scheduled starts run this through the same `PreStartHook`, so they
cannot drift apart.

**Retry.** A failed window now retries at 5s, 10s, 20s then 30s inside its own
window instead of once a minute — a two-minute window was lost entirely by a
single transient failure. Bounded, never a spin, and never a jump to tomorrow
while today's window is still open.

**The contradiction.** `RuntimeStatus.active_occurrence` reports the open
window whatever the stream is doing, so the dashboard and the schedule page
read the same runtime and cannot disagree.

**Verified in the real app** under Xvfb, with nothing clicked after the
schedule was saved: a `12:16 → 12:18` window fired by itself, the dashboard
read `예약 방송 12:16 → 12:18 LIVE` with the `예약 시작` badge and a countdown to
12:18, and at 12:18:00 the log recorded `예약된 종료 시간입니다` /
`방송을 종료했습니다 (예약 종료)` with no FFmpeg process left and the next
occurrence computed. The stream itself read RECONNECTING throughout because
this container's stream key is a fake one that real YouTube rejects.

**NOT TESTED — and this is the part that matters most.** No YouTube account
exists here, so every call above ran against a stand-in that speaks Google's
JSON. Whether `liveBroadcasts.insert` is accepted for a real channel, whether
the bind takes, whether `enableAutoStart` carries a real broadcast to LIVE, and
whether the metadata survives on the created broadcast are all **NOT TESTED**.
TEST A–D in the request are the ones that settle it.

## 14d. Saving a schedule was not the same as running one (2026-09-20)

Adding a schedule on a real Mac produced a row — `21:46 → 21:56 · 매일 · night`
— and a green toggle, and nothing else. Nothing on the screen answered whether
this computer was watching the clock, whether the broadcast was really going to
start, or whether anything at all was going to happen tonight.

It was a real ambiguity, not only a presentational one: a saved rule and a
running scheduler were the same thing in the code, so the app could not have
told the user which it had even if it had tried.

They are now separate. `scheduler_armed` is a persisted setting, the runtime
refuses to act on any schedule while it is off, and the page carries one large
switch that says which state the machine is in.

| Rule | Where |
| --- | --- |
| A saved schedule does nothing on its own | `tick_scheduler` and `recover_on_startup` both return early when not armed |
| The per-row toggle means "use this rule", not "broadcasting" | Labelled `이 예약 사용`; the row says `지금이 예약 시간이지만 자동 방송이 꺼져 있습니다` when a window is open and the scheduler is off |
| Arming checks now, not at 3am | `scheduler_arm` runs the full preflight per enabled schedule — playlist, files, FFmpeg, ffprobe, stream key, times — and refuses with the reason. It also refuses if metadata auto-apply is on with no account connected, since every window would then stop and ask a question nobody is there to answer |
| The waiting screen proves it is waiting | `예약 대기 중`, the next window with its playlist, and a second-by-second countdown to the automatic start |
| A stopped FFmpeg is not a fault while waiting | The dashboard says `예약 방송 대기 중 · 예약 시간에 자동으로 시작합니다` |
| Saving says which case it is | `예약이 저장되었으며 자동 방송에 반영되었습니다` when armed, and otherwise `예약이 저장되었습니다. 자동 방송을 사용하려면 아래 [예약 방송 시작]을 눌러주세요` |
| A reboot restores what the user chose | `예약 방송 상태 자동 복원`, default on. Armed at shutdown means armed at launch; **stopped by the user means stopped after a reboot**, which is the half that matters |

States: `STOPPED` → `ARMING` → `WAITING` → `STARTING` → `LIVE` → `STOPPING` →
`WAITING`, with `ERROR` for an armed scheduler whose start failed.

**Verified in the real app** under Xvfb. Pressing 예약 방송 시작 with no stream
key saved refused with `LL-STREAM-007` and stayed `꺼짐` — the check happening
at the moment the user is present, which is the whole point. With the key
saved, the panel read `예약 대기 중 · 다음 방송 2026-09-20 13:04 → 13:12 ·
Test Playlist` above a running `00:00:22 후 자동 시작`. At 13:04, with nothing
clicked, it became `예약 방송 중 · 13:04 → 13:12 방송 중입니다` and the dashboard
`예약 방송 13:04 → 13:12 LIVE` with `FFmpeg Running` and a countdown to the
automatic stop.

## 14e. Six identical error lines for seven different failures (2026-09-20)

Real Mac logs, supplied by the product owner. The scheduler was working:

```
22:34:45  SCHEDULER_ARMED: 예약 1개를 감시합니다
22:35:01  LL-YOUTUBE-004    22:35:38  LL-YOUTUBE-004
22:35:07  LL-YOUTUBE-004    22:36:08  LL-YOUTUBE-004
22:35:17  LL-YOUTUBE-004    22:36:39  LL-YOUTUBE-004
```

The gaps are 5s, 10s, 20s, 30s, 30s — the bounded retry of §14c, doing exactly
what it was built to do. `stream.log` shows `방송 시작: night (… Scheduled)` at
each of those times and never `방송이 시작되었습니다`, so the failure is in the
YouTube preparation, before FFmpeg is asked for anything.

What could not be answered from that log is *which* request failed. A start
makes seven of them — refresh the token, list the broadcasts, create one, list
the streams, bind, update the broadcast, update the video — and every one of
them raises `LL-YOUTUBE-004 · YouTube에 연결하지 못했습니다`. A manual broadcast
on the same Mac had reached `방송이 시작되었습니다` earlier the same evening,
which narrows nothing: that run was still holding the access token consent had
just minted, so it exercised none of the refresh path a 22:35 start depends on.

So this change is instrumentation, not a fix. No guess about the cause has been
written into the code.

| What | Where |
| --- | --- |
| Each step writes `_START` / `_OK` / `_FAIL` | `youtube/steps.rs`, threaded through `provision_broadcast`, `apply_to` and `try_go_live` |
| The eight families | `YOUTUBE_TOKEN_REFRESH_*`, `YOUTUBE_BROADCAST_LIST_*`, `YOUTUBE_BROADCAST_INSERT_*`, `YOUTUBE_STREAM_LIST_*`, `YOUTUBE_BROADCAST_BIND_*`, `YOUTUBE_METADATA_APPLY_*`, `YOUTUBE_STREAM_ACTIVE_WAIT`/`YOUTUBE_STREAM_ACTIVE`, `YOUTUBE_BROADCAST_TRANSITION_*` |
| Google's words are kept | `ApiFailure` reads `error.code`, `error.errors[0].reason` and `error.message`, and the detail reads `liveBroadcasts.insert HTTP 403 reason=liveStreamingNotEnabled · …` |
| The API method comes from the quota classifier | One source for the name and the units, so a log line and a charge cannot disagree. `POST` and `PUT` on the same resource are now told apart (`insert` vs `update`) |
| A failed refresh is its own error | `LL-YOUTUBE-AUTH-REFRESH`, raised only by the `refresh_token` grant. The consent exchange keeps `LL-YOUTUBE-002` |
| Manual and scheduled are comparable | Every line carries `origin=manual` or `origin=scheduled`, and the step sequence is kept on the apply state |
| The screen names the stage | `예약 방송 생성 실패`, `Google 인증 갱신 실패`, `스트림 연결 실패` — with the remedy under it and Google's own words behind 상세정보 |
| Four values are never written | Access token, refresh token, client secret, stream key. No recorder method accepts one; the OK lines carry resource ids, and the FAIL lines carry only the `error` object |

**Evidence.** `apps/desktop/src-tauri/tests/provisioning_log.rs` drives the real
`YoutubeService` — real `TokenStore`, real transport, real quota meter — against
local servers speaking Google's JSON, and asserts the log:

- an empty channel provisions end to end, writing all fifteen expected lines,
  with the refresh actually performed (`새 토큰 발급`, `grant_type=refresh_token`
  with `client_secret=` in the body);
- a 403 on `liveBroadcasts.insert` writes
  `YOUTUBE_BROADCAST_INSERT_FAIL … liveBroadcasts.insert HTTP 403 reason=liveStreamingNotEnabled`,
  after a `YOUTUBE_BROADCAST_LIST_OK` that tells the reader how far it got;
- a revoked refresh token fails as `LL-YOUTUBE-AUTH-REFRESH` and the YouTube API
  is never called at all;
- a manual and a scheduled start take the identical step sequence, differing
  only in the label.

Every one of those tests also searches the whole log for the four credentials.

**The documented log path was wrong.** `~/Library/Logs/com.louver.live/` is a
folder the app has never written to; `AppPaths::logs_dir` puts them under
`~/Library/Application Support/LouverLive/logs/`. Four documents said the wrong
thing, including the diagnostic commands. Corrected, and
`no_document_sends_a_mac_user_to_a_folder_the_app_never_writes` now fails the
build if it comes back.

**Status: NOT TESTED against real Google.** This adds no claim that a scheduled
start now works. It is PASS only when a real Mac log shows
`YOUTUBE_BROADCAST_CREATED → YOUTUBE_BROADCAST_BOUND → FFmpeg Running →
YOUTUBE_BROADCAST_LIVE` and the channel is live. The procedure is
`YOUTUBE_SETUP.md` §3, TEST 15~22.

## 14f. The cause, from the log above (2026-09-20)

The instrumentation paid for itself on the first run. Real Mac:

```
YOUTUBE_TOKEN_REFRESH_OK
YOUTUBE_BROADCAST_LIST_FAIL
liveBroadcasts.list HTTP 400 reason=incompatibleParameters ·
Incompatible parameters specified in the request: broadcastStatus, mine
```

The scheduler, OAuth, the refresh token and the client credentials were all
working — the refresh succeeded on the line above. `liveBroadcasts.list` accepts
exactly one of `id`, `mine` and `broadcastStatus`, and the app was sending
`mine=true` and `broadcastStatus=upcoming` together. Every scheduled start had
been failing on its first request to the channel, six times a window, for as
long as the feature has existed.

| Change | Why |
| --- | --- |
| `mine=true&broadcastType=all` and nothing else | One filter is the rule. `broadcastType` is not a filter, so it may stay |
| The narrowing moved into `choose_broadcast` | It costs nothing in memory and cannot be refused |
| Reusable statuses are an allowlist: `created`, `ready` | `testing` and `live` are in progress, `complete` and `revoked` are over. An allowlist is right about the statuses Google may yet add |
| `scheduledStartTime` within ±15 minutes, still | §4: never borrow an unrelated broadcast. A window with nothing prepared creates one rather than renaming the user's own |
| Paging, bounded | 50 to a page, at most 4 pages, and it stops at the first page that answers. A channel with 60 broadcasts must not be told it has none — that would leave a duplicate every night — and one with 6,000 must not be read to the end at a quota unit a page |
| A refused request is not a lost connection | `LL-YOUTUBE-004`'s message covers everything from a bad parameter to an unreachable host, so it is vague enough to be wrong most of the time. Each step now supplies its own: "예약 방송 정보를 조회하지 못했습니다." The code and Google's words are unchanged, and the words move behind 상세정보 where a user need not read them |

**The fakes were the reason this shipped.** Both stand-ins answered
`mine=true&broadcastStatus=upcoming` with a cheerful 200, so eleven tests passed
against a request real Google has never accepted. They now enforce the rule and
return Google's own `400 incompatibleParameters`, which is what makes the
regression real: restoring the old query fails **11 tests** across the two
files, not one.

New coverage: the request is asserted on the wire (exactly one filter, no
`broadcastStatus`); a mixed channel — finished, another day's, on air, revoked,
and this window's — yields only this window's broadcast; a window with nothing
prepared for it creates rather than borrowing the user's; a second page is read
and the bound is honoured; and the `incompatibleParameters` refusal reads as
"예약 방송 정보를 조회하지 못했습니다" on screen, with Google's words behind the
disclosure.

**Status: NOT TESTED against real Google.** A confirmed cause and a fix that
the refused request can no longer pass through is not the same as a working
scheduled broadcast. The next real Mac run should show
`YOUTUBE_BROADCAST_LIST_OK`; whatever `_FAIL` comes after it, if any, is the
next thing to fix.

## 14g. Shipping it (2026-09-21)

The release workflow built three of the four platforms the product supports,
and would have uploaded nothing for any of them.

| Defect | Why it mattered |
| --- | --- |
| Linux was not in the matrix | `.deb` and `.AppImage` are in `tauri.conf.json`'s bundle targets and `fetch-ffmpeg.mjs` has had a Linux source all along; nothing built them |
| Every artifact path was wrong | They read `apps/desktop/src-tauri/target/<triple>/release/bundle/...`. This is a cargo workspace, so the target directory is the repository root. A tag push would have produced four green jobs and four empty artifacts |
| A tag published nothing | Run artifacts expire in 30 days and need a GitHub login to download, so they are not something a user can be sent to |
| `--require-download` accepted the development fallback | The "already present, use --force" check ran *before* the release check, so a runner with a cached workspace could ship the machine's own FFmpeg — dynamically linked against 215 libraries, and unable to run anywhere else |

The last one had a second line of defence that did work: `ffmpeg-manifest.mjs
--check` reads the binaries and refuses a dynamically linked one. Confirmed
here — it exits 1 on the fallback sidecar with "it will not run on a user's
machine". Both gates now agree, and the release step passes `--force` so a
cached workspace cannot supply a stale one either.

Building the Linux bundle here before trusting the workflow found two more,
neither of which any amount of reading would have shown:

- **The AppImage bundler needs `xdg-utils`**, which the apt list did not
  install. It fails with `xdg-open binary not found`, *after* the `.deb` has
  been written — so the job fails with one of its two installers already on
  disk, which reads like a flake and is not one. `xdg-utils`, `fuse` and
  `libfuse2` are now installed.
- **Every bundler names its output after the product**, so the file is
  `Louver Live_1.0.0_amd64.deb`. The collect step iterated `$(find ...)`
  unquoted, which splits that on the space: it produced `.../bundle/deb/Louver`
  and `Live_1.0.0_amd64.deb`, neither of which exists, and under `set -e` the
  job dies. `find -print0` into a `read -d ''` loop now.

Pushing it then found three more on the runners themselves, and one of them
is why no Windows installer has ever been buildable:

- **`fetch-ffmpeg.mjs` shelled out to `find`.** On Windows that is
  `C:\Windows\System32\find.exe`, a text search, which answers
  `FIND: Parameter format not correct` and exits 2. The download path has
  never once worked there, so the Windows release job would have died at the
  sidecar step before compiling a line. It walks the directory in Node now,
  and the zip extraction falls back to bsdtar, which Windows has and `unzip`
  is not.
- **A newer clippy than this machine's** refuses `&haystack.chars()…` inside
  an `assert!` as a redundant reference. Local clippy is 0.1.94; the runner's
  cites 1.98. Fixed rather than allowed.
- **`FedericoCarboni/setup-ffmpeg@v3` has no arm64 macOS build** and fails in
  under a second on `macos-latest`, which is arm64 now. Each platform's own
  package manager installs it instead — `apt`, `brew`, `choco` — because that
  action was the only thing putting ffmpeg on `PATH`, and without it the
  sidecar step had nothing to fall back to when the download failed.
- **`npm run verify` could never have passed on a fresh checkout.**
  `tauri::generate_context!()` reads `frontendDist` at compile time and panics
  if `apps/desktop/dist` is absent — `proc macro panicked … this path doesn't
  exist` — and `verify.mjs` ran `frontend build` *after* the Rust steps. It
  passed on a developer's machine only because an earlier build had left a
  `dist/` behind, which is why this survived to a release. The build now runs
  before the Rust steps. Reproduced by deleting `apps/desktop/dist` and
  `cargo clean -p louver-desktop`, which fails exactly as the runner did, and
  the reordered run is green from a clean tree.

The matrix is now Windows x64, macOS Apple Silicon, macOS Intel and Linux x64.
Linux builds on `ubuntu-22.04` deliberately: glibc is forward-compatible only,
so a `.deb` built on an older distribution installs on newer ones and not the
reverse. Each job collects its installers into one directory and **fails if it
produced none**, which is the check that would have caught the path bug.

A `v*` tag now publishes a **draft** GitHub Release with every installer and
each build's `BUILD-INFO-<target>.txt` — commit, whether it was signed, and
whether it carries an OAuth client. Draft rather than public because of §15
below: nothing here has been run against a real YouTube channel, and macOS and
Windows have never been executed at all. A person decides when it goes out.

`RELEASING.md` is the procedure, including what each missing secret costs — an
unsigned macOS build makes the user Control-click to open it, and a release
without `LOUVER_GOOGLE_CLIENT_ID`/`_SECRET` has a YouTube button that cannot
work.

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
