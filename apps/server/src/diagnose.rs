//! `louver-server --diagnose`: what is actually running, right now.
//!
//! Written because of a stream that the dashboard called live, that was sending
//! bytes, and that never appeared on the channel — and there was no way, from
//! inside the container, to see where those bytes were going. It reads the
//! database and `/proc`, so it needs no HTTP client and no `ps`, and it redacts
//! the stream key out of the command line it prints.

use louver_cloud::CloudDb;
use std::path::PathBuf;

pub fn run() -> std::process::ExitCode {
    let data = PathBuf::from(std::env::var("LOUVER_DATA_DIR").unwrap_or_else(|_| "/var/lib/louver".into()));
    let db = match CloudDb::open(&data.join("cloud.db")) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("[louver] 데이터베이스를 열 수 없습니다 ({}): {e}", data.display());
            return std::process::ExitCode::FAILURE;
        }
    };

    let broadcasts = match db.all_broadcasts() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[louver] 방송 목록을 읽을 수 없습니다: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    println!("=== 247streams 진단 ===");
    println!("data dir : {}", data.display());
    println!("ffmpeg   : {}", std::env::var("LOUVER_FFMPEG_DIR").unwrap_or_else(|_| "(PATH)".into()));
    println!("방송     : {}개\n", broadcasts.len());

    for b in broadcasts {
        let dest = db.destination(&b.destination_id).ok();
        println!("--- {} [{}]", b.name, b.id);
        println!(
            "    상태     : desired={} runtime={} restarts={} uptime={}s",
            b.desired_state.id(),
            b.runtime_state.id(),
            b.restart_count,
            b.uptime_secs
        );
        println!(
            "    플레이리스트: {}/{} now={} next={}",
            b.current_index,
            b.play_count.max(b.item_count),
            b.current_item.as_deref().unwrap_or("—"),
            b.next_item.as_deref().unwrap_or("—")
        );
        println!(
            "    전송     : {} bytes ({:.2} Mbps 평균)",
            b.bytes_sent,
            b.average_bitrate_bps() as f64 / 1_000_000.0
        );
        match &dest {
            Some(d) => {
                let (scheme, host, path) = split_url(&d.rtmps_url);
                println!("    대상     : scheme={scheme} host={host} path={path} ({})", d.label);
            }
            None => println!("    대상     : (찾을 수 없음: {})", b.destination_id),
        }
        if let Some(e) = &b.last_error {
            println!("    마지막 오류: {e}");
        }

        // The process itself, from /proc, with the key taken out.
        match b.ffmpeg_pid {
            Some(pid) => match std::fs::read(format!("/proc/{pid}/cmdline")) {
                Ok(raw) => {
                    let argv: Vec<String> = String::from_utf8_lossy(&raw)
                        .split('\0')
                        .filter(|s| !s.is_empty())
                        .map(|a| redact_ingest_url(a.to_string()))
                        .collect();
                    println!("    FFmpeg   : pid={pid} 살아 있음");
                    println!("    명령     : {}", argv.join(" "));
                    let manifest = argv.iter().position(|a| a == "-i").and_then(|i| argv.get(i + 1));
                    if let Some(m) = manifest {
                        match std::fs::read_to_string(m) {
                            Ok(text) => {
                                println!("    매니페스트: {m}");
                                for line in text.lines() {
                                    println!("        {line}");
                                }
                            }
                            Err(e) => println!("    매니페스트: {m} (읽을 수 없음: {e})"),
                        }
                    }
                }
                Err(_) => println!("    FFmpeg   : pid={pid} 이지만 그런 프로세스가 없습니다"),
            },
            None => println!("    FFmpeg   : 실행 중이 아닙니다"),
        }

        match db.events_for(&b.id, 15) {
            Ok(events) => {
                println!("    최근 기록:");
                for e in events.iter().rev() {
                    println!("        {} [{}] {}", e.at, e.level, e.message);
                }
            }
            Err(e) => println!("    기록을 읽을 수 없습니다: {e}"),
        }
        println!();
    }

    println!("스트림 키는 이 출력에 포함되지 않습니다 ([REDACTED] 로 대체됩니다).");
    std::process::ExitCode::SUCCESS
}

/// `rtmps://host/live2/KEY` → `rtmps://host/live2/[REDACTED]`.
///
/// Only the last segment of an ingest URL is removed, because the host and the
/// application path are exactly what has to be readable.
fn redact_ingest_url(arg: String) -> String {
    if !(arg.starts_with("rtmp://") || arg.starts_with("rtmps://")) {
        return arg;
    }
    match arg.rsplit_once('/') {
        Some((head, key)) if !key.is_empty() => format!("{head}/[REDACTED:{}자]", key.chars().count()),
        _ => arg,
    }
}

fn split_url(url: &str) -> (String, String, String) {
    let url = url.trim_end_matches('/');
    let scheme = url.split("://").next().unwrap_or("").to_string();
    let rest = url.split("://").nth(1).unwrap_or("");
    let host = rest.split('/').next().unwrap_or("").to_string();
    let path = rest.strip_prefix(&host).unwrap_or("").to_string();
    (scheme, host, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_never_survives_being_printed() {
        assert_eq!(
            redact_ingest_url("rtmps://a.rtmps.youtube.com/live2/abcd-1234-efgh-5678".into()),
            "rtmps://a.rtmps.youtube.com/live2/[REDACTED:19자]"
        );
        // Anything that is not an ingest URL is left alone.
        assert_eq!(redact_ingest_url("-c".into()), "-c");
        assert_eq!(
            redact_ingest_url("/var/lib/louver/work/x/manifest.txt".into()),
            "/var/lib/louver/work/x/manifest.txt"
        );
    }

    #[test]
    fn a_url_is_split_into_the_parts_that_are_safe_to_show() {
        let (s, h, p) = split_url("rtmps://a.rtmps.youtube.com/live2");
        assert_eq!((s.as_str(), h.as_str(), p.as_str()), ("rtmps", "a.rtmps.youtube.com", "/live2"));
    }
}
