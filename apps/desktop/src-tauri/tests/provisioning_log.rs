//! The whole YouTube preparation, driven against local stand-ins for Google.
//!
//! Written for a failure that could not be diagnosed: on a real Mac, a
//! scheduled window retried six times and wrote six copies of
//! `LL-YOUTUBE-004 · YouTube에 연결하지 못했습니다`. Every one of the seven
//! requests a start makes fails with that code, so the log said only that
//! something about YouTube did not work.
//!
//! Nothing below asserts that provisioning *succeeds* against real Google —
//! that needs an account and a channel, and is marked NOT TESTED until it has
//! been run there. What it does assert is that when a step fails, the log and
//! the screen say which one, in Google's own words, and that no credential
//! travels with the report.

use louver_core::database::Database;
use louver_core::logging::Logger;
use louver_core::security::{MemorySecretStore, SecretStore, StreamKeyStore};
use louver_core::youtube::keys;
use louver_core::youtube::oauth::TokenStore;
use louver_desktop::youtube_service::{ApplyStage, YoutubeService};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

const STREAM_KEY: &str = "abcd-efgh-ijkl-mnop";
const REFRESH_TOKEN: &str = "1//04realrefreshtoken";
const ACCESS_TOKEN: &str = "ya29.a0AfB_realaccesstoken";
const CLIENT_SECRET: &str = "GOCSPX-realclientsecret";

/// A local HTTP server that answers from a routing table and records what it
/// was asked. Used for both Google endpoints this path touches.
struct Fake {
    base: String,
    seen: Arc<Mutex<Vec<String>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl Fake {
    /// `routes` maps a "METHOD /path?query" prefix to (status, body), first
    /// match winning, so a more specific route is written first.
    fn start(routes: Vec<(&'static str, u16, String)>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let recorded = Arc::clone(&seen);
        let stopping = Arc::clone(&stop);

        std::thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !stopping.load(std::sync::atomic::Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // Windows hands back an accepted socket that inherits
                        // the listener's non-blocking mode; Linux and macOS
                        // give a blocking one. Left as it comes, `read_line`
                        // below returns WouldBlock, the request is dropped
                        // without a reply, and the client waits out its whole
                        // timeout — a Windows-only flake that looks like a
                        // hung server.
                        let _ = stream.set_nonblocking(false);
                        let mut reader = BufReader::new(stream.try_clone().unwrap());
                        let mut line = String::new();
                        if reader.read_line(&mut line).is_err() {
                            continue;
                        }
                        let mut parts = line.split_whitespace();
                        let method = parts.next().unwrap_or("").to_string();
                        let path = parts.next().unwrap_or("").to_string();
                        let mut length = 0usize;
                        loop {
                            let mut h = String::new();
                            if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
                                break;
                            }
                            if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                                length = v.trim().parse().unwrap_or(0);
                            }
                        }
                        let mut body = vec![0u8; length];
                        if length > 0 {
                            let _ = reader.read_exact(&mut body);
                        }
                        recorded
                            .lock()
                            .unwrap()
                            .push(format!("{method} {path} {}", String::from_utf8_lossy(&body)));

                        // Google's own rule, enforced here so no test can
                        // pass against a request the real API refuses:
                        // `liveBroadcasts.list` takes exactly one of `id`,
                        // `mine` and `broadcastStatus`. Sending two is what
                        // stopped every scheduled start on a real Mac, and a
                        // fake that shrugged at it is why nothing caught it.
                        if method == "GET" && path.starts_with("/liveBroadcasts") {
                            let filters = ["&id=", "?id=", "mine=", "broadcastStatus="]
                                .iter()
                                .filter(|f| path.contains(**f))
                                .count();
                            if filters != 1 {
                                let payload = r#"{"error":{"code":400,"message":"Incompatible parameters specified in the request: broadcastStatus, mine","errors":[{"reason":"incompatibleParameters","domain":"youtube.liveBroadcast"}]}}"#;
                                let response = format!(
                                    "HTTP/1.1 400 X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                                    payload.len()
                                );
                                let _ = stream.write_all(response.as_bytes());
                                let _ = stream.flush();
                                continue;
                            }
                        }

                        // The query is part of the key: `liveBroadcasts.list`
                        // and a lookup by id are the same path, and so are
                        // `insert` and `bind` under a path-only key.
                        let key = format!("{method} {path}");
                        let (status, payload) = routes
                            .iter()
                            .find(|(k, _, _)| key.starts_with(*k))
                            .map(|(_, s, b)| (*s, b.clone()))
                            .unwrap_or((404, r#"{"error":{"message":"no stub"}}"#.into()));
                        let response = format!(
                            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                            payload.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
        });
        Self { base: format!("http://127.0.0.1:{port}"), seen, stop }
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

const TOKEN_OK: &str = r#"{"access_token":"ya29.a0AfB_realaccesstoken","expires_in":3599}"#;

/// The channel listing. `mine` in the key, because the read-back of one
/// broadcast by id is the same path with a different filter.
const LIST_BROADCASTS: &str = "GET /liveBroadcasts?part=id,snippet,status,contentDetails&mine=";
/// Reading one broadcast back after the metadata write.
const GET_BROADCAST: &str = "GET /liveBroadcasts?part=id,snippet,status,contentDetails&id=";

struct Harness {
    service: YoutubeService,
    log_dir: tempfile::TempDir,
}

impl Harness {
    /// A connected account, a saved stream key, saved metadata, and both
    /// Google endpoints pointed at local fakes.
    fn new(api: &Fake, token_endpoint: &Fake) -> Self {
        let db = Database::open_in_memory().unwrap();
        // The OAuth client, given to this service alone.
        //
        // Not `set_var`: the environment is process-global and cargo runs the
        // tests in this binary on several threads at once, so one test's write
        // races every other test's read. macOS is where that showed — two of
        // these failed there with the refresh step erroring while six passed,
        // which is exactly the shape of a racing `setenv`. The developer-mode
        // override is per-service state and needs no such coordination.
        db.set_setting(louver_core::settings_keys::DEVELOPER_MODE, "true").unwrap();
        db.set_setting(keys::CLIENT_ID, "test-client.apps.googleusercontent.com").unwrap();
        db.set_setting(keys::API_BASE, &api.base).unwrap();
        db.set_setting(keys::TOKEN_ENDPOINT, &format!("{}/token", token_endpoint.base)).unwrap();
        db.set_setting(keys::APPLY_ON_START, "true").unwrap();
        db.set_setting(keys::METADATA_TITLE, "COLORIST 24시간 편집샵 느낌 플레이리스트").unwrap();
        db.set_setting(keys::METADATA_DESCRIPTION, "24시간 편집샵 플레이리스트").unwrap();
        db.set_setting(keys::METADATA_TAGS, "lofi\njazz").unwrap();
        db.set_setting(keys::METADATA_CATEGORY, "10").unwrap();
        db.set_setting(keys::METADATA_PRIVACY, "unlisted").unwrap();

        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::default());
        let keystore = Arc::new(StreamKeyStore::new(Arc::clone(&secrets)));
        keystore.set(STREAM_KEY).unwrap();
        // Connected, the way a user who finished consent weeks ago is.
        let tokens = TokenStore::new(Arc::clone(&secrets));
        tokens.save_refresh_token(REFRESH_TOKEN).unwrap();
        tokens.save_client_secret(CLIENT_SECRET).unwrap();

        let log_dir = tempfile::tempdir().unwrap();
        let logger = Logger::new(log_dir.path()).unwrap();
        let service = YoutubeService::new(db, secrets, logger, keystore);
        Self { service, log_dir }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.log_dir.path().join("app.log")).unwrap_or_default()
    }

    /// A scheduled window starting now and ending in ten minutes.
    fn prepare_scheduled(&self) -> louver_core::error::Result<()> {
        let start = chrono::Utc::now();
        self.service
            .prepare_for_broadcast_reason(true, Some((start, Some(start + chrono::Duration::minutes(10)))))
    }
}

/// Every value that must never appear in a log line.
fn assert_no_credentials(log: &str) {
    for secret in [ACCESS_TOKEN, REFRESH_TOKEN, CLIENT_SECRET, STREAM_KEY] {
        assert!(!log.contains(secret), "a credential reached the log:\n{log}");
    }
}

#[test]
fn a_scheduled_start_logs_every_step_it_takes() {
    // The channel is empty, which is the normal case for a scheduled window:
    // list finds nothing, insert makes one, the saved key picks the endpoint,
    // bind attaches them, and the metadata goes on before FFmpeg is asked for.
    let api = Fake::start(vec![
        (LIST_BROADCASTS, 200, r#"{"items":[]}"#.into()),
        (
            "POST /liveBroadcasts/bind",
            200,
            r#"{"id":"b-new","snippet":{"title":"COLORIST 24시간 편집샵 느낌 플레이리스트"},
                "status":{"privacyStatus":"unlisted","lifeCycleStatus":"ready"},
                "contentDetails":{"boundStreamId":"s-mine","enableAutoStart":true}}"#
                .into(),
        ),
        (
            "POST /liveBroadcasts?",
            200,
            r#"{"id":"b-new","snippet":{"title":"COLORIST 24시간 편집샵 느낌 플레이리스트",
                "scheduledStartTime":"2026-09-20T12:59:00Z"},
                "status":{"privacyStatus":"unlisted","lifeCycleStatus":"created"},
                "contentDetails":{"enableAutoStart":true,"enableAutoStop":true}}"#
                .into(),
        ),
        (
            "GET /liveStreams",
            200,
            r#"{"items":[
                {"id":"s-other","snippet":{"title":"Old"},"status":{"streamStatus":"inactive"},
                 "cdn":{"ingestionInfo":{"streamName":"wrong-key-0000"}}},
                {"id":"s-mine","snippet":{"title":"Main"},"status":{"streamStatus":"inactive"},
                 "cdn":{"ingestionInfo":{"streamName":"abcd-efgh-ijkl-mnop"}}}]}"#
                .into(),
        ),
        ("PUT /liveBroadcasts", 200, r#"{"id":"b-new","status":{"privacyStatus":"unlisted"}}"#.into()),
        ("PUT /videos", 200, r#"{"id":"b-new"}"#.into()),
        (
            "GET /videos",
            200,
            r#"{"items":[{"id":"b-new","snippet":{"title":"COLORIST 24시간 편집샵 느낌 플레이리스트",
                "description":"24시간 편집샵 플레이리스트","tags":["lofi","jazz"],"categoryId":"10"}}]}"#
                .into(),
        ),
    ]);
    let tokens = Fake::start(vec![("POST /token", 200, TOKEN_OK.into())]);
    let h = Harness::new(&api, &tokens);

    h.prepare_scheduled().expect("the whole preparation should succeed against this channel");

    let log = h.log();
    for line in [
        "YOUTUBE_PREPARE_START: origin=scheduled",
        "YOUTUBE_TOKEN_REFRESH_START: origin=scheduled",
        "YOUTUBE_TOKEN_REFRESH_OK: origin=scheduled",
        "YOUTUBE_BROADCAST_LIST_START",
        "YOUTUBE_BROADCAST_LIST_OK",
        "YOUTUBE_BROADCAST_INSERT_START",
        "YOUTUBE_BROADCAST_INSERT_OK",
        "YOUTUBE_BROADCAST_CREATED",
        "YOUTUBE_STREAM_LIST_START",
        "YOUTUBE_STREAM_LIST_OK",
        "YOUTUBE_BROADCAST_BIND_START",
        "YOUTUBE_BROADCAST_BIND_OK",
        "YOUTUBE_BROADCAST_BOUND",
        "YOUTUBE_METADATA_APPLY_START",
        "YOUTUBE_METADATA_APPLY_OK",
    ] {
        assert!(log.contains(line), "missing {line} in:\n{log}");
    }
    // The refresh really happened — the fake token endpoint was asked, with
    // the client secret this build carries.
    let token_request = tokens.seen().join("\n");
    assert!(token_request.contains("grant_type=refresh_token"), "{token_request}");
    assert!(token_request.contains("client_secret="), "{token_request}");
    // A token that was minted rather than reused, and a bind that landed where
    // the saved key points.
    assert!(log.contains("새 토큰 발급"), "{log}");
    assert!(log.contains("boundStreamId=s-mine"), "{log}");

    assert_eq!(h.service.apply_state().stage, ApplyStage::Applied);
    assert_no_credentials(&log);

    // The key was matched in memory and never sent.
    for r in api.seen() {
        assert!(!r.contains(STREAM_KEY), "the stream key reached a request: {r}");
    }
}

#[test]
fn a_refused_insert_names_the_request_google_refused() {
    // What the Mac log should have said. `liveStreamingNotEnabled` is a real
    // Google reason and a real remedy — it is the channel, not the app.
    let api = Fake::start(vec![
        (LIST_BROADCASTS, 200, r#"{"items":[]}"#.into()),
        (
            "POST /liveBroadcasts?",
            403,
            r#"{"error":{"code":403,"message":"The user is not enabled for live streaming.",
                "errors":[{"reason":"liveStreamingNotEnabled"}]}}"#
                .into(),
        ),
    ]);
    let tokens = Fake::start(vec![("POST /token", 200, TOKEN_OK.into())]);
    let h = Harness::new(&api, &tokens);

    let err = h.prepare_scheduled().expect_err("a 403 on the insert is not a success");
    assert_eq!(err.code_str, "LL-YOUTUBE-004");

    let log = h.log();
    assert!(log.contains("YOUTUBE_BROADCAST_INSERT_FAIL"), "{log}");
    assert!(log.contains("liveBroadcasts.insert HTTP 403"), "{log}");
    assert!(log.contains("reason=liveStreamingNotEnabled"), "{log}");
    assert!(log.contains("The user is not enabled for live streaming."), "{log}");
    // It got past the list, so the reader knows that much worked.
    assert!(log.contains("YOUTUBE_BROADCAST_LIST_OK"), "{log}");
    assert!(log.contains("YOUTUBE_PREPARE_FAIL: origin=scheduled · 예약 방송 생성 실패"), "{log}");

    // And the screen says the stage, not the sentence every failure shares.
    let state = h.service.apply_state();
    assert_eq!(state.stage, ApplyStage::Failed);
    assert_eq!(state.failed_stage.as_deref(), Some("예약 방송 생성 실패"));
    assert_eq!(state.origin.as_deref(), Some("scheduled"));
    assert!(state.steps.iter().any(|s| s.error_code.as_deref() == Some("LL-YOUTUBE-004")));
    assert_no_credentials(&log);
}

#[test]
fn a_stale_login_fails_as_a_refresh_rather_than_as_a_youtube_api_error() {
    // The case the real Mac evidence points at: the manual broadcast an hour
    // earlier was still using the access token consent had just minted, so it
    // proved nothing about the refresh. Here the refresh is refused, and the
    // report must not send the reader looking at YouTube calls that were never
    // made.
    let api = Fake::start(vec![]);
    let tokens = Fake::start(vec![(
        "POST /token",
        400,
        r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#.into(),
    )]);
    let h = Harness::new(&api, &tokens);

    let err = h.prepare_scheduled().expect_err("a revoked login is not a success");
    assert_eq!(err.code_str, "LL-YOUTUBE-AUTH-REFRESH");

    let log = h.log();
    assert!(log.contains("YOUTUBE_TOKEN_REFRESH_FAIL"), "{log}");
    assert!(log.contains("oauth2.token(refresh_token)"), "{log}");
    assert!(log.contains("invalid_grant"), "{log}");
    assert!(log.contains("Token has been expired or revoked."), "{log}");

    // Nothing was asked of the YouTube API, and the log does not imply it was.
    assert!(api.seen().is_empty(), "the API was called with no token: {:?}", api.seen());
    assert!(!log.contains("YOUTUBE_BROADCAST_LIST_START"), "{log}");

    let state = h.service.apply_state();
    assert_eq!(state.failed_stage.as_deref(), Some("Google 인증 갱신 실패"));
    assert!(state.failed_remedy.as_deref().unwrap().contains("다시 연결"));
    assert_no_credentials(&log);
}

/// The channel's ingestion endpoints, one of which the saved key publishes to.
const MY_STREAMS: &str = r#"{"items":[
    {"id":"s-other","snippet":{"title":"Old"},"status":{"streamStatus":"inactive"},
     "cdn":{"ingestionInfo":{"streamName":"wrong-key-0000"}}},
    {"id":"s-mine","snippet":{"title":"Main"},"status":{"streamStatus":"inactive"},
     "cdn":{"ingestionInfo":{"streamName":"abcd-efgh-ijkl-mnop"}}}]}"#;

/// The three metadata calls, answering with what was asked for.
fn metadata_routes(id: &str) -> Vec<(&'static str, u16, String)> {
    vec![
        (
            GET_BROADCAST,
            200,
            format!(
                r#"{{"items":[{{"id":"{id}","snippet":{{"title":"t"}},
                "status":{{"privacyStatus":"unlisted","lifeCycleStatus":"ready"}},
                "contentDetails":{{"boundStreamId":"s-mine"}}}}]}}"#
            ),
        ),
        ("PUT /liveBroadcasts", 200, format!(r#"{{"id":"{id}","status":{{"privacyStatus":"unlisted"}}}}"#)),
        ("PUT /videos", 200, format!(r#"{{"id":"{id}"}}"#)),
        (
            "GET /videos",
            200,
            format!(
                r#"{{"items":[{{"id":"{id}","snippet":{{"title":"COLORIST 24시간 편집샵 느낌 플레이리스트",
                "description":"24시간 편집샵 플레이리스트","tags":["lofi","jazz"],"categoryId":"10"}}}}]}}"#
            ),
        ),
    ]
}

#[test]
fn the_list_that_real_google_refused_is_never_sent_again() {
    // The confirmed cause, asserted on the wire. `liveBroadcasts.list` takes
    // exactly one of `id`, `mine` and `broadcastStatus`; the app was sending
    // `mine=true` and `broadcastStatus=upcoming` together, and real Google
    // answered 400 incompatibleParameters before the channel was ever read.
    let api = Fake::start(
        vec![
            (LIST_BROADCASTS, 200, r#"{"items":[]}"#.into()),
            (
                "POST /liveBroadcasts?",
                200,
                r#"{"id":"b-new","snippet":{"title":"t"},
            "status":{"privacyStatus":"unlisted","lifeCycleStatus":"created"},
            "contentDetails":{"enableAutoStart":true}}"#
                    .into(),
            ),
            ("GET /liveStreams", 200, MY_STREAMS.into()),
            (
                "POST /liveBroadcasts/bind",
                200,
                r#"{"id":"b-new","snippet":{"title":"t"},
            "status":{"privacyStatus":"unlisted","lifeCycleStatus":"ready"},
            "contentDetails":{"boundStreamId":"s-mine","enableAutoStart":true}}"#
                    .into(),
            ),
        ]
        .into_iter()
        .chain(metadata_routes("b-new"))
        .collect(),
    );
    let tokens = Fake::start(vec![("POST /token", 200, TOKEN_OK.into())]);
    let h = Harness::new(&api, &tokens);

    h.prepare_scheduled().expect("the preparation should get all the way through");

    let listings: Vec<String> =
        api.seen().into_iter().filter(|r| r.starts_with("GET /liveBroadcasts")).collect();
    assert!(!listings.is_empty(), "the channel was never listed");
    for r in &listings {
        let filters = ["&id=", "mine=", "broadcastStatus="].iter().filter(|f| r.contains(**f)).count();
        assert_eq!(filters, 1, "exactly one filter is allowed: {r}");
        assert!(!r.contains("broadcastStatus"), "{r}");
    }
}

#[test]
fn a_mixed_channel_yields_this_windows_broadcast_and_no_other() {
    // §4: never borrow an unrelated upcoming broadcast. The list is unfiltered
    // now, so the channel's whole history arrives — and only the broadcast
    // scheduled for this window may be taken over.
    let window_start = chrono::Utc::now();
    let mixed = format!(
        r#"{{"items":[
          {{"id":"b-done","snippet":{{"title":"어제","scheduledStartTime":"{}"}},
           "status":{{"privacyStatus":"public","lifeCycleStatus":"complete"}}}},
          {{"id":"b-other","snippet":{{"title":"내일 아침","scheduledStartTime":"{}"}},
           "status":{{"privacyStatus":"public","lifeCycleStatus":"ready"}}}},
          {{"id":"b-mine","snippet":{{"title":"오늘 밤","scheduledStartTime":"{}"}},
           "status":{{"privacyStatus":"unlisted","lifeCycleStatus":"ready"}},
           "contentDetails":{{"boundStreamId":"s-mine"}}}}
        ]}}"#,
        (window_start - chrono::Duration::days(1)).to_rfc3339(),
        (window_start + chrono::Duration::hours(9)).to_rfc3339(),
        window_start.to_rfc3339(),
    );
    let api = Fake::start(
        vec![(LIST_BROADCASTS, 200, mixed), ("GET /liveStreams", 200, MY_STREAMS.into())]
            .into_iter()
            .chain(metadata_routes("b-mine"))
            .collect(),
    );
    let tokens = Fake::start(vec![("POST /token", 200, TOKEN_OK.into())]);
    let h = Harness::new(&api, &tokens);

    h.prepare_scheduled().expect("this window's broadcast is right there");

    let log = h.log();
    // §6: the sequence a reused broadcast should produce.
    assert!(log.contains("YOUTUBE_BROADCAST_LIST_OK"), "{log}");
    assert!(log.contains("YOUTUBE_BROADCAST_REUSED: 이 예약의 방송을 다시 사용합니다 (b-mine)"), "{log}");
    assert!(log.contains("YOUTUBE_BROADCAST_INSERT_SKIP"), "{log}");
    assert!(log.contains("YOUTUBE_STREAM_LIST_OK"), "{log}");
    assert!(log.contains("YOUTUBE_METADATA_APPLY_OK"), "{log}");

    // Nothing was created — the user's other broadcast was left alone, and no
    // second one was made for a window that already had one.
    assert!(
        !api.seen().iter().any(|r| r.starts_with("POST /liveBroadcasts?")),
        "a broadcast was created even though this window already had one"
    );
    assert_eq!(h.service.provisioned().unwrap().broadcast_id, "b-mine");
    assert_no_credentials(&log);
}

#[test]
fn a_window_with_nothing_prepared_for_it_creates_rather_than_borrowing() {
    // The same channel, a window nine hours before the only other broadcast.
    // Taking that one over would rename the user's broadcast and stream into
    // it, so it is left alone and a new one is made.
    let window_start = chrono::Utc::now();
    let elsewhere = format!(
        r#"{{"items":[{{"id":"b-other","snippet":{{"title":"내일 아침","scheduledStartTime":"{}"}},
           "status":{{"privacyStatus":"public","lifeCycleStatus":"ready"}}}}]}}"#,
        (window_start + chrono::Duration::hours(9)).to_rfc3339(),
    );
    let api = Fake::start(
        vec![
            (LIST_BROADCASTS, 200, elsewhere),
            (
                "POST /liveBroadcasts?",
                200,
                r#"{"id":"b-new","snippet":{"title":"t"},
                "status":{"privacyStatus":"unlisted","lifeCycleStatus":"created"},
                "contentDetails":{"enableAutoStart":true}}"#
                    .into(),
            ),
            ("GET /liveStreams", 200, MY_STREAMS.into()),
            (
                "POST /liveBroadcasts/bind",
                200,
                r#"{"id":"b-new","snippet":{"title":"t"},
                "status":{"privacyStatus":"unlisted","lifeCycleStatus":"ready"},
                "contentDetails":{"boundStreamId":"s-mine","enableAutoStart":true}}"#
                    .into(),
            ),
        ]
        .into_iter()
        .chain(metadata_routes("b-new"))
        .collect(),
    );
    let tokens = Fake::start(vec![("POST /token", 200, TOKEN_OK.into())]);
    let h = Harness::new(&api, &tokens);

    h.prepare_scheduled().unwrap();

    let log = h.log();
    assert!(log.contains("YOUTUBE_BROADCAST_INSERT_OK"), "{log}");
    assert!(log.contains("YOUTUBE_BROADCAST_CREATED"), "{log}");
    assert!(
        !log.contains("b-other"),
        "the user's own broadcast was touched:
{log}"
    );
    assert_eq!(h.service.provisioned().unwrap().broadcast_id, "b-new");
}

#[test]
fn the_parameter_refusal_reads_as_a_listing_failure_not_a_lost_connection() {
    // §7. The account is connected and the token was refreshed one line
    // earlier; telling the user YouTube could not be reached sends them to
    // check their internet and reconnect, neither of which is the problem.
    let api = Fake::start(vec![(
        LIST_BROADCASTS,
        400,
        r#"{"error":{"code":400,
            "message":"Incompatible parameters specified in the request: broadcastStatus, mine",
            "errors":[{"reason":"incompatibleParameters"}]}}"#
            .into(),
    )]);
    let tokens = Fake::start(vec![("POST /token", 200, TOKEN_OK.into())]);
    let h = Harness::new(&api, &tokens);

    let err = h.prepare_scheduled().unwrap_err();
    assert_eq!(err.message, "예약 방송 정보를 조회하지 못했습니다.");
    assert!(!err.message.contains("연결하지 못했습니다"));
    assert!(err.detail.unwrap().contains("reason=incompatibleParameters"));

    let log = h.log();
    assert!(log.contains("YOUTUBE_TOKEN_REFRESH_OK"), "{log}");
    assert!(log.contains("YOUTUBE_BROADCAST_LIST_FAIL"), "{log}");
    assert_eq!(h.service.apply_state().failed_stage.as_deref(), Some("예약 방송 목록 조회 실패"));
}

#[test]
fn a_manual_start_and_a_scheduled_start_take_the_same_steps() {
    // §4 of the report: run both in one session and compare. They share one
    // pipeline, so the sequences must be identical apart from the label —
    // if a real Mac shows only the scheduled one failing, the difference is
    // in the values, not in the path.
    // Scheduled for the window being started, so it is reused rather than
    // duplicated — which is what puts the failure on `liveStreams.list`.
    let window_start = chrono::Utc::now();
    let routes = move || {
        vec![
            (
                LIST_BROADCASTS,
                200,
                format!(
                    r#"{{"items":[{{"id":"b-1","snippet":{{"title":"t","scheduledStartTime":"{}"}},
                    "status":{{"privacyStatus":"unlisted","lifeCycleStatus":"ready"}},
                    "contentDetails":{{"boundStreamId":"s-mine"}}}}]}}"#,
                    window_start.to_rfc3339()
                ),
            ),
            (
                "GET /liveStreams",
                403,
                r#"{"error":{"code":403,"message":"Request had insufficient authentication scopes.",
                    "errors":[{"reason":"insufficientPermissions"}]}}"#
                    .into(),
            ),
        ]
    };
    let steps = |scheduled: bool| {
        let api = Fake::start(routes());
        let tokens = Fake::start(vec![("POST /token", 200, TOKEN_OK.into())]);
        let h = Harness::new(&api, &tokens);
        let _ = h.service.prepare_for_broadcast_reason(scheduled, Some((window_start, None)));
        let state = h.service.apply_state();
        (
            state.steps.iter().map(|s| (s.step, s.outcome)).collect::<Vec<_>>(),
            state.failed_stage.clone(),
            h.log(),
        )
    };

    let (manual_steps, manual_stage, manual_log) = steps(false);
    let (scheduled_steps, scheduled_stage, scheduled_log) = steps(true);

    assert_eq!(manual_steps, scheduled_steps, "the two starts must go through the same steps");
    assert_eq!(manual_stage.as_deref(), Some("스트림 연결 실패"));
    assert_eq!(scheduled_stage, manual_stage);
    // Only the label differs, which is what makes them comparable in one log.
    assert!(manual_log.contains("origin=manual"), "{manual_log}");
    assert!(scheduled_log.contains("origin=scheduled"), "{scheduled_log}");
    assert!(scheduled_log.contains("liveStreams.list HTTP 403 reason=insufficientPermissions"));
    // The broadcast for this window was reused, so no second one was created.
    assert!(scheduled_log.contains("YOUTUBE_BROADCAST_INSERT_SKIP"), "{scheduled_log}");
}
