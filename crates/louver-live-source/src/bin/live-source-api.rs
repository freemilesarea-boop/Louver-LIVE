//! The isolated API, as a command.
//!
//! It opens no production database, holds no OAuth token, and reaches
//! 247streams for exactly one thing: asking `/api/me` who a session cookie
//! belongs to. Point it at a test RTMP sink and a directory of test media.
//!
//! ```text
//! LOUVER_LIVE_SOURCE_SECRET=<32+ bytes> \
//! LOUVER_LIVE_SOURCE_GATE_SECRET=<32+ bytes> \
//! LOUVER_LIVE_SOURCE_DESTINATIONS='{"<user-id>":{"test-sink":"rtmp://127.0.0.1:1935/live/test"}}' \
//! live-source-api \
//!   --listen 127.0.0.1:9080 \
//!   --origin https://247streams.kr \
//!   --media-dir /srv/live-source/media \
//!   --state-dir /var/lib/live-source
//! ```
//!
//! All three secrets come from the environment, never from argv: a process
//! listing is readable by every user on the machine, the destination map
//! contains stream keys, and the gate secret is what keeps a request that did
//! not come through Caddy from reaching a handler.
//!
//! Note the destination shape: it is keyed by **user id** first. A flat
//! `{name: url}` map would be one map shared by every beta account, which is
//! the isolation defect this configuration exists to prevent — so the flat
//! shape is refused rather than silently reinterpreted.
//!
//! There is no way to run this with the gate off. A missing or short
//! `LOUVER_LIVE_SOURCE_GATE_SECRET` is a refusal to start, because a flag that
//! disables a security check is a flag somebody eventually leaves set.

use louver_core::streaming::ffmpeg::FfmpegTools;
use louver_core::OutputProfile;
use louver_live_source::{
    api::Api,
    auth::ProductionMe,
    destinations::Destinations,
    gate::Gate,
    jobs::{Registry, Settings},
    limits::Limits,
    media::MediaRoot,
    origin::AllowedOrigins,
    token::Signer,
    IdentitySource, YtDlpResolver,
};
use std::sync::Arc;
use std::time::Duration;

fn arg(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).cloned()
}

fn die(message: &str) -> ! {
    eprintln!("[louver][live-source-api] {message}");
    std::process::exit(2)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let listen = arg(&args, "--listen").unwrap_or_else(|| "127.0.0.1:9080".to_string());
    let Some(media_dir) = arg(&args, "--media-dir") else { die("--media-dir 가 필요합니다.") };
    let Some(state_dir) = arg(&args, "--state-dir") else { die("--state-dir 가 필요합니다.") };
    let origin = arg(&args, "--origin").unwrap_or_else(|| "https://247streams.kr".to_string());
    // Where a browser may start a session. Defaults to the production origin,
    // which is also where the beta page is served from; a local test adds its
    // own with `--allow-origin`.
    let allow_origin = arg(&args, "--allow-origin").unwrap_or_else(|| origin.clone());
    let beta_dir =
        arg(&args, "--beta-dir").unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/beta").to_string());

    // --- the two secrets, from the environment only ------------------------
    let secret = std::env::var("LOUVER_LIVE_SOURCE_SECRET")
        .unwrap_or_else(|_| die("LOUVER_LIVE_SOURCE_SECRET 를 설정해 주세요 (32바이트 이상)."));
    let signer = match Signer::new(&secret) {
        Ok(s) => Arc::new(s),
        Err(e) => die(&e.message),
    };
    // Dropped as soon as it is in the signer; nothing below can print it.
    drop(secret);

    // Fail-closed: no gate secret, no server. There is deliberately no flag
    // that turns this off.
    let gate_secret = std::env::var("LOUVER_LIVE_SOURCE_GATE_SECRET").unwrap_or_else(|_| {
        die("LOUVER_LIVE_SOURCE_GATE_SECRET 를 설정해 주세요 (32바이트 이상). 게이트를 끌 수는 없습니다.")
    });
    let gate = match Gate::new(&gate_secret) {
        Ok(g) => Arc::new(g),
        // The variable is named, because "32 bytes or more" on its own leaves
        // the operator guessing which of the two secrets is being complained
        // about. The value itself is not printed.
        Err(e) => die(&format!("LOUVER_LIVE_SOURCE_GATE_SECRET: {}", e.message)),
    };
    drop(gate_secret);

    let raw_destinations = std::env::var("LOUVER_LIVE_SOURCE_DESTINATIONS").unwrap_or_else(|_| {
        die("LOUVER_LIVE_SOURCE_DESTINATIONS 를 설정해 주세요 ({\"<user-id>\":{\"name\":\"rtmp://…\"}}).")
    });
    // The error is not printed: a malformed value may still contain a stream
    // key, and serde's message quotes the input.
    let destinations = match Destinations::parse(&raw_destinations) {
        Ok(d) => d,
        Err(e) => die(&e.message),
    };
    drop(raw_destinations);
    // Captured now, because `destinations` moves into `Settings` below and the
    // operator's line wants a count rather than the map.
    let api_destination_users = destinations.user_count();

    let origins = match AllowedOrigins::parse(&allow_origin) {
        Ok(o) => Arc::new(o),
        Err(e) => die(&e.message),
    };

    let tools = FfmpegTools::new(
        arg(&args, "--ffmpeg").unwrap_or_else(|| "ffmpeg".into()),
        arg(&args, "--ffprobe").unwrap_or_else(|| "ffprobe".into()),
    );
    let media = match MediaRoot::new(&media_dir, tools.clone()) {
        Ok(m) => m,
        Err(e) => die(&e.message),
    };
    if let Err(e) = std::fs::create_dir_all(&state_dir) {
        die(&format!("상태 디렉터리를 만들 수 없습니다: {e}"));
    }

    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let absolute_max = arg(&args, "--max-concurrent").and_then(|v| v.parse().ok()).unwrap_or(4);
    let mut limits = Limits::for_machine(cores, absolute_max);
    if let Some(per_user) = arg(&args, "--max-per-user").and_then(|v| v.parse().ok()) {
        limits = limits.with_per_user(per_user);
    }

    let settings = Settings {
        state_dir: state_dir.clone().into(),
        media,
        destinations,
        tools,
        profile: OutputProfile::P1080p30,
        limits,
        max_restarts: arg(&args, "--max-restarts").and_then(|v| v.parse().ok()).unwrap_or(5),
        stall_after: Duration::from_secs(
            arg(&args, "--stall-after").and_then(|v| v.parse().ok()).unwrap_or(12),
        ),
        grace: Duration::from_secs(arg(&args, "--grace").and_then(|v| v.parse().ok()).unwrap_or(20)),
    };

    let resolver = Arc::new(YtDlpResolver::new(arg(&args, "--yt-dlp").unwrap_or_else(|| "yt-dlp".into())));
    let jobs = Arc::new(Registry::new(settings, resolver));
    let identity: Arc<dyn IdentitySource> = match ProductionMe::new(&origin) {
        Ok(p) => Arc::new(p),
        Err(e) => die(&e.message),
    };

    let (budget_cores, budget_mb) = limits.budget();
    println!(
        "[louver][live-source-api] cores={cores} max_concurrent={} max_per_user={} 예상 비용={budget_cores:.1} core / {budget_mb} MB",
        limits.max_concurrent, limits.max_per_user
    );
    println!("[louver][live-source-api] state={state_dir} media={media_dir} origin={origin}");
    // Counts and the allowed origins, never a name and never a URL.
    println!(
        "[louver][live-source-api] gate=on allow-origin={allow_origin} 송출 대상 사용자 {}명",
        api_destination_users
    );

    // Anything that was meant to be running when this process last died.
    let restored = jobs.recover();
    println!("[louver][live-source-api] 복원된 작업 {restored}건");

    let api = Api { jobs: Arc::clone(&jobs), signer, identity, gate, origins };
    let app = api.router_with_beta(std::path::Path::new(&beta_dir));
    let listener = match tokio::net::TcpListener::bind(&listen).await {
        Ok(l) => l,
        Err(e) => die(&format!("{listen} 에 바인드할 수 없습니다: {e}")),
    };
    println!("[louver][live-source-api] listening on http://{listen}  (beta UI: /beta/)");

    let shutdown = {
        let jobs = Arc::clone(&jobs);
        async move {
            let _ = tokio::signal::ctrl_c().await;
            println!("[louver][live-source-api] 종료 신호 — 실행 중인 작업을 정리합니다");
            // `desired` stays as it is, so the next start resumes them.
            tokio::task::spawn_blocking(move || jobs.shutdown()).await.ok();
        }
    };
    if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(shutdown).await {
        eprintln!("[louver][live-source-api] 서버 오류: {e}");
        std::process::exit(1);
    }
}
