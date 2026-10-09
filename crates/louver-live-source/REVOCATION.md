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

The requirement is 24 hours of broadcasting with the browser closed, and a
plain logout must not stop anything. Both follow from one rule:

> **Renewal is liveness for the UI. It is never authority over a job.**

A job's lifetime comes from its own `desired` field plus a hard cap — never
from whether a browser is still attached.

- **Browser closed** → no renewal → *nothing happens*. The worker never asked.
- **Logout** → the next renewal 401s → *nothing happens to the job*. The page
  says "signed out"; the broadcast continues.
- **Hard cap** → `MAX_JOB_SECS = 24 * 3600`, measured from a `started_at` field
  added to `WorkerState`. At the cap the worker stops the job itself and writes
  `desired = Stopped` with a reason, so a forgotten tab cannot hold an encode
  for a week.

This is the separation the brief asks for, stated as a rule rather than left to
emerge from error handling.

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

## 5. Operator-triggered revocation — zero production change

The achievable answer today. The worker gains one route:

```
POST /admin/revoke     { "user_id": "<id>" }
X-Louver-Admin: <LOUVER_LIVE_SOURCE_ADMIN_SECRET>
→ { "revoked": 2 }
```

- A **third** secret, distinct from the signing secret and the gate secret.
  Constant-time compared, env-var only, fail-closed, 32 bytes minimum — the
  same construction as [`crate::gate`]. Reusing the gate secret here would mean
  that anything able to reach the beta through Caddy could also revoke.
- **Not reachable from a browser.** Caddy's `handle` blocks match only
  `/api/live-source/*` and `/beta/*`, so `/admin/revoke` falls through to the
  production catch-all and never reaches the worker from outside. It is
  reachable only on the worker's own port — an operator on the VPS, over SSH,
  against loopback. That is the right exposure for an operator action, and it
  comes for free from the routing that already exists.
- **Effect:** look up the running jobs whose `owner` is that user, set each
  one's own stop flag, write `desired = Stopped`. This reuses the existing
  `cancel` path, which already touches exactly one job's flag per call and is
  covered by the isolation tests. Another user's job cannot be reached, because
  the map is filtered by owner exactly as `running_count_for` does.
- **Idempotent**: revoking an account with nothing running returns `0`.

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
| 24h broadcast with the browser closed | §3 — needs `MAX_JOB_SECS` + `started_at` |
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
