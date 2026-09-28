//! `louver-server --diagnose`: what is actually running, right now.
//!
//! Written because of a stream that the dashboard called live, that was sending
//! bytes, and that never appeared on the channel — and there was no way, from
//! inside the container, to see where those bytes were going. It reads the
//! database and `/proc`, so it needs no HTTP client and no `ps`, and it redacts
//! the stream key out of the command line it prints.

use louver_cloud::CloudDb;
use louver_core::security::SecretStore;
use std::path::PathBuf;
use std::sync::Arc;

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

    // Opened when the master key is available, so a key can be fingerprinted.
    // Its absence is itself a diagnosis — it is why no broadcast could start —
    // so it is reported rather than fatal.
    let keys: Option<Arc<dyn SecretStore>> = match louver_cloud::credentials::master_key_from_env() {
        Ok(master) => Some(Arc::new(louver_cloud::credentials::CredentialStore::new(db.raw(), master))),
        Err(e) => {
            println!("[louver] 경고: master key 를 읽을 수 없습니다 ({e}). 키 지문은 생략됩니다.");
            None
        }
    };

    println!("=== 247streams 진단 ===");
    println!("data dir : {}", data.display());
    println!("ffmpeg   : {}", std::env::var("LOUVER_FFMPEG_DIR").unwrap_or_else(|_| "(PATH)".into()));
    println!("방송     : {}개", broadcasts.len());
    // The number an operator wants first when anything is behaving oddly: a
    // server that cannot write cannot broadcast, and the symptoms look like
    // everything else.
    let free = louver_core::system::available_disk_bytes(&data) / 1_048_576;
    let floor = louver_cloud::ingest::DISK_FLOOR_BYTES / 1_048_576;
    println!(
        "디스크   : 여유 {free}MB (업로드 차단 기준 {floor}MB){}",
        if free != 0 && free < floor { "  ← 업로드가 거부됩니다" } else { "" }
    );
    youtube_configuration();
    accounts(&db, keys.as_ref());
    println!();

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
                println!(
                    "    대상     : provider={} scheme={scheme} host={host} path={path} ({})",
                    d.kind.id(),
                    d.label
                );
                println!("    스트림 키: {}", key_fingerprint(keys.as_ref(), &d.id));
            }
            None => println!("    대상     : (찾을 수 없음: {})", b.destination_id),
        }
        // §19: which channel, which YouTube resources, and what YouTube itself
        // last said — never inferred from FFmpeg being alive.
        match &b.youtube.account_id {
            Some(account_id) => {
                let channel = db
                    .youtube_accounts_for(&b.user_id)
                    .ok()
                    .and_then(|list| list.into_iter().find(|a| &a.id == account_id))
                    .map(|a| format!("{} ({})", a.channel_title, a.channel_id))
                    .unwrap_or_else(|| "(계정 행을 찾을 수 없음)".into());
                println!("    YouTube  : account={account_id} channel={channel}");
                println!(
                    "               broadcast={} stream={} status={}",
                    b.youtube.broadcast_id.as_deref().unwrap_or("—"),
                    b.youtube.stream_id.as_deref().unwrap_or("—"),
                    b.youtube.status.as_deref().unwrap_or("—")
                );
                if let Some(e) = &b.youtube.last_error {
                    println!("               마지막 API 오류: {e}");
                }
            }
            None => println!("    YouTube  : 연결된 계정 없음 (manual_rtmps)"),
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
    println!("client_secret=[REDACTED] refresh_token=[REDACTED] access_token=[REDACTED]");
    std::process::ExitCode::SUCCESS
}

/// Is YouTube connecting configured here, and with which redirect URI?
///
/// Presence only. §15: the client secret is never printed, and the line says so
/// explicitly rather than leaving a reader to wonder whether it was omitted or
/// simply missing.
fn youtube_configuration() {
    let id = std::env::var("YOUTUBE_CLIENT_ID").unwrap_or_default();
    let secret = std::env::var("YOUTUBE_CLIENT_SECRET").unwrap_or_default();
    println!(
        "YouTube  : client_id={} client_secret={} redirect_uri={}",
        if id.trim().is_empty() {
            "(없음)".to_string()
        } else {
            format!("설정됨 len={}", id.trim().len())
        },
        if secret.trim().is_empty() { "(없음)" } else { "[REDACTED]" },
        louver_cloud::youtube::redirect_uri()
    );
}

/// The connected channels, across every account on this server.
fn accounts(db: &CloudDb, keys: Option<&Arc<dyn SecretStore>>) {
    let Ok(users) = db.all_user_ids() else { return };
    let mut any = false;
    for user_id in users {
        let Ok(list) = db.youtube_accounts_for(&user_id) else { continue };
        for a in list {
            any = true;
            println!(
                "계정     : {} / {} ({}) expiry={} verified={}",
                a.id,
                a.channel_title,
                a.channel_id,
                a.token_expiry.as_deref().unwrap_or("—"),
                a.last_verified_at.as_deref().unwrap_or("—")
            );
            // Presence, length and a fingerprint. Never the token.
            println!(
                "           refresh_token={} access_token=[REDACTED]",
                match keys {
                    Some(k) => {
                        let f = louver_cloud::credentials::fingerprint(
                            k,
                            &louver_cloud::youtube::refresh_account(&a.id),
                        );
                        if f.present() {
                            f.to_string()
                        } else {
                            "(없음 — 다시 연결해야 합니다)".to_string()
                        }
                    }
                    None => "(master key 없음)".to_string(),
                }
            );
        }
    }
    if !any {
        println!("계정     : 연결된 YouTube 계정이 없습니다");
    }
}

/// `stream_key=[REDACTED len=N sha256:8hex]`, or why there is none. §15.
fn key_fingerprint(keys: Option<&Arc<dyn SecretStore>>, destination_id: &str) -> String {
    let Some(keys) = keys else { return "[REDACTED] (master key 없음 — 지문 생략)".into() };
    let f = louver_cloud::credentials::fingerprint(
        keys,
        &louver_cloud::credentials::destination_account(destination_id),
    );
    if f.present() {
        f.to_string()
    } else {
        format!("{f} — 이 대상으로는 송출할 수 없습니다")
    }
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
