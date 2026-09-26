//! Several broadcasts at once, each one the desktop's engine.
//!
//! §2 asks for up to three independent lifecycles where a fault in one cannot
//! touch another. The cheapest way to get that — and the only way that reuses
//! the tested engine — is not to generalise `BroadcastRuntime` into something
//! multi-tenant. It is to run **one of them per broadcast**, each on its own
//! thread, with its own working directory, its own core database, its own
//! manifest and its own FFmpeg child. Isolation then comes from the operating
//! system rather than from care.
//!
//! What this module therefore does *not* contain: a restart policy, a backoff
//! schedule, a health check, or a state machine. `StreamSupervisor` has all
//! four, tested, including the rule that a broadcast the user stopped is never
//! restarted. This module reconciles intent with reality and stays out of the
//! way.

use crate::db::CloudDb;
use crate::models::{Broadcast, DesiredState, RuntimeState};
use crate::storage::Storage;
use crate::Result;
use louver_core::clock::SystemClock;
use louver_core::config::{OutputProfile, StreamMode};
use louver_core::database::models::{EventLevel, Media as CoreMedia, MediaStatus};
use louver_core::database::Database;
use louver_core::runtime::{
    BroadcastRuntime, FfmpegLauncher, RuntimeEvents, RuntimeStatus, StartOptions, StartReason, StreamLauncher,
};
use louver_core::security::StreamKeyStore;
use louver_core::session::SessionStore;
use louver_core::streaming::ffmpeg::{FfmpegCommandBuilder, FfmpegTools};
use louver_core::streaming::playlist::PlaybackMode;
use louver_core::system::NoopSleepPreventer;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// How often each broadcast's thread ticks. The desktop uses the same period.
const TICK: std::time::Duration = std::time::Duration::from_secs(1);

/// Consecutive engine failures before a broadcast is given up on.
///
/// The supervisor's own backoff caps at 60 seconds and would retry for ever,
/// which is right for a desktop a person is watching and wrong for a server.
/// §5 asks for a ceiling, so past this the broadcast becomes `FAILED` and the
/// thread stops rather than reconnecting into the night.
const MAX_RESTARTS: i64 = 10;

/// Makes the thing that launches a broadcast's process.
///
/// One per broadcast, because `FfmpegLauncher` carries the log sink that writes
/// into *that* broadcast's event table. A seam rather than a hardcoded
/// constructor so the isolation, crash and stop tests can drive the worker loop
/// without spawning real encoders — the same reason `StreamLauncher` is a trait
/// in the core.
pub trait LauncherFactory: Send + Sync + std::fmt::Debug {
    fn for_broadcast(&self, db: &CloudDb, broadcast_id: &str) -> Arc<dyn StreamLauncher>;
}

/// The real one: the bundled FFmpeg, logging into the broadcast's events.
#[derive(Debug)]
pub struct FfmpegLaunchers {
    pub program: PathBuf,
}

impl LauncherFactory for FfmpegLaunchers {
    fn for_broadcast(&self, db: &CloudDb, broadcast_id: &str) -> Arc<dyn StreamLauncher> {
        let db = db.clone();
        let id = broadcast_id.to_string();
        Arc::new(FfmpegLauncher {
            program: self.program.clone(),
            // The core masks the stream key before a line reaches here.
            log: Arc::new(move |line: &str| {
                let _ = db.append_event(&id, EventLevel::Info, line);
            }),
        })
    }
}

/// Everything a worker thread needs, owned rather than borrowed.
struct Worker {
    handle: Option<std::thread::JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

/// Writes the engine's own status and log lines into the cloud database.
///
/// This is where a broadcast's row learns that FFmpeg died: the engine reports
/// it through the same `RuntimeEvents` trait the desktop uses to update its
/// dashboard.
struct DbEvents {
    db: CloudDb,
    broadcast_id: String,
    sent: Mutex<Meter>,
}

/// Bytes pushed to the ingest, accumulated across restarts.
///
/// Each FFmpeg reports its own running total and a replacement process starts
/// again at zero, so the outgoing one's final figure is banked before the new
/// one begins counting. Without that, a night of reconnects would bill as less
/// bandwidth than it used — §22 wants a number that can be costed, not a
/// number that resets.
#[derive(Debug, Default)]
struct Meter {
    banked: i64,
    current: i64,
}

impl Meter {
    fn observe(&mut self, reported: i64) -> i64 {
        if reported < self.current {
            self.banked += self.current;
        }
        self.current = reported;
        self.banked + self.current
    }
}

impl RuntimeEvents for DbEvents {
    fn on_status(&self, status: &RuntimeStatus) {
        let sent = self
            .sent
            .lock()
            .map(|mut m| m.observe(status.supervisor.progress.total_bytes as i64))
            .unwrap_or(0);
        let _ = self.db.record_runtime(
            &self.broadcast_id,
            RuntimeState::from_engine(status.supervisor.state),
            status.supervisor.restart_count as i64,
            status.elapsed_secs,
            sent,
        );
    }

    fn on_log(&self, level: EventLevel, message: &str) {
        // `message` has already been through the core's masking on its way
        // here; nothing in this module adds a secret to it.
        let _ = self.db.append_event(&self.broadcast_id, level, message);
    }
}

/// Owns every running broadcast.
#[derive(Clone)]
pub struct BroadcastManager {
    db: CloudDb,
    storage: Arc<dyn Storage>,
    work_root: PathBuf,
    tools: FfmpegTools,
    encoder: String,
    keys: Arc<dyn louver_core::security::SecretStore>,
    launchers: Arc<dyn LauncherFactory>,
    workers: Arc<Mutex<HashMap<String, Worker>>>,
}

impl std::fmt::Debug for BroadcastManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BroadcastManager").field("storage", &self.storage.backend_name()).finish()
    }
}

impl BroadcastManager {
    pub fn new(
        db: CloudDb,
        storage: Arc<dyn Storage>,
        work_root: impl Into<PathBuf>,
        tools: FfmpegTools,
        encoder: String,
        keys: Arc<dyn louver_core::security::SecretStore>,
        launchers: Arc<dyn LauncherFactory>,
    ) -> Self {
        Self {
            db,
            storage,
            work_root: work_root.into(),
            tools,
            encoder,
            keys,
            launchers,
            workers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The store broadcasts read their stream keys from.
    ///
    /// Exposed so the process that saves a destination's key writes it where
    /// the manager will look, rather than each guessing an account name.
    pub fn secret_store(&self) -> Arc<dyn louver_core::security::SecretStore> {
        Arc::clone(&self.keys)
    }

    /// Broadcast ids with a live worker thread.
    pub fn running_ids(&self) -> Vec<String> {
        self.workers.lock().unwrap().keys().cloned().collect()
    }

    fn work_dir(&self, broadcast_id: &str) -> PathBuf {
        self.work_root.join(broadcast_id)
    }

    /// Start a broadcast the caller owns, if their plan has room.
    ///
    /// The slot is claimed first, inside a transaction, so the entitlement
    /// decision is made before any process is spawned. A start that then fails
    /// releases the slot rather than leaving it held by nothing.
    pub fn start(&self, user_id: &str, broadcast_id: &str) -> Result<()> {
        self.db.claim_stream_slot(user_id, broadcast_id)?;
        match self.spawn_worker(broadcast_id) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = self.db.release_stream_slot(user_id, broadcast_id);
                let _ = self.db.record_failure(broadcast_id, &e.to_string());
                Err(e)
            }
        }
    }

    /// Stop a broadcast the caller owns.
    ///
    /// `desired_state` goes to `stopped` before the thread is asked to finish,
    /// so a tick racing this sees the intent and does not restart.
    pub fn stop(&self, user_id: &str, broadcast_id: &str) -> Result<()> {
        self.db.release_stream_slot(user_id, broadcast_id)?;
        self.halt_worker(broadcast_id);
        self.db.record_runtime_only(broadcast_id, RuntimeState::Stopped)?;
        Ok(())
    }

    pub fn restart(&self, user_id: &str, broadcast_id: &str) -> Result<()> {
        self.stop(user_id, broadcast_id)?;
        self.start(user_id, broadcast_id)
    }

    /// Bring back everything that was meant to be running. §6.
    ///
    /// Reads `desired_state` alone. A broadcast the user stopped before the
    /// restart has `desired_state = stopped` and is left alone, which is the
    /// same rule the supervisor applies to a crash.
    pub fn recover_all(&self) -> Result<usize> {
        let wanted = self.db.broadcasts_wanting_to_run()?;
        let mut started = 0;
        for b in wanted {
            match self.spawn_worker(&b.id) {
                Ok(()) => started += 1,
                Err(e) => {
                    let _ = self.db.record_failure(&b.id, &format!("복구 실패: {e}"));
                }
            }
        }
        Ok(started)
    }

    /// Ask every worker to finish, and wait. For a clean shutdown.
    pub fn shutdown(&self) {
        let ids: Vec<String> = self.running_ids();
        for id in ids {
            self.halt_worker(&id);
        }
    }

    fn halt_worker(&self, broadcast_id: &str) {
        let worker = self.workers.lock().unwrap().remove(broadcast_id);
        if let Some(mut w) = worker {
            w.stop.store(true, Ordering::SeqCst);
            if let Some(h) = w.handle.take() {
                let _ = h.join();
            }
        }
    }

    /// Build the working directory, then run the engine on its own thread.
    fn spawn_worker(&self, broadcast_id: &str) -> Result<()> {
        if self.workers.lock().unwrap().contains_key(broadcast_id) {
            return Ok(()); // already running; starting twice is not an error
        }

        let b = self.db.broadcast(broadcast_id)?;
        let prepared = self.db.prepared_media_for(&b.media_id)?;
        let dest = self.db.destination(&b.destination_id)?;

        let dir = self.work_dir(broadcast_id);
        std::fs::create_dir_all(&dir)?;

        // A core database of this broadcast's own, holding one playlist and one
        // media row. The engine then behaves exactly as it does on a desktop,
        // against the schema its own tests were written for.
        let core_db = Database::open(&dir.join("louver.db"))?;
        let local = self.storage.localize(&prepared.prepared_key)?;
        let playlist_id = project_into_core(&core_db, &local, &prepared)?;

        core_db.set_setting(louver_core::settings_keys::RTMPS_URL, &dest.rtmps_url)?;

        // The key store the engine reads. Scoped to this destination's account,
        // so one broadcast cannot read another's key.
        let key_store = Arc::new(StreamKeyStore::with_account(
            Arc::clone(&self.keys),
            crate::credentials::destination_account(&dest.id),
        ));

        let builder = FfmpegCommandBuilder::new(self.tools.clone(), OutputProfile::P1080p30)
            .with_encoder(self.encoder.clone());
        let events: Arc<dyn RuntimeEvents> =
            Arc::new(DbEvents {
                db: self.db.clone(),
                broadcast_id: broadcast_id.to_string(),
                sent: Mutex::new(Meter::default()),
            });
        let launcher = self.launchers.for_broadcast(&self.db, broadcast_id);

        let mut rt = BroadcastRuntime::new(
            core_db,
            builder,
            launcher,
            Arc::new(SystemClock),
            key_store,
            Arc::new(NoopSleepPreventer::default()),
            events,
            SessionStore::new(dir.join("session.json")),
            dir.join("manifest.txt"),
            dir.join("dry-run"),
        );

        // A hard kill of this process leaves its FFmpeg children publishing.
        // Starting a second sender to the same ingest URL is worse than not
        // recovering at all — YouTube sees two streams on one key — so the
        // previous run's process is killed first, by pid, after the core has
        // verified that the pid really is an FFmpeg.
        if let Some(pid) = rt.clean_orphan_process() {
            let _ = self.db.append_event(
                broadcast_id,
                EventLevel::Warn,
                &format!("이전 실행이 남긴 FFmpeg({pid})를 정리했습니다"),
            );
        }

        rt.start(StartOptions {
            playlist_id,
            reason: StartReason::Manual,
            dry_run: false,
            scheduled_end: None,
            occurrence: None,
            order_seed: None,
            skip_pre_start: true, // the cloud has no YouTube metadata hook yet
        })?;

        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let stop = Arc::clone(&stop);
            let db = self.db.clone();
            let id = broadcast_id.to_string();
            let workers = Arc::clone(&self.workers);
            std::thread::spawn(move || {
                run_until_stopped(rt, &db, &id, &stop);
                // The thread is finishing; drop its own entry so a later start
                // is not refused by a worker that no longer exists.
                workers.lock().unwrap().remove(&id);
            })
        };

        self.workers.lock().unwrap().insert(broadcast_id.to_string(), Worker { handle: Some(handle), stop });
        Ok(())
    }
}

/// The tick loop for one broadcast.
///
/// Deliberately thin: `tick()` already health-checks the child, applies the
/// backoff and restarts. This decides only when to give up and when to leave.
fn run_until_stopped(mut rt: BroadcastRuntime, db: &CloudDb, broadcast_id: &str, stop: &AtomicBool) {
    loop {
        if stop.load(Ordering::SeqCst) {
            let _ = rt.stop(true);
            let _ = db.record_runtime_only(broadcast_id, RuntimeState::Stopped);
            return;
        }

        // The user may have pressed stop through another process or request.
        match db.desired_state(broadcast_id) {
            Ok(DesiredState::Stopped) => {
                let _ = rt.stop(true);
                let _ = db.record_runtime_only(broadcast_id, RuntimeState::Stopped);
                return;
            }
            Err(_) => {
                // The row is gone — the broadcast was deleted under us.
                let _ = rt.stop(true);
                return;
            }
            Ok(DesiredState::Running) => {}
        }

        rt.tick();
        let _ = db.touch_heartbeat(broadcast_id);

        if rt.status().supervisor.restart_count as i64 > MAX_RESTARTS {
            let _ = rt.stop(true);
            let _ = db.record_failure(
                broadcast_id,
                &format!("{MAX_RESTARTS}회 연속 재시작에도 방송이 유지되지 않았습니다"),
            );
            let _ = db.give_up(broadcast_id);
            return;
        }

        std::thread::sleep(TICK);
    }
}

/// What a broadcast needs to know about its prepared file.
#[derive(Debug, Clone)]
pub struct PreparedMedia {
    pub media_id: String,
    pub filename: String,
    pub prepared_key: String,
    pub duration_secs: f64,
    pub width: i64,
    pub height: i64,
    pub fps: f64,
}

/// Write one playlist and one media row into a fresh core database.
///
/// The engine reads playlists and media from a `louver-core` `Database`; the
/// cloud's tables are its own. This is the whole of the translation, and it is
/// the price of not altering migrations that run on customers' machines.
fn project_into_core(db: &Database, local: &Path, m: &PreparedMedia) -> Result<i64> {
    let playlist_id = db.create_playlist(&m.filename, PlaybackMode::Sequential, OutputProfile::P1080p30)?;
    let path = local.to_string_lossy().into_owned();
    let media_id = db.upsert_media(&CoreMedia {
        id: 0,
        source_path: path.clone(),
        display_name: m.filename.clone(),
        // Already prepared by the upload pipeline, so the engine treats it as
        // broadcastable and never re-encodes at broadcast time.
        status: MediaStatus::Normalized,
        media_hash: m.media_id.clone(),
        normalized_path: Some(path),
        normalized_profile: Some(OutputProfile::P1080p30.id().to_string()),
        normalized_duration_secs: Some(m.duration_secs),
        duration_secs: m.duration_secs,
        width: u32::try_from(m.width).unwrap_or(0),
        height: u32::try_from(m.height).unwrap_or(0),
        fps: m.fps,
        video_codec: "h264".into(),
        audio_codec: Some("aac".into()),
        pixel_format: Some("yuv420p".into()),
        is_hdr: false,
        file_size: std::fs::metadata(local).map(|x| x.len()).unwrap_or(0),
        added_at: String::new(),
        last_error: None,
    })?;
    db.add_playlist_item(playlist_id, media_id)?;
    Ok(playlist_id)
}

/// Stream mode the cloud broadcasts in. Copy, always — the file was prepared.
pub const CLOUD_STREAM_MODE: StreamMode = StreamMode::StreamCopy;

impl BroadcastManager {
    /// Rows the dashboard shows, with the slot arithmetic §12 asks for.
    pub fn dashboard(&self, user_id: &str) -> Result<Dashboard> {
        let sub = self.db.subscription(user_id)?;
        let broadcasts = self.db.broadcasts_for(user_id)?;
        let allowed = sub.limits.get(crate::entitlement::MAX_CONCURRENT_STREAMS).copied().unwrap_or(0);
        let active = self.db.active_stream_count(user_id)?;
        Ok(Dashboard { plan_label: sub.plan_label, active, allowed, broadcasts })
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Dashboard {
    pub plan_label: String,
    pub active: i64,
    pub allowed: i64,
    pub broadcasts: Vec<Broadcast>,
}

#[cfg(test)]
mod meter_tests {
    use super::Meter;

    #[test]
    fn a_restart_banks_what_the_previous_process_sent() {
        let mut m = Meter::default();
        assert_eq!(m.observe(1_000), 1_000);
        assert_eq!(m.observe(5_000), 5_000);
        // FFmpeg died and its replacement starts counting from zero again.
        assert_eq!(m.observe(10), 5_010);
        assert_eq!(m.observe(2_000), 7_000);
        // A second restart banks the second process too.
        assert_eq!(m.observe(0), 7_000);
        assert_eq!(m.observe(500), 7_500);
    }
}
