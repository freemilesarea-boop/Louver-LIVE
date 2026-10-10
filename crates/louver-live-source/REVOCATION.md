# Account suspension and job revocation — design

Design only. Nothing here is implemented, and no production change is made.

The question: an operator suspends an account; the account's beta broadcast is
still on air. What stops it, without (a) stopping broadcasts that should keep
running, and (b) touching production?

## 1. What the worker can and cannot know

Three facts, read from production's source rather than assumed. They decide
everything below.

**The worker holds no user credential.** By design (PHASE 3): the session
cookie reaches it on `POST …/session`, is used once against `/api/me`, and is
dropped. After that the worker has a signed token of its own and nothing it
could present to production on the user's behalf. It cannot ask "is user X
still enabled?" because it cannot authenticate as anyone.

**Logout and suspension are indistinguishable from outside.**

| | what production does | what an outsider sees |
|---|---|---|
| logout | `delete_auth_session(hash)` — one row (`apps/server/src/auth.rs:331`) | `/api/me` → 401 |
| suspension | `disabled_at` set **and** `DELETE FROM auth_sessions WHERE user_id=?1` — every row (`crates/louver-cloud/src/admin.rs:388`) | `/api/me` → 401 |
| session expiry | `expires_at` in `user_for_token` (`db.rs:1632`) | `/api/me` → 401 |

All three are the same 401. A worker that stopped jobs on a 401 would stop them
on a logout, which the policy forbids.

**No production route accepts a service credential.** There is no
`/api/internal/*` or `/api/service/*` in `apps/server/src/lib.rs`; every route
is either public or behind `Caller`/`Admin`, both of which need a user session.

### Conclusion

**Automatic, suspension-driven termination of a running job is impossible with
zero production change.** Not difficult — impossible: the worker has no
credential to ask with, and the one signal it could observe cannot tell
suspension from logout. Everything below is built around that.

## 2. What is already solved, today, with no change at all

**Requirement: a suspended account cannot create a new broadcast.** Already
true, and tested.

`POST /jobs` requires a bearer token. A bearer token is minted only by the
handshake. The handshake needs a session row that `user_for_token` will
resolve. Suspension deletes every one of that user's session rows. So:

- the moment an account is suspended, no new token can be minted for it;
- the token it already holds expires within `TOKEN_TTL_SECS` = **5 minutes**;
- after that, `POST /jobs` is a 401.

Bounded by five minutes, automatic, nothing to build. This is the main reason
the TTL was cut from thirty minutes to five.

## 3. The policy that keeps a broadcast alive

The requirement is that a broadcast survives the browser closing, and that a
plain logout does not stop anything. Both follow from one rule:

> **Renewal is liveness for the UI. It is never authority over a job.**

A job's lifetime comes from its own `desired` field — never from whether a
browser is still attached.

- **Browser closed** → no renewal → *nothing happens*. The worker never asked.
- **Logout** → the next renewal 401s → *nothing happens to the job*. The page
  says "signed out"; the broadcast continues.

### A correction: there is no 24-hour cap, and there should not be one

An earlier draft of this document proposed `MAX_JOB_SECS = 24 * 3600` — a hard
cap that stopped a job after a day "so a forgotten tab cannot hold an encode
for a week". **That was wrong, and it is withdrawn.** It came from conflating
two unrelated numbers: the length of the *acceptance test* (24 hours, because
that is long enough to expose a leak or a drift) and the maximum duration of a
*real broadcast*. 247streams is a 24/7 unattended service. A cap that stops a
healthy broadcast after a day is not a safety limit, it is an outage on a
timer.

Production already settles the question, and in the opposite direction.
`louver_cloud::Schedule` is:

```rust
pub struct Schedule {
    pub enabled: bool,
    pub start_at: Option<String>,
    /// RFC 3339, UTC. Optional automatic stop.
    pub stop_at: Option<String>,
    …
}
```

`stop_at` is an **`Option`**, and its own comment says *optional*. A broadcast
with `stop_at: None` runs until the source ends or somebody stops it — that is
the shipped semantics of the product this worker is a beta feature of. The beta
must mirror it, not invent a limit the rest of the service does not have.

See §9 for the three lifecycle options compared, and what stops a broadcast
instead of a clock.

## 4. Fail-open on continuation, fail-closed on admission

The requirement that a transient production failure must not stop a healthy
customer's broadcast is the inverse of how the gate works, and deliberately so:

| | posture | why |
|---|---|---|
| admission (gate, origin, token, destination) | **fail-closed** | a request that cannot be shown to be allowed is refused |
| continuation (an already-running job) | **fail-open** | an outage must not become a mass outage of broadcasts |

So **only an explicit, authenticated revocation stops a job.** A timeout, a
5xx, a DNS failure, an unreachable production, an expired token, a renewal that
never comes — none of them stop anything. There is deliberately no "I could not
confirm this user is still allowed, so I am stopping" path. That path is how a
sixty-second production blip takes every beta broadcast off air at once.

## 5. Operator-triggered revocation — zero production change — **IMPLEMENTED**

The achievable answer today, and now built (`src/admin.rs`, three routes in
`src/api.rs`, ten tests in `tests/admin_api.rs`):

```
POST /admin/revoke     { "user_id": "<id>" }   → { changed, stopped, jobs[] }
POST /admin/restore    { "user_id": "<id>" }   → { changed, stopped: 0 }
GET  /admin/revoked                            → { users[] }
X-Louver-Admin: <LOUVER_LIVE_SOURCE_ADMIN_SECRET>
```

`restore` exists because an operator who re-enables an account must be able to
undo this; without it the only way back is editing a file on the VPS. It lifts
the decision and starts nothing — anything stopped is `desired = Stopped` on
disk, and the user starts it again themselves.

A revocation is **not just a stop**. A stop alone is undone twice over: by the
account's own bearer token, valid for up to five minutes after suspension and
able to start a new job in that window; and by a restart, which reads the state
directory. So it is a persisted decision that [`jobs::Registry`] consults on
both `create` and `recover`, written to `revoked.json` by temp-and-rename so a
crash mid-write cannot leave a file that reads as "nobody is revoked".

- A **third** secret, distinct from the signing secret and the gate secret.
  Constant-time compared, env-var only, fail-closed, 32 bytes minimum. The
  binary will not start without it; there is no flag that disables the operator
  API.

  The three are not interchangeable, and that is enforced cryptographically
  rather than by configuration discipline: each is hashed under its own
  domain-separation label (`src/secret.rs`), so **the gate secret offered in
  the admin header fails even if an operator sets both variables to the same
  string.** That matters because the gate secret travels on every proxied beta
  request — a leak of it must not become the power to take customers off air.
- **Not reachable from a browser.** Caddy's `handle` blocks match only
  `/api/live-source/*` and `/beta/*`, so `/admin/*` falls through to the
  production catch-all and never reaches the worker from outside. It is
  reachable only on the worker's own port — an operator on the VPS, over SSH,
  against loopback. That is the right exposure for an operator action, and it
  comes for free from the routing that already exists. `Caddyfile.sample` now
  says so explicitly, and a test pins the paths.
- **Deliberately outside the gate layer.** The gate asks "did this come through
  Caddy?", and for an operator action the honest answer is no. Requiring it
  would make the operator forge Caddy's header, and would protect nothing: if
  Caddy ever did route `/admin/*`, it would add the gate header itself. So the
  admin routes are merged after the gate layer — which also meant giving the
  customer-facing router an explicit fallback, because a layer wraps a
  router's fallback and the merge would otherwise have moved it outside the
  gate too. That regression was caught by an existing test, not by review.
- **Effect:** look up the running jobs whose `owner` is that user, then stop
  each through the existing `cancel` path — which writes the intent, sets only
  that job's flag, joins, and writes the intent again to close the race with
  the worker's read-modify-write. Reusing it means revocation cannot drift from
  cancellation. The ids are collected under the map lock and the lock is
  released before any join, because joining a worker thread while holding the
  map would deadlock against the worker's own state write.
- **Only that account's.** The map is filtered by `owner`, exactly as
  `running_count_for` does. Tested: Alice's two jobs stop, Bob's keeps running,
  and Bob is not revoked.
- **Idempotent**: a second call changes no decision and finds nothing to stop,
  returning `changed: false, stopped: 0` rather than failing. Both calls are
  recorded, the repeat marked as one.
- **Audited**: one JSONL line per call in `admin-audit.jsonl` beside the job
  state — timestamp, action, subject, how many jobs stopped and which, and
  whether the decision changed. A failure to write the log is reported but
  never undoes the action. Tested to contain no secret, no token and no
  destination.
- **Durable across a restart**: tested both ways — a revoked account's jobs are
  not recovered, and a state file forged back to `Running` (a stale backup, or
  a crash between the two writes) still does not put it back on air, because
  `recover` consults the list as well as `desired`.

### Operator runbook

1. Suspend the account in the admin console. New beta broadcasts are blocked
   within five minutes automatically (§2).
2. On the beta VPS: `curl -X POST -H "X-Louver-Admin: $SECRET" \
   localhost:9080/admin/revoke -d '{"user_id":"…"}'`

Step 2 is manual. That is the honest price of changing nothing in production,
and it should be written into the suspension procedure rather than remembered.

## 6. Full automation, and why it is not free

The minimum production change, if the manual step is not acceptable.

### Option A — one read-only endpoint *(recommended of the three)*

```
GET /api/internal/accounts/status?ids=a,b,c
Authorization: Bearer <service secret, from the environment>
→ { "a": {"enabled": true}, "b": {"enabled": false} }
```

- Read-only. No schema change, no write, no new table. One new environment
  variable and roughly forty lines in `apps/server`.
- The worker polls it every 60s for the distinct owners of its *running* jobs
  only — a handful of ids, not a user list. `enabled: false` → revoke that
  owner's jobs via the §5 path. Any error, timeout or unparseable answer →
  **do nothing** (§4).
- It leaks nothing a suspended-account check needs to hide, and it cannot be
  used to enumerate: ids go in, booleans come out, and the caller already has
  to hold the service secret.

### Deploy risk — this is the blocker, not the code

Any change to `louver-server` means a new image, and the deploy is
`docker compose --profile https up -d --build` (`scripts/deploy-vps.sh:139`).
That **recreates the `louver` container**. FFmpeg runs as a child of the louver
process (`Command::new(&self.ffmpeg)` in `louver-core`), and compose has no
separate FFmpeg service — so recreating the container kills every encode it
spawned and **interrupts every running customer broadcast.**

The mitigation is scheduling, not engineering:

- fold Option A into a maintenance window that is already going to recreate the
  container, and announce it;
- do **not** deploy it for the beta's sake alone.

Until then, §5's manual step is the mechanism.

### Option B — a shared revocation file

Production writes disabled ids to a file the worker reads. **Rejected:** it
needs a volume shared between production and the beta VPS, which is exactly the
isolation this whole project rests on.

### Option C — production calls the worker on suspension

An outbound webhook from `set_disabled`. **Rejected:** it couples production's
admin path to the beta VPS's availability, needs a retry queue to be correct,
and a worker outage would make suspension appear to fail — turning a beta
problem into a production one.

## 7. Requirements, against this design

| requirement | status |
|---|---|
| broadcast survives the browser closing | §3 — holds today, no change needed |
| a plain logout does not stop a broadcast | §3 — holds by the renewal rule |
| suspension blocks new broadcasts | §2 — **already true**, ≤5 min, tested |
| suspension can stop a running broadcast | §5 manual, or §6 Option A automatic |
| a transient API failure stops nothing | §4 — fail-open on continuation |
| no effect on other users | §5 — keyed by owner, reuses the isolated stop path |
| possible without production change? | §1 — **not automatically**; §5 is the zero-change answer |
| minimum production change + risk | §6 — one read-only endpoint; recreate interrupts broadcasts |

## 8. Launch position

Until either the §5 runbook is accepted as the operating procedure, or §6
Option A has ridden a scheduled maintenance window, **the beta stays closed to
customers.** A suspended account whose broadcast keeps running is a real
failure, and five minutes of token life does not bound it.

## 9. Broadcast lifecycle for a 24/7 service

Three ways a broadcast can end, compared. They are not alternatives — the
recommendation is all three, in this order of precedence.

### A. Permanent until something ends it *(the default, and what ships)*

No time limit. The job runs until the source ends, the user stops it, or an
operator stops it.

- **Fits the product.** 247streams sells unattended broadcasting; a feature
  that quietly stops after a day is not that feature.
- **Matches production.** `Schedule.stop_at: None` is already the default for
  every existing broadcast.
- **The resource worry is already handled, and not by a clock.** The fear a cap
  was reaching for is "an abandoned job holds a core forever". But admission
  control already bounds that: `max_per_user` and `max_concurrent` cap how many
  encodes can exist at all, so an abandoned job costs one slot — it cannot
  grow. A time cap would not reduce the ceiling; it would only make healthy
  broadcasts fail too.
- **What it does cost** is egress: ~6.4 Mbps is ~2 TB/month per broadcast left
  running. That is a billing question for the operator, answered by C, not a
  reason to stop a customer's stream.

### B. A user-set stop time *(optional, mirror production's field)*

The user may give an absolute instant to stop at — not a duration the system
imposes.

- **Already modelled**: `Schedule.stop_at`, RFC 3339 UTC, honoured by
  `louver_cloud::schedule`. The beta should take the same shape and the same
  semantics so the two do not diverge, rather than inventing a second concept.
- **Must default to `None`.** The moment a default is anything else, A is
  broken for every user who did not ask for it.
- Worth having because a finite event — a concert, a match, a service — is a
  real case, and "remember to come back and press stop" is a bad answer to it.
- Not needed for the first VPS test; it is additive and can follow.

### C. Operator forced stop *(§5, necessary regardless)*

The admin revocation API. Needed whatever A and B do, because the cases it
covers are not time-based at all: a suspended account, abuse, a copyright
complaint, a cost runaway, a source that turned out not to be the user's.

- Stops nothing on its own, so it cannot cause an outage.
- It is the answer to "what if a broadcast should not be running" — which is a
  judgement, and judgement is exactly what a clock cannot make.

### What stops a broadcast *instead of* a clock

The thing a 24-hour cap was really trying to catch is a broadcast that has
*stopped working* but not stopped running. That already has a mechanism, and it
is time-based in the right way — seconds of no progress, not hours of
operation:

- the **watchdog** notices the picture frozen while the sound runs on
  (`VideoStalled`) or both frozen (`ProcessStalled`) — the measured FFmpeg
  defect where a vanished live source does not make FFmpeg exit;
- **`max_restarts`** bounds the retries, after which the job is `GaveUp` with a
  reason and stops.

So a dead broadcast stops within a minute or so, and a healthy one never does.
That is the correct shape, and it is already implemented and tested.

### Recommendation

| | ship it? | default |
|---|---|---|
| A — permanent | **yes, now** | the only default |
| B — user-set `stop_at` | later, additive | `None` |
| C — operator forced stop | **yes, now** (§5) | n/a |
| a system-imposed maximum duration | **no** | — |

And a naming discipline, because the confusion that produced the withdrawn cap
is easy to repeat: the 24 hours in `VPS.md` §13 is a **test duration**. It is
how long the acceptance test runs. It is not, and must not become, a maximum
broadcast length.
