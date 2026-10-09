//! This worker's own state, in its own file.
//!
//! **Not** the production database. Nothing here opens `cloud.db`, and this
//! crate has no SQL at all. That is the point: a worker that cannot reach the
//! production database cannot write a row, cannot change a broadcast and cannot
//! hold the one connection mutex that every live broadcast goes through.
//!
//! A JSON file per worker, rewritten whole. Rewritten whole because a partial
//! write of a status file is worse than a stale one, and because there is
//! nothing here worth a database: a handful of counters an operator reads.
//!
//! Nothing stored here is a secret. The destination URL is **not** in it — a
//! stream key is the one thing a status file must never carry, and the way to
//! guarantee that is not to have a field for it.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// What the operator asked for, as opposed to what is happening.
///
/// Split from [`Phase`] because the two answer different questions and only one
/// of them survives a restart usefully. After the worker process dies and comes
/// back, "it was Sending" is history; "it is meant to be running" is an
/// instruction. Recovery reads this field and nothing else, which is what makes
/// a crash mid-send resume instead of being forgotten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Desired {
    Running,
    Stopped,
}

/// What is actually happening, as last observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Resolving,
    Starting,
    Sending,
    /// A fault was seen and a restart is waiting out its backoff.
    Reconnecting,
    /// Out of attempts, or a fault a restart will not fix.
    GaveUp,
    Stopped,
}

/// What an operator can read while this is running.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerState {
    /// This worker's own id, chosen by the caller.
    pub worker_id: String,
    /// Who the job belongs to. Checked on every read through the API, so one
    /// user cannot see or cancel another's job.
    #[serde(default)]
    pub owner: String,
    /// The instruction. Recovery after a restart reads this and not `phase`.
    #[serde(default = "desired_running")]
    pub desired: Desired,
    /// The YouTube video id, when the source is a watch URL. Not a secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_id: Option<String>,
    pub phase: Phase,
    /// Frames sent by the current FFmpeg.
    pub frames: u64,
    /// How many times this worker has restarted FFmpeg.
    pub restarts: u32,
    /// The last watchdog verdict, as a token.
    pub last_verdict: String,
    /// Why it last stopped or restarted. A sentence, never a URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Output size actually produced, once known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    pub updated_at: String,
}

impl WorkerState {
    pub fn new(worker_id: impl Into<String>) -> Self {
        Self {
            worker_id: worker_id.into(),
            owner: String::new(),
            desired: Desired::Running,
            video_id: None,
            phase: Phase::Resolving,
            frames: 0,
            restarts: 0,
            last_verdict: "healthy".into(),
            last_error: None,
            output: None,
            updated_at: now(),
        }
    }
}

fn desired_running() -> Desired {
    Desired::Running
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Where one worker's state file lives.
#[derive(Debug, Clone)]
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    /// One file per worker id, inside a directory this worker owns.
    pub fn new(dir: &Path, worker_id: &str) -> Self {
        // The id is used as a file name, so it may not escape the directory.
        let safe: String = worker_id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .take(64)
            .collect();
        Self { path: dir.join(format!("worker-{safe}.json")) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn save(&self, state: &WorkerState) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut s = state.clone();
        s.updated_at = now();
        let body = serde_json::to_vec_pretty(&s).map_err(std::io::Error::other)?;
        // Write beside it and rename, so a reader never sees half a file.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &body)?;
        std::fs::rename(&tmp, &self.path)
    }

    pub fn load(&self) -> std::io::Result<WorkerState> {
        let body = std::fs::read(&self.path)?;
        serde_json::from_slice(&body).map_err(std::io::Error::other)
    }
}

/// Every job this worker has a state file for, for recovery and for listing.
///
/// A file that cannot be parsed is skipped rather than failing the whole scan:
/// one corrupt file must not stop the other jobs from being restored.
pub fn scan(dir: &Path) -> Vec<WorkerState> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        let is_state = p.extension().map(|x| x == "json").unwrap_or(false)
            && p.file_name().and_then(|n| n.to_str()).map(|n| n.starts_with("worker-")).unwrap_or(false);
        if !is_state {
            continue;
        }
        if let Ok(body) = std::fs::read(&p) {
            if let Ok(s) = serde_json::from_slice::<WorkerState>(&body) {
                out.push(s);
            }
        }
    }
    out.sort_by(|a, b| a.worker_id.cmp(&b.worker_id));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path(), "w1");
        let mut s = WorkerState::new("w1");
        s.video_id = Some("dQw4w9WgXcQ".into());
        s.phase = Phase::Sending;
        s.frames = 401;
        store.save(&s).unwrap();
        let back = store.load().unwrap();
        assert_eq!(back.worker_id, "w1");
        assert_eq!(back.frames, 401);
        assert_eq!(back.phase, Phase::Sending);
        assert_eq!(back.video_id.as_deref(), Some("dQw4w9WgXcQ"));
    }

    #[test]
    fn a_worker_id_cannot_escape_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        for id in ["../../etc/passwd", "a/b", "..", "with space", "a\0b"] {
            let store = StateStore::new(dir.path(), id);
            assert_eq!(store.path().parent().unwrap(), dir.path(), "{id:?} escaped");
            store.save(&WorkerState::new(id)).unwrap();
        }
    }

    #[test]
    fn two_workers_do_not_share_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (StateStore::new(dir.path(), "a"), StateStore::new(dir.path(), "b"));
        assert_ne!(a.path(), b.path());
        let mut sa = WorkerState::new("a");
        sa.frames = 10;
        let mut sb = WorkerState::new("b");
        sb.frames = 20;
        a.save(&sa).unwrap();
        b.save(&sb).unwrap();
        assert_eq!(a.load().unwrap().frames, 10);
        assert_eq!(b.load().unwrap().frames, 20);
    }

    #[test]
    fn there_is_no_field_a_stream_key_could_live_in() {
        // Enforced by serialising a fully-populated state and reading the keys:
        // if someone adds a destination field, this fails.
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path(), "w");
        let mut s = WorkerState::new("w");
        s.owner = "user-1".into();
        s.last_error = Some("영상 소스가 멈췄습니다".into());
        s.output = Some("1920x1080".into());
        s.video_id = Some("dQw4w9WgXcQ".into());
        store.save(&s).unwrap();
        let raw = std::fs::read_to_string(store.path()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        let allowed = [
            "worker_id",
            // Checked on every API read so one user cannot see another's job;
            // never serialised back to a client (see `jobs::JobView`).
            "owner",
            "desired",
            "video_id",
            "phase",
            "frames",
            "restarts",
            "last_verdict",
            "last_error",
            "output",
            "updated_at",
        ];
        for k in &keys {
            assert!(allowed.contains(&k.as_str()), "unexpected field in the state file: {k}");
        }
        for forbidden in ["rtmp", "rtmps", "stream_key", "destination", "token", "key"] {
            assert!(!raw.to_lowercase().contains(forbidden), "{forbidden} in {raw}");
        }
    }
}
