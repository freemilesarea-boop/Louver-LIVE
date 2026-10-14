//! `louver-server --youtube-audit`, run as an operator runs it.
//!
//! The unit tests next to the code prove the verdicts and the masking. These
//! prove the thing only a real run can: that the binary opens a real database
//! read-only, asks a real HTTP endpoint, prints the verdict, and leaves the
//! database byte-for-byte as it found it.
//!
//! The database is seeded through `louver-cloud`'s own API, so the schema is
//! whatever the current release writes, and the YouTube end is a stub served
//! from this test over a loopback port. Nothing here reaches the internet.

use louver_cloud::credentials::CredentialStore;
use louver_cloud::youtube::{access_account, refresh_account};
use louver_cloud::CloudDb;
use louver_core::security::SecretStore;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;

const MASTER: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

/// What the stub should say about the broadcast and its stream.
#[derive(Clone, Copy)]
struct Scenario {
    life_cycle_status: &'static str,
    stream_status: &'static str,
    monitor_stream: bool,
    bound_stream_id: &'static str,
}

/// A stand-in for the YouTube Data API, serving exactly the two `list` calls
/// the audit makes. Returns the port it is listening on.
fn stub_youtube(s: Scenario) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            // Drain the headers so the client is not left waiting on a write.
            loop {
                let mut h = String::new();
                match reader.read_line(&mut h) {
                    Ok(0) => break,
                    Ok(_) if h.trim().is_empty() => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            let body = if line.contains("/liveBroadcasts") {
                format!(
                    r#"{{"items":[{{"id":"bcast-1",
                       "status":{{"lifeCycleStatus":"{}","privacyStatus":"public"}},
                       "contentDetails":{{"boundStreamId":"{}","enableAutoStart":true,
                         "enableAutoStop":true,
                         "monitorStream":{{"enableMonitorStream":{}}}}},
                       "snippet":{{"title":"t"}}}}]}}"#,
                    s.life_cycle_status, s.bound_stream_id, s.monitor_stream
                )
            } else if line.contains("/liveStreams") {
                format!(
                    r#"{{"items":[{{"id":"stream-1",
                       "cdn":{{"ingestionInfo":{{"ingestionAddress":"rtmp://stub/live2",
                         "streamName":"STUB-KEY-NEVER-PRINTED"}}}},
                       "status":{{"streamStatus":"{}",
                         "healthStatus":{{"status":"good","configurationIssues":[]}}}}}}]}}"#,
                    s.stream_status
                )
            } else {
                r#"{"error":{"code":404,"message":"not found"}}"#.to_string()
            };
            let res = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = conn.write_all(res.as_bytes());
            let _ = conn.flush();
        }
    });
    // Wait for it to accept, so the binary does not race the listener.
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    port
}

/// A database with one YouTube-connected broadcast that believes it is running.
fn seed(dir: &Path) -> String {
    let db = CloudDb::open(&dir.join("cloud.db")).unwrap();
    let master = hex_master();
    let keys: Arc<dyn SecretStore> = Arc::new(CredentialStore::new(db.raw(), master));

    let user = db.create_user("dj@example.com", "hash", "business").unwrap().id;
    let media = db.create_media(&user, "night.mp4", 42, "night-key").unwrap().id;
    db.record_media_prepared(&media, "night-key", 60.0, 0, 42).unwrap();

    let account = db.upsert_youtube_account(&user, "UC-chan", "채널 이름", None).unwrap();
    let dest = db.reserve_youtube_destination(&user, &account.id).unwrap();
    let b = db.create_broadcast(&user, "밤의 플레이리스트", &media, &dest.id, true).unwrap();

    // Sealed the way the provider seals them, so the audit reads a real token.
    keys.set(&refresh_account(&account.id), "refresh-token-1").unwrap();
    keys.set(&access_account(&account.id), "access-token-1").unwrap();
    db.set_youtube_token_expiry(&account.id, 3600).unwrap();
    db.attach_youtube(&b.id, &account.id, "bcast-1", "stream-1", "waiting_for_ingest").unwrap();

    // Running, with a pid nothing can be using: above the kernel's maximum,
    // so `pid_alive` is false for a reason that holds on any machine. (pid 2
    // does *not* work here — in a container it is a live kernel thread, and
    // the audit would correctly report an encoder as running.)
    db.record_runtime(&b.id, louver_cloud::RuntimeState::Running, 0, 120, 1_000_000, Some(4_194_305))
        .unwrap();
    b.id
}

fn hex_master() -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&MASTER[i * 2..i * 2 + 2], 16).unwrap();
    }
    out
}

/// Every byte of the database file, to prove a read changed nothing.
fn file_bytes(p: &Path) -> Vec<u8> {
    std::fs::read(p).unwrap_or_default()
}

fn audit(dir: &Path, port: u16, extra: &[&str]) -> std::process::Output {
    let exe = env!("CARGO_BIN_EXE_louver-server");
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--youtube-audit")
        .args(extra)
        .env("LOUVER_DATA_DIR", dir)
        .env("LOUVER_MASTER_KEY", MASTER)
        .env("YOUTUBE_CLIENT_ID", "test-client.apps.googleusercontent.com")
        .env("YOUTUBE_CLIENT_SECRET", "test-secret")
        .env("YOUTUBE_API_BASE", format!("http://127.0.0.1:{port}/youtube/v3"))
        // So a stray refresh cannot reach Google even if the logic changed.
        .env("YOUTUBE_TOKEN_ENDPOINT", format!("http://127.0.0.1:{port}/token"));
    cmd.output().unwrap()
}

/// Seed a database wherever `LOUVER_AUDIT_SEED_DIR` points, for driving
/// `scripts/youtube-audit.sh` by hand against a real file.
///
/// `#[ignore]` so it never runs in CI: it writes outside the test's temporary
/// directory, which a test has no business doing unasked.
#[test]
#[ignore = "fixture generator; run with LOUVER_AUDIT_SEED_DIR set"]
fn seed_a_database_for_the_operator_script() {
    let dir = std::env::var("LOUVER_AUDIT_SEED_DIR").expect("set LOUVER_AUDIT_SEED_DIR");
    let dir = Path::new(&dir);
    std::fs::create_dir_all(dir).unwrap();
    let id = seed(dir);
    println!("seeded {} with broadcast {id}", dir.display());
}

#[test]
fn the_audit_names_the_p0_state_and_leaves_the_database_untouched() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let db_file = dir.path().join("cloud.db");

    // The outage: ingest arriving, broadcast parked in the monitor preview.
    let port = stub_youtube(Scenario {
        life_cycle_status: "testing",
        stream_status: "active",
        monitor_stream: true,
        bound_stream_id: "stream-1",
    });

    let before = file_bytes(&db_file);
    let out = audit(dir.path(), port, &[]);
    let text = String::from_utf8_lossy(&out.stdout);

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("STUCK TESTING"), "no verdict in:\n{text}");
    assert!(text.contains("실시간 시작"), "the remedy is missing:\n{text}");
    assert!(text.contains("enableMonitorStream=true"), "the cause is missing:\n{text}");
    assert!(text.contains("조치가 필요한 방송 1개"), "the summary is missing:\n{text}");

    // The promise this tool is built on.
    assert_eq!(file_bytes(&db_file), before, "the audit wrote to the database");
}

#[test]
fn the_audit_never_prints_a_stream_key_or_a_token() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let port = stub_youtube(Scenario {
        life_cycle_status: "live",
        stream_status: "active",
        monitor_stream: false,
        bound_stream_id: "stream-1",
    });

    let out = audit(dir.path(), port, &["--show-ids"]);
    let all = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    for secret in ["STUB-KEY-NEVER-PRINTED", "access-token-1", "refresh-token-1", MASTER] {
        assert!(!all.contains(secret), "{secret} appeared in the output:\n{all}");
    }
    assert!(all.contains("CONFIRMED LIVE") || all.contains("BROADCAST MISMATCH"), "{all}");
}

#[test]
fn identifiers_are_masked_unless_the_operator_asks() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let port = stub_youtube(Scenario {
        life_cycle_status: "live",
        stream_status: "active",
        monitor_stream: false,
        bound_stream_id: "stream-1",
    });

    let hidden = String::from_utf8_lossy(&audit(dir.path(), port, &[]).stdout).to_string();
    assert!(!hidden.contains("bcast-1"), "a customer identifier leaked:\n{hidden}");
    assert!(hidden.contains("bc…-1") || hidden.contains("•"), "nothing was shown at all:\n{hidden}");

    let shown = String::from_utf8_lossy(&audit(dir.path(), port, &["--show-ids"]).stdout).to_string();
    assert!(shown.contains("bcast-1"), "--show-ids did not show it:\n{shown}");
}

#[test]
fn a_stream_bound_elsewhere_is_reported_as_a_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let port = stub_youtube(Scenario {
        life_cycle_status: "live",
        stream_status: "active",
        monitor_stream: false,
        bound_stream_id: "somebody-elses-stream",
    });

    let text = String::from_utf8_lossy(&audit(dir.path(), port, &[]).stdout).to_string();
    assert!(text.contains("BROADCAST MISMATCH"), "{text}");
    assert!(text.contains("불일치"), "the linkage check is missing:\n{text}");
}

#[test]
fn an_unreachable_youtube_is_never_reported_as_a_live_broadcast() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    // A port with nothing on it. The audit has to come back, and come back
    // honest: the lesson of the outage, at the level of the tool.
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        drop(l);
        p
    };

    let out = audit(dir.path(), dead, &[]);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "the tool must not crash: {}", String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("UNKNOWN"), "{text}");
    assert!(!text.contains("CONFIRMED LIVE"), "silence was read as success:\n{text}");
    assert!(text.contains("관측은 없습니다"), "evidence and inference are not separated:\n{text}");
}

#[test]
fn an_expired_token_is_reported_rather_than_refreshed() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    // Expire the cached token. The default must not refresh it, because a
    // refresh can rotate a refresh token a read-only process cannot store.
    {
        let db = CloudDb::open(&dir.path().join("cloud.db")).unwrap();
        db.raw()
            .lock()
            .unwrap()
            .execute("UPDATE youtube_accounts SET token_expiry = datetime('now','-1 hour')", [])
            .unwrap();
    }
    let port = stub_youtube(Scenario {
        life_cycle_status: "live",
        stream_status: "active",
        monitor_stream: false,
        bound_stream_id: "stream-1",
    });

    let text = String::from_utf8_lossy(&audit(dir.path(), port, &[]).stdout).to_string();
    assert!(text.contains("AUTH ERROR"), "{text}");
    assert!(text.contains("갱신하지 않습니다"), "the operator is not told why:\n{text}");
}

#[test]
fn a_database_older_than_the_binary_is_refused_rather_than_half_read() {
    // A read-only open does not migrate, so the row mapper would be selecting
    // columns that are not there. Saying so beats inventing the fields.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cloud.db");
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE broadcasts (id TEXT PRIMARY KEY);").unwrap();
    }

    let out = audit(dir.path(), 1, &[]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "it must not claim to have audited anything");
    assert!(err.contains("오래된 스키마") || err.contains("읽을 수 없습니다"), "{err}");
}
