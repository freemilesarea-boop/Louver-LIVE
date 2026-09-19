//! Versioned schema migrations (§36).
//!
//! Each migration is applied once inside a transaction and recorded in
//! `schema_migrations`. Never edit a shipped migration; append a new one.

use crate::error::{ErrorCode, LouverError, Result};
use rusqlite::Connection;

pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial_schema",
        sql: r#"
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE media (
    id                      INTEGER PRIMARY KEY AUTOINCREMENT,
    source_path             TEXT NOT NULL UNIQUE,
    display_name            TEXT NOT NULL,
    status                  TEXT NOT NULL,
    media_hash              TEXT NOT NULL,
    normalized_path         TEXT,
    normalized_profile      TEXT,
    duration_secs           REAL NOT NULL DEFAULT 0,
    normalized_duration_secs REAL,
    width                   INTEGER NOT NULL DEFAULT 0,
    height                  INTEGER NOT NULL DEFAULT 0,
    fps                     REAL NOT NULL DEFAULT 0,
    video_codec             TEXT NOT NULL DEFAULT '',
    audio_codec             TEXT,
    pixel_format            TEXT,
    is_hdr                  INTEGER NOT NULL DEFAULT 0,
    file_size               INTEGER NOT NULL DEFAULT 0,
    added_at                TEXT NOT NULL DEFAULT (datetime('now')),
    last_error              TEXT
);
CREATE INDEX idx_media_hash ON media(media_hash);

CREATE TABLE playlists (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    name           TEXT NOT NULL,
    playback_mode  TEXT NOT NULL DEFAULT 'sequential',
    output_profile TEXT NOT NULL DEFAULT '1080p30',
    created_at     TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at     TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE playlist_items (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    media_id    INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX idx_items_playlist ON playlist_items(playlist_id, position);

CREATE TABLE schedules (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    playlist_id  INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    days_of_week INTEGER NOT NULL,
    start_time   TEXT NOT NULL,
    end_time     TEXT NOT NULL,
    enabled      INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE stream_sessions (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    playlist_id          INTEGER NOT NULL,
    started_at           TEXT NOT NULL,
    ended_at             TEXT,
    scheduled_end        TEXT,
    state                TEXT NOT NULL,
    mode                 TEXT NOT NULL DEFAULT 'stream_copy',
    playback_mode        TEXT NOT NULL DEFAULT 'sequential',
    order_seed           INTEGER NOT NULL DEFAULT 0,
    restart_count        INTEGER NOT NULL DEFAULT 0,
    user_requested_stop  INTEGER NOT NULL DEFAULT 0,
    last_error           TEXT
);
CREATE INDEX idx_sessions_started ON stream_sessions(started_at DESC);

CREATE TABLE stream_events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER REFERENCES stream_sessions(id) ON DELETE CASCADE,
    at         TEXT NOT NULL DEFAULT (datetime('now')),
    level      TEXT NOT NULL,
    code       TEXT,
    message    TEXT NOT NULL
);
CREATE INDEX idx_events_session ON stream_events(session_id, at DESC);
"#,
    },
];

/// Highest schema version this build knows about.
pub fn latest_version() -> i64 {
    MIGRATIONS.iter().map(|m| m.version).max().unwrap_or(0)
}

/// Apply every migration newer than the recorded version.
pub fn run(conn: &mut Connection) -> Result<i64> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    INTEGER PRIMARY KEY,
             name       TEXT NOT NULL,
             applied_at TEXT NOT NULL DEFAULT (datetime('now'))
         );",
    )
    .map_err(|e| LouverError::with_detail(ErrorCode::DbMigration, e.to_string()))?;

    let current: i64 = conn
        .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_migrations", [], |r| r.get(0))
        .map_err(|e| LouverError::with_detail(ErrorCode::DbMigration, e.to_string()))?;

    for m in MIGRATIONS.iter().filter(|m| m.version > current) {
        let tx = conn
            .transaction()
            .map_err(|e| LouverError::with_detail(ErrorCode::DbMigration, e.to_string()))?;
        tx.execute_batch(m.sql).map_err(|e| {
            LouverError::with_detail(
                ErrorCode::DbMigration,
                format!("migration {} ({}): {e}", m.version, m.name),
            )
        })?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![m.version, m.name],
        )
        .map_err(|e| LouverError::with_detail(ErrorCode::DbMigration, e.to_string()))?;
        tx.commit()
            .map_err(|e| LouverError::with_detail(ErrorCode::DbMigration, e.to_string()))?;
    }

    Ok(latest_version())
}

/// Recorded schema version of an open database.
pub fn current_version(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COALESCE(MAX(version), 0) FROM schema_migrations", [], |r| r.get(0))
        .map_err(|e| LouverError::with_detail(ErrorCode::DbMigration, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_versions_are_unique_and_ordered() {
        let mut seen = std::collections::HashSet::new();
        let mut last = 0;
        for m in MIGRATIONS {
            assert!(seen.insert(m.version), "duplicate migration {}", m.version);
            assert!(m.version > last, "migrations must be in ascending order");
            last = m.version;
        }
    }

    #[test]
    fn migrations_create_every_required_table() {
        let mut c = Connection::open_in_memory().unwrap();
        run(&mut c).unwrap();
        let required = [
            "settings", "media", "playlists", "playlist_items",
            "schedules", "stream_sessions", "stream_events",
        ];
        for t in required {
            let n: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "table {t} missing (§36)");
        }
    }

    #[test]
    fn running_migrations_twice_is_a_no_op() {
        let mut c = Connection::open_in_memory().unwrap();
        let v1 = run(&mut c).unwrap();
        let v2 = run(&mut c).unwrap();
        assert_eq!(v1, v2);
        assert_eq!(current_version(&c).unwrap(), latest_version());
        let applied: i64 = c.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0)).unwrap();
        assert_eq!(applied, MIGRATIONS.len() as i64);
    }

    #[test]
    fn version_is_recorded_so_upgrades_are_detectable() {
        let mut c = Connection::open_in_memory().unwrap();
        assert!(current_version(&c).is_err(), "no table yet");
        run(&mut c).unwrap();
        assert_eq!(current_version(&c).unwrap(), 1);
    }
}
