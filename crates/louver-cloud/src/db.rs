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
    ffmpeg_exit_code INTEGER
);
CREATE INDEX IF NOT EXISTS idx_broadcast_user ON broadcasts(user_id);
CREATE INDEX IF NOT EXISTS idx_broadcast_desired ON broadcasts(desired_state);

CREATE TABLE IF NOT EXISTS broadcast_events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    broadcast_id TEXT NOT NULL REFERENCES broadcasts(id) ON DELETE CASCADE,
    at           TEXT NOT NULL DEFAULT (datetime('now')),
    level        TEXT NOT NULL,
    message      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_event_broadcast ON broadcast_events(broadcast_id, id DESC);
"#;

/// The plans the service ships with.
///
/// Rows, not constants in the code. `entitlement` reads limits by name, so a
/// fourth plan is an INSERT and never an `if`.
type SeedPlan = (&'static str, &'static str, &'static [(&'static str, i64)]);

const SEED_PLANS: &[SeedPlan] = &[
    (
        "basic",
        "Basic",
        &[
            ("max_concurrent_streams", 1),
            ("max_broadcasts", 3),
            ("max_storage_bytes", 20 * 1024 * 1024 * 1024),
            ("max_upload_bytes", 8 * 1024 * 1024 * 1024),
            ("scheduling_enabled", 0),
            ("priority_recovery", 0),
        ],
    ),
    (
        "pro",
        "Pro",
        &[
            ("max_concurrent_streams", 2),
            ("max_broadcasts", 10),
            ("max_storage_bytes", 100 * 1024 * 1024 * 1024),
            ("max_upload_bytes", 16 * 1024 * 1024 * 1024),
            ("scheduling_enabled", 1),
            ("priority_recovery", 0),
        ],
    ),
    (
        "business",
        "Business",
        &[
            ("max_concurrent_streams", 3),
            ("max_broadcasts", 30),
            ("max_storage_bytes", 400 * 1024 * 1024 * 1024),
            ("max_upload_bytes", 32 * 1024 * 1024 * 1024),
            ("scheduling_enabled", 1),
            ("priority_recovery", 1),
        ],
    ),
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

        let db = Self { conn: Arc::new(Mutex::new(conn)) };
        db.seed_plans()?;
        Ok(db)
    }

    /// The connection, for the credential store to share.
    pub fn raw(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.conn)
    }

    fn seed_plans(&self) -> Result<()> {
        let c = self.conn.lock().unwrap();
        for (id, label, limits) in SEED_PLANS {
            let map: BTreeMap<&str, i64> = limits.iter().copied().collect();
            let json = serde_json::to_string(&map).unwrap_or_else(|_| "{}".into());
            // Existing plans are left alone: an operator may have edited a
            // limit, and a restart must not undo that.
            c.execute(
                "INSERT INTO plans (id, label, limits) VALUES (?1, ?2, ?3)
                 ON CONFLICT(id) DO NOTHING",
                params![id, label, json],
            )?;
        }
        Ok(())
    }

    // --- plans ------------------------------------------------------------

    pub fn plan(&self, id: &str) -> Result<Plan> {
        let c = self.conn.lock().unwrap();
        let (label, json): (String, String) = c
            .query_row("SELECT label, limits FROM plans WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?
            .ok_or(CloudError::NotFound("plan"))?;
        Ok(Plan { id: id.to_string(), label, limits: serde_json::from_str(&json).unwrap_or_default() })
    }

    // --- users ------------------------------------------------------------

    pub fn create_user(&self, email: &str, password_hash: &str, plan_id: &str) -> Result<User> {
        let id = crate::new_id();
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO users (id, email, password_hash, plan_id) VALUES (?1, ?2, ?3, ?4)",
            params![id, email.trim(), password_hash, plan_id],
        )
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
                CloudError::EmailTaken
            }
            other => CloudError::Db(other),
        })?;
        c.execute("INSERT INTO subscriptions (user_id, plan_id) VALUES (?1, ?2)", params![id, plan_id])?;
        drop(c);
        self.user(&id)
    }

    pub fn user(&self, id: &str) -> Result<User> {
        self.conn
            .lock()
            .unwrap()
            .query_row("SELECT id, email, plan_id, created_at FROM users WHERE id=?1", [id], |r| {
                Ok(User { id: r.get(0)?, email: r.get(1)?, plan_id: r.get(2)?, created_at: r.get(3)? })
            })
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

    pub fn subscription(&self, user_id: &str) -> Result<Subscription> {
        let u = self.user(user_id)?;
        let plan = self.plan(&u.plan_id)?;
        let status: String = self
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT status FROM subscriptions WHERE user_id=?1", [user_id], |r| r.get(0))
            .optional()?
            .unwrap_or_else(|| "active".into());
        Ok(Subscription {
            user_id: u.id,
            plan_id: plan.id,
            plan_label: plan.label,
            status,
            limits: plan.limits,
        })
    }

    pub fn set_plan(&self, user_id: &str, plan_id: &str) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute("UPDATE users SET plan_id=?2 WHERE id=?1", params![user_id, plan_id])?;
        c.execute(
            "INSERT INTO subscriptions (user_id, plan_id) VALUES (?1, ?2)
             ON CONFLICT(user_id) DO UPDATE SET plan_id=excluded.plan_id,
                                                updated_at=datetime('now')",
            params![user_id, plan_id],
        )?;
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
                "SELECT id, user_id, label, rtmps_url, key_masked, created_at
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
                "SELECT id, user_id, label, rtmps_url, key_masked, created_at
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
            "SELECT id, user_id, label, rtmps_url, key_masked, created_at
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

    // --- what the manager writes back -------------------------------------

    /// The engine's own report: state, restarts and uptime. §17, §22.
    pub fn record_runtime(
        &self,
        id: &str,
        state: RuntimeState,
        restart_count: i64,
        uptime_secs: i64,
        bytes_sent: i64,
    ) -> Result<()> {
        self.raw().lock().unwrap().execute(
            "UPDATE broadcasts SET runtime_state=?2, restart_count=?3, uptime_secs=?4,
                    bytes_sent=?5, last_heartbeat=datetime('now')
             WHERE id=?1",
            params![id, state.id(), restart_count, uptime_secs, bytes_sent],
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

const BROADCAST_COLUMNS: &str = "SELECT id, user_id, name, media_id, destination_id, loop_forever,
        desired_state, runtime_state, restart_count, last_error, created_at, started_at,
        stopped_at, last_heartbeat, bytes_sent, uptime_secs, ffmpeg_exit_code FROM broadcasts";

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
    Ok(StreamDestination {
        id: r.get(0)?,
        user_id: r.get(1)?,
        label: r.get(2)?,
        rtmps_url: r.get(3)?,
        key_masked: r.get(4)?,
        created_at: r.get(5)?,
    })
}

fn row_to_broadcast(r: &rusqlite::Row<'_>) -> rusqlite::Result<Broadcast> {
    let desired: String = r.get(6)?;
    let runtime: String = r.get(7)?;
    Ok(Broadcast {
        id: r.get(0)?,
        user_id: r.get(1)?,
        name: r.get(2)?,
        media_id: r.get(3)?,
        destination_id: r.get(4)?,
        loop_forever: r.get::<_, i64>(5)? != 0,
        desired_state: DesiredState::from_id(&desired).unwrap_or(DesiredState::Stopped),
        runtime_state: RuntimeState::from_id(&runtime).unwrap_or(RuntimeState::Failed),
        restart_count: r.get(8)?,
        last_error: r.get(9)?,
        created_at: r.get(10)?,
        started_at: r.get(11)?,
        stopped_at: r.get(12)?,
        last_heartbeat: r.get(13)?,
        bytes_sent: r.get(14)?,
        uptime_secs: r.get(15)?,
        ffmpeg_exit_code: r.get(16)?,
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
