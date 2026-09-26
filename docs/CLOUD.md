# Louver Live Cloud — audit, plan, risks

Written before any cloud code existed, from reading the repository rather than
from assumption. Every claim below names the file it came from.

---

# PHASE 1 — Repository audit

## What is where

| | lines | depends on Tauri? |
| --- | --- | --- |
| `crates/louver-core` | 15,451 | **no** |
| `apps/desktop/src-tauri` | 3,679 | yes, by definition |
| `apps/desktop/src` (React) | — | only through `ipc.ts` |

The engine is four fifths of the code and none of it imports Tauri. The only
matches for `tauri` under `crates/` are test helpers pointing at the sidecar
directory and a docs-sync assertion. That is the single most important fact in
this document: **the cloud does not need a new engine.**

## Trait seams that already exist

`louver-core` defines 13 traits. Five of them are exactly the seams a server
port needs, and they were put there for testability rather than for this:

| trait | file | what the cloud plugs in |
| --- | --- | --- |
| `SecretStore` | `security/mod.rs:16` | an encrypted, database-backed store |
| `StreamLauncher` | `runtime.rs:29` | nothing — `FfmpegLauncher` works as-is |
| `RuntimeEvents` | `runtime.rs:47` | a sink that writes to the cloud database |
| `SleepPreventer` | `system/mod.rs:116` | `NoopSleepPreventer`, which exists |
| `Clock` | `clock.rs:6` | nothing — `SystemClock` works as-is |

`SecretStore::set/get/delete` each take an `account: &str`. The trait already
holds many secrets; `StreamKeyStore` simply hardcodes one account name. A
per-destination key needs a different account string, not a different trait.

## The runtime is one broadcast per instance — and that is fine

`BroadcastRuntime` (`runtime.rs:281`) holds singular state: one `plan`, one
`session_id`, one `supervisor`, one `manifest_path`. It cannot run two
broadcasts.

But every dependency arrives through `new()`: the database, the manifest path,
the key store, the launcher, the events sink, the session store. So **N
broadcasts means N instances**, each with its own paths and its own sinks. No
change to the runtime is required for concurrency.

## The watchdog §5 asks for is already built and tested

`streaming/supervisor.rs` owns the restart policy. Reading its tests:

- `backoff_follows_the_specified_schedule` — 2, 5, 10, 20 seconds
- `backoff_is_monotonic_and_capped_at_60s` — proven for 200 attempts
- `a_successful_reconnect_resets_the_backoff`
- `user_stop_terminates_the_child_and_never_restarts`

That last one is §5's "사용자가 직접 STOP한 방송은 절대로 watchdog이 다시
시작하면 안 된다", already enforced. Rewriting any of this would be replacing
tested code with untested code.

## Recovery state is already modelled

`session/mod.rs:16` `SessionState` carries `user_requested_stop`,
`ffmpeg_pid`, `heartbeat_at`, `last_error`, `stream_state`, `order_seed`. That
is most of §6's table. It is persisted to a single `session.json`, which is
correct for one machine running one broadcast and wrong for a server running
many — so the cloud persists the same facts per broadcast in its own database
instead of to a file.

## State machine

`StreamState` is `Idle, Preparing, Connecting, Live, Reconnecting, Stopping,
Stopped, Error`. §2 asks for `CREATED, PREPARING, STARTING, RUNNING,
RECONNECTING, STOPPING, STOPPED, FAILED`. These are the same eight states
under different names. The cloud maps them; it does not add a second machine.

## Database

`database/migrations.rs` creates `settings, media, playlists, playlist_items,
schedules, stream_sessions, stream_events, broadcast_presets, chat_messages`.
There is no `user_id` anywhere — it is a single-tenant schema, correctly, for a
desktop app. `Database` is `Clone` over `Arc<Mutex<Connection>>`.

## Media pipeline

`media/probe.rs` decides compatibility; `media/normalize.rs` prepares a cache
entry; `streaming/ffmpeg.rs` builds every argv. §8's "UPLOAD → FFPROBE →
COMPATIBLE? → NORMALIZE → READY" is `plan_for` + `normalize_one`, already
written and measured (remux at 119x realtime, full encode at 3.45x). The cloud
calls these functions.

`a_source_file_used_untouched_breaks_the_loop` proves a conformant source still
cannot be streamed straight from its own path, so the cloud prepares every
upload exactly as the desktop does.

## Frontend

`ipc.ts` funnels every call through one `call()` and every event through one
`listen()`. 72 commands, 4 events (`louver://status`, `normalize`, `media`,
`navigate`). `isTauri()` already branches, and a mock backend already serves
the browser — that is how the 57-test e2e suite runs. The transport split §11
asks for is a change to one file.

## Tests and CI

`npm run verify` runs 11 checks. `ci.yml` has `verify` on three OSes and
`sidecars` on four targets; Linux is verified in CI and excluded only from the
release matrix. So a Linux server build is already exercised.

---

# PHASE 2 — Architecture plan

## Crate layout

```
crates/louver-core     unchanged — the shared engine
crates/louver-cloud    NEW — tenancy, entitlements, storage, credentials,
                             the broadcast manager
apps/server            NEW — axum HTTP + SSE; routes only
apps/desktop           unchanged
apps/desktop/src       transport split added; components untouched
```

`louver-cloud` depends on `louver-core`. Nothing depends on `louver-cloud`
except `apps/server`. The desktop never links it.

## Two databases, on purpose

The cloud keeps its own SQLite file with its own migrations: `users`,
`subscriptions`, `plans`, `media`, `stream_destinations`, `broadcasts`,
`broadcast_events`, `sessions`.

Each *running* broadcast gets its own working directory containing a plain
`louver-core` database, populated with the one playlist and the media rows it
needs. Three reasons, and the third is the deciding one:

1. The desktop's migrations ship to customer machines. Adding `user_id`
   columns to them would alter a schema already running in the field.
2. Separate files mean separate write locks — three broadcasts cannot block
   each other on one `Arc<Mutex<Connection>>`.
3. The engine then runs *byte for byte* as it does on the desktop, against the
   schema its 354 unit tests were written for.

The cost is a projection step: cloud rows in, core rows out. That is a small,
testable function, and it is cheaper than the risk of touching shipped
migrations.

## Broadcast manager

One `BroadcastRuntime` per broadcast, each on its own thread running the same
one-second tick the desktop runs (`lib.rs:289`). The manager owns a map from
`broadcast_id` to a handle holding the runtime, its thread, and its cancel
flag. Isolation is structural: separate processes, separate runtimes, separate
databases, separate directories.

`desired_state` lives in the cloud database and is the user's intent;
`runtime_state` is what the engine reports. The manager reconciles them. On
boot it loads every broadcast with `desired_state = RUNNING` and starts it.

## Entitlements

`plans` rows carry named limits; code reads `max_concurrent_streams`,
`max_storage_bytes`, `max_upload_bytes`, `max_broadcasts` by name and never
branches on a plan's name. Starting a broadcast counts running broadcasts and
inserts the start inside one `BEGIN IMMEDIATE` transaction, so two concurrent
requests cannot both see room for the last slot.

## Credentials

`CloudSecretStore` implements the existing `SecretStore`. Secrets are sealed
with ChaCha20-Poly1305 from `ring` — already in the dependency tree via rustls
— under a master key from the environment, and stored as ciphertext keyed by
account string. Passwords are PBKDF2-HMAC-SHA256, also `ring`. No new crypto
dependency is introduced.

A stream key is never returned to a client. The API answers with a mask.

## API

Domain-shaped, not a mechanical copy of 72 commands:

```
POST   /api/auth/register           POST /api/auth/login    POST /api/auth/logout
GET    /api/me                      GET  /api/me/subscription
GET    /api/media                   POST /api/media          DELETE /api/media/:id
GET    /api/stream-destinations     POST /api/stream-destinations
GET    /api/broadcasts              POST /api/broadcasts
GET    /api/broadcasts/:id          DELETE /api/broadcasts/:id
POST   /api/broadcasts/:id/start    POST /api/broadcasts/:id/stop
POST   /api/broadcasts/:id/restart
GET    /api/broadcasts/:id/logs
GET    /api/events                  (SSE — status for every broadcast the caller owns)
```

SSE rather than WebSocket: the traffic is one-way status and log lines, and SSE
reconnects by itself.

## Files

| action | path |
| --- | --- |
| new | `crates/louver-cloud/**` |
| new | `apps/server/**` |
| new | `Dockerfile`, `docker-compose.yml`, `.env.example` |
| modify | `Cargo.toml` (workspace members, deps) |
| modify | `apps/desktop/src/services/ipc.ts` (transport split) |
| new | `apps/desktop/src/services/transport.ts` |
| unchanged | everything in `crates/louver-core/src` |
| unchanged | everything in `apps/desktop/src-tauri` |

---

# PHASE 3 — Risk check

| risk | finding | verdict |
| --- | --- | --- |
| Tauri leaking into shared code | `louver-core` has no Tauri import | **clear** |
| Stream-copy compatibility | already decided by `plan_for`; every upload is prepared | **clear** |
| Linux | CI already verifies Linux on every push | **clear** |
| Process recovery | `StreamSupervisor` + `user_requested_stop` already tested | **clear** |
| Multi-stream race | `BEGIN IMMEDIATE` around count-and-insert | **clear, needs a test** |
| Credential security | `SecretStore` seam exists; sealing with `ring` | **clear** |
| One runtime per broadcast | singular fields, but all injected | **clear** |
| SQLite write contention | per-broadcast core DB; cloud DB writes are small | **clear** |
| **Bandwidth** | 6 Mbps sustained = **~1.9 TB/month per 24/7 stream**, 3 streams = ~5.8 TB | **cost blocker, not a code blocker** |
| **Storage** | a 90-minute prepared file is ~4 GB per video | **cost blocker** |

## Blockers

**No architecture blocker.** The two real risks are money, not code: egress and
storage. Three 24/7 streams move roughly 5.8 TB a month. On metered egress that
dominates every other cost; on an unmetered dedicated host it is close to free.
That decision belongs to pricing, not to this design, so §22's metering is
built in from the start and no price is written into the code.

One scope note recorded rather than silently decided: broadcast **scheduling**
is P1 in §18. The cloud stores `desired_state` and reconciles it, which is the
mechanism a scheduler would drive, but no cron is built in this pass.

---

# PHASE 4 — What was built

## Crates and apps

| where | what |
| --- | --- |
| `crates/louver-cloud` | tenancy, entitlements, credentials, storage, ingest, the broadcast manager |
| `apps/server` | the HTTP API (axum), auth, SSE, `--health-check` |
| `apps/web` | the cloud front end, sharing the desktop's primitives through `@` |
| `Dockerfile`, `docker-compose.yml`, `.env.example` | one image, one container, Linux |

`crates/louver-core` gained two things and lost nothing: `StreamKeyStore::with_account`,
so one process can hold one key per destination, and nothing else. Every file
under `apps/desktop` is untouched except `tailwind.config.js`'s content globs and
the `typecheck`/`lint` scripts, which now cover `apps/web` too.

The plan said the transport split would modify `apps/desktop/src/services/ipc.ts`.
It did not need to: `apps/web/src/transport.ts` defines the interface and
`desktopTransport.ts` satisfies it *from* `ipc.ts` without changing it, which
leaves the desktop app's own call sites exactly as they were.

## The API

Nine nouns, not seventy-two commands. Every row below takes a session and, where
it names a thing, that thing must belong to the caller.

| method | path | what |
| --- | --- | --- |
| POST | `/api/auth/register`, `/api/auth/login` | sets an HttpOnly session cookie |
| POST | `/api/auth/logout` | ends the session server-side |
| GET | `/api/me`, `/api/me/subscription` | who, and on what plan |
| GET POST | `/api/media`, `/api/media/upload` | list, upload (multipart, streamed) |
| GET DELETE | `/api/media/{id}` | one video |
| GET POST | `/api/stream-destinations` | list, save (key sealed, never returned) |
| DELETE | `/api/stream-destinations/{id}` | forget a destination and its key |
| GET POST | `/api/broadcasts` | the dashboard, and create |
| GET DELETE | `/api/broadcasts/{id}` | one broadcast |
| POST | `/api/broadcasts/{id}/start|stop|restart` | lifecycle |
| GET | `/api/broadcasts/{id}/logs` | that broadcast's events |
| GET | `/api/events` | SSE: the dashboard as it changes |
| GET | `/api/health` | for the container |

`Forbidden` answers **404**, deliberately: "this exists but is not yours" is
itself a fact about another account.

---

# PHASE 5 — Verification

`npm run verify` — all 11 steps pass (secret scan, typecheck, lint, frontend
tests, UI e2e, release tooling, frontend build, fmt, clippy, workspace tests).

## Measured on Linux, against a real RTMP endpoint

The server was run as a real process against `scripts/rtmp-sink.mjs`, which
speaks the actual RTMP protocol, with the distro's FFmpeg 6.1.1 — the same setup
the Dockerfile produces.

| step | measured |
| --- | --- |
| upload of a 39 MB clip → row returned | **0.22 s** |
| that clip analysed and prepared (20 s, 1080p30, remux) | **1.24 s** total |
| broadcast started → publisher connected at the ingest | **~1 s** |
| bytes at the ingest after 12 s | 23.9 MB |
| `kill -9` the server, restart → broadcast running again | yes, ~1 s after boot |
| bytes at the ingest after recovery | 49.0 MB, continuing |
| publishers after recovery | **exactly 1** (the orphan is killed by pid first) |
| user stop, then `kill -9`, then restart | stays `STOPPED`, `active` 0 |
| `bytes_sent` on the row | 26,258,834 — matches the sink's own count |

## Two things the real run found, and the fix

**An orphaned FFmpeg.** A hard-killed server leaves its child publishing.
Recovery then started a second sender to the same key. The core already knew how
to kill a previous run's process after verifying it is an FFmpeg; the manager now
does that before starting. One publisher after recovery, measured.

**`bytes_sent` was always 0.** Each FFmpeg reports its own total and a
replacement starts at zero, so the outgoing figure is banked before the new one
counts.

And one the test suite found: two preparations of one upload destroyed each
other's output. A media id is now claimed while it is being prepared.

---

# Final report

## P0 (§18)

| # | item | verdict | evidence |
| --- | --- | --- | --- |
| 1 | Broadcast Manager independent of the request | **PASS** | worker thread per broadcast; HTTP returns immediately |
| 2 | Broadcast lifecycle + state machine | **PASS** | `RuntimeState` (8 states) ← engine; `broadcast_manager.rs` |
| 3 | `desired_state` vs `runtime_state` | **PASS** | `recovery_finds_only_what_was_meant_to_be_running` |
| 4 | Web authentication | **PASS** | PBKDF2 600k, HttpOnly cookie, 7 HTTP tests |
| 5 | Upload + probe + prepare | **PASS** | 5 ingest tests on real media; 0.22 s to row |
| 6 | Stream destination stored safely | **PASS** | sealed with ChaCha20-Poly1305; DB holds no plaintext |
| 7 | Create / start / stop a broadcast | **PASS** | HTTP tests + the live run |
| 8 | Plan limit on concurrent streams | **PASS** | `BEGIN IMMEDIATE`; refused with 402, no process spawned |
| 9 | Watchdog with backoff | **PASS** | `StreamSupervisor`, reused unchanged |
| 10 | A user stop is never resumed | **PASS** | two tests + the live restart |
| 11 | Recovery after a server restart | **PASS** | live `kill -9` → running again, bytes resumed |
| 12 | One broadcast's failure isolates | **PASS** | `three_broadcasts_run_at_once_and_one_crash_is_isolated` |
| 13 | Ownership on every request | **PASS** | `knowing_another_users_ids_buys_nothing` (9 routes) |
| 14 | Stream key never leaves the server | **PASS** | no `key` field exists; DB file scanned; logs masked |
| 15 | Storage abstraction | **PASS** | `Storage` trait; `LocalStorage` now, S3 later |
| 16 | Web dashboard | **PASS** | slots as `2 / 3`, live over SSE, 18 front-end tests |
| 17 | Docker / Linux | **PARTIAL** | files written and compose interpolation checked; **the image was not built** — no Docker daemon in this environment |
| 18 | Metering for costing | **PASS** | `bytes_sent`, `uptime_secs`, `restart_count` per broadcast |

## §21 — the ten prohibitions

| # | prohibition | verdict |
| --- | --- | --- |
| 1 | desktop app deleted or broken | **kept** — `apps/desktop` untouched; its tests and e2e suite pass |
| 2 | verified FFmpeg logic rewritten | **kept** — `plan_for`, `normalize_one`, `StreamSupervisor` called, not copied |
| 3 | FFmpeg in the browser | **kept** — the browser uploads and clicks; FFmpeg runs on the server |
| 4 | lifecycle tied to the browser | **kept** — proved by killing the server, not the tab |
| 5 | stream key in the front end | **kept** — no storage write; asserted against `localStorage`, `sessionStorage`, the DOM |
| 6 | plan limits only in the front end | **kept** — enforced in a transaction; the browser only displays |
| 7 | a VPS per user | **kept** — one process, many tenants |
| 8 | Kubernetes from the start | **kept** — one container |
| 9 | a large refactor without tests | **kept** — 62 new tests |
| 10 | guessing instead of reading the code | **kept** — PHASE 1 read it; the live run measured it |

## Not done, and said so

| item | status |
| --- | --- |
| Docker image built and run | **NOT VERIFIED** — no Docker daemon here; `docker compose config` parses |
| S3-compatible storage | **NOT IMPLEMENTED** — the trait is there; only `LocalStorage` exists |
| Scheduled broadcasts in the cloud | **NOT IMPLEMENTED** — P1 in §18; `desired_state` is the seam a scheduler drives |
| YouTube OAuth / metadata / chat on the server | **NOT IMPLEMENTED** — `skip_pre_start: true`; the desktop keeps these |
| Storage quota enforcement per plan | **PARTIAL** — `max_upload_bytes` is enforced per file; `max_storage_bytes` is stored and read but not yet refused at upload |
| Billing, invoicing, prices | **NOT IMPLEMENTED** by intent — §22 asks for metrics, not prices |

## Resource report (for pricing, not priced here)

| resource | per 24/7 1080p30 6 Mbps stream | three of them |
| --- | --- | --- |
| egress | ~1.9 TB / month | ~5.8 TB / month |
| CPU while streaming | stream copy: a fraction of one core | still under one core |
| CPU at upload | remux ~119× realtime; full encode ~3.45× realtime | one core per upload, once |
| storage | ~4 GB per 90-minute prepared video, plus the original | grows with the library |
