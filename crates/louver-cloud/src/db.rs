//! The cloud's own database.
//!
//! Separate from the desktop's schema on purpose, and the reason matters: the
//! desktop's migrations run on customers' machines. Adding `user_id` columns to
//! them would alter a schema already in the field, to serve a product those
//! machines will never run. So the cloud gets its own file and its own
//! migrations, and a *running* broadcast still uses an ordinary, unmodified
//! `louver_core::database::Database` in its own working directory — which also
//! means three broadcasts never contend for one write lock.

use crate::models::*;
use crate::{CloudError, Result};
use louver_core::database::models::EventLevel;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// A playlist long enough for a day of music and short enough to draw.
pub const MAX_PLAYLIST_ITEMS: usize = 200;

/// What the API hands [`CloudDb::replace_items`].
#[derive(Debug, Clone, serde::Deserialize)]
pub struct NewItem {
    pub media_id: String,
    #[serde(default = "crate::db::yes")]
    pub enabled: bool,
    #[serde(default = "crate::db::one")]
    pub repeat_count: i64,
}

pub(crate) fn yes() -> bool {
    true
}

pub(crate) fn one() -> i64 {
    1
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS plans (
    id      TEXT PRIMARY KEY,
    label   TEXT NOT NULL,
    limits  TEXT NOT NULL            -- JSON object of named limits
);

CREATE TABLE IF NOT EXISTS users (
    id             TEXT PRIMARY KEY,
    email          TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash  TEXT NOT NULL,
    plan_id        TEXT NOT NULL REFERENCES plans(id),
    created_at     TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS subscriptions (
    user_id    TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    plan_id    TEXT NOT NULL REFERENCES plans(id),
    status     TEXT NOT NULL DEFAULT 'active',
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS auth_sessions (
    token_hash TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_auth_user ON auth_sessions(user_id);

-- Sealed blobs. Never a plaintext secret, never a foreign key to a user:
-- the account string carries the scope.
CREATE TABLE IF NOT EXISTS credentials (
    account TEXT PRIMARY KEY,
    sealed  BLOB NOT NULL
);

CREATE TABLE IF NOT EXISTS media (
    id                     TEXT PRIMARY KEY,
    user_id                TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    filename               TEXT NOT NULL,
    size_bytes             INTEGER NOT NULL DEFAULT 0,
    state                  TEXT NOT NULL,
    duration_secs          REAL NOT NULL DEFAULT 0,
    width                  INTEGER NOT NULL DEFAULT 0,
    height                 INTEGER NOT NULL DEFAULT 0,
    fps                    REAL NOT NULL DEFAULT 0,
    video_codec            TEXT NOT NULL DEFAULT '',
    audio_codec            TEXT,
    container              TEXT NOT NULL DEFAULT '',
    bitrate_bps            INTEGER NOT NULL DEFAULT 0,
    storage_path           TEXT NOT NULL,
    prepared_path          TEXT,
    prepared_duration_secs REAL,
    last_error             TEXT,
    created_at             TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_media_user ON media(user_id);

CREATE TABLE IF NOT EXISTS stream_destinations (
    id         TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    label      TEXT NOT NULL,
    rtmps_url  TEXT NOT NULL,
    key_masked TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_dest_user ON stream_destinations(user_id);

CREATE TABLE IF NOT EXISTS broadcasts (
    id               TEXT PRIMARY KEY,
    user_id          TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name             TEXT NOT NULL,
    media_id         TEXT NOT NULL REFERENCES media(id),
    destination_id   TEXT NOT NULL REFERENCES stream_destinations(id),
    loop_forever     INTEGER NOT NULL DEFAULT 1,
    desired_state    TEXT NOT NULL DEFAULT 'stopped',
    runtime_state    TEXT NOT NULL DEFAULT 'CREATED',
    restart_count    INTEGER NOT NULL DEFAULT 0,
    last_error       TEXT,
    created_at       TEXT NOT NULL DEFAULT (datetime('now')),
    started_at       TEXT,
    stopped_at       TEXT,
    last_heartbeat   TEXT,
    bytes_sent       INTEGER NOT NULL DEFAULT 0,
    uptime_secs      INTEGER NOT NULL DEFAULT 0,
    ffmpeg_exit_code INTEGER,
    ffmpeg_pid       INTEGER
);
CREATE INDEX IF NOT EXISTS idx_broadcast_user ON broadcasts(user_id);
CREATE INDEX IF NOT EXISTS idx_broadcast_desired ON broadcasts(desired_state);

-- A connected YouTube channel. §3. No token here: the refresh and access
-- tokens are sealed in `credentials`, under this row's id.
CREATE TABLE IF NOT EXISTS youtube_accounts (
    id               TEXT PRIMARY KEY,
    user_id          TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider         TEXT NOT NULL DEFAULT 'youtube',
    channel_id       TEXT NOT NULL,
    channel_title    TEXT NOT NULL DEFAULT '',
    thumbnail_url    TEXT,
    token_expiry     TEXT,
    created_at       TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at       TEXT NOT NULL DEFAULT (datetime('now')),
    last_verified_at TEXT,
    -- One row per channel per user: reconnecting the same channel updates it
    -- rather than leaving two accounts that disagree about the tokens.
    UNIQUE (user_id, channel_id)
);
CREATE INDEX IF NOT EXISTS idx_yt_account_user ON youtube_accounts(user_id);

-- A consent attempt in flight. §15: single use, and it expires.
CREATE TABLE IF NOT EXISTS oauth_states (
    state      TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    verifier   TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    used_at    TEXT
);

-- One subscription somebody is paying for, or about to.
--
-- Separate from `subscriptions` on purpose. That table says what a user is
-- entitled to; this one says what a payment provider knows about them. They can
-- disagree legitimately: a cancelled billing subscription leaves the entitlement
-- in place until the period it paid for is over.
CREATE TABLE IF NOT EXISTS billing_subscriptions (
    -- Ours, opaque, and the value that travels in the provider's `var1`. Never
    -- a user id or an email: it comes back to us through a callback we do not
    -- control the transport of.
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    plan_id      TEXT NOT NULL REFERENCES plans(id),
    provider     TEXT NOT NULL DEFAULT 'payapp',
    -- PayApp's `rebill_no`. NULL between our INSERT and their answer.
    provider_subscription_id TEXT,
    status       TEXT NOT NULL,
    -- What we will check the callback's `price` against. In whole won.
    amount_krw   INTEGER NOT NULL,
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at   TEXT NOT NULL DEFAULT (datetime('now')),
    activated_at TEXT,
    cancelled_at TEXT,
    last_paid_at TEXT,
    -- Through when the last payment has paid for. What a cancellation at period
    -- end would be measured against; nothing acts on it automatically yet.
    current_period_end TEXT
);
CREATE INDEX IF NOT EXISTS idx_billing_user ON billing_subscriptions(user_id);
-- One `rebill_no` belongs to one record. A partial index because the column is
-- NULL until the provider answers, and several pending rows may be NULL at once.
CREATE UNIQUE INDEX IF NOT EXISTS idx_billing_rebill
    ON billing_subscriptions(provider, provider_subscription_id)
    WHERE provider_subscription_id IS NOT NULL;

-- One payment notification, recorded once.
--
-- The UNIQUE index on the provider's own event key is the whole idempotency
-- mechanism: a callback that arrives ten times inserts one row, and the nine
-- that lose the race do nothing and are answered SUCCESS.
--
-- Deliberately not the raw callback body. The provider posts our own link keys
-- back to us in it, and a table that stored them would be a table that leaked
-- them. Only these columns, all of them safe to read.
CREATE TABLE IF NOT EXISTS billing_events (
    id           TEXT PRIMARY KEY,
    provider     TEXT NOT NULL,
    provider_event_key TEXT NOT NULL,
    billing_id   TEXT,
    user_id      TEXT,
    provider_subscription_id TEXT,
    pay_state    TEXT NOT NULL,
    amount_krw   INTEGER NOT NULL,
    pay_date     TEXT,
    pay_type     TEXT,
    -- Why we did or did not act on it, in our own words.
    outcome      TEXT NOT NULL,
    processed_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_billing_event_key
    ON billing_events(provider, provider_event_key);
CREATE INDEX IF NOT EXISTS idx_billing_event_user ON billing_events(user_id, processed_at DESC);

-- One video in one broadcast's playlist. §1.
CREATE TABLE IF NOT EXISTS broadcast_items (
    id           TEXT PRIMARY KEY,
    broadcast_id TEXT NOT NULL REFERENCES broadcasts(id) ON DELETE CASCADE,
    media_id     TEXT NOT NULL REFERENCES media(id),
    position     INTEGER NOT NULL,
    enabled      INTEGER NOT NULL DEFAULT 1,
    repeat_count INTEGER NOT NULL DEFAULT 1,
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_item_broadcast ON broadcast_items(broadcast_id, position);

CREATE TABLE IF NOT EXISTS broadcast_events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    broadcast_id TEXT NOT NULL REFERENCES broadcasts(id) ON DELETE CASCADE,
    at           TEXT NOT NULL DEFAULT (datetime('now')),
    level        TEXT NOT NULL,
    message      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_event_broadcast ON broadcast_events(broadcast_id, id DESC);
"#;

/// The plan an account with no subscription is on.
///
/// A real row rather than a null `plan_id`, for two reasons. `users.plan_id` is
/// `NOT NULL` with a foreign key, and making it nullable would mean rewriting a
/// table that has production rows in it. And a plan whose every limit is zero
/// fails closed everywhere by itself: code that forgets to ask about the
/// subscription still cannot start a broadcast, because the limit it reads is 0.
pub const UNSUBSCRIBED_PLAN: &str = "none";

/// `subscriptions.status`, spelt once.
pub const SUBSCRIPTION_ACTIVE: &str = "active";
pub const SUBSCRIPTION_UNSUBSCRIBED: &str = "unsubscribed";

/// The plans the service ships with.
///
/// Rows, not constants in the code. `entitlement` reads limits by name, so a
/// fourth plan is an INSERT and never an `if`.
///
/// Prices are whole won. Money is never a float.
struct SeedPlan {
    id: &'static str,
    label: &'static str,
    monthly_price_krw: i64,
    description: &'static str,
    /// Offered on the pricing page. The unsubscribed plan is not.
    active: bool,
    sort_order: i64,
    limits: &'static [(&'static str, i64)],
}

const SEED_PLANS: &[SeedPlan] = &[
    SeedPlan {
        id: "basic",
        label: "Basic",
        monthly_price_krw: 19_900,
        description: "개인 크리에이터 / 테스트",
        active: true,
        sort_order: 1,
        limits: &[
            ("max_concurrent_streams", 1),
            ("max_broadcasts", 3),
            ("max_storage_bytes", 5 * 1024 * 1024 * 1024),
            ("max_upload_bytes", 2 * 1024 * 1024 * 1024),
            // Scheduling is a common feature of every paid plan now: a
            // broadcaster who cannot schedule cannot run 24/7 unattended, which
            // is the thing being sold.
            ("scheduling_enabled", 1),
            ("priority_recovery", 0),
        ],
    },
    SeedPlan {
        id: "pro",
        label: "Pro",
        monthly_price_krw: 39_900,
        description: "여러 채널 운영자",
        active: true,
        sort_order: 2,
        limits: &[
            ("max_concurrent_streams", 2),
            ("max_broadcasts", 10),
            ("max_storage_bytes", 10 * 1024 * 1024 * 1024),
            ("max_upload_bytes", 4 * 1024 * 1024 * 1024),
            ("scheduling_enabled", 1),
            ("priority_recovery", 0),
        ],
    },
    SeedPlan {
        id: "business",
        label: "Business",
        monthly_price_krw: 59_900,
        description: "전문 채널 / 다중 라이브 운영",
        active: true,
        sort_order: 3,
        limits: &[
            ("max_concurrent_streams", 3),
            ("max_broadcasts", 30),
            ("max_storage_bytes", 20 * 1024 * 1024 * 1024),
            ("max_upload_bytes", 8 * 1024 * 1024 * 1024),
            ("scheduling_enabled", 1),
            ("priority_recovery", 1),
        ],
    },
    SeedPlan {
        id: UNSUBSCRIBED_PLAN,
        label: "요금제 없음",
        monthly_price_krw: 0,
        description: "요금제를 선택하면 방송을 시작할 수 있습니다.",
        // Never on the pricing page: it is a state, not something to buy.
        active: false,
        sort_order: 99,
        limits: &[
            // Every one of these is zero on purpose. An account here can sign
            // in, look around and choose a plan, and can do nothing that costs
            // the server anything.
            ("max_concurrent_streams", 0),
            ("max_broadcasts", 0),
            ("max_storage_bytes", 0),
            ("max_upload_bytes", 0),
            ("scheduling_enabled", 0),
            ("priority_recovery", 0),
        ],
    },
];

#[derive(Clone)]
pub struct CloudDb {
    conn: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for CloudDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CloudDb")
    }
}

/// Add a column unless it is already there. The cloud's own small migration.
fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let mut st = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let existing: Vec<String> =
        st.query_map([], |r| r.get::<_, String>(1))?.collect::<std::result::Result<_, _>>()?;
    if existing.iter().any(|c| c == column) {
        return Ok(());
    }
    conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
    Ok(())
}

/// A signup, on its way into the database.
///
/// The name is expected to have been through [`clean_name`] and the email
/// through the API's own normaliser, both of which are where the messages a user
/// reads come from. Borrows rather than owned strings, so this is a description
/// of one insert rather than a second place the values live.
///
/// There is deliberately **no plan field**. Which plan a new account gets is the
/// server's decision, so no request body can ask for Business.
#[derive(Debug, Clone, Copy)]
pub struct Signup<'a> {
    pub name: &'a str,
    pub email: &'a str,
    pub password_hash: &'a str,
    /// Which version of the documents was agreed to.
    pub terms_version: &'a str,
}

/// Longer than any real name, short enough that a row cannot be used as storage.
///
/// Counted in characters rather than bytes: 100 bytes is 33 Korean characters,
/// which would reject names that are obviously fine.
pub const MAX_NAME_CHARS: usize = 60;

/// Trim a display name and refuse the ones that are not one. Shared by the API
/// and by anything else that ever writes this column.
pub fn clean_name(raw: &str) -> Result<String> {
    // Control characters would let a name break a log line or a terminal.
    let name: String = raw.trim().chars().filter(|c| !c.is_control()).collect();
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(CloudError::Invalid("이름을 입력해주세요.".into()));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(CloudError::Invalid(format!("이름은 {MAX_NAME_CHARS}자 이내로 입력해주세요.")));
    }
    Ok(name)
}

impl CloudDb {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        Self::from_connection(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        // WAL so a reader never blocks the manager's status writes, and
        // foreign keys on so ownership cascades actually happen.
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // A start request contends with the manager's heartbeat writes; five
        // seconds of waiting beats returning "database is locked" to a user.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        // `CREATE TABLE IF NOT EXISTS` does nothing to a table that is already
        // there, so a column added after the first release needs its own step.
        // Adding one that exists is an error, not a no-op, hence the check.
        ensure_column(&conn, "broadcasts", "ffmpeg_pid", "INTEGER")?;

        // Everything below arrived with playlists, metadata, settings and
        // schedules. A column at a time, each with the default the old rows
        // should be read as, so a database from the previous release opens and
        // keeps working rather than being migrated by hand.
        for (column, decl) in [
            // §4 metadata
            ("title", "TEXT NOT NULL DEFAULT ''"),
            ("description", "TEXT NOT NULL DEFAULT ''"),
            ("tags", "TEXT NOT NULL DEFAULT ''"),
            ("category", "TEXT NOT NULL DEFAULT ''"),
            ("privacy", "TEXT NOT NULL DEFAULT 'private'"),
            // §6 settings
            ("resolution", "TEXT NOT NULL DEFAULT 'auto'"),
            ("fps", "TEXT NOT NULL DEFAULT 'auto'"),
            ("video_bitrate_kbps", "INTEGER NOT NULL DEFAULT 0"),
            ("audio_bitrate_kbps", "INTEGER NOT NULL DEFAULT 0"),
            // §8 schedule
            ("sched_enabled", "INTEGER NOT NULL DEFAULT 0"),
            ("sched_start_at", "TEXT"),
            ("sched_stop_at", "TEXT"),
            ("sched_timezone", "TEXT NOT NULL DEFAULT 'UTC'"),
            ("sched_repeat_days", "INTEGER NOT NULL DEFAULT 0"),
            // Minutes east of UTC, captured from the browser when the schedule
            // was set. Enough to know which local day an instant falls on
            // without shipping a timezone database; the name above is what the
            // UI shows.
            ("sched_offset_minutes", "INTEGER NOT NULL DEFAULT 0"),
            ("sched_last_run_at", "TEXT"),
            // §2/§9 playlist runtime, checkpointed as it plays
            ("item_count", "INTEGER NOT NULL DEFAULT 0"),
            // Entries in one pass once repeats are expanded, and where the
            // playlist was rotated to when it was last started (§9's resume).
            ("play_count", "INTEGER NOT NULL DEFAULT 0"),
            ("playlist_offset", "INTEGER NOT NULL DEFAULT 0"),
            // §7: where this broadcast's YouTube resources are. Null on every
            // row that was ever created with a pasted stream key, which is what
            // keeps those broadcasts behaving exactly as they do now.
            ("youtube_account_id", "TEXT"),
            ("youtube_broadcast_id", "TEXT"),
            ("youtube_stream_id", "TEXT"),
            ("youtube_status", "TEXT"),
            ("youtube_error", "TEXT"),
            ("current_index", "INTEGER NOT NULL DEFAULT 0"),
            ("current_item", "TEXT"),
            ("next_item", "TEXT"),
            ("current_position_secs", "REAL NOT NULL DEFAULT 0"),
            ("current_duration_secs", "REAL NOT NULL DEFAULT 0"),
            ("cycle_duration_secs", "REAL NOT NULL DEFAULT 0"),
        ] {
            ensure_column(&conn, "broadcasts", column, decl)?;
        }
        // Pricing. `plans` held a label and a bag of limits; a paid service also
        // has to say what it costs and who it is for. Defaults on every column so
        // that a production row reads as free-and-offered until `seed_plans`
        // below writes the real figures over it.
        ensure_column(&conn, "plans", "monthly_price_krw", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(&conn, "plans", "description", "TEXT NOT NULL DEFAULT ''")?;
        ensure_column(&conn, "plans", "active", "INTEGER NOT NULL DEFAULT 1")?;
        ensure_column(&conn, "plans", "sort_order", "INTEGER NOT NULL DEFAULT 0")?;

        // Signup, which until now asked for an email and a password and nothing
        // else. All three are nullable and stay NULL on every account that
        // already exists: a display name is not an identifier, and nobody can
        // retroactively have agreed to terms.
        ensure_column(&conn, "users", "name", "TEXT")?;
        ensure_column(&conn, "users", "terms_accepted_at", "TEXT")?;
        ensure_column(&conn, "users", "privacy_accepted_at", "TEXT")?;
        // Which version of the documents was agreed to. One column now rather
        // than a table, because versioning terms is a string comparison until
        // somebody needs the history — and a NULL here reads as "before this
        // was recorded", which is the truth for every existing row.
        ensure_column(&conn, "users", "terms_version", "TEXT")?;

        // Who may run the service, and whose account is switched off.
        //
        // Both additive and both defaulted to the status quo: every existing row
        // becomes an ordinary, enabled user. There is no route that writes
        // either column — `--set-admin` and the admin API do, and nothing a
        // signup or a session can reach.
        ensure_column(&conn, "users", "role", "TEXT NOT NULL DEFAULT 'user'")?;
        ensure_column(&conn, "users", "disabled_at", "TEXT")?;

        // What an operator did to somebody else's account, and when.
        //
        // Append-only in practice: nothing in this codebase updates or deletes a
        // row. Deliberately holds *no* secret — not a password hash, not a
        // provider key, not a token — because an audit log is the one table
        // most likely to be read out loud in a support thread.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS admin_audit (
                 id          INTEGER PRIMARY KEY AUTOINCREMENT,
                 at          TEXT NOT NULL DEFAULT (datetime('now')),
                 admin_id    TEXT NOT NULL,
                 admin_email TEXT NOT NULL,
                 action      TEXT NOT NULL,
                 target_type TEXT NOT NULL,
                 target_id   TEXT NOT NULL,
                 before      TEXT,
                 after       TEXT,
                 note        TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_audit_at ON admin_audit(id DESC);
             CREATE INDEX IF NOT EXISTS idx_audit_target ON admin_audit(target_type, target_id, id DESC);",
        )?;

        // Indexes the admin console's aggregations need, and nothing else does.
        // `CREATE INDEX IF NOT EXISTS` on a table with production rows is a
        // one-off build of an index, not a rewrite.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_users_created ON users(created_at);
             CREATE INDEX IF NOT EXISTS idx_billing_event_paid
                 ON billing_events(pay_state, processed_at);
             CREATE INDEX IF NOT EXISTS idx_billing_status ON billing_subscriptions(status);
             CREATE INDEX IF NOT EXISTS idx_broadcast_runtime ON broadcasts(runtime_state);",
        )?;

        // What the prepared file actually is, and what to aim for next time.
        //
        // `prepared_signature` is the exact shape of the prepared file — codec,
        // geometry, frame rate, time base, SPS checksum, audio layout — because
        // a playlist is concatenated and stream-copied and every item has to
        // agree. NULL means a file prepared before this release: those all came
        // out of the one canonical encode, so a NULL reads as "canonical".
        //
        // `prepare_target` is how the *next* preparation should run: `auto`
        // keeps the source's own geometry when that is safe, `canonical` forces
        // the 1080p30 re-encode. It moves to `canonical` only when a playlist
        // mixes formats and the items have to be made to match.
        ensure_column(&conn, "media", "prepared_signature", "TEXT")?;
        ensure_column(&conn, "media", "prepared_mode", "TEXT")?;
        ensure_column(&conn, "media", "prepare_target", "TEXT NOT NULL DEFAULT 'auto'")?;

        // §5: which kind of destination this is. Every existing row is a stream
        // key someone pasted, which is exactly what the default says.
        ensure_column(&conn, "stream_destinations", "kind", "TEXT NOT NULL DEFAULT 'manual_rtmps'")?;
        // A destination YouTube gave us, rather than one somebody pasted.
        ensure_column(&conn, "stream_destinations", "youtube_account_id", "TEXT")?;
        ensure_column(&conn, "stream_destinations", "youtube_stream_id", "TEXT")?;

        let db = Self { conn: Arc::new(Mutex::new(conn)) };
        db.seed_plans()?;
        db.adopt_single_video_broadcasts()?;
        Ok(db)
    }

    /// Cheapest possible "is the database answering?".
    ///
    /// A real query, not a connection check: WAL recovery, a full disk and a
    /// corrupt page all present as a failing read, not as a missing handle.
    pub fn ping(&self) -> Result<()> {
        self.conn.lock().unwrap().query_row("SELECT count(*) FROM plans", [], |r| r.get::<_, i64>(0))?;
        Ok(())
    }

    /// Look a user up by the address they sign in with. For the bootstrap CLI.
    pub fn user_by_email(&self, email: &str) -> Result<User> {
        let id: Option<String> = self
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT id FROM users WHERE email=?1", [email.trim().to_lowercase()], |r| r.get(0))
            .optional()?;
        self.user(&id.ok_or(CloudError::NotFound("user"))?)
    }

    /// Every plan id the service knows, for a CLI that must not guess.
    pub fn plan_ids(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare("SELECT id FROM plans ORDER BY id")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// The connection, for the credential store to share.
    pub fn raw(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.conn)
    }

    /// Give every pre-playlist broadcast the one-item playlist it always was.
    ///
    /// A broadcast used to be a row with a `media_id`. That column is still
    /// there and still written, so nothing that reads it breaks; this adds the
    /// `broadcast_items` row the new code reads, for the broadcasts that were
    /// created before the table existed. Idempotent, and it never touches a
    /// broadcast that already has items — including one whose items were all
    /// removed on purpose.
    fn adopt_single_video_broadcasts(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let orphans: Vec<(String, String)> = {
            let mut st = conn.prepare(
                "SELECT b.id, b.media_id FROM broadcasts b
                 WHERE NOT EXISTS (SELECT 1 FROM broadcast_items i WHERE i.broadcast_id = b.id)",
            )?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (broadcast_id, media_id) in orphans {
            conn.execute(
                "INSERT INTO broadcast_items (id, broadcast_id, media_id, position, enabled, repeat_count)
                 VALUES (?1, ?2, ?3, 0, 1, 1)",
                params![crate::new_id(), broadcast_id, media_id],
            )?;
            conn.execute(
                "UPDATE broadcasts SET item_count = 1, title = CASE WHEN title = '' THEN name ELSE title END
                 WHERE id = ?1",
                [&broadcast_id],
            )?;
        }
        Ok(())
    }

    fn seed_plans(&self) -> Result<()> {
        let c = self.conn.lock().unwrap();
        for p in SEED_PLANS {
            let map: BTreeMap<&str, i64> = p.limits.iter().copied().collect();
            let json = serde_json::to_string(&map).unwrap_or_else(|_| "{}".into());
            // A plan's *limits* are still left alone on conflict: an operator may
            // have raised one for a customer, and a restart must not undo that.
            //
            // Its price and its description are not, and must not be. They
            // arrived after these rows existed, so a production database has a
            // Basic row with no price in it; `DO NOTHING` would leave the pricing
            // page showing ₩0 for ever. What the service charges is the
            // service's to state, not a per-row edit to preserve.
            c.execute(
                "INSERT INTO plans (id, label, limits, monthly_price_krw, description, active, sort_order)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                     monthly_price_krw = excluded.monthly_price_krw,
                     description       = excluded.description,
                     active            = excluded.active,
                     sort_order        = excluded.sort_order",
                params![
                    p.id,
                    p.label,
                    json,
                    p.monthly_price_krw,
                    p.description,
                    p.active as i64,
                    p.sort_order
                ],
            )?;
        }
        Ok(())
    }

    // --- plans ------------------------------------------------------------

    /// What each plan's storage limits are now, against what this build seeds.
    ///
    /// Exists because `seed_plans` deliberately does **not** overwrite a plan's
    /// limits: an operator may have raised one for a customer and a restart must
    /// not undo that. The consequence is that changing a number in `SEED_PLANS`
    /// changes nothing for a database that already has these rows — so lowering
    /// the storage ceilings to fit an 80 GB server needs somebody to look at the
    /// difference and say yes. This is the looking.
    pub fn storage_audit(&self) -> Result<Vec<StorageAudit>> {
        let mut out = Vec::new();
        for p in SEED_PLANS {
            let target: BTreeMap<&str, i64> = p.limits.iter().copied().collect();
            let Ok(current) = self.plan(p.id) else { continue };
            out.push(StorageAudit {
                plan_id: p.id.to_string(),
                label: current.label.clone(),
                monthly_price_krw: current.monthly_price_krw,
                concurrent_streams: current.max_concurrent_streams(),
                storage_now: current.limits.get(crate::entitlement::MAX_STORAGE_BYTES).copied().unwrap_or(0),
                storage_target: target.get(crate::entitlement::MAX_STORAGE_BYTES).copied().unwrap_or(0),
                upload_now: current.limits.get(crate::entitlement::MAX_UPLOAD_BYTES).copied().unwrap_or(0),
                upload_target: target.get(crate::entitlement::MAX_UPLOAD_BYTES).copied().unwrap_or(0),
            });
        }
        Ok(out)
    }

    /// Who is storing how much, against what the new ceiling would be.
    ///
    /// The question an operator has to answer before lowering a limit: whose
    /// account is already over it. Nobody's files are touched by the answer —
    /// over the ceiling only blocks the *next* upload.
    pub fn storage_usage(&self) -> Result<Vec<UsageRow>> {
        let targets: BTreeMap<&str, i64> = SEED_PLANS
            .iter()
            .map(|p| {
                let m: BTreeMap<&str, i64> = p.limits.iter().copied().collect();
                (p.id, m.get(crate::entitlement::MAX_STORAGE_BYTES).copied().unwrap_or(0))
            })
            .collect();
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare(
            "SELECT u.email, u.plan_id, COALESCE(SUM(m.size_bytes), 0)
             FROM users u LEFT JOIN media m ON m.user_id = u.id
             GROUP BY u.id ORDER BY 3 DESC",
        )?;
        let rows =
            st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (email, plan_id, used) = row?;
            let ceiling_after = targets.get(plan_id.as_str()).copied().unwrap_or(0);
            out.push(UsageRow { email, plan_id, used_bytes: used, ceiling_after });
        }
        Ok(out)
    }

    /// Write this build's storage limits into `plans`, and nothing else.
    ///
    /// Reads each row's own `limits`, replaces exactly the two storage keys and
    /// writes it back, so `max_concurrent_streams`, `max_broadcasts` and
    /// anything an operator added survive untouched. The `monthly_price_krw`
    /// column is not in the statement at all: there is no arrangement of this
    /// call that can change a price.
    ///
    /// Deliberately not called at boot. See `storage_audit`.
    pub fn apply_seed_storage_limits(&self) -> Result<Vec<String>> {
        let mut changed = Vec::new();
        for p in SEED_PLANS {
            let target: BTreeMap<&str, i64> = p.limits.iter().copied().collect();
            let Ok(current) = self.plan(p.id) else { continue };
            let mut limits = current.limits.clone();
            let mut moved = false;
            for key in [crate::entitlement::MAX_STORAGE_BYTES, crate::entitlement::MAX_UPLOAD_BYTES] {
                let want = target.get(key).copied().unwrap_or(0);
                if limits.get(key).copied() != Some(want) {
                    limits.insert(key.to_string(), want);
                    moved = true;
                }
            }
            if !moved {
                continue;
            }
            let json = serde_json::to_string(&limits)
                .map_err(|e| CloudError::Invalid(format!("한도를 저장할 수 없습니다: {e}")))?;
            self.conn
                .lock()
                .unwrap()
                .execute("UPDATE plans SET limits = ?2 WHERE id = ?1", params![p.id, json])?;
            changed.push(p.id.to_string());
        }
        Ok(changed)
    }

    pub fn plan(&self, id: &str) -> Result<Plan> {
        self.conn
            .lock()
            .unwrap()
            .query_row(&format!("{PLAN_COLUMNS} WHERE id=?1"), [id], row_to_plan)
            .optional()?
            .ok_or(CloudError::NotFound("plan"))
    }

    /// The plans a visitor may buy, in the order the pricing page shows them.
    ///
    /// Filtered on `active` rather than on a list of names, so the unsubscribed
    /// plan — and any internal one an operator adds later — stays off the public
    /// page without this function knowing they exist.
    pub fn plans_for_sale(&self) -> Result<Vec<Plan>> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare(&format!("{PLAN_COLUMNS} WHERE active = 1 ORDER BY sort_order, id"))?;
        let rows = st.query_map([], row_to_plan)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // --- users ------------------------------------------------------------

    /// An account with no display name and no recorded consent.
    ///
    /// The bootstrap CLI's path, and every test's. Kept at this exact signature
    /// on purpose: a deploy script that creates the operator's account must not
    /// need a name it has no way to ask for, and consent is something a person
    /// gives, not something a shell script can give on their behalf.
    pub fn create_user(&self, email: &str, password_hash: &str, plan_id: &str) -> Result<User> {
        self.insert_user(None, email, password_hash, plan_id, None, SUBSCRIPTION_ACTIVE)
    }

    /// An account made by somebody filling in the signup form.
    ///
    /// Lands on the unsubscribed plan, always. There is no plan argument at all,
    /// which is a stronger guarantee than validating one: signing up cannot grant
    /// an entitlement because there is no parameter through which it could.
    /// Paying for a plan goes through [`CloudDb::activate_subscription`].
    pub fn register_user(&self, s: &Signup<'_>) -> Result<User> {
        self.insert_user(
            Some(s.name),
            s.email,
            s.password_hash,
            UNSUBSCRIBED_PLAN,
            Some(s.terms_version),
            SUBSCRIPTION_UNSUBSCRIBED,
        )
    }

    /// The user row and its subscription, in one transaction.
    ///
    /// Together, because an account with no subscription row is an account whose
    /// plan lookups fall back to a default — recoverable, but only after
    /// somebody notices. Two statements that must both land are a transaction.
    ///
    /// `consented_to` is `Some(version)` only for a real signup, and the
    /// timestamps come from SQLite's own clock: a client that could send its own
    /// `accepted_at` could claim to have agreed last year.
    fn insert_user(
        &self,
        name: Option<&str>,
        email: &str,
        password_hash: &str,
        plan_id: &str,
        consented_to: Option<&str>,
        status: &str,
    ) -> Result<User> {
        let id = crate::new_id();
        let conn = self.conn.clone();
        let mut guard = conn.lock().unwrap();
        let tx = guard.transaction()?;
        tx.execute(
            "INSERT INTO users (id, email, password_hash, plan_id, name,
                                terms_accepted_at, privacy_accepted_at, terms_version)
             VALUES (?1, ?2, ?3, ?4, ?5,
                     CASE WHEN ?6 IS NULL THEN NULL ELSE datetime('now') END,
                     CASE WHEN ?6 IS NULL THEN NULL ELSE datetime('now') END,
                     ?6)",
            params![id, email.trim(), password_hash, plan_id, name, consented_to],
        )
        .map_err(|e| match e {
            // The UNIQUE index is what decides, not a SELECT before the insert:
            // two simultaneous signups for one address both pass a check and
            // only one can pass this.
            rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
                CloudError::EmailTaken
            }
            other => CloudError::Db(other),
        })?;
        tx.execute(
            "INSERT INTO subscriptions (user_id, plan_id, status) VALUES (?1, ?2, ?3)",
            params![id, plan_id, status],
        )?;
        tx.commit()?;
        drop(guard);
        self.user(&id)
    }

    pub fn user(&self, id: &str) -> Result<User> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT id, email, plan_id, created_at, name, terms_accepted_at, privacy_accepted_at,
                        COALESCE(role, 'user'), disabled_at
                 FROM users WHERE id=?1",
                [id],
                |r| {
                    Ok(User {
                        id: r.get(0)?,
                        email: r.get(1)?,
                        plan_id: r.get(2)?,
                        created_at: r.get(3)?,
                        // NULL on every account that existed before signup asked
                        // for a name. Readers show the email instead.
                        name: r.get(4)?,
                        terms_accepted_at: r.get(5)?,
                        privacy_accepted_at: r.get(6)?,
                        role: r.get(7)?,
                        disabled_at: r.get(8)?,
                    })
                },
            )
            .optional()?
            .ok_or(CloudError::NotFound("user"))
    }

    /// The stored hash for an email, for login to check against.
    pub fn password_hash_for(&self, email: &str) -> Result<(String, String)> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT id, password_hash FROM users WHERE email=?1 COLLATE NOCASE",
                [email.trim()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(CloudError::BadCredentials)
    }

    /// What this user is entitled to, and why. Also `GET /api/me/subscription`.
    ///
    /// Two conditions, and both have to hold: the status says active, **and** the
    /// plan grants something. Either one alone would be a hole — an account
    /// parked on the unsubscribed plan must not become entitled because a status
    /// column says `active`, and a Business plan must not keep working after the
    /// subscription behind it is cancelled.
    ///
    /// A missing `subscriptions` row reads as active, which is what keeps every
    /// account made before this existed working exactly as it did. That is safe
    /// precisely because of the second condition: the unsubscribed plan grants
    /// nothing, so "active with no row" cannot conjure an entitlement.
    pub fn subscription(&self, user_id: &str) -> Result<Subscription> {
        let u = self.user(user_id)?;
        let plan = self.plan(&u.plan_id)?;
        let status: String = self
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT status FROM subscriptions WHERE user_id=?1", [user_id], |r| r.get(0))
            .optional()?
            .unwrap_or_else(|| SUBSCRIPTION_ACTIVE.into());

        let active = status == SUBSCRIPTION_ACTIVE && plan.can_broadcast();
        Ok(Subscription {
            user_id: u.id,
            plan_id: plan.id.clone(),
            plan_label: plan.label.clone(),
            // Whatever the column says, an account on the unsubscribed plan is
            // unsubscribed. Reporting `active` there would make the dashboard
            // offer a broadcast the server would then refuse.
            status: if active { status } else { SUBSCRIPTION_UNSUBSCRIBED.to_string() },
            limits: plan.limits.clone(),
            active,
            // `None` is what lets a client tell "no plan" from "Basic" without
            // comparing against a plan id it would have to hard-code.
            plan: active.then_some(plan),
        })
    }

    /// Refuse anything that costs the server money when there is no subscription.
    ///
    /// The one gate, called from the places that spend resources. Its error names
    /// nothing internal and tells the user what to do about it.
    pub fn require_active_subscription(&self, user_id: &str) -> Result<Subscription> {
        // Belt as well as braces. Disabling an account already deletes its
        // sessions and stops its broadcasts, but this is the gate every path
        // that spends the server's resources goes through — START, the
        // scheduler, boot recovery — and a switched-off account must not get
        // past any of them.
        self.require_enabled(user_id)?;
        let sub = self.subscription(user_id)?;
        if !sub.active {
            return Err(CloudError::NoSubscription);
        }
        Ok(sub)
    }

    // --- billing (PayApp) --------------------------------------------------

    /// Reserve a billing record before the provider is told anything.
    ///
    /// The row has to exist first because its id is what travels in the
    /// provider's `var1` and comes back in the callback. `status = pending`, which
    /// grants nothing: a record here is a payment that has been *asked for*.
    pub fn open_billing_subscription(
        &self,
        user_id: &str,
        plan_id: &str,
        provider: &str,
        amount_krw: i64,
    ) -> Result<BillingSubscription> {
        let id = crate::new_id();
        self.conn.lock().unwrap().execute(
            "INSERT INTO billing_subscriptions (id, user_id, plan_id, provider, status, amount_krw)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, user_id, plan_id, provider, BillingStatus::Pending.id(), amount_krw],
        )?;
        self.billing_subscription(&id)
    }

    pub fn billing_subscription(&self, id: &str) -> Result<BillingSubscription> {
        self.conn
            .lock()
            .unwrap()
            .query_row(&format!("{BILLING_COLUMNS} WHERE id = ?1"), [id], row_to_billing)
            .optional()?
            .ok_or(CloudError::NotFound("billing subscription"))
    }

    /// A billing record that belongs to this caller, or nothing.
    ///
    /// The owner check is in the SQL, so knowing somebody else's billing id buys
    /// nothing — the same rule every other `*_owned` reader here follows.
    pub fn billing_subscription_owned(&self, user_id: &str, id: &str) -> Result<BillingSubscription> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                &format!("{BILLING_COLUMNS} WHERE id = ?1 AND user_id = ?2"),
                params![id, user_id],
                row_to_billing,
            )
            .optional()?
            .ok_or(CloudError::NotFound("billing subscription"))
    }

    /// This user's billing records, newest first.
    pub fn billing_subscriptions_for(&self, user_id: &str) -> Result<Vec<BillingSubscription>> {
        let conn = self.conn.lock().unwrap();
        let mut st =
            conn.prepare(&format!("{BILLING_COLUMNS} WHERE user_id = ?1 ORDER BY created_at DESC"))?;
        let rows = st.query_map([user_id], row_to_billing)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// The one record the provider is still on the hook for, if there is one.
    ///
    /// §14's gate. At most one of these should ever exist per user, and the
    /// checkout path refuses to create a second.
    pub fn live_billing_subscription(&self, user_id: &str) -> Result<Option<BillingSubscription>> {
        Ok(self.billing_subscriptions_for(user_id)?.into_iter().find(|b| b.status.holds_the_provider()))
    }

    /// Store the provider's own reference for a record we just registered.
    pub fn attach_provider_subscription(&self, id: &str, provider_subscription_id: &str) -> Result<()> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE billing_subscriptions
             SET provider_subscription_id = ?2, updated_at = datetime('now')
             WHERE id = ?1",
            params![id, provider_subscription_id],
        )?;
        if n == 0 {
            return Err(CloudError::NotFound("billing subscription"));
        }
        Ok(())
    }

    pub fn set_billing_status(&self, id: &str, status: BillingStatus) -> Result<()> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE billing_subscriptions
             SET status = ?2,
                 updated_at = datetime('now'),
                 cancelled_at = CASE WHEN ?2 IN ('cancelled', 'cancel_at_period_end')
                                     THEN COALESCE(cancelled_at, datetime('now'))
                                     ELSE cancelled_at END
             WHERE id = ?1",
            params![id, status.id()],
        )?;
        if n == 0 {
            return Err(CloudError::NotFound("billing subscription"));
        }
        Ok(())
    }

    /// End a billing subscription and take back the entitlement it paid for, in
    /// one transaction.
    ///
    /// The whole reason this is one function rather than two calls: the two halves
    /// must not be separable. Half of it landing would leave either an entitlement
    /// nobody is paying for, or — worse — a cancelled entitlement whose recurring
    /// payment is still running at the provider.
    ///
    /// **Only the entitlement this record actually granted is taken back.** The
    /// test is `users.plan_id == billing.plan_id`, plus the record having been paid
    /// at least once (`activated_at`). So:
    ///
    /// * an account the operator put on Business by hand, with no billing record,
    ///   is never reached — there is nothing to cancel;
    /// * an account that pays for Basic *and* was granted Business by the operator
    ///   keeps the Business when the Basic is cancelled, because the plans differ;
    /// * a registration that was never paid revokes nothing, because it granted
    ///   nothing.
    ///
    /// Returns the record as it now reads, and whether an entitlement was taken.
    pub fn cancel_billing_and_revoke(&self, billing_id: &str) -> Result<(BillingSubscription, bool)> {
        let conn = self.conn.clone();
        let mut guard = conn.lock().unwrap();
        let tx = guard.transaction()?;

        let (user_id, plan_id, activated_at): (String, String, Option<String>) = tx
            .query_row(
                "SELECT user_id, plan_id, activated_at FROM billing_subscriptions WHERE id = ?1",
                [billing_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .ok_or(CloudError::NotFound("billing subscription"))?;

        tx.execute(
            "UPDATE billing_subscriptions
             SET status = ?2,
                 updated_at = datetime('now'),
                 cancelled_at = COALESCE(cancelled_at, datetime('now'))
             WHERE id = ?1",
            params![billing_id, BillingStatus::Cancelled.id()],
        )?;

        // Nothing was ever paid, so nothing was ever granted by this record.
        let mut revoked = false;
        if activated_at.is_some() {
            // `AND plan_id = ?2` is the guard. Without it this would revoke
            // whatever the account happens to be on, including a plan an operator
            // granted deliberately and a different subscription paid for.
            let moved = tx.execute(
                "UPDATE users SET plan_id = ?3 WHERE id = ?1 AND plan_id = ?2",
                params![user_id, plan_id, UNSUBSCRIBED_PLAN],
            )?;
            if moved > 0 {
                tx.execute(
                    "INSERT INTO subscriptions (user_id, plan_id, status) VALUES (?1, ?2, ?3)
                     ON CONFLICT(user_id) DO UPDATE SET plan_id = excluded.plan_id,
                                                        status = excluded.status,
                                                        updated_at = datetime('now')",
                    params![user_id, UNSUBSCRIBED_PLAN, SUBSCRIPTION_UNSUBSCRIBED],
                )?;
                revoked = true;
            }
        }
        tx.commit()?;
        drop(guard);
        Ok((self.billing_subscription(billing_id)?, revoked))
    }

    /// Accounts whose provider subscription is over but whose entitlement is not.
    ///
    /// For `louver-server --audit-billing`. This state was reachable under the old
    /// policy, which kept the entitlement until the end of the period already paid
    /// for; under the current one it should never appear again, and an entry here
    /// means either a row from before the change or something that went wrong
    /// halfway.
    ///
    /// Narrow on purpose. It joins the billing record to the entitlement and
    /// requires the plans to be the same one, so an operator-granted plan is not
    /// in the answer — the fix must never be "set every Basic to none".
    pub fn billing_mismatches(&self) -> Result<Vec<BillingMismatch>> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare(
            "SELECT b.id, b.user_id, u.email, b.plan_id, b.status, b.cancelled_at,
                    COALESCE(s.status, ?1) AS entitlement_status
             FROM billing_subscriptions b
             JOIN users u ON u.id = b.user_id
             LEFT JOIN subscriptions s ON s.user_id = b.user_id
             WHERE b.status IN ('cancelled', 'cancel_at_period_end')
               -- Only a record that actually paid for something can have granted
               -- the entitlement that is still standing.
               AND b.activated_at IS NOT NULL
               -- The entitlement has to be the one this record bought.
               AND u.plan_id = b.plan_id
               AND u.plan_id <> ?2
               AND COALESCE(s.status, ?1) = ?1
             ORDER BY b.cancelled_at DESC",
        )?;
        let rows = st.query_map(params![SUBSCRIPTION_ACTIVE, UNSUBSCRIBED_PLAN], |r| {
            Ok(BillingMismatch {
                billing_id: r.get(0)?,
                user_id: r.get(1)?,
                email: r.get(2)?,
                plan_id: r.get(3)?,
                billing_status: r.get(4)?,
                cancelled_at: r.get(5)?,
                entitlement_status: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Bring one mismatched account in line with the provider. §4.
    ///
    /// Refuses anything [`CloudDb::billing_mismatches`] does not list, so a typo
    /// cannot take a plan off an account that is paying for it or was given it by
    /// the operator. No provider call: the recurring payment was already cancelled,
    /// which is what put the row in this state.
    pub fn fix_billing_mismatch(&self, billing_id: &str) -> Result<BillingMismatch> {
        let row =
            self.billing_mismatches()?.into_iter().find(|m| m.billing_id == billing_id).ok_or_else(|| {
                CloudError::Invalid("이 billing 기록은 '해지됐지만 권한이 남아 있는' 상태가 아닙니다".into())
            })?;
        let (_, revoked) = self.cancel_billing_and_revoke(billing_id)?;
        if !revoked {
            return Err(CloudError::Invalid("권한을 회수하지 못했습니다".into()));
        }
        Ok(row)
    }

    /// The record a provider callback is about, found by our own opaque id.
    ///
    /// By `var1`, not by the user id or the email the callback carries: those
    /// arrive over a transport we do not control, and a lookup by them would be a
    /// lookup by something an attacker chooses.
    pub fn billing_subscription_for_order(
        &self,
        provider: &str,
        order_id: &str,
    ) -> Result<BillingSubscription> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                &format!("{BILLING_COLUMNS} WHERE id = ?1 AND provider = ?2"),
                params![order_id, provider],
                row_to_billing,
            )
            .optional()?
            .ok_or(CloudError::NotFound("billing subscription"))
    }

    /// Record a payment notification, exactly once.
    ///
    /// Returns `true` when this call is the one that inserted it, and `false` when
    /// the provider has sent this event before. The whole of the idempotency lives
    /// in the UNIQUE index: ten simultaneous callbacks all run this, one inserts,
    /// nine are told `false` and do nothing.
    ///
    /// Takes the ledger row and the status change together in one transaction, so
    /// a crash between them cannot leave a payment recorded but unapplied, or an
    /// entitlement granted with nothing to show for it.
    pub fn record_billing_payment(&self, p: &BillingPayment<'_>) -> Result<bool> {
        let conn = self.conn.clone();
        let mut guard = conn.lock().unwrap();
        let tx = guard.transaction()?;
        let inserted = tx.execute(
            "INSERT INTO billing_events
                 (id, provider, provider_event_key, billing_id, user_id,
                  provider_subscription_id, pay_state, amount_krw, pay_date, pay_type, outcome)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(provider, provider_event_key) DO NOTHING",
            params![
                crate::new_id(),
                p.provider,
                p.event_key,
                p.billing_id,
                p.user_id,
                p.provider_subscription_id,
                p.pay_state,
                p.amount_krw,
                p.pay_date,
                p.pay_type,
                p.outcome,
            ],
        )?;
        if inserted == 0 {
            // Already seen. Nothing else may run: re-applying is the bug this
            // whole arrangement exists to prevent.
            return Ok(false);
        }
        if let Some(status) = p.status {
            tx.execute(
                "UPDATE billing_subscriptions
                 SET status = ?2,
                     updated_at = datetime('now'),
                     activated_at = CASE WHEN ?2 = 'active' THEN COALESCE(activated_at, datetime('now'))
                                         ELSE activated_at END,
                     last_paid_at = COALESCE(?3, last_paid_at),
                     current_period_end = COALESCE(?4, current_period_end)
                 WHERE id = ?1",
                params![p.billing_id, status.id(), p.paid_at, p.period_end],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// This user's payment notifications, newest first. For support questions.
    pub fn billing_events_for(&self, user_id: &str, limit: i64) -> Result<Vec<BillingEventRow>> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare(
            "SELECT provider, provider_event_key, pay_state, amount_krw, pay_date, pay_type,
                    outcome, processed_at
             FROM billing_events WHERE user_id = ?1 ORDER BY processed_at DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![user_id, limit], |r| {
            Ok(BillingEventRow {
                provider: r.get(0)?,
                event_key: r.get(1)?,
                pay_state: r.get(2)?,
                amount_krw: r.get(3)?,
                pay_date: r.get(4)?,
                pay_type: r.get(5)?,
                outcome: r.get(6)?,
                processed_at: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Every account, with enough to tell how it came to be on its plan.
    ///
    /// For `louver-server --audit-plans`. The distinguishing signal is
    /// `terms_accepted_at`: only the public signup form records it, so an account
    /// that has one came through the browser, and one that does not was made by
    /// the bootstrap CLI — which is how the operator's own account was made.
    pub fn plan_audit(&self) -> Result<Vec<PlanAudit>> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare(
            "SELECT u.id, u.email, u.plan_id,
                    COALESCE(s.status, ?1) AS status,
                    u.terms_accepted_at IS NOT NULL AS from_signup,
                    u.created_at
             FROM users u LEFT JOIN subscriptions s ON s.user_id = u.id
             ORDER BY u.created_at, u.email",
        )?;
        let rows = st.query_map([SUBSCRIPTION_ACTIVE], |r| {
            Ok(PlanAudit {
                user_id: r.get(0)?,
                email: r.get(1)?,
                plan_id: r.get(2)?,
                status: r.get(3)?,
                from_public_signup: r.get::<_, i64>(4)? != 0,
                created_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Move one account off an entitlement nobody paid for.
    ///
    /// The reverse of the accident: a release before paid plans put every public
    /// signup on `LOUVER_DEFAULT_PLAN`, and those accounts kept a free Basic when
    /// the paid plans arrived. This is how an operator takes it back, one account
    /// at a time and only after seeing the list.
    ///
    /// Deliberately not a boot-time migration. It removes an entitlement somebody
    /// is currently using, and that is not a thing to do silently to a production
    /// database while nobody is looking.
    ///
    /// Refuses anything [`PlanAudit::is_unpaid_grant`] would not flag, so the
    /// operator's own account — made by the CLI, with no consent timestamp —
    /// cannot be revoked through here even by a typo.
    ///
    /// **When payments exist, this must also refuse an account with a billing
    /// record.** Nobody has paid yet, so there is nothing to check for; the day
    /// there is, the check belongs here.
    pub fn revoke_unpaid_grant(&self, user_id: &str) -> Result<()> {
        let row = self
            .plan_audit()?
            .into_iter()
            .find(|r| r.user_id == user_id)
            .ok_or(CloudError::NotFound("user"))?;
        if !row.is_unpaid_grant(UNSUBSCRIBED_PLAN) {
            return Err(CloudError::Invalid(format!(
                "{} 계정은 자동 부여된 요금제가 아닙니다 (plan={}, 가입경로={})",
                row.email,
                row.plan_id,
                if row.from_public_signup { "회원가입" } else { "관리자" }
            )));
        }
        self.write_plan(user_id, UNSUBSCRIBED_PLAN, SUBSCRIPTION_UNSUBSCRIBED)
    }

    /// Put a user on a plan, and mark the subscription active. §13.
    ///
    /// **Not reachable from a browser, by design.** There is no route that calls
    /// this; the only callers are the bootstrap CLI and, when it exists, a
    /// payment webhook that has already verified a payment. That is the whole
    /// reason it is a function here rather than a handler: adding the webhook
    /// means calling this, and never means opening a door.
    pub fn activate_subscription(&self, user_id: &str, plan_id: &str) -> Result<Subscription> {
        // Both checked before anything is written: an unknown plan id, or the
        // unsubscribed plan, would otherwise leave an account "active" on
        // something that grants nothing.
        let plan = self.plan(plan_id)?;
        if !plan.can_broadcast() {
            return Err(CloudError::Invalid(format!("'{plan_id}' 요금제로는 구독을 활성화할 수 없습니다")));
        }
        self.user(user_id)?;
        self.write_plan(user_id, plan_id, SUBSCRIPTION_ACTIVE)?;
        self.subscription(user_id)
    }

    /// End a subscription, leaving everything the user owns in place. §13.
    ///
    /// The account goes to the unsubscribed plan, so nothing can be started and
    /// nothing is deleted: the videos, the destinations, the connected YouTube
    /// account and the broadcast rows are all still there for when they come
    /// back. Running broadcasts are not killed here — stopping somebody
    /// mid-stream is a decision for the caller that knows why.
    pub fn cancel_subscription(&self, user_id: &str) -> Result<Subscription> {
        self.user(user_id)?;
        self.write_plan(user_id, UNSUBSCRIBED_PLAN, SUBSCRIPTION_UNSUBSCRIBED)?;
        self.subscription(user_id)
    }

    /// The plan a user is on, as the CLI sets it.
    ///
    /// Kept at this signature because `--create-user` and the deploy script use
    /// it to put the operator's account on Business. It marks the subscription
    /// active, which is what it always did.
    pub fn set_plan(&self, user_id: &str, plan_id: &str) -> Result<()> {
        self.write_plan(user_id, plan_id, SUBSCRIPTION_ACTIVE)
    }

    /// Both halves of "what plan is this account on", in one transaction.
    ///
    /// `users.plan_id` is what `entitlement` reads and `subscriptions` is what
    /// the status comes from. Half of this landing would leave an account whose
    /// plan and status disagree, which is the one state nothing else here knows
    /// how to interpret.
    fn write_plan(&self, user_id: &str, plan_id: &str, status: &str) -> Result<()> {
        let conn = self.conn.clone();
        let mut guard = conn.lock().unwrap();
        let tx = guard.transaction()?;
        tx.execute("UPDATE users SET plan_id=?2 WHERE id=?1", params![user_id, plan_id])?;
        tx.execute(
            "INSERT INTO subscriptions (user_id, plan_id, status) VALUES (?1, ?2, ?3)
             ON CONFLICT(user_id) DO UPDATE SET plan_id=excluded.plan_id,
                                                status=excluded.status,
                                                updated_at=datetime('now')",
            params![user_id, plan_id, status],
        )?;
        tx.commit()?;
        Ok(())
    }

    // --- auth sessions ----------------------------------------------------

    pub fn create_auth_session(&self, user_id: &str, token_hash: &str, days: i64) -> Result<()> {
        // SQLite wants one sign, not two: `+-1 days` is not a modifier, it
        // yields NULL, and the NOT NULL constraint then fires. A negative
        // number already carries its own sign.
        let modifier = if days < 0 { format!("{days} days") } else { format!("+{days} days") };
        self.conn.lock().unwrap().execute(
            "INSERT INTO auth_sessions (token_hash, user_id, expires_at)
             VALUES (?1, ?2, datetime('now', ?3))",
            params![token_hash, user_id, modifier],
        )?;
        Ok(())
    }

    /// The user a token belongs to, if it exists and has not expired.
    pub fn user_for_token(&self, token_hash: &str) -> Result<String> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT user_id FROM auth_sessions
                 WHERE token_hash=?1 AND expires_at > datetime('now')",
                [token_hash],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(CloudError::Forbidden)
    }

    pub fn delete_auth_session(&self, token_hash: &str) -> Result<()> {
        self.conn.lock().unwrap().execute("DELETE FROM auth_sessions WHERE token_hash=?1", [token_hash])?;
        Ok(())
    }
}

// --- media, destinations, broadcasts ---------------------------------------
//
// Every read takes the owner as well as the id. §15 asks that knowing another
// user's broadcast id be useless, and the way to guarantee that is to make it
// impossible to write a query that forgets: the `*_owned` functions are what
// the API layer calls, and they filter on `user_id` in SQL rather than checking
// afterwards in Rust.

impl CloudDb {
    pub fn create_media(
        &self,
        user_id: &str,
        filename: &str,
        size_bytes: i64,
        storage_path: &str,
    ) -> Result<CloudMedia> {
        let id = crate::new_id();
        self.raw().lock().unwrap().execute(
            "INSERT INTO media (id, user_id, filename, size_bytes, state, storage_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, user_id, filename, size_bytes, MediaState::Uploaded.id(), storage_path],
        )?;
        self.media_owned(user_id, &id)
    }

    pub fn media_owned(&self, user_id: &str, id: &str) -> Result<CloudMedia> {
        self.raw()
            .lock()
            .unwrap()
            .query_row(
                "SELECT id, user_id, filename, size_bytes, state, duration_secs, width, height,
                        fps, video_codec, audio_codec, container, bitrate_bps, storage_path,
                        prepared_path, prepared_duration_secs, last_error, created_at
                 FROM media WHERE id=?1 AND user_id=?2",
                params![id, user_id],
                row_to_media,
            )
            .optional()?
            .ok_or(CloudError::NotFound("media"))
    }

    pub fn media_for(&self, user_id: &str) -> Result<Vec<CloudMedia>> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare(
            "SELECT id, user_id, filename, size_bytes, state, duration_secs, width, height,
                    fps, video_codec, audio_codec, container, bitrate_bps, storage_path,
                    prepared_path, prepared_duration_secs, last_error, created_at
             FROM media WHERE user_id=?1 ORDER BY created_at DESC, id DESC",
        )?;
        let rows = st.query_map([user_id], row_to_media)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Write what the probe found, and mark the file ready or not.
    #[allow(clippy::too_many_arguments)]
    pub fn record_media_analysis(&self, id: &str, info: &louver_core::media::probe::MediaInfo) -> Result<()> {
        self.raw().lock().unwrap().execute(
            "UPDATE media SET state=?2, duration_secs=?3, width=?4, height=?5, fps=?6,
                    video_codec=?7, audio_codec=?8, container=?9, bitrate_bps=?10
             WHERE id=?1",
            params![
                id,
                MediaState::Preparing.id(),
                info.duration_secs,
                info.width,
                info.height,
                info.fps,
                info.video_codec,
                info.audio_codec,
                info.container,
                info.video_bitrate.unwrap_or(0) as i64,
            ],
        )?;
        Ok(())
    }

    pub fn record_media_prepared(
        &self,
        id: &str,
        prepared_path: &str,
        duration_secs: f64,
        total_bytes: i64,
    ) -> Result<()> {
        self.raw().lock().unwrap().execute(
            "UPDATE media SET state=?2, prepared_path=?3, prepared_duration_secs=?4,
                    size_bytes=?5, last_error=NULL
             WHERE id=?1",
            params![id, MediaState::Ready.id(), prepared_path, duration_secs, total_bytes],
        )?;
        Ok(())
    }

    /// Record what the prepared file turned out to be.
    ///
    /// Written from a probe of the *output*, not from what was asked for: the
    /// only signature worth comparing is the one the file actually has.
    pub fn record_prepared_signature(&self, id: &str, mode: &str, signature: &str) -> Result<()> {
        self.raw().lock().unwrap().execute(
            "UPDATE media SET prepared_mode=?2, prepared_signature=?3 WHERE id=?1",
            params![id, mode, signature],
        )?;
        Ok(())
    }

    /// How the next preparation of this media should run: `auto` or `canonical`.
    pub fn prepare_target(&self, id: &str) -> Result<String> {
        Ok(self
            .raw()
            .lock()
            .unwrap()
            .query_row("SELECT COALESCE(prepare_target, 'auto') FROM media WHERE id=?1", [id], |r| r.get(0))
            .optional()?
            .unwrap_or_else(|| "auto".to_string()))
    }

    /// Aim the next preparation of this media at the canonical profile.
    ///
    /// One direction only. Nothing moves a media back to `auto`, because the
    /// reason it was pinned — a playlist that mixes formats — does not go away
    /// when the playlist is edited again, and flapping between the two would
    /// mean re-encoding hours of video twice.
    pub fn pin_to_canonical(&self, id: &str) -> Result<()> {
        self.raw()
            .lock()
            .unwrap()
            .execute("UPDATE media SET prepare_target='canonical' WHERE id=?1", [id])?;
        Ok(())
    }

    /// The prepared shape of every enabled item in a playlist, in order.
    ///
    /// `(media_id, mode, signature)`. A legacy row — prepared before signatures
    /// were recorded — reads as `("canonical", None)`, which is what it is.
    pub fn playlist_shapes(&self, broadcast_id: &str) -> Result<Vec<(String, String, Option<String>)>> {
        let mut ids: Vec<String> =
            self.items_for(broadcast_id)?.into_iter().filter(|i| i.enabled).map(|i| i.media_id).collect();
        if ids.is_empty() {
            ids.push(self.broadcast(broadcast_id)?.media_id);
        }
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut out = Vec::new();
        for id in ids {
            let row: Option<(Option<String>, Option<String>)> = guard
                .query_row("SELECT prepared_mode, prepared_signature FROM media WHERE id=?1", [&id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?;
            let (mode, sig) = row.unwrap_or((None, None));
            out.push((id, mode.unwrap_or_else(|| "canonical".into()), sig));
        }
        Ok(out)
    }

    /// Refuse to broadcast a playlist whose items cannot be joined.
    ///
    /// The last line of defence, checked on every start and every recovery
    /// rather than only when the playlist is edited. Concatenating packets from
    /// files that disagree on geometry or SPS produces a stream that decodes as
    /// garbage from the seam onwards, and it would do so *live*, an hour in,
    /// with nobody watching the server.
    ///
    /// Safe when every item is canonical (legacy files included — they all came
    /// out of the one canonical encode) or when every item is native with the
    /// same signature.
    pub fn check_playlist_joinable(&self, broadcast_id: &str) -> Result<()> {
        let shapes = self.playlist_shapes(broadcast_id)?;
        if shapes.len() < 2 {
            return Ok(());
        }
        // Only the canonical conversion makes files that are interchangeable
        // with each other. `direct`, `hybrid` and `live_normalize` all keep
        // something of the source, so two of those agree only when their
        // signatures do — exactly, extradata included.
        let all_canonical = shapes.iter().all(|(_, mode, _)| mode == "canonical");
        let first = shapes[0].2.clone();
        let all_same_native =
            first.is_some() && shapes.iter().all(|(_, mode, sig)| mode != "canonical" && *sig == first);
        if all_canonical || all_same_native {
            return Ok(());
        }
        Err(CloudError::Invalid(
            "플레이리스트의 영상 형식이 서로 달라 아직 방송할 수 없습니다.              영상을 방송 형식으로 맞추는 중이며, 끝나면 시작할 수 있습니다."
                .into(),
        ))
    }

    pub fn record_media_failed(&self, id: &str, why: &str) -> Result<()> {
        self.raw().lock().unwrap().execute(
            "UPDATE media SET state=?2, last_error=?3 WHERE id=?1",
            params![id, MediaState::Failed.id(), why],
        )?;
        Ok(())
    }

    pub fn set_media_state(&self, id: &str, state: MediaState) -> Result<()> {
        self.raw()
            .lock()
            .unwrap()
            .execute("UPDATE media SET state=?2 WHERE id=?1", params![id, state.id()])?;
        Ok(())
    }

    pub fn delete_media_owned(&self, user_id: &str, id: &str) -> Result<CloudMedia> {
        let m = self.media_owned(user_id, id)?;
        let used: i64 = self.raw().lock().unwrap().query_row(
            "SELECT COUNT(*) FROM broadcasts WHERE media_id=?1",
            [id],
            |r| r.get(0),
        )?;
        if used > 0 {
            return Err(CloudError::Invalid("이 영상을 사용하는 방송이 있습니다".into()));
        }
        self.raw()
            .lock()
            .unwrap()
            .execute("DELETE FROM media WHERE id=?1 AND user_id=?2", params![id, user_id])?;
        Ok(m)
    }

    /// What the manager needs to start a broadcast from this media row.
    pub fn prepared_media_for(&self, media_id: &str) -> Result<crate::manager::PreparedMedia> {
        let (filename, state, prepared, dur, w, h, fps): (
            String,
            String,
            Option<String>,
            f64,
            i64,
            i64,
            f64,
        ) = self
            .raw()
            .lock()
            .unwrap()
            .query_row(
                "SELECT filename, state, prepared_path, COALESCE(prepared_duration_secs, duration_secs),
                    width, height, fps
             FROM media WHERE id=?1",
                [media_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )
            .optional()?
            .ok_or(CloudError::NotFound("media"))?;

        if MediaState::from_id(&state) != Some(MediaState::Ready) {
            return Err(CloudError::Invalid("영상 준비가 끝나지 않았습니다".into()));
        }
        let prepared_key = prepared.ok_or(CloudError::Invalid("준비된 파일이 없습니다".into()))?;
        Ok(crate::manager::PreparedMedia {
            media_id: media_id.to_string(),
            filename,
            prepared_key,
            duration_secs: dur,
            width: w,
            height: h,
            fps,
        })
    }

    // --- destinations -----------------------------------------------------

    pub fn create_destination(
        &self,
        user_id: &str,
        label: &str,
        rtmps_url: &str,
        key_masked: &str,
    ) -> Result<StreamDestination> {
        let id = crate::new_id();
        self.raw().lock().unwrap().execute(
            "INSERT INTO stream_destinations (id, user_id, label, rtmps_url, key_masked)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, user_id, label, rtmps_url, key_masked],
        )?;
        self.destination_owned(user_id, &id)
    }

    pub fn destination_owned(&self, user_id: &str, id: &str) -> Result<StreamDestination> {
        self.raw()
            .lock()
            .unwrap()
            .query_row(
                "SELECT id, user_id, label, rtmps_url, key_masked, created_at, kind
                 FROM stream_destinations WHERE id=?1 AND user_id=?2",
                params![id, user_id],
                row_to_destination,
            )
            .optional()?
            .ok_or(CloudError::NotFound("destination"))
    }

    /// Without an owner check. Only the manager calls this, for a broadcast
    /// whose ownership was already established.
    pub fn destination(&self, id: &str) -> Result<StreamDestination> {
        self.raw()
            .lock()
            .unwrap()
            .query_row(
                "SELECT id, user_id, label, rtmps_url, key_masked, created_at, kind
                 FROM stream_destinations WHERE id=?1",
                [id],
                row_to_destination,
            )
            .optional()?
            .ok_or(CloudError::NotFound("destination"))
    }

    pub fn destinations_for(&self, user_id: &str) -> Result<Vec<StreamDestination>> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare(
            "SELECT id, user_id, label, rtmps_url, key_masked, created_at, kind
             FROM stream_destinations WHERE user_id=?1 ORDER BY created_at DESC, id DESC",
        )?;
        let rows = st.query_map([user_id], row_to_destination)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn delete_destination_owned(&self, user_id: &str, id: &str) -> Result<()> {
        // Ownership first, always. Asking "is it in use?" before "is it yours?"
        // answers a stranger's question: the in-use message confirms the row
        // exists and is busy, which is a fact about another tenant. A test
        // caught this — `delete_media_owned` had the order right and this did
        // not.
        self.destination_owned(user_id, id)?;

        let used: i64 = self.raw().lock().unwrap().query_row(
            "SELECT COUNT(*) FROM broadcasts WHERE destination_id=?1",
            [id],
            |r| r.get(0),
        )?;
        if used > 0 {
            return Err(CloudError::Invalid("이 대상을 사용하는 방송이 있습니다".into()));
        }
        self.raw()
            .lock()
            .unwrap()
            .execute("DELETE FROM stream_destinations WHERE id=?1 AND user_id=?2", params![id, user_id])?;
        Ok(())
    }

    // --- YouTube accounts and OAuth (§2, §3) -------------------------------

    /// Start a consent attempt and return its state parameter.
    ///
    /// The state is the row's key, so an unknown state is an unknown row and
    /// there is nothing to compare by hand.
    pub fn create_oauth_state(&self, user_id: &str, verifier: &str) -> Result<String> {
        let state = crate::new_id();
        self.raw().lock().unwrap().execute(
            "INSERT INTO oauth_states (state, user_id, verifier) VALUES (?1, ?2, ?3)",
            params![state, user_id, verifier],
        )?;
        Ok(state)
    }

    /// Spend a consent attempt. Once, and only while it is fresh.
    ///
    /// The update is the check: a second callback with the same state changes no
    /// rows and is refused, which is what makes a replayed redirect useless.
    pub fn claim_oauth_state(&self, state: &str, ttl_minutes: i64) -> Result<OauthClaim> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let changed = guard.execute(
            &format!(
                "UPDATE oauth_states SET used_at = datetime('now')
                 WHERE state = ?1 AND used_at IS NULL
                   AND created_at > datetime('now', '-{ttl_minutes} minutes')"
            ),
            [state],
        )?;
        if changed == 0 {
            return Err(CloudError::Invalid(
                "연결 요청이 만료되었거나 이미 사용되었습니다. 다시 시도해 주세요.".into(),
            ));
        }
        let (user_id, verifier) =
            guard.query_row("SELECT user_id, verifier FROM oauth_states WHERE state = ?1", [state], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        // Housekeeping, while we are here: spent and stale attempts are of no
        // further use to anyone.
        let _ = guard.execute(
            "DELETE FROM oauth_states
             WHERE used_at IS NOT NULL AND used_at < datetime('now', '-1 hours')
                OR created_at < datetime('now', '-1 days')",
            [],
        );
        Ok(OauthClaim { user_id, verifier })
    }

    /// Create or update the row for a channel. Reconnecting the same channel
    /// keeps its id, so the sealed tokens stay where the account expects them.
    pub fn upsert_youtube_account(
        &self,
        user_id: &str,
        channel_id: &str,
        channel_title: &str,
        thumbnail_url: Option<&str>,
    ) -> Result<crate::youtube::YoutubeAccount> {
        let existing: Option<String> = self
            .raw()
            .lock()
            .unwrap()
            .query_row(
                "SELECT id FROM youtube_accounts WHERE user_id = ?1 AND channel_id = ?2",
                params![user_id, channel_id],
                |r| r.get(0),
            )
            .optional()?;
        let id = existing.unwrap_or_else(crate::new_id);
        self.raw().lock().unwrap().execute(
            "INSERT INTO youtube_accounts (id, user_id, channel_id, channel_title, thumbnail_url)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                 channel_title = excluded.channel_title,
                 thumbnail_url = COALESCE(excluded.thumbnail_url, youtube_accounts.thumbnail_url),
                 updated_at = datetime('now')",
            params![id, user_id, channel_id, channel_title, thumbnail_url],
        )?;
        self.youtube_account_owned(user_id, &id)
    }

    pub fn youtube_account_owned(&self, user_id: &str, id: &str) -> Result<crate::youtube::YoutubeAccount> {
        self.raw()
            .lock()
            .unwrap()
            .query_row(
                &format!("{YOUTUBE_ACCOUNT_COLUMNS} WHERE id = ?1 AND user_id = ?2"),
                params![id, user_id],
                row_to_youtube_account,
            )
            .optional()?
            .ok_or(CloudError::NotFound("youtube account"))
    }

    pub fn youtube_accounts_for(&self, user_id: &str) -> Result<Vec<crate::youtube::YoutubeAccount>> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st =
            guard.prepare(&format!("{YOUTUBE_ACCOUNT_COLUMNS} WHERE user_id = ?1 ORDER BY created_at"))?;
        let rows = st.query_map([user_id], row_to_youtube_account)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn delete_youtube_account(&self, user_id: &str, id: &str) -> Result<()> {
        self.raw()
            .lock()
            .unwrap()
            .execute("DELETE FROM youtube_accounts WHERE id = ?1 AND user_id = ?2", params![id, user_id])?;
        Ok(())
    }

    pub fn touch_youtube_account(&self, id: &str) -> Result<()> {
        self.raw().lock().unwrap().execute(
            "UPDATE youtube_accounts SET last_verified_at = datetime('now'),
                    updated_at = datetime('now') WHERE id = ?1",
            [id],
        )?;
        Ok(())
    }

    /// When the cached access token stops being usable. Seconds from now.
    pub fn set_youtube_token_expiry(&self, id: &str, seconds: i64) -> Result<()> {
        let modifier = format!("{}{} seconds", if seconds < 0 { "-" } else { "+" }, seconds.abs());
        self.raw().lock().unwrap().execute(
            "UPDATE youtube_accounts SET token_expiry = datetime('now', ?2),
                    updated_at = datetime('now') WHERE id = ?1",
            params![id, modifier],
        )?;
        Ok(())
    }

    /// True when there is no usable cached token — including when there never
    /// was one, which is the state a fresh server restart is in.
    pub fn youtube_token_expired(&self, id: &str) -> Result<bool> {
        let expiry: Option<String> = self
            .raw()
            .lock()
            .unwrap()
            .query_row("SELECT token_expiry FROM youtube_accounts WHERE id = ?1", [id], |r| r.get(0))
            .optional()?
            .flatten();
        let Some(expiry) = expiry else { return Ok(true) };
        let still_good: i64 = self.raw().lock().unwrap().query_row(
            "SELECT CASE WHEN ?1 > datetime('now') THEN 1 ELSE 0 END",
            [expiry],
            |r| r.get(0),
        )?;
        Ok(still_good == 0)
    }

    // --- a broadcast's YouTube resources (§7) ------------------------------

    /// A destination that YouTube gave us, for one broadcast.
    ///
    /// An ordinary `stream_destinations` row on purpose: from here on the
    /// sending path is the one that is already on air, and the stream key is
    /// sealed by the same store as a pasted one.
    pub fn upsert_youtube_destination(
        &self,
        user_id: &str,
        broadcast_id: &str,
        ingestion_address: &str,
        account_id: &str,
        stream_id: &str,
    ) -> Result<StreamDestination> {
        let title: String = self
            .raw()
            .lock()
            .unwrap()
            .query_row("SELECT channel_title FROM youtube_accounts WHERE id = ?1", [account_id], |r| r.get(0))
            .optional()?
            .unwrap_or_else(|| "YouTube".to_string());
        let existing: Option<String> = self
            .raw()
            .lock()
            .unwrap()
            .query_row(
                "SELECT d.id FROM stream_destinations d JOIN broadcasts b ON b.destination_id = d.id
                 WHERE b.id = ?1 AND d.kind = 'youtube_account'",
                [broadcast_id],
                |r| r.get(0),
            )
            .optional()?;
        let id = existing.unwrap_or_else(crate::new_id);
        self.raw().lock().unwrap().execute(
            "INSERT INTO stream_destinations
                 (id, user_id, label, rtmps_url, key_masked, kind, youtube_account_id, youtube_stream_id)
             VALUES (?1, ?2, ?3, ?4, '••••••••••••', 'youtube_account', ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                 label = excluded.label,
                 rtmps_url = excluded.rtmps_url,
                 youtube_account_id = excluded.youtube_account_id,
                 youtube_stream_id = excluded.youtube_stream_id",
            params![id, user_id, title, ingestion_address, account_id, stream_id],
        )?;
        self.destination_owned(user_id, &id)
    }

    /// A destination for a YouTube-connected broadcast that does not exist yet.
    ///
    /// `create_broadcast` insists on a destination the caller owns, and YouTube
    /// cannot be asked for an ingestion address until there is a broadcast to
    /// title. This breaks the circle: an empty row of the right kind, which
    /// `upsert_youtube_destination` then fills in place once Google has
    /// answered. It carries no key, so a broadcast pointed at it and never
    /// provisioned refuses to start rather than sending somewhere wrong.
    pub fn reserve_youtube_destination(&self, user_id: &str, account_id: &str) -> Result<StreamDestination> {
        let title: String = self
            .raw()
            .lock()
            .unwrap()
            .query_row(
                "SELECT channel_title FROM youtube_accounts WHERE id = ?1 AND user_id = ?2",
                params![account_id, user_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(CloudError::NotFound("youtube account"))?;
        let id = crate::new_id();
        self.raw().lock().unwrap().execute(
            "INSERT INTO stream_destinations
                 (id, user_id, label, rtmps_url, key_masked, kind, youtube_account_id)
             VALUES (?1, ?2, ?3, '', '••••••••••••', 'youtube_account', ?4)",
            params![id, user_id, title, account_id],
        )?;
        self.destination_owned(user_id, &id)
    }

    /// Point a broadcast at a destination we just made for it.
    pub fn point_broadcast_at(&self, user_id: &str, broadcast_id: &str, destination_id: &str) -> Result<()> {
        self.destination_owned(user_id, destination_id)?;
        self.raw().lock().unwrap().execute(
            "UPDATE broadcasts SET destination_id = ?2 WHERE id = ?1 AND user_id = ?3",
            params![broadcast_id, destination_id, user_id],
        )?;
        Ok(())
    }

    pub fn attach_youtube(
        &self,
        broadcast_id: &str,
        account_id: &str,
        youtube_broadcast_id: &str,
        youtube_stream_id: &str,
        status: &str,
    ) -> Result<()> {
        self.raw().lock().unwrap().execute(
            "UPDATE broadcasts SET youtube_account_id = ?2, youtube_broadcast_id = ?3,
                    youtube_stream_id = ?4, youtube_status = ?5, youtube_error = NULL
             WHERE id = ?1",
            params![broadcast_id, account_id, youtube_broadcast_id, youtube_stream_id, status],
        )?;
        Ok(())
    }

    /// YouTube's own view of the broadcast, which is not FFmpeg's. §8.
    pub fn set_youtube_status(&self, broadcast_id: &str, status: &str) -> Result<()> {
        self.raw().lock().unwrap().execute(
            "UPDATE broadcasts SET youtube_status = ?2 WHERE id = ?1",
            params![broadcast_id, status],
        )?;
        Ok(())
    }

    pub fn set_youtube_error(&self, broadcast_id: &str, message: &str) -> Result<()> {
        self.raw().lock().unwrap().execute(
            "UPDATE broadcasts SET youtube_status = 'error', youtube_error = ?2 WHERE id = ?1",
            params![broadcast_id, message],
        )?;
        Ok(())
    }

    // --- playlists (§1, §2) -----------------------------------------------

    /// One broadcast's playlist, in order, with what the UI needs to draw it.
    pub fn items_owned(&self, user_id: &str, broadcast_id: &str) -> Result<Vec<BroadcastItem>> {
        self.broadcast_owned(user_id, broadcast_id)?;
        self.items_for(broadcast_id)
    }

    /// Without an owner check. For the manager, which already established it.
    pub fn items_for(&self, broadcast_id: &str) -> Result<Vec<BroadcastItem>> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare(
            "SELECT i.id, i.broadcast_id, i.media_id, i.position, i.enabled, i.repeat_count,
                    m.filename, COALESCE(m.prepared_duration_secs, m.duration_secs) AS duration_secs,
                    m.state
             FROM broadcast_items i JOIN media m ON m.id = i.media_id
             WHERE i.broadcast_id = ?1
             ORDER BY i.position",
        )?;
        let rows = st.query_map([broadcast_id], |r| {
            let state: String = r.get("state")?;
            Ok(BroadcastItem {
                id: r.get("id")?,
                broadcast_id: r.get("broadcast_id")?,
                media_id: r.get("media_id")?,
                position: r.get("position")?,
                enabled: r.get::<_, i64>("enabled")? != 0,
                repeat_count: r.get("repeat_count")?,
                filename: r.get("filename")?,
                duration_secs: r.get("duration_secs")?,
                state: MediaState::from_id(&state).unwrap_or(MediaState::Failed),
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Replace the whole playlist, in one transaction.
    ///
    /// The whole list rather than one item at a time, because that is what the
    /// screen actually produces: a drag leaves every following position changed,
    /// and sending the result as a list makes the stored order and the drawn
    /// order the same thing by construction.
    pub fn replace_items(
        &self,
        user_id: &str,
        broadcast_id: &str,
        items: &[NewItem],
    ) -> Result<Vec<BroadcastItem>> {
        self.broadcast_owned(user_id, broadcast_id)?;
        if items.is_empty() {
            return Err(CloudError::Invalid("영상을 최소 한 개 선택해 주세요".into()));
        }
        if items.len() > MAX_PLAYLIST_ITEMS {
            return Err(CloudError::Invalid(format!("플레이리스트는 최대 {MAX_PLAYLIST_ITEMS}개까지입니다")));
        }
        for it in items {
            if !(1..=100).contains(&it.repeat_count) {
                return Err(CloudError::Invalid("반복 횟수는 1~100 사이여야 합니다".into()));
            }
            // Every video must be the caller's. Without this, a playlist would
            // be a way to broadcast someone else's upload.
            self.media_owned(user_id, &it.media_id)?;
        }

        let conn = self.raw();
        let mut guard = conn.lock().unwrap();
        let tx = guard.transaction()?;
        tx.execute("DELETE FROM broadcast_items WHERE broadcast_id = ?1", [broadcast_id])?;
        for (position, it) in items.iter().enumerate() {
            tx.execute(
                "INSERT INTO broadcast_items
                     (id, broadcast_id, media_id, position, enabled, repeat_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    crate::new_id(),
                    broadcast_id,
                    it.media_id,
                    position as i64,
                    it.enabled as i64,
                    it.repeat_count,
                ],
            )?;
        }
        // `media_id` stays in step with the first item so that anything reading
        // the old column — including the previous release — still works.
        tx.execute(
            "UPDATE broadcasts SET item_count = ?2, media_id = ?3 WHERE id = ?1",
            params![broadcast_id, items.len() as i64, items[0].media_id],
        )?;
        tx.commit()?;
        drop(guard);
        self.items_for(broadcast_id)
    }

    /// What the worker will actually play, in order, ready to stream.
    ///
    /// Disabled items are left out, `repeat_count` is expanded into repeats, and
    /// a broadcast with no rows at all falls back to its `media_id` — which is
    /// what makes a broadcast created before playlists existed start unchanged.
    pub fn prepared_items_for(&self, broadcast_id: &str) -> Result<Vec<crate::manager::PreparedMedia>> {
        let items = self.items_for(broadcast_id)?;
        if items.is_empty() {
            let b = self.broadcast(broadcast_id)?;
            return Ok(vec![self.prepared_media_for(&b.media_id)?]);
        }
        let mut out = Vec::new();
        for it in items.iter().filter(|i| i.enabled) {
            let prepared = self.prepared_media_for(&it.media_id)?;
            for _ in 0..it.repeat_count.max(1) {
                out.push(prepared.clone());
            }
        }
        if out.is_empty() {
            return Err(CloudError::Invalid("사용 가능한 영상이 없습니다".into()));
        }
        Ok(out)
    }

    // --- broadcasts -------------------------------------------------------

    pub fn create_broadcast(
        &self,
        user_id: &str,
        name: &str,
        media_id: &str,
        destination_id: &str,
        loop_forever: bool,
    ) -> Result<Broadcast> {
        // Both halves must belong to the caller, or a broadcast could point at
        // someone else's video.
        self.media_owned(user_id, media_id)?;
        self.destination_owned(user_id, destination_id)?;
        self.check_can_create_broadcast(user_id)?;

        let id = crate::new_id();
        self.raw().lock().unwrap().execute(
            "INSERT INTO broadcasts (id, user_id, name, media_id, destination_id, loop_forever,
                                     desired_state, runtime_state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                user_id,
                name,
                media_id,
                destination_id,
                loop_forever as i64,
                DesiredState::Stopped.id(),
                RuntimeState::Created.id(),
            ],
        )?;
        self.broadcast_owned(user_id, &id)
    }

    pub fn broadcast_owned(&self, user_id: &str, id: &str) -> Result<Broadcast> {
        self.raw()
            .lock()
            .unwrap()
            .query_row(
                &format!("{BROADCAST_COLUMNS} WHERE id=?1 AND user_id=?2"),
                params![id, user_id],
                row_to_broadcast,
            )
            .optional()?
            .ok_or(CloudError::NotFound("broadcast"))
    }

    /// Without an owner check. The manager only, for its own workers.
    pub fn broadcast(&self, id: &str) -> Result<Broadcast> {
        self.raw()
            .lock()
            .unwrap()
            .query_row(&format!("{BROADCAST_COLUMNS} WHERE id=?1"), [id], row_to_broadcast)
            .optional()?
            .ok_or(CloudError::NotFound("broadcast"))
    }

    pub fn broadcasts_for(&self, user_id: &str) -> Result<Vec<Broadcast>> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st =
            guard.prepare(&format!("{BROADCAST_COLUMNS} WHERE user_id=?1 ORDER BY created_at, id"))?;
        let rows = st.query_map([user_id], row_to_broadcast)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Everything meant to be running. The only query recovery needs.
    pub fn broadcasts_wanting_to_run(&self) -> Result<Vec<Broadcast>> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st =
            guard.prepare(&format!("{BROADCAST_COLUMNS} WHERE desired_state='running' ORDER BY id"))?;
        let rows = st.query_map([], row_to_broadcast)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn desired_state(&self, id: &str) -> Result<DesiredState> {
        let s: String = self
            .raw()
            .lock()
            .unwrap()
            .query_row("SELECT desired_state FROM broadcasts WHERE id=?1", [id], |r| r.get(0))
            .optional()?
            .ok_or(CloudError::NotFound("broadcast"))?;
        DesiredState::from_id(&s).ok_or(CloudError::NotFound("broadcast"))
    }

    pub fn delete_broadcast_owned(&self, user_id: &str, id: &str) -> Result<()> {
        let n = self
            .raw()
            .lock()
            .unwrap()
            .execute("DELETE FROM broadcasts WHERE id=?1 AND user_id=?2", params![id, user_id])?;
        if n == 0 {
            return Err(CloudError::NotFound("broadcast"));
        }
        Ok(())
    }

    /// Change what a broadcast is, without touching what it is doing.
    ///
    /// Only the fields present are written. A playlist, a destination or a
    /// settings change takes effect at the next start — the running FFmpeg is
    /// deliberately not interrupted, because an edit is not a reason to drop a
    /// live stream.
    pub fn update_broadcast_owned(
        &self,
        user_id: &str,
        id: &str,
        patch: &BroadcastPatch,
    ) -> Result<Broadcast> {
        self.broadcast_owned(user_id, id)?;
        if let Some(s) = &patch.settings {
            s.validate()?;
        }
        if let Some(sc) = &patch.schedule {
            crate::schedule::validate(sc)?;
        }
        if let Some(d) = &patch.destination_id {
            // Moving a broadcast to a destination that is not yours would be a
            // way to send your video to someone else's channel.
            self.destination_owned(user_id, d)?;
        }

        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let set = |sql: &str, value: &dyn rusqlite::ToSql| -> Result<()> {
            guard.execute(&format!("UPDATE broadcasts SET {sql} WHERE id = ?1"), params![id, value])?;
            Ok(())
        };
        if let Some(v) = &patch.name {
            set("name = ?2", v)?;
        }
        if let Some(v) = &patch.title {
            set("title = ?2", v)?;
        }
        if let Some(v) = &patch.description {
            set("description = ?2", v)?;
        }
        if let Some(v) = &patch.tags {
            set("tags = ?2", v)?;
        }
        if let Some(v) = &patch.category {
            set("category = ?2", v)?;
        }
        if let Some(v) = &patch.privacy {
            set("privacy = ?2", &v.id())?;
        }
        if let Some(v) = patch.loop_forever {
            set("loop_forever = ?2", &(v as i64))?;
        }
        if let Some(v) = &patch.destination_id {
            set("destination_id = ?2", v)?;
        }
        if let Some(v) = &patch.settings {
            set("resolution = ?2", &v.resolution)?;
            set("fps = ?2", &v.fps)?;
            set("video_bitrate_kbps = ?2", &v.video_bitrate_kbps)?;
            set("audio_bitrate_kbps = ?2", &v.audio_bitrate_kbps)?;
        }
        if let Some(v) = &patch.schedule {
            set("sched_enabled = ?2", &(v.enabled as i64))?;
            set("sched_start_at = ?2", &v.start_at)?;
            set("sched_stop_at = ?2", &v.stop_at)?;
            set("sched_timezone = ?2", &v.timezone)?;
            set("sched_offset_minutes = ?2", &v.offset_minutes)?;
            set("sched_repeat_days = ?2", &v.repeat_days)?;
        }
        drop(guard);
        self.broadcast_owned(user_id, id)
    }

    /// Where the playlist has got to. Written every tick while it plays. §9.
    pub fn record_playlist_progress(&self, id: &str, p: &PlaylistProgress) -> Result<()> {
        self.raw().lock().unwrap().execute(
            "UPDATE broadcasts SET current_index = ?2, current_item = ?3, next_item = ?4,
                    current_position_secs = ?5, current_duration_secs = ?6, cycle_duration_secs = ?7,
                    play_count = CASE WHEN ?8 > 0 THEN ?8 ELSE play_count END
             WHERE id = ?1",
            params![
                id,
                p.index,
                p.current_item,
                p.next_item,
                p.position_secs,
                p.duration_secs,
                p.cycle_secs,
                p.play_count,
            ],
        )?;
        Ok(())
    }

    /// Remember which entry the playlist was rotated to when it started, so a
    /// display index can be mapped back to the order the user sees.
    pub fn record_playlist_offset(&self, id: &str, offset: i64, play_count: i64) -> Result<()> {
        self.raw().lock().unwrap().execute(
            "UPDATE broadcasts SET playlist_offset = ?2, play_count = ?3 WHERE id = ?1",
            params![id, offset, play_count],
        )?;
        Ok(())
    }

    /// The entry a recovered worker should resume at, and where it rotated from.
    pub fn playlist_resume_point(&self, id: &str) -> Result<(i64, i64)> {
        Ok(self.raw().lock().unwrap().query_row(
            "SELECT current_index, playlist_offset FROM broadcasts WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    /// Every broadcast, for the operator's diagnostic command. No owner filter,
    /// because the caller is a shell on the server rather than a request.
    pub fn all_broadcasts(&self) -> Result<Vec<Broadcast>> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare(&format!("{BROADCAST_COLUMNS} ORDER BY created_at"))?;
        let rows = st.query_map([], row_to_broadcast)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Every user id, for the same diagnostic command. Same reason: the caller
    /// is a shell on the server, not a request.
    pub fn all_user_ids(&self) -> Result<Vec<String>> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare("SELECT id FROM users ORDER BY created_at")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// One broadcast's events, without an owner check. Same reason.
    pub fn events_for(&self, broadcast_id: &str, limit: i64) -> Result<Vec<BroadcastEvent>> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare(
            "SELECT id, broadcast_id, at, level, message FROM broadcast_events
             WHERE broadcast_id=?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![broadcast_id, limit], |r| {
            Ok(BroadcastEvent {
                id: r.get(0)?,
                broadcast_id: r.get(1)?,
                at: r.get(2)?,
                level: r.get(3)?,
                message: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Every broadcast with a schedule switched on. The scheduler reads this and
    /// decides in one place; nothing about a due time is stored in memory, which
    /// is what makes a restart recover schedules for free.
    pub fn scheduled_broadcasts(&self) -> Result<Vec<Broadcast>> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare(&format!("{BROADCAST_COLUMNS} WHERE sched_enabled = 1"))?;
        let rows = st.query_map([], row_to_broadcast)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Remember which occurrence was started, so the same window cannot start
    /// twice — including after the server is restarted inside it.
    pub fn mark_scheduled_run(&self, id: &str, occurrence: &str) -> Result<()> {
        self.raw()
            .lock()
            .unwrap()
            .execute("UPDATE broadcasts SET sched_last_run_at = ?2 WHERE id = ?1", params![id, occurrence])?;
        Ok(())
    }

    // --- what the manager writes back -------------------------------------

    /// The engine's own report: state, restarts and uptime. §17, §22.
    pub fn record_runtime(
        &self,
        id: &str,
        state: RuntimeState,
        restart_count: i64,
        uptime_secs: i64,
        bytes_sent: i64,
        ffmpeg_pid: Option<i64>,
    ) -> Result<()> {
        self.raw().lock().unwrap().execute(
            "UPDATE broadcasts SET runtime_state=?2, restart_count=?3, uptime_secs=?4,
                    bytes_sent=?5, ffmpeg_pid=?6, last_heartbeat=datetime('now')
             WHERE id=?1",
            params![id, state.id(), restart_count, uptime_secs, bytes_sent, ffmpeg_pid],
        )?;
        Ok(())
    }

    pub fn record_runtime_only(&self, id: &str, state: RuntimeState) -> Result<()> {
        self.raw().lock().unwrap().execute(
            "UPDATE broadcasts SET runtime_state=?2, last_heartbeat=datetime('now') WHERE id=?1",
            params![id, state.id()],
        )?;
        Ok(())
    }

    pub fn touch_heartbeat(&self, id: &str) -> Result<()> {
        self.raw()
            .lock()
            .unwrap()
            .execute("UPDATE broadcasts SET last_heartbeat=datetime('now') WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn record_failure(&self, id: &str, why: &str) -> Result<()> {
        self.raw()
            .lock()
            .unwrap()
            .execute("UPDATE broadcasts SET last_error=?2 WHERE id=?1", params![id, why])?;
        self.append_event(id, EventLevel::Error, why)?;
        Ok(())
    }

    /// Stop trying. `desired_state` goes to stopped so no recovery resumes it.
    pub fn give_up(&self, id: &str) -> Result<()> {
        self.raw().lock().unwrap().execute(
            "UPDATE broadcasts SET desired_state='stopped', runtime_state=?2,
                    stopped_at=datetime('now')
             WHERE id=?1",
            params![id, RuntimeState::Failed.id()],
        )?;
        Ok(())
    }

    pub fn append_event(&self, broadcast_id: &str, level: EventLevel, message: &str) -> Result<()> {
        // Bounded: a broadcast reconnecting all night must not grow the
        // database without limit.
        let c = self.raw();
        let guard = c.lock().unwrap();
        guard.execute(
            "INSERT INTO broadcast_events (broadcast_id, level, message) VALUES (?1, ?2, ?3)",
            params![broadcast_id, format!("{level:?}").to_lowercase(), message],
        )?;
        guard.execute(
            "DELETE FROM broadcast_events
             WHERE broadcast_id=?1 AND id NOT IN (
                 SELECT id FROM broadcast_events WHERE broadcast_id=?1 ORDER BY id DESC LIMIT 500
             )",
            [broadcast_id],
        )?;
        Ok(())
    }

    pub fn events_owned(&self, user_id: &str, broadcast_id: &str, limit: i64) -> Result<Vec<BroadcastEvent>> {
        self.broadcast_owned(user_id, broadcast_id)?;
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare(
            "SELECT id, broadcast_id, at, level, message FROM broadcast_events
             WHERE broadcast_id=?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![broadcast_id, limit], |r| {
            Ok(BroadcastEvent {
                id: r.get(0)?,
                broadcast_id: r.get(1)?,
                at: r.get(2)?,
                level: r.get(3)?,
                message: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

// `SELECT *` would break the moment a migration adds a column in a different
// order, and reading by name means adding one here is the only edit needed.
/// What a consent attempt was for, once it is spent.
#[derive(Debug, Clone)]
pub struct OauthClaim {
    pub user_id: String,
    pub verifier: String,
}

const YOUTUBE_ACCOUNT_COLUMNS: &str = "SELECT id, user_id, provider, channel_id, channel_title,
        thumbnail_url, token_expiry, created_at, updated_at, last_verified_at
        FROM youtube_accounts";

fn row_to_youtube_account(r: &rusqlite::Row<'_>) -> rusqlite::Result<crate::youtube::YoutubeAccount> {
    Ok(crate::youtube::YoutubeAccount {
        id: r.get("id")?,
        user_id: r.get("user_id")?,
        provider: r.get("provider")?,
        channel_id: r.get("channel_id")?,
        channel_title: r.get("channel_title")?,
        thumbnail_url: r.get("thumbnail_url")?,
        token_expiry: r.get("token_expiry")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
        last_verified_at: r.get("last_verified_at")?,
    })
}

const BROADCAST_COLUMNS: &str = "SELECT * FROM broadcasts";

const PLAN_COLUMNS: &str = "SELECT id, label, limits, monthly_price_krw, description, active, sort_order
     FROM plans";

const BILLING_COLUMNS: &str = "SELECT id, user_id, plan_id, provider, provider_subscription_id, status,
            amount_krw, created_at, updated_at, activated_at, cancelled_at, last_paid_at,
            current_period_end
     FROM billing_subscriptions";

fn row_to_billing(r: &rusqlite::Row<'_>) -> rusqlite::Result<BillingSubscription> {
    let status: String = r.get("status")?;
    Ok(BillingSubscription {
        id: r.get("id")?,
        user_id: r.get("user_id")?,
        plan_id: r.get("plan_id")?,
        provider: r.get("provider")?,
        provider_subscription_id: r.get("provider_subscription_id")?,
        // An unreadable status is `pending`, which grants nothing. The only way
        // to get one is a hand-edited row, and failing closed is the answer.
        status: BillingStatus::from_id(&status).unwrap_or(BillingStatus::Pending),
        amount_krw: r.get("amount_krw")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
        activated_at: r.get("activated_at")?,
        cancelled_at: r.get("cancelled_at")?,
        last_paid_at: r.get("last_paid_at")?,
        current_period_end: r.get("current_period_end")?,
    })
}

/// One payment notification on its way into the ledger.
///
/// A struct because it is eleven fields and every one of them is a string: a
/// positional call would be a swap waiting to happen, in the one place where a
/// swap means money.
#[derive(Debug, Clone)]
pub struct BillingPayment<'a> {
    pub provider: &'a str,
    /// The provider's own id for this payment. The idempotency key.
    pub event_key: &'a str,
    pub billing_id: &'a str,
    pub user_id: &'a str,
    pub provider_subscription_id: Option<&'a str>,
    pub pay_state: &'a str,
    pub amount_krw: i64,
    pub pay_date: Option<&'a str>,
    pub pay_type: Option<&'a str>,
    /// What we did about it, in our own words.
    pub outcome: &'a str,
    /// The status to move the subscription to, or `None` to only record.
    pub status: Option<BillingStatus>,
    pub paid_at: Option<&'a str>,
    pub period_end: Option<&'a str>,
}

/// One plan's storage limits, now and as this build would set them.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StorageAudit {
    pub plan_id: String,
    pub label: String,
    /// Printed so an operator can see for themselves that this does not move.
    pub monthly_price_krw: i64,
    /// The same.
    pub concurrent_streams: i64,
    pub storage_now: i64,
    pub storage_target: i64,
    pub upload_now: i64,
    pub upload_target: i64,
}

/// One account's stored bytes, against the ceiling it would have.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageRow {
    pub email: String,
    pub plan_id: String,
    pub used_bytes: i64,
    pub ceiling_after: i64,
}

impl UsageRow {
    /// Would this account be over the new ceiling? Only blocks new uploads.
    pub fn over(&self) -> bool {
        self.ceiling_after > 0 && self.used_bytes > self.ceiling_after
    }
}

/// A billing record whose provider subscription is over while the entitlement it
/// paid for is still standing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BillingMismatch {
    pub billing_id: String,
    pub user_id: String,
    pub email: String,
    pub plan_id: String,
    pub billing_status: String,
    pub cancelled_at: Option<String>,
    pub entitlement_status: String,
}

/// One row of the payment ledger, as an account screen may show it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BillingEventRow {
    pub provider: String,
    pub event_key: String,
    pub pay_state: String,
    pub amount_krw: i64,
    pub pay_date: Option<String>,
    pub pay_type: Option<String>,
    pub outcome: String,
    pub processed_at: String,
}

fn row_to_plan(r: &rusqlite::Row<'_>) -> rusqlite::Result<Plan> {
    let json: String = r.get("limits")?;
    Ok(Plan {
        id: r.get("id")?,
        label: r.get("label")?,
        // A plan whose limits will not parse grants nothing, rather than
        // everything. The only way this happens is a hand-edited row.
        limits: serde_json::from_str(&json).unwrap_or_default(),
        monthly_price_krw: r.get("monthly_price_krw")?,
        description: r.get("description")?,
        active: r.get::<_, i64>("active")? != 0,
        sort_order: r.get("sort_order")?,
    })
}

fn row_to_media(r: &rusqlite::Row<'_>) -> rusqlite::Result<CloudMedia> {
    let state: String = r.get(4)?;
    Ok(CloudMedia {
        id: r.get(0)?,
        user_id: r.get(1)?,
        filename: r.get(2)?,
        size_bytes: r.get(3)?,
        state: MediaState::from_id(&state).unwrap_or(MediaState::Failed),
        duration_secs: r.get(5)?,
        width: r.get(6)?,
        height: r.get(7)?,
        fps: r.get(8)?,
        video_codec: r.get(9)?,
        audio_codec: r.get(10)?,
        container: r.get(11)?,
        bitrate_bps: r.get(12)?,
        storage_path: r.get(13)?,
        prepared_path: r.get(14)?,
        prepared_duration_secs: r.get(15)?,
        last_error: r.get(16)?,
        created_at: r.get(17)?,
    })
}

fn row_to_destination(r: &rusqlite::Row<'_>) -> rusqlite::Result<StreamDestination> {
    let kind: String = r.get("kind")?;
    Ok(StreamDestination {
        id: r.get("id")?,
        user_id: r.get("user_id")?,
        label: r.get("label")?,
        rtmps_url: r.get("rtmps_url")?,
        key_masked: r.get("key_masked")?,
        created_at: r.get("created_at")?,
        kind: DestinationKind::from_id(&kind).unwrap_or(DestinationKind::ManualRtmps),
    })
}

fn row_to_broadcast(r: &rusqlite::Row<'_>) -> rusqlite::Result<Broadcast> {
    // By name, not by position: there are now thirty-odd columns and a
    // migration appends to the end, so counting them was a bug waiting to
    // happen.
    let desired: String = r.get("desired_state")?;
    let runtime: String = r.get("runtime_state")?;
    let privacy: String = r.get("privacy")?;
    Ok(Broadcast {
        id: r.get("id")?,
        user_id: r.get("user_id")?,
        name: r.get("name")?,
        media_id: r.get("media_id")?,
        destination_id: r.get("destination_id")?,
        loop_forever: r.get::<_, i64>("loop_forever")? != 0,
        desired_state: DesiredState::from_id(&desired).unwrap_or(DesiredState::Stopped),
        runtime_state: RuntimeState::from_id(&runtime).unwrap_or(RuntimeState::Failed),
        restart_count: r.get("restart_count")?,
        last_error: r.get("last_error")?,
        created_at: r.get("created_at")?,
        started_at: r.get("started_at")?,
        stopped_at: r.get("stopped_at")?,
        last_heartbeat: r.get("last_heartbeat")?,
        bytes_sent: r.get("bytes_sent")?,
        uptime_secs: r.get("uptime_secs")?,
        ffmpeg_exit_code: r.get("ffmpeg_exit_code")?,
        ffmpeg_pid: r.get("ffmpeg_pid")?,
        title: r.get("title")?,
        description: r.get("description")?,
        tags: r.get("tags")?,
        category: r.get("category")?,
        privacy: Privacy::from_id(&privacy).unwrap_or(Privacy::Private),
        settings: StreamSettings {
            resolution: r.get("resolution")?,
            fps: r.get("fps")?,
            video_bitrate_kbps: r.get("video_bitrate_kbps")?,
            audio_bitrate_kbps: r.get("audio_bitrate_kbps")?,
        },
        schedule: Schedule {
            enabled: r.get::<_, i64>("sched_enabled")? != 0,
            start_at: r.get("sched_start_at")?,
            stop_at: r.get("sched_stop_at")?,
            timezone: r.get("sched_timezone")?,
            offset_minutes: r.get("sched_offset_minutes")?,
            repeat_days: r.get("sched_repeat_days")?,
            last_run_at: r.get("sched_last_run_at")?,
        },
        item_count: r.get("item_count")?,
        play_count: r.get("play_count")?,
        current_index: r.get("current_index")?,
        current_item: r.get("current_item")?,
        next_item: r.get("next_item")?,
        current_position_secs: r.get("current_position_secs")?,
        current_duration_secs: r.get("current_duration_secs")?,
        cycle_duration_secs: r.get("cycle_duration_secs")?,
        youtube: crate::youtube::YoutubeLink {
            account_id: r.get("youtube_account_id")?,
            broadcast_id: r.get("youtube_broadcast_id")?,
            stream_id: r.get("youtube_stream_id")?,
            status: r.get("youtube_status")?,
            watch_url: r
                .get::<_, Option<String>>("youtube_broadcast_id")?
                .map(|id| format!("https://www.youtube.com/watch?v={id}")),
            last_error: r.get("youtube_error")?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_plans_are_rows_with_the_limits_the_spec_names() {
        let db = CloudDb::open_in_memory().unwrap();
        for (id, want) in [("basic", 1), ("pro", 2), ("business", 3)] {
            let p = db.plan(id).unwrap();
            assert_eq!(p.limits.get("max_concurrent_streams"), Some(&want), "{id}");
            // Every plan carries every limit, so a lookup never silently
            // returns "no limit" because a row was written incompletely.
            for k in ["max_broadcasts", "max_storage_bytes", "max_upload_bytes"] {
                assert!(p.limits.contains_key(k), "{id} is missing {k}");
            }
        }
    }

    #[test]
    fn an_edited_limit_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cloud.db");
        {
            let db = CloudDb::open(&path).unwrap();
            db.raw()
                .lock()
                .unwrap()
                .execute("UPDATE plans SET limits='{\"max_concurrent_streams\":9}' WHERE id='basic'", [])
                .unwrap();
        }
        let db = CloudDb::open(&path).unwrap();
        assert_eq!(db.plan("basic").unwrap().limits.get("max_concurrent_streams"), Some(&9));
    }

    #[test]
    fn a_second_account_on_one_email_is_refused() {
        let db = CloudDb::open_in_memory().unwrap();
        db.create_user("a@b.com", "h", "basic").unwrap();
        let again = db.create_user("A@B.COM", "h", "basic");
        assert!(matches!(again, Err(CloudError::EmailTaken)), "email must be unique, case aside");
    }

    #[test]
    fn a_token_expires_and_stops_working() {
        let db = CloudDb::open_in_memory().unwrap();
        let u = db.create_user("a@b.com", "h", "pro").unwrap();
        db.create_auth_session(&u.id, "livehash", 7).unwrap();
        assert_eq!(db.user_for_token("livehash").unwrap(), u.id);

        db.create_auth_session(&u.id, "deadhash", -1).unwrap();
        assert!(matches!(db.user_for_token("deadhash"), Err(CloudError::Forbidden)));
        assert!(matches!(db.user_for_token("never-issued"), Err(CloudError::Forbidden)));

        db.delete_auth_session("livehash").unwrap();
        assert!(matches!(db.user_for_token("livehash"), Err(CloudError::Forbidden)));
    }

    #[test]
    fn a_subscription_reports_the_plan_and_its_limits() {
        let db = CloudDb::open_in_memory().unwrap();
        let u = db.create_user("a@b.com", "h", "basic").unwrap();
        assert_eq!(db.subscription(&u.id).unwrap().plan_label, "Basic");
        db.set_plan(&u.id, "business").unwrap();
        let s = db.subscription(&u.id).unwrap();
        assert_eq!(s.plan_label, "Business");
        assert_eq!(s.limits.get("max_concurrent_streams"), Some(&3));
    }
}
