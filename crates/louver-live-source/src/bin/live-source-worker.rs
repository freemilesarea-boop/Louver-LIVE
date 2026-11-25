//! The isolated worker, as a command.
//!
//! Nothing about this binary can reach production: it takes a destination on
//! the command line, keeps its state in a directory you name, and opens no
//! database. Point it at the test RTMP sink in `scripts/test-rtmp-sink.sh`.
//!
//! ```text
//! live-source-worker \
//!   --source   https://www.youtube.com/watch?v=VIDEO_ID \
//!   --manifest /path/to/concat-manifest.txt \
//!   --dest     rtmp://127.0.0.1:1935/live/test \
//!   --state-dir /tmp/live-source-state \
//!   --run-for  60
//! ```
//!
//! The destination is read from `--dest` or, preferably, from
//! `LOUVER_LIVE_SOURCE_DEST`, so a stream key never lands in a shell history
//! or a process listing.

use louver_core::streaming::ffmpeg::FfmpegTools;
use louver_live_source::{
    limits::Limits,
    worker::{LiveWorker, WorkerConfig},
    LiveSourceResolver, YtDlpResolver,
};
use std::sync::Arc;
use std::time::Duration;

fn arg(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).cloned()
}

fn usage() -> ! {
    eprintln!(
        "usage: live-source-worker --source <url> --manifest <file> --state-dir <dir> \\\n\
         \x20                        [--dest <rtmp-url> | $LOUVER_LIVE_SOURCE_DEST] \\\n\
         \x20                        [--worker-id <id>] [--run-for <secs>] [--max-restarts <n>] \\\n\
         \x20                        [--stall-after <secs>] [--yt-dlp <path>] [--ffmpeg <path>]"
    );
    std::process::exit(2)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(source) = arg(&args, "--source") else { usage() };
    let Some(manifest) = arg(&args, "--manifest") else { usage() };
    let Some(state_dir) = arg(&args, "--state-dir") else { usage() };
    // Preferred from the environment: a stream key in argv is visible to every
    // process on the machine.
    let dest = std::env::var("LOUVER_LIVE_SOURCE_DEST").ok().or_else(|| arg(&args, "--dest"));
    let Some(dest) = dest else {
        eprintln!("송출 대상이 없습니다. --dest 또는 LOUVER_LIVE_SOURCE_DEST 를 설정해 주세요.");
        std::process::exit(2);
    };

    let worker_id = arg(&args, "--worker-id").unwrap_or_else(|| "w1".to_string());
    let mut cfg = WorkerConfig::new(worker_id, source, manifest, dest, state_dir);
    cfg.tools = FfmpegTools::new(
        arg(&args, "--ffmpeg").unwrap_or_else(|| "ffmpeg".into()),
        arg(&args, "--ffprobe").unwrap_or_else(|| "ffprobe".into()),
    );
    if let Some(v) = arg(&args, "--run-for").and_then(|v| v.parse().ok()) {
        cfg.run_for = Some(Duration::from_secs(v));
    }
    if let Some(v) = arg(&args, "--max-restarts").and_then(|v| v.parse().ok()) {
        cfg.max_restarts = v;
    }
    if let Some(v) = arg(&args, "--stall-after").and_then(|v| v.parse().ok()) {
        cfg.stall_after = Duration::from_secs(v);
    }
    if let Some(v) = arg(&args, "--grace").and_then(|v| v.parse().ok()) {
        cfg.grace = Duration::from_secs(v);
    }

    // One worker per process, so the ceiling is a statement about the machine
    // the operator is on rather than a counter this process keeps.
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let limits = Limits::for_machine(cores, 4);
    let (budget_cores, budget_mb) = limits.budget();
    println!(
        "[louver][live-source] cores={cores} max_concurrent={} 예상 비용={budget_cores:.1} core / {budget_mb} MB",
        limits.max_concurrent
    );

    let resolver: Arc<dyn LiveSourceResolver> =
        Arc::new(YtDlpResolver::new(arg(&args, "--yt-dlp").unwrap_or_else(|| "yt-dlp".into())));
    let mut worker = LiveWorker::new(cfg, resolver);
    println!("[louver][live-source] state={}", worker.state_path().display());

    match worker.run() {
        Ok(outcome) => {
            println!("[louver][live-source] 종료 — {outcome:?}");
        }
        Err(e) => {
            eprintln!("[louver][live-source] 실패 [{}] — {}", e.kind.as_str(), e.message);
            std::process::exit(1);
        }
    }
}
