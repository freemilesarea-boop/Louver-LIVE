//! That this worker cannot reach anything it does not own.
//!
//! Two kinds of check, because the property has two halves:
//!
//!  * **By measurement** — two workers run at once, one source is taken away,
//!    and the other keeps sending without a restart. That is the behaviour
//!    customers depend on.
//!  * **By reading the source** — the crate contains no way to name a process
//!    it did not spawn, and no way to open the production database. A test that
//!    greps its own crate is unusual, but this is the one guarantee that cannot
//!    be shown by running the happy path, and it is cheap to keep honest.

mod common;

use common::*;
use louver_core::streaming::ffmpeg::FfmpegTools;
use louver_live_source::{
    resolver::{LiveSourceResolver, ResolvedSource},
    state::{Phase, StateStore},
    worker::{LiveWorker, WorkerConfig},
    Result,
};
use std::sync::Arc;
use std::time::Duration;

struct FixedSource(String);
impl LiveSourceResolver for FixedSource {
    fn resolve(&self, _: &str) -> Result<ResolvedSource> {
        Ok(ResolvedSource {
            manifest_url: self.0.clone(),
            is_live: true,
            width: Some(640),
            height: Some(360),
            title: None,
        })
    }
}

const WATCH: &str = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";

#[test]
#[ignore = "runs two real encodes at once; run explicitly"]
fn one_workers_source_failing_does_not_disturb_another_worker() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let manifest = make_playlist(dir.path());
    let state_dir = dir.path().join("state");

    // Two independent sources, each with its own generator and its own server.
    let mut rigs = Vec::new();
    for name in ["a", "b"] {
        let hls = dir.path().join(format!("hls_{name}"));
        let generator = spawn_hls_realtime(&hls, "640x360", 150);
        rigs.push((name, hls, generator));
    }
    std::thread::sleep(Duration::from_secs(7));

    let mut workers = Vec::new();
    for (name, hls, _) in &rigs {
        let server = StaticServer::start(hls);
        let port = free_port();
        let sink = spawn_rtmp_sink(port, &dir.path().join(format!("recv_{name}.flv")), 150);
        std::thread::sleep(Duration::from_millis(900));
        let mut cfg = WorkerConfig::new(
            *name,
            WATCH,
            &manifest,
            format!("rtmp://127.0.0.1:{port}/live/test"),
            &state_dir,
        );
        cfg.tools = FfmpegTools::new(ffmpeg(), ffprobe());
        cfg.stall_after = Duration::from_secs(3);
        cfg.grace = Duration::from_secs(6);
        cfg.max_restarts = 3;
        cfg.run_for = Some(Duration::from_secs(70));
        let store = StateStore::new(&state_dir, name);
        let resolver = Arc::new(FixedSource(server.url("live.m3u8")));
        let stop_probe = store.clone();
        let handle = std::thread::spawn(move || {
            let mut w = LiveWorker::new(cfg, resolver);
            w.run()
        });
        workers.push((*name, server, sink, stop_probe, handle));
    }

    let frames = |s: &StateStore| s.load().map(|x| x.frames).unwrap_or(0);
    let restarts = |s: &StateStore| s.load().map(|x| x.restarts).unwrap_or(0);
    let phase = |s: &StateStore| s.load().ok().map(|x| x.phase);

    // Both sending.
    assert!(
        wait_until(Duration::from_secs(45), || workers
            .iter()
            .all(|(_, _, _, st, _)| phase(st) == Some(Phase::Sending) && frames(st) > 0)),
        "both workers should be sending"
    );

    // Separate state files, which is what "independent state" means on disk.
    assert_ne!(workers[0].3.path(), workers[1].3.path());

    let b_before = frames(&workers[1].3);

    // Take worker A's source away. Only A's.
    let _ = rigs[0].2.kill();
    let _ = rigs[0].2.wait();

    // A notices.
    assert!(
        wait_until(Duration::from_secs(60), || restarts(&workers[0].3) >= 1),
        "worker a should have noticed its stalled source"
    );

    // B is untouched: still sending, still advancing, never restarted.
    assert_eq!(restarts(&workers[1].3), 0, "worker b must not restart because a's source died");
    assert_eq!(phase(&workers[1].3), Some(Phase::Sending), "worker b must still be sending");
    assert!(
        wait_until(Duration::from_secs(30), || frames(&workers[1].3) > b_before + 30),
        "worker b's picture should keep moving (was {b_before}, now {})",
        frames(&workers[1].3)
    );
    assert_eq!(restarts(&workers[1].3), 0, "and still no restart after the wait");

    for (_, mut server, mut sink, _, handle) in workers {
        server.kill();
        let _ = sink.kill();
        let _ = sink.wait();
        let _ = handle.join();
    }
    for (_, _, mut g) in rigs {
        let _ = g.kill();
        let _ = g.wait();
    }
}

/// Every `.rs` file in this crate, as text.
fn crate_sources() -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().map(|x| x == "rs").unwrap_or(false) {
                out.push((p.display().to_string(), std::fs::read_to_string(&p).unwrap()));
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    walk(&root.join("src"), &mut out);
    out
}

#[test]
fn nothing_in_this_crate_can_name_a_process_it_did_not_spawn() {
    // The isolation guarantee, as a property of the source rather than of one
    // run. `Child::kill` acts on a handle from `spawn`; everything below acts
    // on a process chosen by number or by name, which is how a worker could
    // reach another broadcast's FFmpeg.
    const FORBIDDEN: &[&str] = &[
        "pkill",
        "killall",
        "pgrep",
        "/proc/",
        "libc::kill",
        "signal::kill",
        "sysinfo",
        "from_pid",
        "Pid::from",
    ];
    for (path, body) in crate_sources() {
        for needle in FORBIDDEN {
            // Allowed in a comment that explains why it is not used.
            let in_code = body
                .lines()
                .filter(|l| !l.trim_start().starts_with("//") && !l.trim_start().starts_with("//!"))
                .any(|l| l.contains(needle));
            assert!(!in_code, "{path} names {needle} outside a comment");
        }
    }
}

#[test]
fn this_crate_cannot_open_the_production_database() {
    // No SQL, no rusqlite, no cloud.db. The worker's state is a JSON file it
    // owns, so there is no connection and no mutex shared with a live broadcast.
    const FORBIDDEN: &[&str] = &["rusqlite", "cloud.db", "SELECT ", "INSERT ", "UPDATE ", "CloudDb"];
    for (path, body) in crate_sources() {
        for needle in FORBIDDEN {
            let in_code = body
                .lines()
                .filter(|l| !l.trim_start().starts_with("//") && !l.trim_start().starts_with("//!"))
                .any(|l| l.contains(needle));
            assert!(!in_code, "{path} names {needle} outside a comment");
        }
    }
    // And the dependency is not declared either, so it cannot creep back in.
    let toml =
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    assert!(!toml.contains("rusqlite"), "rusqlite must not be a dependency of this crate");
}

#[test]
fn this_crate_contains_no_oauth_or_youtube_api_code() {
    // The 401 investigation is a separate issue and this feature must not touch
    // it. Named here so a future change that reaches for a token fails a test
    // rather than a review.
    const FORBIDDEN: &[&str] =
        &["refresh_token", "access_token", "liveBroadcasts", "liveStreams", "oauth2", "client_secret"];
    for (path, body) in crate_sources() {
        for needle in FORBIDDEN {
            let in_code = body
                .lines()
                .filter(|l| !l.trim_start().starts_with("//") && !l.trim_start().starts_with("//!"))
                .any(|l| l.contains(needle));
            assert!(!in_code, "{path} names {needle} outside a comment");
        }
    }
}

#[test]
fn standalone_mode_cannot_reach_production_at_all() {
    use louver_live_source::standalone;

    // The crate has exactly ONE outbound HTTP call — `{origin}/api/me` in
    // `auth.rs` — so "where can this reach production?" has one answer, and
    // the standalone guard is on it. If a second HTTP client appears, this
    // fails and the guard has to be extended to cover it.
    let with_http: Vec<String> = crate_sources()
        .into_iter()
        .filter(|(_, body)| {
            body.lines()
                .filter(|l| !l.trim_start().starts_with("//") && !l.trim_start().starts_with("//!"))
                .any(|l| l.contains("ureq") || l.contains("reqwest") || l.contains("TcpStream::connect"))
        })
        .map(|(path, _)| path)
        .collect();
    assert_eq!(with_http.len(), 1, "expected one outbound HTTP call site, found {with_http:?}");
    assert!(with_http[0].ends_with("auth.rs"), "the outbound call moved to {:?}", with_http[0]);

    // And on that one call site, standalone mode refuses the service's domain
    // and every subdomain of it, in every spelling, so the configured target
    // cannot be the live service.
    for production in [
        "https://247streams.kr",
        "https://247streams.kr/",
        "https://247STREAMS.KR",
        "https://247streams.kr.",
        "https://247streams.kr:443",
        "https://www.247streams.kr",
        "https://api.247streams.kr",
        "https://beta-test.247streams.kr.:443/",
    ] {
        assert!(standalone::check(Some(production), None).is_err(), "{production} was not refused");
        // The same policy on the browser origin, not only the auth origin.
        assert!(
            standalone::check(Some("https://beta-test.example.com"), Some(production)).is_err(),
            "{production} was not refused as --allow-origin"
        );
    }
    // Omitting it is refused too, which is what stops the production default.
    assert!(standalone::check(None, Some("https://beta-test.example.com")).is_err());

    // Nothing else production owns is nameable from here: no database and no
    // OAuth (their own tests above), and no Caddy configuration or container.
    const FORBIDDEN: &[&str] = &["Caddyfile", "docker compose", "docker-compose", "louver:8080"];
    for (path, body) in crate_sources() {
        for needle in FORBIDDEN {
            let in_code = body
                .lines()
                .filter(|l| !l.trim_start().starts_with("//") && !l.trim_start().starts_with("//!"))
                .any(|l| l.contains(needle));
            assert!(!in_code, "{path} names {needle} outside a comment");
        }
    }
}
