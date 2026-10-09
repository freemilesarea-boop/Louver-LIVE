//! What survives the worker process dying.
//!
//! A restart is simulated the way it actually happens: one registry starts
//! jobs, then goes away **without** being told the jobs should stop — which is
//! what a `kill -9`, an OOM or a host reboot looks like on disk. A second
//! registry is then built over the same state directory and asked to recover.
//!
//! The field that decides is `desired`, not `phase`. After a crash the last
//! observed phase is history ("it was Sending"); `desired` is still an
//! instruction ("it is meant to be running"). A recovery that read `phase`
//! would restart a job that had already been cancelled, and would skip one that
//! died while resolving.

mod common;

use common::*;
use louver_core::streaming::ffmpeg::FfmpegTools;
use louver_core::OutputProfile;
use louver_live_source::{
    destinations::Destinations,
    jobs::{NewJob, Registry, Settings},
    limits::Limits,
    media::MediaRoot,
    resolver::{LiveSourceResolver, ResolvedSource},
    state::{Desired, StateStore},
    Result,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

struct Never;
impl LiveSourceResolver for Never {
    fn resolve(&self, _: &str) -> Result<ResolvedSource> {
        Err(louver_live_source::LiveSourceError::unavailable("해석하지 않습니다."))
    }
}

struct World {
    _dir: tempfile::TempDir,
    state_dir: PathBuf,
    media_dir: PathBuf,
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let media_dir = dir.path().join("media");
    std::fs::create_dir_all(&media_dir).unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    // Each user's own directory, because media is resolved per user now — and
    // a recovery has to find the file again under the owner recorded in the
    // state file rather than under whoever is asking.
    let root = MediaRoot::new(&media_dir, FfmpegTools::new(ffmpeg(), ffprobe())).unwrap();
    for user in ["user-alice", "user-bob"] {
        std::fs::write(root.dir_for(user).unwrap().join("song-a.mp4"), b"x").unwrap();
    }
    World { _dir: dir, state_dir, media_dir }
}

/// Where each user may send. Both have a `test-sink`; only Alice has the
/// second, so a recovery that resolved under the wrong owner would fail.
fn destinations() -> Destinations {
    let url = "rtmp://127.0.0.1:1/live/abcd-efgh-ijkl-mnop".to_string();
    let mut by_user: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    by_user.insert("user-alice".into(), BTreeMap::from([("test-sink".to_string(), url.clone())]));
    by_user.insert("user-bob".into(), BTreeMap::from([("test-sink".to_string(), url)]));
    Destinations::from_map(by_user).unwrap()
}

/// A fresh registry over an existing state directory — i.e. a restarted process.
fn registry(w: &World, max_concurrent: usize) -> Arc<Registry> {
    registry_limits(w, Limits { max_concurrent, max_per_user: max_concurrent })
}

fn registry_limits(w: &World, limits: Limits) -> Arc<Registry> {
    let settings = Settings {
        state_dir: w.state_dir.clone(),
        media: MediaRoot::new(&w.media_dir, FfmpegTools::new(ffmpeg(), ffprobe())).unwrap(),
        destinations: destinations(),
        tools: FfmpegTools::new(ffmpeg(), ffprobe()),
        profile: OutputProfile::P1080p30,
        limits,
        max_restarts: 50,
        stall_after: Duration::from_secs(60),
        grace: Duration::from_secs(60),
    };
    Arc::new(Registry::new(settings, Arc::new(Never)))
}

fn job(id: &str) -> NewJob {
    NewJob {
        broadcast_id: id.to_string(),
        source_url: "https://93.184.216.34/live/stream.m3u8".to_string(),
        playlist: vec!["song-a.mp4".to_string()],
        destination: "test-sink".to_string(),
    }
}

fn state(dir: &Path, id: &str) -> louver_live_source::WorkerState {
    StateStore::new(dir, id).load().expect("state file")
}

#[test]
fn a_job_that_was_meant_to_be_running_is_restarted_by_the_next_process() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let w = world();

    // --- the first process ------------------------------------------------
    let a = registry(&w, 2);
    a.create("user-alice", "business", &job("keep-me")).unwrap();
    assert_eq!(a.running_count(), 1);
    let before = state(&w.state_dir, "keep-me");
    assert_eq!(before.owner, "user-alice");
    assert_eq!(before.desired, Desired::Running);

    // It dies. `shutdown` stops the threads without touching `desired`, which
    // is what a kill looks like: the instruction outlives the process.
    a.shutdown();
    drop(a);
    assert_eq!(
        state(&w.state_dir, "keep-me").desired,
        Desired::Running,
        "the instruction must survive the process"
    );

    // --- the next process -------------------------------------------------
    let b = registry(&w, 2);
    assert_eq!(b.running_count(), 0, "a fresh registry starts with nothing running");
    assert_eq!(b.recover(), 1, "the job should be restored");
    assert_eq!(b.running_count(), 1);

    // Still Alice's, and still readable only by her.
    let got = b.get("user-alice", "keep-me").unwrap();
    assert_eq!(got.desired, Desired::Running);
    assert!(b.get("user-bob", "keep-me").is_err(), "ownership survives recovery");
    b.shutdown();
}

#[test]
fn a_cancelled_job_is_not_resurrected() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let w = world();
    let a = registry(&w, 2);
    a.create("user-alice", "business", &job("stop-me")).unwrap();
    a.cancel("user-alice", "stop-me").unwrap();
    assert_eq!(state(&w.state_dir, "stop-me").desired, Desired::Stopped);
    a.shutdown();
    drop(a);

    let b = registry(&w, 2);
    assert_eq!(b.recover(), 0, "a stopped job must stay stopped");
    assert_eq!(b.running_count(), 0);
    // And it is still visible to its owner, as a stopped job.
    assert_eq!(b.get("user-alice", "stop-me").unwrap().desired, Desired::Stopped);
}

#[test]
fn recovery_skips_a_job_whose_request_or_manifest_is_gone_rather_than_failing() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let w = world();
    let a = registry(&w, 3);
    a.create("user-alice", "business", &job("intact")).unwrap();
    a.create("user-alice", "business", &job("no-request")).unwrap();
    a.create("user-alice", "business", &job("no-manifest")).unwrap();
    a.shutdown();
    drop(a);

    // Damage two of the three, the way a half-finished delete would.
    std::fs::remove_file(w.state_dir.join("job-no-request").join("request.json")).unwrap();
    std::fs::remove_file(w.state_dir.join("job-no-manifest").join("manifest.txt")).unwrap();

    let b = registry(&w, 3);
    assert_eq!(b.recover(), 1, "only the intact job comes back");
    assert_eq!(b.running_count(), 1);
    assert!(b.get("user-alice", "intact").is_ok());
    b.shutdown();
}

#[test]
fn a_corrupt_state_file_does_not_stop_the_other_jobs_from_recovering() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let w = world();
    let a = registry(&w, 3);
    a.create("user-alice", "business", &job("good-1")).unwrap();
    a.shutdown();
    drop(a);
    // A truncated write, which is what a power cut leaves behind.
    std::fs::write(w.state_dir.join("worker-broken.json"), b"{\"worker_id\":").unwrap();

    let b = registry(&w, 3);
    assert_eq!(b.recover(), 1);
    // The scan skipped the bad file rather than refusing to read the directory.
    assert_eq!(louver_live_source::state::scan(&w.state_dir).len(), 1);
    b.shutdown();
}

#[test]
fn recovery_respects_the_concurrency_ceiling() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let w = world();
    // Three jobs were running when a bigger machine died.
    let a = registry(&w, 3);
    for id in ["r-1", "r-2", "r-3"] {
        a.create("user-alice", "business", &job(id)).unwrap();
    }
    a.shutdown();
    drop(a);

    // The replacement is smaller. Recovery must not start more than it can run.
    let b = registry(&w, 2);
    let started = b.recover();
    assert_eq!(started, 2, "only as many as the ceiling allows");
    assert_eq!(b.running_count(), 2);
    b.shutdown();
}

#[test]
fn a_stopped_job_can_be_started_again_under_the_same_id() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    // Stopping and starting again is ordinary operation, not a conflict. The
    // idempotency rule only returns the existing job while it is still meant to
    // be running.
    let w = world();
    let a = registry(&w, 2);
    a.create("user-alice", "business", &job("again")).unwrap();
    a.cancel("user-alice", "again").unwrap();
    assert_eq!(a.get("user-alice", "again").unwrap().desired, Desired::Stopped);

    let back = a.create("user-alice", "business", &job("again")).unwrap();
    assert_eq!(back.desired, Desired::Running);
    assert_eq!(a.running_count(), 1);
    // And it is still hers.
    assert!(a.get("user-bob", "again").is_err());
    a.shutdown();
}

#[test]
fn a_second_users_job_is_never_recovered_into_the_wrong_owner() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let w = world();
    let a = registry(&w, 3);
    a.create("user-alice", "business", &job("a-job")).unwrap();
    a.create("user-bob", "basic", &job("b-job")).unwrap();
    a.shutdown();
    drop(a);

    let b = registry(&w, 3);
    assert_eq!(b.recover(), 2);
    assert!(b.get("user-alice", "a-job").is_ok());
    assert!(b.get("user-bob", "b-job").is_ok());
    assert!(b.get("user-alice", "b-job").is_err(), "recovery must not move ownership");
    assert!(b.get("user-bob", "a-job").is_err());
    assert_eq!(b.list("user-alice").len(), 1);
    assert_eq!(b.list("user-bob").len(), 1);
    b.shutdown();
}

#[test]
fn the_worker_cannot_undo_a_cancel_by_writing_its_own_state() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    // Two writers share the state file and own different fields. The worker
    // persists every couple of seconds; if it wrote its own `desired` it would
    // flip a cancelled job back to running, and the next recovery would restart
    // a broadcast the user had stopped.
    let w = world();
    let a = registry(&w, 2);
    a.create("user-alice", "business", &job("race")).unwrap();
    a.cancel("user-alice", "race").unwrap();

    // Give any in-flight worker write time to land.
    std::thread::sleep(Duration::from_secs(5));
    let s = state(&w.state_dir, "race");
    assert_eq!(s.desired, Desired::Stopped, "a worker write must not resurrect it");
    assert_eq!(s.owner, "user-alice", "and must not blank the owner");
    a.shutdown();
}
