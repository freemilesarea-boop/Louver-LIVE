//! SQLite persistence (§36).

pub mod migrations;
pub mod models;

use crate::config::{OutputProfile, StreamMode};
use crate::error::{ErrorCode, LouverError, Result};
use crate::streaming::playlist::PlaybackMode;
use crate::streaming::state::StreamState;
use models::*;
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Thread-safe handle to the application database.
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
    path: Option<PathBuf>,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database").field("path", &self.path).finish()
    }
}

impl Database {
    /// Open (or create) the database, run migrations, and enable the pragmas
    /// that matter for a process that may be killed by a power cut (§32).
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut conn =
            Connection::open(path).map_err(|e| LouverError::with_detail(ErrorCode::DbOpen, e.to_string()))?;
        Self::configure(&conn)?;
        migrations::run(&mut conn)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)), path: Some(path.to_path_buf()) })
    }

    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()
            .map_err(|e| LouverError::with_detail(ErrorCode::DbOpen, e.to_string()))?;
        Self::configure(&conn)?;
        migrations::run(&mut conn)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)), path: None })
    }

    fn configure(conn: &Connection) -> Result<()> {
        // WAL survives an unclean shutdown far better than the rollback journal,
        // which is what a broadcast PC losing power actually does.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;",
        )
        .map_err(|e| LouverError::with_detail(ErrorCode::DbOpen, e.to_string()))?;
        Ok(())
    }

    /// Open the database, recovering from a corrupt file by moving it aside (§51).
    ///
    /// Losing the library is far better than refusing to start: the user can
    /// re-add their videos, but a broadcast PC that will not boot the app is
    /// dead until someone visits it.
    pub fn open_or_recover(path: &Path) -> Result<(Self, Option<PathBuf>)> {
        match Self::open(path) {
            Ok(db) if db.integrity_ok() => Ok((db, None)),
            Ok(_) | Err(_) => {
                if !path.exists() {
                    return Ok((Self::open(path)?, None));
                }
                let backup =
                    path.with_extension(format!("corrupt-{}.db", chrono::Utc::now().format("%Y%m%d%H%M%S")));
                std::fs::rename(path, &backup)?;
                // WAL sidecars belong to the old file.
                for ext in ["db-wal", "db-shm"] {
                    let _ = std::fs::remove_file(path.with_extension(ext));
                }
                Ok((Self::open(path)?, Some(backup)))
            }
        }
    }

    pub fn integrity_ok(&self) -> bool {
        let c = self.conn.lock().unwrap();
        c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .map(|s| s == "ok")
            .unwrap_or(false)
    }

    pub fn schema_version(&self) -> Result<i64> {
        migrations::current_version(&self.conn.lock().unwrap())
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    // -- settings ----------------------------------------------------------

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, datetime('now'))
             ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=datetime('now')",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn get_setting_or(&self, key: &str, default: &str) -> String {
        self.get_setting(key).ok().flatten().unwrap_or_else(|| default.to_string())
    }

    pub fn all_settings(&self) -> Result<Vec<(String, String)>> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare("SELECT key, value FROM settings ORDER BY key")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // -- media -------------------------------------------------------------

    pub fn upsert_media(&self, m: &Media) -> Result<i64> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO media (source_path, display_name, status, media_hash, normalized_path,
                normalized_profile, duration_secs, normalized_duration_secs, width, height, fps,
                video_codec, audio_codec, pixel_format, is_hdr, file_size, last_error)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
             ON CONFLICT(source_path) DO UPDATE SET
                display_name=excluded.display_name, status=excluded.status,
                media_hash=excluded.media_hash, normalized_path=excluded.normalized_path,
                normalized_profile=excluded.normalized_profile, duration_secs=excluded.duration_secs,
                normalized_duration_secs=excluded.normalized_duration_secs,
                width=excluded.width, height=excluded.height, fps=excluded.fps,
                video_codec=excluded.video_codec, audio_codec=excluded.audio_codec,
                pixel_format=excluded.pixel_format, is_hdr=excluded.is_hdr,
                file_size=excluded.file_size, last_error=excluded.last_error",
            params![
                m.source_path,
                m.display_name,
                m.status.id(),
                m.media_hash,
                m.normalized_path,
                m.normalized_profile,
                m.duration_secs,
                m.normalized_duration_secs,
                m.width,
                m.height,
                m.fps,
                m.video_codec,
                m.audio_codec,
                m.pixel_format,
                m.is_hdr as i32,
                m.file_size as i64,
                m.last_error
            ],
        )?;
        Ok(c.query_row("SELECT id FROM media WHERE source_path=?1", [&m.source_path], |r| r.get(0))?)
    }

    pub fn get_media(&self, id: i64) -> Result<Option<Media>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT * FROM media WHERE id=?1", [id], row_to_media)
            .optional()?)
    }

    /// The row for a source path, if the library already holds it.
    ///
    /// Adding the same file twice must not discard what is already known about
    /// it — a second add of a prepared file would otherwise reset it to
    /// "analysing" and prepare it all over again.
    pub fn find_media_by_path(&self, source_path: &str) -> Result<Option<Media>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT * FROM media WHERE source_path=?1", [source_path], row_to_media)
            .optional()?)
    }

    /// Write back what the probe found, leaving status and cache columns alone.
    #[allow(clippy::too_many_arguments)]
    pub fn update_media_metadata(&self, id: i64, m: &Media) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE media SET media_hash=?2, duration_secs=?3, width=?4, height=?5, fps=?6,
                video_codec=?7, audio_codec=?8, pixel_format=?9, is_hdr=?10, file_size=?11
             WHERE id=?1",
            params![
                id,
                m.media_hash,
                m.duration_secs,
                m.width,
                m.height,
                m.fps,
                m.video_codec,
                m.audio_codec,
                m.pixel_format,
                m.is_hdr,
                m.file_size,
            ],
        )?;
        Ok(())
    }

    pub fn list_media(&self) -> Result<Vec<Media>> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare("SELECT * FROM media ORDER BY added_at DESC, id DESC")?;
        let rows = st.query_map([], row_to_media)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn update_media_status(
        &self,
        id: i64,
        status: MediaStatus,
        normalized_path: Option<&str>,
        normalized_profile: Option<&str>,
        normalized_duration: Option<f64>,
        last_error: Option<&str>,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE media SET status=?2, normalized_path=COALESCE(?3, normalized_path),
                normalized_profile=COALESCE(?4, normalized_profile),
                normalized_duration_secs=COALESCE(?5, normalized_duration_secs),
                last_error=?6
             WHERE id=?1",
            params![id, status.id(), normalized_path, normalized_profile, normalized_duration, last_error],
        )?;
        Ok(())
    }

    pub fn delete_media(&self, id: i64) -> Result<()> {
        self.conn.lock().unwrap().execute("DELETE FROM media WHERE id=?1", [id])?;
        Ok(())
    }

    // -- playlists ---------------------------------------------------------

    pub fn create_playlist(&self, name: &str, mode: PlaybackMode, profile: OutputProfile) -> Result<i64> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO playlists (name, playback_mode, output_profile) VALUES (?1,?2,?3)",
            params![name, mode.id(), profile.id()],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn get_playlist(&self, id: i64) -> Result<Option<Playlist>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT * FROM playlists WHERE id=?1", [id], row_to_playlist)
            .optional()?)
    }

    pub fn list_playlists(&self) -> Result<Vec<Playlist>> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare("SELECT * FROM playlists ORDER BY created_at, id")?;
        let rows = st.query_map([], row_to_playlist)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn update_playlist(
        &self,
        id: i64,
        name: &str,
        mode: PlaybackMode,
        profile: OutputProfile,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE playlists SET name=?2, playback_mode=?3, output_profile=?4,
                updated_at=datetime('now') WHERE id=?1",
            params![id, name, mode.id(), profile.id()],
        )?;
        Ok(())
    }

    pub fn delete_playlist(&self, id: i64) -> Result<()> {
        self.conn.lock().unwrap().execute("DELETE FROM playlists WHERE id=?1", [id])?;
        Ok(())
    }

    // -- playlist items ----------------------------------------------------

    pub fn add_playlist_item(&self, playlist_id: i64, media_id: i64) -> Result<i64> {
        let c = self.conn.lock().unwrap();
        let next: i64 = c.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM playlist_items WHERE playlist_id=?1",
            [playlist_id],
            |r| r.get(0),
        )?;
        c.execute(
            "INSERT INTO playlist_items (playlist_id, media_id, position, enabled) VALUES (?1,?2,?3,1)",
            params![playlist_id, media_id, next],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn list_playlist_items(&self, playlist_id: i64) -> Result<Vec<PlaylistItem>> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare(
            "SELECT id, playlist_id, media_id, position, enabled FROM playlist_items
             WHERE playlist_id=?1 ORDER BY position, id",
        )?;
        let rows = st.query_map([playlist_id], |r| {
            Ok(PlaylistItem {
                id: r.get(0)?,
                playlist_id: r.get(1)?,
                media_id: r.get(2)?,
                position: r.get(3)?,
                enabled: r.get::<_, i64>(4)? != 0,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Persist a drag-and-drop reorder. `ordered_item_ids` is the new order (§11).
    pub fn reorder_playlist_items(&self, playlist_id: i64, ordered_item_ids: &[i64]) -> Result<()> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        for (pos, id) in ordered_item_ids.iter().enumerate() {
            tx.execute(
                "UPDATE playlist_items SET position=?3 WHERE id=?1 AND playlist_id=?2",
                params![id, playlist_id, pos as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_item_enabled(&self, item_id: i64, enabled: bool) -> Result<()> {
        self.conn
            .lock()
            .unwrap()
            .execute("UPDATE playlist_items SET enabled=?2 WHERE id=?1", params![item_id, enabled as i32])?;
        Ok(())
    }

    pub fn remove_playlist_item(&self, item_id: i64) -> Result<()> {
        self.conn.lock().unwrap().execute("DELETE FROM playlist_items WHERE id=?1", [item_id])?;
        Ok(())
    }

    // -- schedules ---------------------------------------------------------

    pub fn create_schedule(&self, s: &Schedule) -> Result<i64> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO schedules (playlist_id, days_of_week, start_time, end_time, enabled)
             VALUES (?1,?2,?3,?4,?5)",
            params![s.playlist_id, s.days_of_week.0 as i64, s.start_time, s.end_time, s.enabled as i32],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn list_schedules(&self) -> Result<Vec<Schedule>> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare(
            "SELECT id, playlist_id, days_of_week, start_time, end_time, enabled FROM schedules ORDER BY id",
        )?;
        let rows = st.query_map([], |r| {
            Ok(Schedule {
                id: r.get(0)?,
                playlist_id: r.get(1)?,
                days_of_week: DaysOfWeek(r.get::<_, i64>(2)? as u8),
                start_time: r.get(3)?,
                end_time: r.get(4)?,
                enabled: r.get::<_, i64>(5)? != 0,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn update_schedule(&self, s: &Schedule) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE schedules SET playlist_id=?2, days_of_week=?3, start_time=?4,
                end_time=?5, enabled=?6 WHERE id=?1",
            params![s.id, s.playlist_id, s.days_of_week.0 as i64, s.start_time, s.end_time, s.enabled as i32],
        )?;
        Ok(())
    }

    pub fn delete_schedule(&self, id: i64) -> Result<()> {
        self.conn.lock().unwrap().execute("DELETE FROM schedules WHERE id=?1", [id])?;
        Ok(())
    }

    // -- sessions ----------------------------------------------------------

    pub fn create_session(
        &self,
        playlist_id: i64,
        scheduled_end: Option<&str>,
        mode: StreamMode,
        playback_mode: PlaybackMode,
        order_seed: i64,
    ) -> Result<i64> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO stream_sessions
                (playlist_id, started_at, state, mode, playback_mode, order_seed, scheduled_end)
             VALUES (?1, datetime('now'), ?2, ?3, ?4, ?5, ?6)",
            params![
                playlist_id,
                StreamState::Preparing.as_str(),
                serde_json::to_string(&mode)?.trim_matches('"'),
                playback_mode.id(),
                order_seed,
                scheduled_end
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn update_session_state(
        &self,
        id: i64,
        state: StreamState,
        restart_count: i64,
        user_stop: bool,
        last_error: Option<&str>,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE stream_sessions SET state=?2, restart_count=?3, user_requested_stop=?4,
                last_error=?5,
                ended_at = CASE WHEN ?2 IN ('STOPPED','ERROR') THEN datetime('now') ELSE ended_at END
             WHERE id=?1",
            params![id, state.as_str(), restart_count, user_stop as i32, last_error],
        )?;
        Ok(())
    }

    pub fn get_session(&self, id: i64) -> Result<Option<StreamSession>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT * FROM stream_sessions WHERE id=?1", [id], row_to_session)
            .optional()?)
    }

    /// The most recent session, used at startup to detect an unclean exit (§32).
    pub fn latest_session(&self) -> Result<Option<StreamSession>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT * FROM stream_sessions ORDER BY id DESC LIMIT 1", [], row_to_session)
            .optional()?)
    }

    /// Sessions left in a running state by a crash or power cut.
    pub fn unfinished_sessions(&self) -> Result<Vec<StreamSession>> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare(
            "SELECT * FROM stream_sessions
             WHERE ended_at IS NULL AND state NOT IN ('STOPPED','ERROR','IDLE')
             ORDER BY id DESC",
        )?;
        let rows = st.query_map([], row_to_session)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Close out sessions that a previous process left dangling.
    pub fn mark_orphaned_sessions(&self, reason: &str) -> Result<usize> {
        Ok(self.conn.lock().unwrap().execute(
            "UPDATE stream_sessions SET state='ERROR', ended_at=datetime('now'), last_error=?1
             WHERE ended_at IS NULL AND state NOT IN ('STOPPED','ERROR','IDLE')",
            [reason],
        )?)
    }

    // -- events ------------------------------------------------------------

    pub fn log_event(
        &self,
        session_id: Option<i64>,
        level: EventLevel,
        code: Option<&str>,
        message: &str,
    ) -> Result<()> {
        // Defence in depth: nothing reaches the event table unmasked (§34).
        let masked = crate::streaming::ffmpeg::mask_secrets(message);
        self.conn.lock().unwrap().execute(
            "INSERT INTO stream_events (session_id, level, code, message) VALUES (?1,?2,?3,?4)",
            params![session_id, level.id(), code, masked],
        )?;
        Ok(())
    }

    pub fn recent_events(&self, limit: i64) -> Result<Vec<StreamEvent>> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare(
            "SELECT id, session_id, at, level, code, message FROM stream_events
             ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = st.query_map([limit], |r| {
            Ok(StreamEvent {
                id: r.get(0)?,
                session_id: r.get(1)?,
                at: r.get(2)?,
                level: EventLevel::from_id(&r.get::<_, String>(3)?),
                code: r.get(4)?,
                message: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Keep the event table bounded on a machine that runs for months.
    pub fn prune_events(&self, keep: i64) -> Result<usize> {
        Ok(self.conn.lock().unwrap().execute(
            "DELETE FROM stream_events WHERE id NOT IN
                (SELECT id FROM stream_events ORDER BY id DESC LIMIT ?1)",
            [keep],
        )?)
    }
}

fn row_to_media(r: &Row<'_>) -> rusqlite::Result<Media> {
    Ok(Media {
        id: r.get("id")?,
        source_path: r.get("source_path")?,
        display_name: r.get("display_name")?,
        status: MediaStatus::from_id(&r.get::<_, String>("status")?).unwrap_or(MediaStatus::Failed),
        media_hash: r.get("media_hash")?,
        normalized_path: r.get("normalized_path")?,
        normalized_profile: r.get("normalized_profile")?,
        duration_secs: r.get("duration_secs")?,
        normalized_duration_secs: r.get("normalized_duration_secs")?,
        width: r.get::<_, i64>("width")? as u32,
        height: r.get::<_, i64>("height")? as u32,
        fps: r.get("fps")?,
        video_codec: r.get("video_codec")?,
        audio_codec: r.get("audio_codec")?,
        pixel_format: r.get("pixel_format")?,
        is_hdr: r.get::<_, i64>("is_hdr")? != 0,
        file_size: r.get::<_, i64>("file_size")? as u64,
        added_at: r.get("added_at")?,
        last_error: r.get("last_error")?,
    })
}

fn row_to_playlist(r: &Row<'_>) -> rusqlite::Result<Playlist> {
    Ok(Playlist {
        id: r.get("id")?,
        name: r.get("name")?,
        playback_mode: PlaybackMode::from_id(&r.get::<_, String>("playback_mode")?).unwrap_or_default(),
        output_profile: OutputProfile::from_id(&r.get::<_, String>("output_profile")?).unwrap_or_default(),
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
    })
}

fn row_to_session(r: &Row<'_>) -> rusqlite::Result<StreamSession> {
    Ok(StreamSession {
        id: r.get("id")?,
        playlist_id: r.get("playlist_id")?,
        started_at: r.get("started_at")?,
        ended_at: r.get("ended_at")?,
        scheduled_end: r.get("scheduled_end")?,
        state: StreamState::from_str_opt(&r.get::<_, String>("state")?).unwrap_or_default(),
        mode: match r.get::<_, String>("mode")?.as_str() {
            "compatibility_encode" => StreamMode::CompatibilityEncode,
            _ => StreamMode::StreamCopy,
        },
        playback_mode: PlaybackMode::from_id(&r.get::<_, String>("playback_mode")?).unwrap_or_default(),
        order_seed: r.get("order_seed")?,
        restart_count: r.get("restart_count")?,
        user_requested_stop: r.get::<_, i64>("user_requested_stop")? != 0,
        last_error: r.get("last_error")?,
    })
}

/// Broadcast metadata presets and the chat rotation (V2).
///
/// Kept alongside the rest of the app's state rather than in a separate store:
/// a preset is ordinary user data, and nothing here is a secret.
impl Database {
    pub fn list_presets(&self) -> Result<Vec<crate::youtube::BroadcastPreset>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT id, name, title, description, tags, category_id, privacy
             FROM broadcast_presets ORDER BY name COLLATE NOCASE",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(crate::youtube::BroadcastPreset {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    metadata: crate::youtube::BroadcastMetadata {
                        title: r.get(2)?,
                        description: r.get(3)?,
                        tags: split_tags(&r.get::<_, String>(4)?),
                        category_id: r.get(5)?,
                        privacy: crate::youtube::Privacy::from_api(&r.get::<_, String>(6)?),
                    },
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Insert or replace by name, so saving twice under one name updates it
    /// rather than filling the list with near-duplicates.
    pub fn save_preset(&self, name: &str, m: &crate::youtube::BroadcastMetadata) -> Result<i64> {
        let name = name.trim();
        if name.is_empty() {
            return Err(LouverError::with_detail(ErrorCode::ConfigInvalid, "프리셋 이름을 입력해주세요"));
        }
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO broadcast_presets (name, title, description, tags, category_id, privacy)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(name) DO UPDATE SET
               title = excluded.title, description = excluded.description,
               tags = excluded.tags, category_id = excluded.category_id,
               privacy = excluded.privacy, updated_at = datetime('now')",
            rusqlite::params![
                name,
                m.title,
                m.description,
                m.tags.join("\n"),
                m.category_id,
                m.privacy.as_api()
            ],
        )?;
        Ok(c.query_row("SELECT id FROM broadcast_presets WHERE name = ?1", [name], |r| r.get(0))?)
    }

    pub fn delete_preset(&self, id: i64) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute("DELETE FROM broadcast_presets WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn list_chat_messages(&self) -> Result<Vec<crate::youtube::ChatMessage>> {
        let c = self.conn.lock().unwrap();
        let mut stmt =
            c.prepare("SELECT id, position, text, enabled FROM chat_messages ORDER BY position, id")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(crate::youtube::ChatMessage {
                    id: r.get(0)?,
                    position: r.get(1)?,
                    text: r.get(2)?,
                    enabled: r.get::<_, i64>(3)? != 0,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn add_chat_message(&self, text: &str) -> Result<i64> {
        let text = text.trim();
        if text.is_empty() {
            return Err(LouverError::with_detail(ErrorCode::ConfigInvalid, "메시지를 입력해주세요"));
        }
        if text.chars().count() > crate::youtube::api::MAX_CHAT_MESSAGE_CHARS {
            return Err(LouverError::new(ErrorCode::ChatMessageTooLong));
        }
        let c = self.conn.lock().unwrap();
        let next: i64 =
            c.query_row("SELECT COALESCE(MAX(position), -1) + 1 FROM chat_messages", [], |r| r.get(0))?;
        c.execute(
            "INSERT INTO chat_messages (position, text) VALUES (?1, ?2)",
            rusqlite::params![next, text],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn update_chat_message(&self, id: i64, text: &str, enabled: bool) -> Result<()> {
        let text = text.trim();
        if text.chars().count() > crate::youtube::api::MAX_CHAT_MESSAGE_CHARS {
            return Err(LouverError::new(ErrorCode::ChatMessageTooLong));
        }
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE chat_messages SET text = ?2, enabled = ?3 WHERE id = ?1",
            rusqlite::params![id, text, enabled as i64],
        )?;
        Ok(())
    }

    pub fn delete_chat_message(&self, id: i64) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute("DELETE FROM chat_messages WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Rewrite the order from a list of ids.
    pub fn reorder_chat_messages(&self, ids: &[i64]) -> Result<()> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        for (i, id) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE chat_messages SET position = ?2 WHERE id = ?1",
                rusqlite::params![id, i as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

fn split_tags(s: &str) -> Vec<String> {
    s.lines().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(path: &str) -> Media {
        Media {
            id: 0,
            source_path: path.into(),
            display_name: path.rsplit('/').next().unwrap().into(),
            status: MediaStatus::Imported,
            media_hash: "h".into(),
            normalized_path: None,
            normalized_profile: None,
            duration_secs: 60.0,
            normalized_duration_secs: None,
            width: 1920,
            height: 1080,
            fps: 30.0,
            video_codec: "h264".into(),
            audio_codec: Some("aac".into()),
            pixel_format: Some("yuv420p".into()),
            is_hdr: false,
            file_size: 1000,
            added_at: String::new(),
            last_error: None,
        }
    }

    #[test]
    fn migration_runs_on_open() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), migrations::latest_version());
        assert!(db.integrity_ok());
    }

    #[test]
    fn settings_crud() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.get_setting("rtmps_url").unwrap(), None);
        db.set_setting("rtmps_url", "rtmps://a/live2").unwrap();
        assert_eq!(db.get_setting("rtmps_url").unwrap().as_deref(), Some("rtmps://a/live2"));
        db.set_setting("rtmps_url", "rtmps://b/live2").unwrap();
        assert_eq!(db.get_setting("rtmps_url").unwrap().as_deref(), Some("rtmps://b/live2"));
        assert_eq!(db.get_setting_or("missing", "fallback"), "fallback");
        assert_eq!(db.all_settings().unwrap().len(), 1);
    }

    #[test]
    fn media_crud_and_upsert_by_path() {
        let db = Database::open_in_memory().unwrap();
        let id = db.upsert_media(&media("/v/a.mp4")).unwrap();
        assert_eq!(db.list_media().unwrap().len(), 1);

        // Re-importing the same path updates rather than duplicating.
        let mut m2 = media("/v/a.mp4");
        m2.duration_secs = 99.0;
        let id2 = db.upsert_media(&m2).unwrap();
        assert_eq!(id, id2);
        assert_eq!(db.list_media().unwrap().len(), 1);
        assert_eq!(db.get_media(id).unwrap().unwrap().duration_secs, 99.0);

        db.update_media_status(
            id,
            MediaStatus::Normalized,
            Some("/cache/n.mp4"),
            Some("1080p30"),
            Some(98.9),
            None,
        )
        .unwrap();
        let got = db.get_media(id).unwrap().unwrap();
        assert_eq!(got.status, MediaStatus::Normalized);
        assert_eq!(got.normalized_path.as_deref(), Some("/cache/n.mp4"));
        assert_eq!(got.normalized_duration_secs, Some(98.9));

        db.delete_media(id).unwrap();
        assert!(db.get_media(id).unwrap().is_none());
    }

    #[test]
    fn korean_and_spaced_paths_round_trip_through_sqlite() {
        let db = Database::open_in_memory().unwrap();
        let p = "/Users/test/Music/오늘 밤 재즈.mp4";
        let id = db.upsert_media(&media(p)).unwrap();
        assert_eq!(db.get_media(id).unwrap().unwrap().source_path, p);
    }

    #[test]
    fn playlist_items_order_and_reorder() {
        let db = Database::open_in_memory().unwrap();
        let pl = db.create_playlist("Night Jazz", PlaybackMode::Sequential, OutputProfile::P1080p30).unwrap();
        let ids: Vec<i64> = (0..3)
            .map(|i| {
                let m = db.upsert_media(&media(&format!("/v/{i}.mp4"))).unwrap();
                db.add_playlist_item(pl, m).unwrap()
            })
            .collect();

        let items = db.list_playlist_items(pl).unwrap();
        assert_eq!(items.iter().map(|i| i.position).collect::<Vec<_>>(), vec![0, 1, 2]);

        // Drag the last item to the front.
        db.reorder_playlist_items(pl, &[ids[2], ids[0], ids[1]]).unwrap();
        let items = db.list_playlist_items(pl).unwrap();
        assert_eq!(items.iter().map(|i| i.id).collect::<Vec<_>>(), vec![ids[2], ids[0], ids[1]]);

        db.set_item_enabled(ids[0], false).unwrap();
        assert!(!db.list_playlist_items(pl).unwrap().iter().find(|i| i.id == ids[0]).unwrap().enabled);

        db.remove_playlist_item(ids[0]).unwrap();
        assert_eq!(db.list_playlist_items(pl).unwrap().len(), 2);
    }

    #[test]
    fn deleting_a_playlist_cascades_to_its_items() {
        let db = Database::open_in_memory().unwrap();
        let pl = db.create_playlist("x", PlaybackMode::Sequential, OutputProfile::P1080p30).unwrap();
        let m = db.upsert_media(&media("/v/a.mp4")).unwrap();
        db.add_playlist_item(pl, m).unwrap();
        db.delete_playlist(pl).unwrap();
        assert!(db.list_playlist_items(pl).unwrap().is_empty());
    }

    #[test]
    fn playlist_update_persists_mode_and_profile() {
        let db = Database::open_in_memory().unwrap();
        let pl = db.create_playlist("x", PlaybackMode::Sequential, OutputProfile::P1080p30).unwrap();
        db.update_playlist(pl, "24H Lo-Fi", PlaybackMode::ShuffleOnce, OutputProfile::P720p30).unwrap();
        let p = db.get_playlist(pl).unwrap().unwrap();
        assert_eq!(p.name, "24H Lo-Fi");
        assert_eq!(p.playback_mode, PlaybackMode::ShuffleOnce);
        assert_eq!(p.output_profile, OutputProfile::P720p30);
    }

    #[test]
    fn schedule_crud() {
        let db = Database::open_in_memory().unwrap();
        let pl = db.create_playlist("x", PlaybackMode::Sequential, OutputProfile::P1080p30).unwrap();
        let mut s = Schedule {
            id: 0,
            playlist_id: pl,
            days_of_week: DaysOfWeek::weekdays(),
            start_time: "20:00".into(),
            end_time: "02:00".into(),
            enabled: true,
        };
        s.id = db.create_schedule(&s).unwrap();
        let got = &db.list_schedules().unwrap()[0];
        assert_eq!(got.days_of_week, DaysOfWeek::weekdays());
        assert_eq!(got.end_time, "02:00");

        s.enabled = false;
        db.update_schedule(&s).unwrap();
        assert!(!db.list_schedules().unwrap()[0].enabled);
        db.delete_schedule(s.id).unwrap();
        assert!(db.list_schedules().unwrap().is_empty());
    }

    #[test]
    fn sessions_track_lifecycle_and_orphans() {
        let db = Database::open_in_memory().unwrap();
        let pl = db.create_playlist("x", PlaybackMode::Sequential, OutputProfile::P1080p30).unwrap();
        let sid = db
            .create_session(
                pl,
                Some("2026-01-02 08:00"),
                StreamMode::StreamCopy,
                PlaybackMode::Sequential,
                4242,
            )
            .unwrap();

        db.update_session_state(sid, StreamState::Live, 0, false, None).unwrap();
        let s = db.get_session(sid).unwrap().unwrap();
        assert_eq!(s.state, StreamState::Live);
        assert_eq!(s.order_seed, 4242);
        assert!(s.ended_at.is_none());
        assert_eq!(db.unfinished_sessions().unwrap().len(), 1);

        // Simulate a crash: a new process finds the session still LIVE.
        assert_eq!(db.mark_orphaned_sessions("unclean shutdown").unwrap(), 1);
        assert!(db.unfinished_sessions().unwrap().is_empty());
        assert!(db.get_session(sid).unwrap().unwrap().ended_at.is_some());
    }

    #[test]
    fn stopping_a_session_sets_ended_at() {
        let db = Database::open_in_memory().unwrap();
        let pl = db.create_playlist("x", PlaybackMode::Sequential, OutputProfile::P1080p30).unwrap();
        let sid = db.create_session(pl, None, StreamMode::StreamCopy, PlaybackMode::Sequential, 1).unwrap();
        db.update_session_state(sid, StreamState::Stopped, 3, true, None).unwrap();
        let s = db.latest_session().unwrap().unwrap();
        assert_eq!(s.id, sid);
        assert!(s.ended_at.is_some());
        assert!(s.user_requested_stop);
        assert_eq!(s.restart_count, 3);
    }

    #[test]
    fn events_are_masked_before_storage() {
        let db = Database::open_in_memory().unwrap();
        db.log_event(
            None,
            EventLevel::Error,
            Some("LL-STREAM-002"),
            "failed publishing to rtmps://a.rtmps.youtube.com/live2/abcd-efgh-ijkl-mnop",
        )
        .unwrap();
        let ev = &db.recent_events(10).unwrap()[0];
        assert!(!ev.message.contains("abcd-efgh"), "stream key stored in the DB: {}", ev.message);
        assert!(ev.message.contains("••••"));
        assert_eq!(ev.code.as_deref(), Some("LL-STREAM-002"));
    }

    #[test]
    fn events_can_be_pruned() {
        let db = Database::open_in_memory().unwrap();
        for i in 0..50 {
            db.log_event(None, EventLevel::Info, None, &format!("e{i}")).unwrap();
        }
        db.prune_events(10).unwrap();
        assert_eq!(db.recent_events(100).unwrap().len(), 10);
    }

    #[test]
    fn a_corrupt_database_is_moved_aside_so_the_app_still_starts() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("louver.db");
        std::fs::write(&p, b"this is definitely not a sqlite file, not even close").unwrap();

        let (db, backup) = Database::open_or_recover(&p).unwrap();
        assert!(backup.is_some(), "the corrupt file should have been preserved");
        assert!(backup.unwrap().exists());
        assert!(db.integrity_ok());
        assert_eq!(db.schema_version().unwrap(), migrations::latest_version());
        // And it is usable.
        db.set_setting("k", "v").unwrap();
    }

    #[test]
    fn open_or_recover_leaves_a_healthy_database_alone() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("louver.db");
        {
            let db = Database::open(&p).unwrap();
            db.set_setting("keep", "me").unwrap();
        }
        let (db, backup) = Database::open_or_recover(&p).unwrap();
        assert!(backup.is_none());
        assert_eq!(db.get_setting("keep").unwrap().as_deref(), Some("me"), "data must survive");
    }

    #[test]
    fn data_survives_reopening_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("louver.db");
        let pl = {
            let db = Database::open(&p).unwrap();
            let pl = db
                .create_playlist("Morning Jazz", PlaybackMode::ShuffleOnce, OutputProfile::P720p30)
                .unwrap();
            db.set_setting("launch_at_startup", "true").unwrap();
            pl
        };
        let db = Database::open(&p).unwrap();
        assert_eq!(db.get_playlist(pl).unwrap().unwrap().name, "Morning Jazz");
        assert_eq!(db.get_setting("launch_at_startup").unwrap().as_deref(), Some("true"));
    }
}
