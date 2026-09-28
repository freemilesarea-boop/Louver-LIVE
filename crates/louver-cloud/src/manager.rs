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
use crate::{CloudError, Result};
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
use std::path::PathBuf;
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
/// How often the clock is checked. A minute's granularity is what a schedule
/// form offers, so half of that is enough to never be late.
const SCHEDULE_TICK: std::time::Duration = std::time::Duration::from_secs(30);

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
                say(&id, "info", line);
                let _ = db.append_event(&id, EventLevel::Info, line);
            }),
        })
    }
}

/// Put a broadcast's line where an operator will actually find it.
///
/// The event table is the right home for these — a user reads them in the
/// dashboard — but `docker compose logs` is where someone looks when a stream is
/// not appearing, and until now it held nothing but the boot lines. Every line
/// here has already been through the core's masking, so a stream key cannot
/// reach it.
pub(crate) fn say(broadcast_id: &str, level: &str, line: &str) {
    let short: String = broadcast_id.chars().take(8).collect();
    println!("[louver][{short}][{level}] {line}");
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
    /// Where this run rotated the playlist to, so a position can be reported in
    /// the order the user arranged rather than the order FFmpeg was given.
    offset: i64,
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
            status.supervisor.pid.map(i64::from),
        );

        // What the dashboard shows as NOW PLAYING / NEXT. The engine already
        // tracks it — this only turns its rotated index back into the user's.
        let count = status.item_count.max(1) as i64;
        let engine_index = status.current_index.unwrap_or(0) as i64;
        let _ = self.db.record_playlist_progress(
            &self.broadcast_id,
            &crate::models::PlaylistProgress {
                index: (engine_index + self.offset).rem_euclid(count) + 1,
                current_item: status.current_item.clone(),
                next_item: status.next_item.clone(),
                position_secs: position_in_item(status),
                duration_secs: status.cycle_duration_secs / count as f64,
                cycle_secs: status.cycle_duration_secs,
                play_count: count,
            },
        );
    }

    fn on_log(&self, level: EventLevel, message: &str) {
        // `message` has already been through the core's masking on its way
        // here; nothing in this module adds a secret to it.
        say(&self.broadcast_id, &format!("{level:?}").to_lowercase(), message);
        let _ = self.db.append_event(&self.broadcast_id, level, message);
    }
}

/// Why a broadcast is being stopped.
///
/// Two questions have one answer each per reason, and getting either wrong is a
/// production fault rather than a tidiness problem:
///
/// * **Does the user still want this running?** A restart or a server shutdown
///   must leave `desired_state = running`, or recovery will not bring the
///   broadcast back. Everything else clears it, so no watchdog resumes it.
/// * **Is YouTube's broadcast over?** A completed YouTube broadcast can never go
///   live again, so completing one on a restart bricks that broadcast for good.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The user pressed STOP.
    UserFinalStop,
    /// The subscription was cancelled. Every one of that account's running
    /// broadcasts ends, because the entitlement they were running on is gone.
    SubscriptionCancelled,
    /// A scheduled window reached its end time.
    ScheduledWindowEnd,
    /// "반복 재생" is off and the playlist has played once.
    PlaylistFinished,
    /// The broadcast is being deleted.
    Delete,
    /// The same broadcast is about to be started again.
    Restart,
    /// This process is going down. The broadcast is not.
    Shutdown,
    /// The engine failed too many times in a row and stopped trying.
    GaveUp,
}

impl StopReason {
    /// Does this end the YouTube broadcast?
    pub fn ends_the_youtube_broadcast(self) -> bool {
        !matches!(self, Self::Restart | Self::Shutdown)
    }

    /// Does the user's stored intent become "stopped"?
    pub fn clears_the_intent(self) -> bool {
        !matches!(self, Self::Restart | Self::Shutdown)
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
    /// Set only when this server has YouTube connecting configured. `None`
    /// means every broadcast is a pasted stream key, exactly as before.
    youtube: Option<Arc<crate::youtube::Youtube>>,
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
            youtube: None,
        }
    }

    /// Give this manager a YouTube provider.
    ///
    /// Optional on purpose: without one, every broadcast is a pasted stream key
    /// and not one line of the sending path behaves differently. With one, only
    /// the broadcasts that have an account attached take the extra steps.
    pub fn with_youtube(mut self, yt: crate::youtube::Youtube) -> Self {
        self.youtube = Some(Arc::new(yt));
        self
    }

    pub fn youtube(&self) -> Option<Arc<crate::youtube::Youtube>> {
        self.youtube.clone()
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
        // The subscription first, before anything is spent. `claim_stream_slot`
        // asks again inside its transaction — that is the check that counts —
        // but asking here as well keeps an account with no plan from costing us
        // a YouTube token refresh and an API call on its way to being refused.
        self.db.require_active_subscription(user_id)?;
        // §8: a connected account's broadcast is checked with YouTube before
        // anything is claimed or spawned — a token that will not refresh or a
        // broadcast YouTube has already completed is a reason not to start, not
        // something to discover once FFmpeg is running.
        if let Some(yt) = &self.youtube {
            yt.before_start(broadcast_id)?;
        }
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

    /// Stop a broadcast the caller owns, for good. The STOP button.
    pub fn stop(&self, user_id: &str, broadcast_id: &str) -> Result<()> {
        self.stop_with(user_id, broadcast_id, StopReason::UserFinalStop)
    }

    /// Stop a broadcast, saying *why* — because the why decides two things that
    /// used to be decided by neither.
    ///
    /// `desired_state` goes to `stopped` before the thread is asked to finish,
    /// so a tick racing this sees the intent and does not restart. For a restart
    /// the intent is deliberately left alone: the broadcast is meant to be
    /// running, it is only this process that is changing.
    pub fn stop_with(&self, user_id: &str, broadcast_id: &str, reason: StopReason) -> Result<()> {
        // The ownership check that `release_stream_slot` used to be. A reason
        // that keeps the intent does not write through a `WHERE user_id`, so
        // without this a restart would not check who was asking.
        self.db.broadcast_owned(user_id, broadcast_id)?;

        if reason.clears_the_intent() {
            self.db.release_stream_slot(user_id, broadcast_id)?;
        }
        self.halt_worker(broadcast_id);
        self.db.record_runtime_only(broadcast_id, RuntimeState::Stopped)?;
        // §9: end YouTube's side too, after the sender is down. Tolerant by
        // design — `enableAutoStop` may already have completed it — and never
        // able to leave 247streams thinking the broadcast is still running.
        //
        // Only for a reason that really is the end of the broadcast. A restart
        // that completed it would make the next start impossible: YouTube never
        // reopens a completed broadcast.
        if reason.ends_the_youtube_broadcast() {
            if let Some(yt) = &self.youtube {
                yt.after_stop(broadcast_id);
            }
        }
        Ok(())
    }

    /// Stop and start again, with nothing in between that ends the broadcast.
    ///
    /// The slot is kept across the two halves rather than released and
    /// re-claimed, so a restart cannot lose its place to another broadcast
    /// starting in the same instant.
    pub fn restart(&self, user_id: &str, broadcast_id: &str) -> Result<()> {
        self.stop_with(user_id, broadcast_id, StopReason::Restart)?;
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
            // Recovery spawns workers directly rather than going through
            // `start`, so the subscription gate has to be repeated here. Without
            // it, a restart would revive the broadcasts of an account whose
            // subscription has since ended — the one thing §10 forbids, and the
            // easiest to miss, because nobody is watching a boot.
            if let Err(e) = self.db.require_active_subscription(&b.user_id) {
                let _ = self.db.append_event(
                    &b.id,
                    EventLevel::Warn,
                    "활성화된 요금제가 없어 방송을 복구하지 않았습니다",
                );
                // `desired_state` is left alone. When they subscribe again this
                // broadcast is meant to come back, and rewriting it to stopped
                // would silently discard that intent.
                let _ = self.db.record_failure(&b.id, &format!("복구하지 않음: {e}"));
                continue;
            }
            // §14: a YouTube-connected broadcast has to be checked before its
            // worker is spawned — the access token has almost certainly expired
            // while the server was down, and the broadcast itself may have been
            // ended on YouTube's side in the meantime.
            if let (Some(yt), Some(_)) = (&self.youtube, &b.youtube.broadcast_id) {
                if let Err(e) = yt.before_start(&b.id) {
                    let _ = self.db.record_failure(&b.id, &format!("복구 실패: {e}"));
                    // Ended on YouTube is final. Left wanting to run, this
                    // broadcast would be restarted by every boot for ever,
                    // which is the one thing §14 forbids.
                    if self
                        .db
                        .broadcast(&b.id)
                        .map(|x| x.youtube.status.as_deref() == Some(crate::youtube::COMPLETE))
                        .unwrap_or(false)
                    {
                        let _ = self.db.give_up(&b.id);
                    }
                    continue;
                }
            }
            match self.spawn_worker(&b.id) {
                Ok(()) => started += 1,
                Err(e) => {
                    let _ = self.db.record_failure(&b.id, &format!("복구 실패: {e}"));
                }
            }
        }
        Ok(started)
    }

    /// Start the thread that watches the clock. §8.
    ///
    /// It holds nothing: every tick re-reads the schedules from the database and
    /// asks [`crate::schedule::decide`] what to do. That is what makes a restart
    /// recover schedules without recovery code — and what makes the behaviour
    /// testable without a thread at all.
    pub fn spawn_scheduler(&self) -> std::thread::JoinHandle<()> {
        let mgr = self.clone();
        std::thread::spawn(move || loop {
            mgr.run_schedules(chrono::Utc::now());
            std::thread::sleep(SCHEDULE_TICK);
        })
    }

    /// One pass of the scheduler. Public so a test can drive it with its own
    /// clock rather than waiting for one.
    pub fn run_schedules(&self, now: chrono::DateTime<chrono::Utc>) {
        let Ok(scheduled) = self.db.scheduled_broadcasts() else { return };
        for b in scheduled {
            match crate::schedule::decide(&b.schedule, b.desired_state, now) {
                Some(crate::schedule::ScheduleAction::Start { occurrence }) => {
                    // Recorded before the attempt, not after: a start that fails
                    // on the plan limit must not be retried every thirty seconds
                    // for the rest of the window.
                    let _ = self.db.mark_scheduled_run(&b.id, &occurrence.to_rfc3339());
                    match self.start(&b.user_id, &b.id) {
                        Ok(()) => {
                            let _ = self.db.append_event(
                                &b.id,
                                EventLevel::Info,
                                "예약된 시각이 되어 방송을 시작했습니다",
                            );
                        }
                        Err(CloudError::NoSubscription) => {
                            // Distinguished from every other failure because it
                            // is the one the user can fix, and the one that will
                            // otherwise repeat at every occurrence.
                            let _ = self.db.append_event(
                                &b.id,
                                EventLevel::Warn,
                                "활성화된 요금제가 없어 예약 방송을 시작하지 못했습니다",
                            );
                        }
                        Err(e) => {
                            let _ = self.db.append_event(
                                &b.id,
                                EventLevel::Error,
                                &format!("예약 시작 실패: {e}"),
                            );
                        }
                    }
                }
                Some(crate::schedule::ScheduleAction::Stop) => {
                    // The window is over, so this occurrence's YouTube broadcast
                    // is over with it. The next occurrence gets a new one —
                    // see `Youtube::renew`.
                    let _ = self.stop_with(&b.user_id, &b.id, StopReason::ScheduledWindowEnd);
                    let _ = self.db.append_event(
                        &b.id,
                        EventLevel::Info,
                        "예약된 종료 시각이 되어 방송을 중지했습니다",
                    );
                }
                None => {}
            }
        }
    }

    /// Throw away a deleted broadcast's working directory.
    ///
    /// Each broadcast owns a directory holding a core database, a concat
    /// manifest and a session file. Deleting the row used to leave all three
    /// behind for ever — small individually, unbounded over a year of a class
    /// making and deleting broadcasts, and on the same 80 GB disk as everything
    /// else. Best effort: a directory that will not go is not a reason to fail
    /// a delete the user has already been told succeeded.
    pub fn forget(&self, broadcast_id: &str) {
        let dir = self.work_dir(broadcast_id);
        if dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                eprintln!("[louver] 작업 디렉터리를 지우지 못했습니다: {e}");
            }
        }
    }

    /// Stop every broadcast this account has running, and say which they were.
    ///
    /// The cancellation path's second half. Called only after the provider has
    /// agreed and the entitlement has actually been taken away — a cancellation
    /// that revoked nothing (an account an operator moved to another plan by
    /// hand) must not take that account off air.
    ///
    /// Scoped to one user id in the query itself, so there is no arrangement of
    /// arguments that reaches somebody else's broadcast. Best effort per
    /// broadcast: one that cannot be stopped is logged and the rest still stop,
    /// and its own worker sees `desired_state = stopped` on its next tick and
    /// leaves anyway.
    pub fn stop_all_for(&self, user_id: &str, reason: StopReason) -> Vec<String> {
        let mut stopped = Vec::new();
        let Ok(mine) = self.db.broadcasts_for(user_id) else { return stopped };
        let live = self.running_ids();
        for b in mine {
            let running = b.desired_state == DesiredState::Running || live.contains(&b.id);
            if !running {
                continue;
            }
            match self.stop_with(user_id, &b.id, reason) {
                Ok(()) => {
                    let _ =
                        self.db.append_event(&b.id, EventLevel::Warn, "구독이 해지되어 방송을 종료했습니다");
                    stopped.push(b.id);
                }
                Err(e) => {
                    say(&b.id, "warn", &format!("해지 후 방송을 종료하지 못했습니다: {e}"));
                    let _ = self.db.record_failure(&b.id, &format!("해지 후 종료 실패: {e}"));
                }
            }
        }
        stopped
    }

    /// Ask every worker to finish, and wait. For a clean shutdown.
    ///
    /// [`StopReason::Shutdown`] in everything but name: `desired_state` is left
    /// alone so recovery restarts these, and YouTube is not told anything —
    /// the broadcast is not ending, this process is.
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
        // Every item has to be joinable with every other before a single packet
        // is sent: the concat demuxer copies packets, it does not reconcile
        // them. Checked here rather than in `start` so a boot recovery is held
        // to the same rule.
        self.db.check_playlist_joinable(broadcast_id)?;
        let playlist = self.db.prepared_items_for(broadcast_id)?;
        let dest = self.db.destination(&b.destination_id)?;

        let dir = self.work_dir(broadcast_id);
        std::fs::create_dir_all(&dir)?;

        // A core database of this broadcast's own, holding one playlist and its
        // media rows. The engine then behaves exactly as it does on a desktop,
        // against the schema its own tests were written for.
        let core_db = Database::open(&dir.join("louver.db"))?;
        let mut localized = Vec::with_capacity(playlist.len());
        for m in &playlist {
            let local = self.storage.localize(&m.prepared_key)?;
            localized.push((m.clone(), local));
        }

        // Resume where it was, when there is a where. `current_index` is 1-based
        // and already mapped into the user's order, so the rotation this run
        // needs is that index minus one.
        let resume_at = self
            .db
            .playlist_resume_point(broadcast_id)
            .map(|(index, _)| (index.max(1) - 1) as usize)
            .unwrap_or(0);
        let rotated = rotate(&localized, resume_at.min(localized.len().saturating_sub(1)));
        let playlist_id = project_into_core(&core_db, &rotated, &b.name)?;
        let _ = self.db.record_playlist_offset(broadcast_id, resume_at as i64, rotated.len() as i64);

        core_db.set_setting(louver_core::settings_keys::RTMPS_URL, &dest.rtmps_url)?;

        // Say where this is about to send, in a form that can be pasted into a
        // bug report. A stream not appearing on a channel is almost always one
        // of: the wrong host, a key that is not the one that used to work, or a
        // destination that is not RTMP at all — and none of those could be told
        // apart from the outside before this line existed.
        describe_destination(broadcast_id, &dest, &self.keys, rotated.len());

        // The key store the engine reads. Scoped to this destination's account,
        // so one broadcast cannot read another's key.
        let key_store = Arc::new(StreamKeyStore::with_account(
            Arc::clone(&self.keys),
            crate::credentials::destination_account(&dest.id),
        ));

        let builder = FfmpegCommandBuilder::new(self.tools.clone(), OutputProfile::P1080p30)
            .with_encoder(self.encoder.clone());
        let events: Arc<dyn RuntimeEvents> = Arc::new(DbEvents {
            db: self.db.clone(),
            broadcast_id: broadcast_id.to_string(),
            sent: Mutex::new(Meter::default()),
            offset: resume_at as i64,
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
            let loop_forever = b.loop_forever;
            let user_id = b.user_id.clone();
            let youtube = self.youtube.clone();
            std::thread::spawn(move || {
                run_until_stopped(rt, &db, &id, &stop, loop_forever, user_id, youtube);
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
fn run_until_stopped(
    mut rt: BroadcastRuntime,
    db: &CloudDb,
    broadcast_id: &str,
    stop: &AtomicBool,
    loop_forever: bool,
    user_id: String,
    youtube: Option<Arc<crate::youtube::Youtube>>,
) {
    let mut stderr_cursor = StderrCursor::default();
    let mut watcher = YoutubeWatch::default();
    // Only read when "전체 반복" is off: where the pass being played began, and
    // how many relaunches had happened by then.
    let mut pass_began_at: i64 = 0;
    let mut seen_restarts: i64 = 0;
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

        // What FFmpeg is complaining about, as it complains. At `-loglevel
        // warning` this is exactly the set of things that break a live stream —
        // non-monotonous DTS, av_interleaved_write_frame, a broken pipe, a TLS
        // failure — and before this it was only ever read after the process had
        // already died, which is too late to explain a stream that is running
        // and invisible.
        for line in stderr_cursor.fresh(&rt.ffmpeg_stderr_tail()) {
            say(broadcast_id, "ffmpeg", &line);
            let _ = db.append_event(broadcast_id, EventLevel::Warn, &format!("ffmpeg: {line}"));
        }

        // §2's "repeat off": FFmpeg is always started with `-stream_loop -1`,
        // because a sender that exits is a sender the supervisor would restart.
        // One pass is therefore ended here, by the server, once the engine says
        // it has played as long as the playlist is — and it is ended as a
        // deliberate stop, so nothing brings it back.
        if !loop_forever {
            let status = rt.status();
            // A relaunched FFmpeg reads the manifest from the top, so the pass
            // it is playing began then — not when the broadcast did. Measuring
            // from the start would cut a reconnected single pass short by
            // exactly the time the reconnect took, which is the one thing "한
            // 바퀴만" promises not to do.
            if status.supervisor.restart_count as i64 != seen_restarts {
                seen_restarts = status.supervisor.restart_count as i64;
                pass_began_at = status.elapsed_secs;
                let _ = db.append_event(
                    broadcast_id,
                    EventLevel::Info,
                    "재연결되어 플레이리스트를 처음부터 다시 재생합니다",
                );
            }
            if pass_complete(status.cycle_duration_secs, status.elapsed_secs, pass_began_at) {
                let _ = rt.stop(true);
                let _ = db.append_event(
                    broadcast_id,
                    EventLevel::Info,
                    "플레이리스트를 한 번 재생하고 종료했습니다",
                );
                let _ = db.release_stream_slot(&user_id, broadcast_id);
                let _ = db.record_runtime_only(broadcast_id, RuntimeState::Stopped);
                // A real ending, so YouTube is told. Without this the dashboard
                // says stopped while the channel still shows a live broadcast
                // with nothing arriving on it. `StopReason::PlaylistFinished`.
                if let Some(yt) = &youtube {
                    yt.after_stop(broadcast_id);
                }
                return;
            }
        }

        // §8: YouTube's own view of the stream, asked for on a schedule and a
        // bounded number of times. FFmpeg being alive says nothing about whether
        // YouTube has accepted the ingest, so the two are tracked separately and
        // this is the only thing that writes the YouTube one.
        if let Some(yt) = &youtube {
            watcher.maybe_poll(yt, db, broadcast_id);
        }

        if rt.status().supervisor.restart_count as i64 > MAX_RESTARTS {
            let _ = rt.stop(true);
            let _ = db.record_failure(
                broadcast_id,
                &format!("{MAX_RESTARTS}회 연속 재시작에도 방송이 유지되지 않았습니다"),
            );
            let _ = db.give_up(broadcast_id);
            // Nothing is going to bring this back by itself, so it is an ending
            // like any other: `StopReason::GaveUp`. Leaving YouTube live would
            // leave a channel showing a broadcast that no longer has a sender.
            if let Some(yt) = &youtube {
                yt.after_stop(broadcast_id);
            }
            return;
        }

        std::thread::sleep(TICK);
    }
}

/// Has this single pass played the whole playlist?
///
/// Separated out because it is the whole of "한 바퀴만" and it is the kind of
/// arithmetic that is wrong in a way nobody notices for a month.
fn pass_complete(cycle_secs: f64, elapsed_secs: i64, pass_began_at: i64) -> bool {
    // A cycle of zero means the engine has not worked out how long the playlist
    // is yet. Stopping then would end the broadcast the instant it started.
    cycle_secs > 0.5 && (elapsed_secs - pass_began_at) as f64 >= cycle_secs
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

/// Write one playlist and its videos into a fresh core database.
///
/// The engine reads playlists and media from a `louver-core` `Database`; the
/// cloud's tables are its own. This is the whole of the translation, and it is
/// the price of not altering migrations that run on customers' machines.
///
/// One playlist with N items is what makes continuous streaming work: the engine
/// writes a concat manifest of all of them and runs **one** FFmpeg with
/// `-stream_loop -1 -c copy`, so moving from one video to the next is a file
/// boundary inside a single RTMP connection. YouTube sees one uninterrupted
/// stream; it never sees a disconnect between videos.
fn project_into_core(db: &Database, items: &[(PreparedMedia, PathBuf)], name: &str) -> Result<i64> {
    let playlist_id = db.create_playlist(name, PlaybackMode::Sequential, OutputProfile::P1080p30)?;
    // The same video can appear more than once (a repeat), and one core media
    // row should serve every appearance.
    let mut seen: HashMap<String, i64> = HashMap::new();
    for (m, local) in items {
        let path = local.to_string_lossy().into_owned();
        let media_id = match seen.get(&path) {
            Some(id) => *id,
            None => {
                let id = db.upsert_media(&CoreMedia {
                    id: 0,
                    source_path: path.clone(),
                    display_name: m.filename.clone(),
                    // Already prepared by the upload pipeline, so the engine
                    // treats it as broadcastable and never re-encodes at
                    // broadcast time.
                    status: MediaStatus::Normalized,
                    media_hash: m.media_id.clone(),
                    normalized_path: Some(path.clone()),
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
                seen.insert(path, id);
                id
            }
        };
        db.add_playlist_item(playlist_id, media_id)?;
    }
    Ok(playlist_id)
}

/// Rotate a playlist so that `start_at` plays first.
///
/// This is §9's resume, done without touching the engine: a worker that comes
/// back after a crash is handed the same videos in the same order, beginning at
/// the one that was playing. The offset is stored so the dashboard can still
/// show the position in the order the user arranged.
fn rotate<T: Clone>(items: &[T], start_at: usize) -> Vec<T> {
    if items.is_empty() || start_at == 0 || start_at >= items.len() {
        return items.to_vec();
    }
    items[start_at..].iter().chain(items[..start_at].iter()).cloned().collect()
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
        Ok(Dashboard {
            plan_label: sub.plan_label.clone(),
            active,
            allowed,
            broadcasts,
            // So the dashboard can explain itself without a second request. It
            // is the same value `/api/me/subscription` reports; the screen that
            // needs it is the one this payload already feeds.
            subscribed: sub.active,
            plan_id: sub.plan_id,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Dashboard {
    pub plan_label: String,
    pub active: i64,
    pub allowed: i64,
    pub broadcasts: Vec<Broadcast>,
    /// Is there an active subscription behind this? Additive, so a client
    /// written before plans existed keeps reading the fields it knows.
    pub subscribed: bool,
    pub plan_id: String,
}

/// How far into the current video the engine is.
///
/// The engine reports elapsed time for the session and the length of one pass
/// through the playlist; the position inside the current item is the remainder
/// once whole items are taken out. Derived rather than stored because FFmpeg
/// reports one clock for the whole concat input, not one per file.
fn position_in_item(status: &RuntimeStatus) -> f64 {
    let count = status.item_count.max(1) as f64;
    let per_item = status.cycle_duration_secs / count;
    if per_item <= 0.0 {
        return status.elapsed_secs as f64;
    }
    (status.elapsed_secs as f64) % per_item
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

/// Asks YouTube how the ingest is going, on a schedule and not for ever.
///
/// Every poll costs API quota, and a broadcast that has gone live has nothing
/// left to report, so this stops as soon as it has an answer and gives up after
/// a few minutes of not getting one. §8 forbids an unbounded loop and this is
/// where that rule lives.
#[derive(Debug)]
struct YoutubeWatch {
    next: std::time::Instant,
    polls: u32,
    settled: bool,
}

impl Default for YoutubeWatch {
    fn default() -> Self {
        Self { next: std::time::Instant::now(), polls: 0, settled: false }
    }
}

impl YoutubeWatch {
    /// Every ten seconds, at most thirty times: five minutes for YouTube to
    /// notice an ingest it usually notices in twenty seconds.
    const EVERY: std::time::Duration = std::time::Duration::from_secs(10);
    const LIMIT: u32 = 30;

    fn maybe_poll(&mut self, yt: &Arc<crate::youtube::Youtube>, db: &CloudDb, broadcast_id: &str) {
        if self.settled || self.polls >= Self::LIMIT || std::time::Instant::now() < self.next {
            return;
        }
        self.next = std::time::Instant::now() + Self::EVERY;
        self.polls += 1;
        match yt.poll_status(broadcast_id) {
            // Not a YouTube broadcast at all: never ask again.
            Ok(None) => self.settled = true,
            Ok(Some(status)) => {
                if status == crate::youtube::LIVE || status == crate::youtube::COMPLETE {
                    self.settled = true;
                }
            }
            Err(e) => {
                let _ =
                    db.append_event(broadcast_id, EventLevel::Warn, &format!("YouTube 상태 확인 실패: {e}"));
                if self.polls >= Self::LIMIT {
                    self.settled = true;
                }
            }
        }
    }
}

/// Reads a rotating tail without repeating itself and without losing a line.
///
/// The core keeps the last twenty stderr lines and drops the oldest, so a count
/// is not a usable cursor: once it is full the length stops changing while the
/// content keeps moving. Nor is "the last line I saw", because FFmpeg repeats the
/// same warning and each repeat is news. So this keeps the previous window and
/// finds how far it slid: the smallest shift whose remainder still matches the
/// front of the new window. When nothing matches, the window moved further than
/// it is long and everything in it is forwarded.
#[derive(Default, Debug)]
struct StderrCursor {
    previous: Vec<String>,
}

impl StderrCursor {
    fn fresh(&mut self, tail: &[String]) -> Vec<String> {
        if tail.is_empty() {
            return Vec::new();
        }
        let mut new_from = 0;
        for shift in 0..=self.previous.len() {
            let overlap = self.previous.len() - shift;
            if overlap > tail.len() {
                continue;
            }
            if self.previous[shift..] == tail[..overlap] {
                new_from = overlap;
                break;
            }
        }
        self.previous = tail.to_vec();
        tail[new_from..].to_vec()
    }
}

/// Log where a broadcast is about to send, without logging the key.
///
/// The fingerprint is a truncated SHA-256 of the key. It is enough to answer
/// "is this the same key that worked yesterday?" by comparing two log lines, and
/// it reveals nothing: the key itself never leaves the sealed store.
fn describe_destination(
    broadcast_id: &str,
    dest: &crate::models::StreamDestination,
    keys: &Arc<dyn louver_core::security::SecretStore>,
    items: usize,
) {
    let url = dest.rtmps_url.trim_end_matches('/');
    let scheme = url.split("://").next().unwrap_or("").to_string();
    let rest = url.split("://").nth(1).unwrap_or("");
    let host = rest.split('/').next().unwrap_or("").to_string();
    let path = rest.strip_prefix(&host).unwrap_or("").to_string();

    // No key at all is why FFmpeg would never have started, so the fingerprint
    // says so rather than leaving an empty URL to be guessed at.
    let key = crate::credentials::fingerprint(keys, &crate::credentials::destination_account(&dest.id));

    say(
        broadcast_id,
        "info",
        &format!("송출 대상: scheme={scheme} host={host} path={path} key={key} 영상={items}개"),
    );
    if !matches!(scheme.as_str(), "rtmp" | "rtmps") {
        say(
            broadcast_id,
            "error",
            &format!("송출 대상이 RTMP(S)가 아닙니다 (scheme={scheme}). YouTube에는 도달하지 않습니다."),
        );
    }
    if !key.present() {
        say(broadcast_id, "error", "스트림 키를 읽을 수 없습니다. 대상을 다시 저장해 주세요.");
    }
    // The limit of what a pasted key can do, said at the moment it matters.
    // 247streams sends video to the ingest; it does not create the live
    // broadcast, set its title or press "go live", because none of that is
    // possible without the account being connected (§5).
    if !dest.kind.can_publish_metadata() {
        say(
            broadcast_id,
            "info",
            "RTMPS 전송을 시작합니다. 스트림 키 방식이므로 247streams는 YouTube에 \
             라이브를 만들거나 공개로 전환하지 않습니다 — YouTube Studio에서 수신을 \
             확인하고 '실시간 시작'을 눌러야 채널에 나타납니다.",
        );
    }
}

#[cfg(test)]
mod stderr_cursor_tests {
    use super::StderrCursor;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn it_forwards_each_line_once() {
        let mut c = StderrCursor::default();
        assert_eq!(c.fresh(&lines(&[])), Vec::<String>::new());
        assert_eq!(c.fresh(&lines(&["a", "b"])), lines(&["a", "b"]));
        assert_eq!(c.fresh(&lines(&["a", "b"])), Vec::<String>::new(), "nothing new");
        assert_eq!(c.fresh(&lines(&["a", "b", "c"])), lines(&["c"]));
    }

    #[test]
    fn a_rotated_tail_does_not_silently_repeat_or_stall() {
        let mut c = StderrCursor::default();
        c.fresh(&lines(&["1", "2", "3"]));
        // The window slid: "1" is gone and two new lines arrived. A length-based
        // cursor would have reported nothing at all here.
        assert_eq!(c.fresh(&lines(&["2", "3", "4", "5"])), lines(&["4", "5"]));
        // And when the last seen line is gone entirely, the whole window is
        // forwarded rather than skipped.
        assert_eq!(c.fresh(&lines(&["8", "9"])), lines(&["8", "9"]));
    }

    #[test]
    fn a_repeated_warning_is_not_mistaken_for_an_old_one() {
        let mut c = StderrCursor::default();
        // FFmpeg repeats the same DTS warning; each occurrence is news.
        assert_eq!(c.fresh(&lines(&["dts"])), lines(&["dts"]));
        assert_eq!(c.fresh(&lines(&["dts", "dts"])), lines(&["dts"]));
    }
}

#[cfg(test)]
mod pass_tests {
    use super::pass_complete;

    #[test]
    fn a_pass_ends_only_once_the_whole_playlist_has_played() {
        assert!(!pass_complete(60.0, 0, 0));
        assert!(!pass_complete(60.0, 59, 0));
        assert!(pass_complete(60.0, 60, 0));
    }

    #[test]
    fn a_relaunch_restarts_the_pass_rather_than_shortening_it() {
        // FFmpeg died 50 seconds into a 60-second playlist and was relaunched,
        // so the pass being played began at second 50. Ending at second 60 would
        // give the user 50 seconds of one pass and 10 of another.
        assert!(!pass_complete(60.0, 60, 50));
        assert!(!pass_complete(60.0, 109, 50));
        assert!(pass_complete(60.0, 110, 50));
    }

    #[test]
    fn an_unknown_playlist_length_never_ends_the_broadcast() {
        assert!(!pass_complete(0.0, 10_000, 0));
    }
}
