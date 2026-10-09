//! One job per broadcast, owned by one user, bounded in number.
//!
//! ## Idempotency
//!
//! The key is the caller's `broadcast_id`. Posting the same one twice returns
//! the job that already exists instead of starting a second FFmpeg against the
//! same destination — which is the failure that matters here, because two
//! senders on one ingest URL is worse for a viewer than no sender at all. A
//! double-clicked button, a retried request and a client that lost the response
//! all land on the same job.
//!
//! ## Ownership
//!
//! Every lookup takes the caller's user id and a job is invisible without it.
//! A job belonging to somebody else reads as **not found**, not as forbidden:
//! the difference tells an attacker whether an id exists, which is the whole
//! value of probing.
//!
//! ## Recovery
//!
//! State lives in one JSON file per job ([`crate::state`]), written by the
//! worker thread as it goes. On start, [`Registry::recover`] reads the
//! directory and restarts the jobs whose `desired` is `Running` — the
//! instruction, not the last observed phase. Sequentially, because starting six
//! x264 encodes in the same instant is how a recovery turns into an outage.

use crate::args;
use crate::error::{LiveSourceError, Result};
use crate::limits::Limits;
use crate::media::MediaRoot;
use crate::resolver::LiveSourceResolver;
use crate::state::{Desired, Phase, StateStore, WorkerState};
use crate::worker::{LiveWorker, WorkerConfig};
use louver_core::streaming::ffmpeg::FfmpegTools;
use louver_core::OutputProfile;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What a client may ask for. Note what is **not** here: no user id (the token
/// says who), no file path (names only), no destination URL (a name only).
#[derive(Debug, Clone, Deserialize)]
pub struct NewJob {
    /// The caller's own id for this broadcast. The idempotency key.
    pub broadcast_id: String,
    /// A YouTube Live watch URL, or a direct public stream URL.
    pub source_url: String,
    /// Media file names, in order. Resolved inside the media root.
    pub playlist: Vec<String>,
    /// The name of a destination the operator configured.
    pub destination: String,
}

/// What a client gets back. The destination and the resolved manifest URL are
/// absent by construction.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct JobView {
    pub broadcast_id: String,
    pub desired: Desired,
    pub phase: Phase,
    pub frames: u64,
    pub restarts: u32,
    pub last_verdict: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    pub updated_at: String,
}

impl JobView {
    fn of(s: &WorkerState) -> Self {
        Self {
            broadcast_id: s.worker_id.clone(),
            desired: s.desired,
            phase: s.phase,
            frames: s.frames,
            restarts: s.restarts,
            last_verdict: s.last_verdict.clone(),
            video_id: s.video_id.clone(),
            last_error: s.last_error.clone(),
            output: s.output.clone(),
            updated_at: s.updated_at.clone(),
        }
    }
}

/// How this worker is configured. Operator-supplied, never request-supplied.
#[derive(Clone)]
pub struct Settings {
    pub state_dir: PathBuf,
    pub media: MediaRoot,
    pub tools: FfmpegTools,
    pub profile: OutputProfile,
    pub limits: Limits,
    pub max_restarts: u32,
    pub stall_after: Duration,
    pub grace: Duration,
}

/// One running job, from the registry's point of view.
struct Running {
    owner: String,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

pub struct Registry {
    settings: Settings,
    resolver: Arc<dyn LiveSourceResolver>,
    /// broadcast_id → running job. The lock is held only to read or change the
    /// map, never across a spawn or an FFmpeg call.
    running: Mutex<HashMap<String, Running>>,
}

impl Registry {
    pub fn new(settings: Settings, resolver: Arc<dyn LiveSourceResolver>) -> Self {
        Self { settings, resolver, running: Mutex::new(HashMap::new()) }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The resolver this registry was built with, for the `check` endpoint.
    pub fn resolver(&self) -> &Arc<dyn LiveSourceResolver> {
        &self.resolver
    }

    fn store(&self, broadcast_id: &str) -> StateStore {
        StateStore::new(&self.settings.state_dir, broadcast_id)
    }

    /// A poisoned lock must not take this API down for good: the map holds no
    /// invariant a panic could break.
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Running>> {
        self.running.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The job this caller owns, or nothing.
    ///
    /// Returns `NotFound` for somebody else's job as well as for a missing one.
    pub fn get(&self, owner: &str, broadcast_id: &str) -> Result<JobView> {
        let s = self
            .store(broadcast_id)
            .load()
            .map_err(|_| LiveSourceError::not_found("해당 작업을 찾을 수 없습니다."))?;
        if s.owner != owner {
            return Err(LiveSourceError::not_found("해당 작업을 찾을 수 없습니다."));
        }
        Ok(JobView::of(&s))
    }

    /// Everything this caller owns.
    pub fn list(&self, owner: &str) -> Vec<JobView> {
        crate::state::scan(&self.settings.state_dir)
            .iter()
            .filter(|s| s.owner == owner)
            .map(JobView::of)
            .collect()
    }

    /// How many jobs are currently running, across all users.
    ///
    /// The ceiling is about the machine, so it counts everyone's.
    pub fn running_count(&self) -> usize {
        self.map().len()
    }

    /// Create, or hand back what already exists.
    pub fn create(&self, owner: &str, plan: &str, req: &NewJob) -> Result<JobView> {
        let id = req.broadcast_id.trim();
        if id.is_empty() || id.chars().count() > 64 {
            return Err(LiveSourceError::invalid("broadcast_id 가 올바르지 않습니다."));
        }
        if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(LiveSourceError::invalid("broadcast_id 는 영숫자·하이픈·밑줄만 쓸 수 있습니다."));
        }

        // --- idempotency, and the ownership check that goes with it ----------
        // Done before the limit check: a repeat of an existing job must not be
        // refused for being over the ceiling it is already inside.
        if let Ok(existing) = self.store(id).load() {
            if existing.owner != owner {
                // Somebody else's id. Reported as a conflict with no detail,
                // because saying "that is someone else's" confirms it exists.
                return Err(LiveSourceError::conflict("이 broadcast_id 는 사용할 수 없습니다."));
            }
            if existing.desired == Desired::Running {
                return Ok(JobView::of(&existing));
            }
            // A stopped job of theirs may be started again under the same id.
        }

        // --- the ceiling -----------------------------------------------------
        self.settings.limits.admit(self.running_count())?;
        let _ = plan; // per-plan ceilings are a later refinement; the machine
                      // ceiling is the one that protects the host.

        // --- everything a request may not choose -----------------------------
        let destination = self.settings.media.destination(&req.destination)?;
        let job_dir = self.settings.state_dir.join(format!("job-{id}"));
        let manifest = self.settings.media.write_manifest(&job_dir, &req.playlist)?;
        // The source is validated here as well as inside the worker, so a bad
        // URL is a 400 on the request rather than a job that fails later.
        //
        // `classify` alone is not enough: it decides *what kind* of address this
        // is, and for a direct stream it says nothing about where the address
        // points. Without the second check a request could name
        // `http://127.0.0.1:8080/api/me` and be accepted, with the refusal
        // happening later and out of sight inside the worker. SSRF has to be
        // refused at the boundary, so it is refused here too.
        validate_source(&req.source_url)?;

        // --- what a restart will need, written before the thread exists -----
        // Without this the state file says "meant to be running" and nothing
        // says what to run, so recovery would skip the job silently.
        JobRequest {
            source_url: req.source_url.clone(),
            destination: req.destination.clone(),
            playlist: req.playlist.clone(),
        }
        .save(&self.settings.state_dir, id)
        .map_err(|e| LiveSourceError::invalid(format!("작업 요청을 쓸 수 없습니다: {e}")))?;

        // --- state before the thread, so a crash in between is recoverable ---
        let mut state = WorkerState::new(id);
        state.owner = owner.to_string();
        state.desired = Desired::Running;
        state.video_id = crate::resolver::video_id(&req.source_url);
        state.phase = Phase::Resolving;
        self.store(id)
            .save(&state)
            .map_err(|e| LiveSourceError::invalid(format!("작업 상태를 쓸 수 없습니다: {e}")))?;

        self.spawn(owner, id, &req.source_url, &manifest, &destination)?;
        self.get(owner, id)
    }

    /// Start the worker thread for a job whose state file already exists.
    fn spawn(
        &self,
        owner: &str,
        id: &str,
        source_url: &str,
        manifest: &std::path::Path,
        destination: &str,
    ) -> Result<()> {
        let mut cfg = WorkerConfig::new(id, source_url, manifest, destination, &self.settings.state_dir);
        cfg.tools = self.settings.tools.clone();
        cfg.profile = self.settings.profile;
        cfg.max_restarts = self.settings.max_restarts;
        cfg.stall_after = self.settings.stall_after;
        cfg.grace = self.settings.grace;

        let worker = LiveWorker::new(cfg, Arc::clone(&self.resolver));
        let stop = worker.stop_handle();
        let owner_s = owner.to_string();
        let id_s = id.to_string();
        let store = self.store(id);
        let running = &self.running;
        let mut worker = worker;
        let handle = std::thread::Builder::new()
            .name(format!("live-source-{id}"))
            .spawn(move || {
                let outcome = worker.run();
                // Record the end state so a reader sees why it stopped. The
                // worker already wrote the phase; this only adds the verdict a
                // caller needs when the thread is gone.
                if let Ok(mut s) = store.load() {
                    match &outcome {
                        // `Stopped` means the stop flag was set — and that
                        // happens for two very different reasons: the user
                        // cancelled, or this process is shutting down. Only the
                        // first is an instruction to stay stopped, and `cancel`
                        // has already written it. So `desired` is deliberately
                        // left alone here: writing it would make a planned
                        // restart lose every running broadcast, because a clean
                        // shutdown would mark them all as not wanted.
                        Ok(crate::worker::Outcome::Stopped { .. }) => {
                            s.phase = Phase::Stopped;
                        }
                        // These two are terminal: the source is not coming
                        // back, so there is nothing for a restart to resume and
                        // the intent is cleared.
                        Ok(crate::worker::Outcome::GaveUp { reason, .. }) => {
                            s.desired = Desired::Stopped;
                            s.phase = Phase::GaveUp;
                            s.last_error = Some(reason.clone());
                        }
                        Ok(crate::worker::Outcome::SourceEnded { .. }) => {
                            s.desired = Desired::Stopped;
                            s.phase = Phase::Stopped;
                        }
                        Err(e) => {
                            s.desired = Desired::Stopped;
                            s.phase = Phase::GaveUp;
                            s.last_error = Some(e.message.clone());
                        }
                    }
                    let _ = store.save(&s);
                }
                let _ = id_s;
            })
            .map_err(|e| LiveSourceError::ffmpeg(format!("작업 스레드를 만들 수 없습니다: {e}")))?;

        running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), Running { owner: owner_s, stop, handle: Some(handle) });
        Ok(())
    }

    /// Stop a job this caller owns.
    ///
    /// Only this job's stop flag is set; nothing else is signalled, and no
    /// other job's FFmpeg is touched.
    pub fn cancel(&self, owner: &str, broadcast_id: &str) -> Result<JobView> {
        let mut state = self
            .store(broadcast_id)
            .load()
            .map_err(|_| LiveSourceError::not_found("해당 작업을 찾을 수 없습니다."))?;
        if state.owner != owner {
            return Err(LiveSourceError::not_found("해당 작업을 찾을 수 없습니다."));
        }
        // The instruction is recorded first, so a crash between here and the
        // thread noticing does not resurrect the job on the next start.
        state.desired = Desired::Stopped;
        let _ = self.store(broadcast_id).save(&state);

        let mut map = self.map();
        if let Some(r) = map.get(broadcast_id) {
            // Belt and braces: the map is keyed by id, and the owner was
            // checked above, but a job's stop flag is the one thing that must
            // never be set from the wrong request.
            if r.owner == owner {
                r.stop.store(true, Ordering::SeqCst);
            }
        }
        if let Some(mut r) = map.remove(broadcast_id) {
            if let Some(h) = r.handle.take() {
                drop(map);
                let _ = h.join();
            }
        }

        // And written again, after the thread is gone.
        //
        // The worker persists by read-modify-write: it loads the file, carries
        // `owner` and `desired` over, and saves. A write of its own that began
        // before the line above could therefore land after it and put
        // `Running` back from a stale read — and the next recovery would
        // restart a broadcast the user had just stopped. Writing before the
        // join keeps a crash safe; writing again after it keeps the race safe.
        // Both are cheap, and the two together leave no window.
        if let Ok(mut again) = self.store(broadcast_id).load() {
            if again.desired != Desired::Stopped {
                again.desired = Desired::Stopped;
                let _ = self.store(broadcast_id).save(&again);
            }
        }
        self.get(owner, broadcast_id)
    }

    /// Restart the jobs that were meant to be running when this process died.
    ///
    /// Reads `desired` and not `phase`: after a crash the last observed phase
    /// is history, while "meant to be running" is still an instruction.
    /// Sequential on purpose — six encodes starting at once is an outage.
    pub fn recover(&self) -> usize {
        let mut started = 0;
        for s in crate::state::scan(&self.settings.state_dir) {
            if s.desired != Desired::Running || s.owner.is_empty() {
                continue;
            }
            if self.map().contains_key(&s.worker_id) {
                continue;
            }
            // The request is gone, so what it asked for has to be on disk. The
            // manifest is; the source and destination are recorded alongside.
            let Ok(saved) = JobRequest::load(&self.settings.state_dir, &s.worker_id) else {
                continue;
            };
            let Ok(destination) = self.settings.media.destination(&saved.destination) else {
                continue;
            };
            let manifest = self.settings.state_dir.join(format!("job-{}", s.worker_id)).join("manifest.txt");
            if !manifest.exists() {
                continue;
            }
            if self.settings.limits.admit(self.running_count()).is_err() {
                break;
            }
            if self.spawn(&s.owner, &s.worker_id, &saved.source_url, &manifest, &destination).is_ok() {
                started += 1;
            }
        }
        started
    }

    /// Ask every job to stop, and wait. For a clean shutdown.
    pub fn shutdown(&self) {
        let ids: Vec<String> = self.map().keys().cloned().collect();
        for id in ids {
            let mut map = self.map();
            if let Some(r) = map.get(&id) {
                r.stop.store(true, Ordering::SeqCst);
            }
            if let Some(mut r) = map.remove(&id) {
                if let Some(h) = r.handle.take() {
                    drop(map);
                    let _ = h.join();
                }
            }
        }
    }
}

/// What a job was asked for, kept beside its state so a restart can rebuild it.
///
/// Deliberately **not** the destination URL — only the operator's name for it,
/// which is resolved again on recovery. A stream key is not written to disk by
/// this API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRequest {
    pub source_url: String,
    pub destination: String,
    pub playlist: Vec<String>,
}

impl JobRequest {
    fn path(state_dir: &std::path::Path, id: &str) -> PathBuf {
        state_dir.join(format!("job-{id}")).join("request.json")
    }

    pub fn save(&self, state_dir: &std::path::Path, id: &str) -> std::io::Result<()> {
        let p = Self::path(state_dir, id);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(p, serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)
    }

    pub fn load(state_dir: &std::path::Path, id: &str) -> std::io::Result<Self> {
        let body = std::fs::read(Self::path(state_dir, id))?;
        serde_json::from_slice(&body).map_err(std::io::Error::other)
    }
}

/// Every check a source address must pass before this worker will open it.
///
/// One function, used by both `POST /jobs` and `POST /check`, because two
/// copies of an SSRF check is one copy too many: the one that gets forgotten is
/// the one an attacker finds. `classify` alone decides *what kind* of address
/// this is and says nothing about where a direct stream points, so the second
/// check is what refuses `http://127.0.0.1:8080/api/me` and
/// `http://169.254.169.254/`.
///
/// A watch URL needs no address check here — the resolver validates the
/// manifest it gets back before FFmpeg sees it, and the host is already pinned
/// to YouTube by `classify`.
pub fn validate_source(url: &str) -> Result<crate::resolver::SourceKind> {
    let kind = crate::resolver::classify(url)?;
    if kind == crate::resolver::SourceKind::DirectStream {
        louver_cloud::cctv::validate(url)
            .map_err(|e| LiveSourceError::invalid(format!("영상 주소를 사용할 수 없습니다: {e}")))?;
    }
    Ok(kind)
}

/// The 1080p ceiling this API advertises, so the UI can say it.
pub fn output_cap() -> (u32, u32) {
    (args::MAX_WIDTH, args::MAX_HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_view_never_carries_a_destination_or_a_manifest_url() {
        let mut s = WorkerState::new("b1");
        s.owner = "user-1".into();
        s.video_id = Some("dQw4w9WgXcQ".into());
        s.output = Some("1920x1080".into());
        let v = JobView::of(&s);
        let json = serde_json::to_string(&v).unwrap();
        for forbidden in ["rtmp", "rtmps", "destination", "manifest", "owner", "videoplayback"] {
            assert!(!json.contains(forbidden), "{forbidden} in {json}");
        }
        // The owner is checked, not published: a response must not tell one
        // user who else exists.
        assert!(!json.contains("user-1"), "{json}");
        assert!(json.contains("dQw4w9WgXcQ"), "the video id is useful and not secret: {json}");
    }

    #[test]
    fn a_saved_request_holds_a_destination_name_and_not_a_url() {
        let dir = tempfile::tempdir().unwrap();
        let r = JobRequest {
            source_url: "https://youtu.be/dQw4w9WgXcQ".into(),
            destination: "test-sink".into(),
            playlist: vec!["a.mp4".into()],
        };
        r.save(dir.path(), "b1").unwrap();
        let raw = std::fs::read_to_string(dir.path().join("job-b1").join("request.json")).unwrap();
        assert!(raw.contains("test-sink"));
        assert!(!raw.contains("rtmp"), "no URL on disk: {raw}");
        assert_eq!(JobRequest::load(dir.path(), "b1").unwrap().destination, "test-sink");
    }

    #[test]
    fn the_output_cap_is_what_the_args_module_enforces() {
        assert_eq!(output_cap(), (1920, 1080));
    }
}
