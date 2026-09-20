//! The YouTube client, driven end to end against a stand-in for Google.
//!
//! Real YouTube needs an account, a consent screen and a live broadcast, so it
//! cannot run here — but everything between the button and the socket can. The
//! server below speaks the same JSON Google does, including its failure
//! shapes, and records what it was sent, which is how the merge rule in §3 is
//! checked: not by reading the code, but by looking at the request.

use louver_core::error::Result;
use louver_core::youtube::api::{HttpClient, YoutubeApi};
use louver_core::youtube::metadata::{BroadcastMetadata, Privacy};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// One request the fake received.
#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    path: String,
    authorization: String,
    body: serde_json::Value,
}

/// A local stand-in for the YouTube Data API.
struct FakeYoutube {
    base: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl FakeYoutube {
    /// `responses` maps "METHOD /path-prefix" to (status, body).
    fn start(responses: Vec<(&'static str, u16, String)>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let recorded = Arc::clone(&requests);
        let stopping = Arc::clone(&stop);
        std::thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !stopping.load(std::sync::atomic::Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut reader = BufReader::new(stream.try_clone().unwrap());
                        let mut request_line = String::new();
                        if reader.read_line(&mut request_line).is_err() {
                            continue;
                        }
                        let mut parts = request_line.split_whitespace();
                        let method = parts.next().unwrap_or("").to_string();
                        let path = parts.next().unwrap_or("").to_string();

                        let mut length = 0usize;
                        let mut authorization = String::new();
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                                break;
                            }
                            let lower = line.to_ascii_lowercase();
                            if let Some(v) = lower.strip_prefix("content-length:") {
                                length = v.trim().parse().unwrap_or(0);
                            }
                            if lower.starts_with("authorization:") {
                                authorization = line[14..].trim().to_string();
                            }
                        }
                        let mut body = vec![0u8; length];
                        if length > 0 {
                            let _ = reader.read_exact(&mut body);
                        }
                        let parsed: serde_json::Value =
                            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);

                        recorded.lock().unwrap().push(Recorded {
                            method: method.clone(),
                            path: path.clone(),
                            authorization,
                            body: parsed,
                        });

                        let key = format!("{method} {}", path.split('?').next().unwrap_or(""));
                        let (status, payload) = responses
                            .iter()
                            .find(|(k, _, _)| key.starts_with(*k))
                            .map(|(_, s, b)| (*s, b.clone()))
                            .unwrap_or((404, r#"{"error":{"message":"no stub"}}"#.to_string()));

                        let response = format!(
                            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                            payload.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self { base: format!("http://127.0.0.1:{port}"), requests, stop }
    }

    fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for FakeYoutube {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The real transport, pointed at the fake.
fn client() -> louver_core::youtube::http::UreqClient {
    louver_core::youtube::http::UreqClient::new()
}

fn broadcast_list_json(chat_id: Option<&str>, status: &str) -> String {
    let chat = chat_id.map(|c| format!(r#","liveChatId":"{c}""#)).unwrap_or_default();
    format!(
        r#"{{"items":[{{"id":"bcast-1","snippet":{{"title":"ROOM. 24/7"{chat}}},
        "status":{{"privacyStatus":"unlisted","lifeCycleStatus":"{status}"}}}}]}}"#
    )
}

/// The video as it exists before Louver Live touches it.
const EXISTING_VIDEO: &str = r#"{"items":[{"id":"bcast-1","snippet":{
    "title":"ROOM. 24/7","description":"original description","categoryId":"10",
    "defaultLanguage":"ko","channelId":"UC-room","tags":["old-tag"],
    "thumbnails":{"default":{"url":"https://example/x.jpg"}}}}]}"#;

fn meta() -> BroadcastMetadata {
    BroadcastMetadata {
        title: "PLAYLIST for your room | lofi, chill, jazz mood".into(),
        description: "lofi all night\nfrom ROOM.".into(),
        tags: vec!["lofi".into(), "jazz".into(), "chill".into()],
        category_id: "10".into(),
        privacy: Privacy::Public,
    }
}

#[test]
fn the_channel_is_read_with_the_bearer_token() {
    let fake = FakeYoutube::start(vec![(
        "GET /channels",
        200,
        r#"{"items":[{"id":"UC-room","snippet":{"title":"ROOM."}}]}"#.into(),
    )]);
    let http = client();
    let api = YoutubeApi::with_base(&http, fake.base.clone());

    let ch = api.my_channel("token-123").unwrap();
    assert_eq!(ch.title, "ROOM.");
    assert_eq!(ch.id, "UC-room");

    let req = &fake.requests()[0];
    assert_eq!(req.authorization, "Bearer token-123");
    assert!(req.path.contains("mine=true"));
}

#[test]
fn the_video_write_carries_the_new_title_and_keeps_what_the_user_did_not_choose() {
    // Two rules at once, both checked on the bytes that went over the socket.
    //
    // §3: read the resource, merge, then write — so `defaultLanguage`,
    // `channelId` and the thumbnails survive a tag change.
    //
    // And the one that cost a real broadcast: the *video* is what the watch
    // page and YouTube Studio show, and this write replaces its whole snippet.
    // Carrying the title through from the GET meant writing the channel's
    // default stream title — "Playlist" — back over the title that had just
    // been set on the broadcast.
    let fake = FakeYoutube::start(vec![
        ("GET /videos", 200, EXISTING_VIDEO.into()),
        ("PUT /videos", 200, r#"{"id":"bcast-1"}"#.into()),
    ]);
    let http = client();
    let api = YoutubeApi::with_base(&http, fake.base.clone());

    api.update_video_snippet("tok", "bcast-1", &meta()).unwrap();

    let reqs = fake.requests();
    assert_eq!(reqs.len(), 2, "it must read before it writes");
    assert_eq!(reqs[0].method, "GET");
    assert_eq!(reqs[1].method, "PUT");

    let sent = &reqs[1].body["snippet"];
    assert_eq!(sent["tags"], serde_json::json!(["lofi", "jazz", "chill"]));
    // What the user asked for:
    assert_eq!(sent["title"], meta().title);
    assert_ne!(sent["title"], "ROOM. 24/7", "the stale title must not be written back");
    assert_eq!(sent["description"], meta().description);
    assert_eq!(sent["categoryId"], "10");
    // What the user never touched, and would have lost to a naive update:
    assert_eq!(sent["defaultLanguage"], "ko");
    assert_eq!(sent["channelId"], "UC-room");
    assert!(sent["thumbnails"].is_object(), "thumbnails survived");
}

#[test]
fn update_video_tags_alone_still_leaves_the_title_where_it_was() {
    // The tags-only entry point is a different promise from the full apply:
    // it changes tags and nothing else.
    let fake = FakeYoutube::start(vec![
        ("GET /videos", 200, EXISTING_VIDEO.into()),
        ("PUT /videos", 200, r#"{"id":"bcast-1"}"#.into()),
    ]);
    let http = client();
    let api = YoutubeApi::with_base(&http, fake.base.clone());

    api.update_video_tags("tok", "bcast-1", &["lofi".into()]).unwrap();

    let put = fake.requests().into_iter().find(|r| r.method == "PUT").unwrap();
    assert_eq!(put.body["snippet"]["tags"], serde_json::json!(["lofi"]));
    assert_eq!(put.body["snippet"]["title"], "ROOM. 24/7");
}

#[test]
fn a_two_hundred_is_not_evidence_that_the_title_changed() {
    use louver_core::youtube::api::verify_metadata;

    // Exactly the reported failure: every call succeeded, and the watch page
    // still said "Playlist". Read-back is the only thing that catches it.
    let still_default = serde_json::json!({
        "title": "Playlist",
        "description": meta().description,
        "categoryId": "10",
        "tags": ["lofi", "jazz", "chill"],
    });
    let v = verify_metadata(&meta(), &still_default, "public");
    assert!(!v.all_applied());
    assert_eq!(v.mismatches(), vec!["제목"]);
    assert_eq!(v.title.actual, "Playlist");
    assert!(v.description.applied && v.tags.applied && v.category.applied && v.privacy.applied);
}

#[test]
fn read_back_passes_when_google_agrees_with_what_was_asked_for() {
    use louver_core::youtube::api::verify_metadata;
    let m = meta();
    let applied = serde_json::json!({
        "title": m.title,
        "description": m.description,
        // YouTube is free to reorder tags, and that is not a failure.
        "tags": ["jazz", "chill", "lofi"],
        "categoryId": m.category_id,
    });
    let v = verify_metadata(&m, &applied, "public");
    assert!(v.all_applied(), "mismatches: {:?}", v.mismatches());
    assert!(v.mismatches().is_empty());
}

#[test]
fn read_back_names_every_field_that_did_not_take() {
    use louver_core::youtube::api::verify_metadata;
    let wrong = serde_json::json!({
        "title": "Playlist",
        "description": "",
        "tags": [],
        "categoryId": "22",
    });
    let v = verify_metadata(&meta(), &wrong, "private");
    assert_eq!(v.mismatches(), vec!["제목", "설명", "태그", "카테고리", "공개범위"]);
    assert_eq!(v.privacy.actual, "private");
}

#[test]
fn the_broadcast_update_carries_title_description_and_privacy() {
    let fake = FakeYoutube::start(vec![
        ("GET /liveBroadcasts", 200, broadcast_list_json(None, "ready")),
        ("PUT /liveBroadcasts", 200, r#"{"id":"bcast-1"}"#.into()),
    ]);
    let http = client();
    let api = YoutubeApi::with_base(&http, fake.base.clone());

    let b = api.active_broadcast("tok").unwrap();
    api.update_broadcast("tok", &b.id, &meta(), None).unwrap();

    let put = fake.requests().into_iter().find(|r| r.method == "PUT").unwrap();
    assert_eq!(put.body["id"], "bcast-1");
    assert_eq!(put.body["snippet"]["title"], meta().title);
    assert_eq!(put.body["snippet"]["description"], meta().description);
    assert_eq!(put.body["status"]["privacyStatus"], "public");
}

#[test]
fn a_chat_message_is_posted_to_the_broadcasts_own_chat() {
    let fake = FakeYoutube::start(vec![("POST /liveChat/messages", 200, r#"{"id":"msg-1"}"#.into())]);
    let http = client();
    let api = YoutubeApi::with_base(&http, fake.base.clone());

    api.send_chat_message("tok", "chat-abc", "🎧 구독해주세요").unwrap();

    let req = &fake.requests()[0];
    assert_eq!(req.body["snippet"]["liveChatId"], "chat-abc");
    assert_eq!(req.body["snippet"]["type"], "textMessageEvent");
    assert_eq!(req.body["snippet"]["textMessageDetails"]["messageText"], "🎧 구독해주세요");
}

#[test]
fn a_chat_id_appears_only_once_the_broadcast_is_live() {
    let fake = FakeYoutube::start(vec![("GET /liveBroadcasts", 200, broadcast_list_json(None, "ready"))]);
    let http = client();
    let api = YoutubeApi::with_base(&http, fake.base.clone());
    let before = api.active_broadcast("tok").unwrap();
    assert!(before.active_live_chat_id.is_none());
    assert!(!before.is_live());
    drop(fake);

    let fake = FakeYoutube::start(vec![(
        "GET /liveBroadcasts",
        200,
        broadcast_list_json(Some("chat-live"), "live"),
    )]);
    let http = client();
    let api = YoutubeApi::with_base(&http, fake.base.clone());
    let after = api.broadcast_by_id("tok", "bcast-1").unwrap();
    assert_eq!(after.active_live_chat_id.as_deref(), Some("chat-live"));
    assert!(after.is_live());
}

#[test]
fn googles_failures_arrive_as_codes_the_ui_can_explain() {
    let cases = [
        (403, "liveChatDisabled", "LL-CHAT-002"),
        (403, "liveChatEnded", "LL-CHAT-003"),
        (403, "rateLimitExceeded", "LL-CHAT-001"),
        (403, "quotaExceeded", "LL-YOUTUBE-005"),
        (401, "authError", "LL-YOUTUBE-002"),
    ];
    for (status, reason, expected) in cases {
        let body =
            format!(r#"{{"error":{{"code":{status},"message":"nope","errors":[{{"reason":"{reason}"}}]}}}}"#);
        let fake = FakeYoutube::start(vec![("POST /liveChat/messages", status, body)]);
        let http = client();
        let api = YoutubeApi::with_base(&http, fake.base.clone());
        let err = api.send_chat_message("tok", "chat", "hi").unwrap_err();
        assert_eq!(err.code_str, expected, "{reason} should map to {expected}");
    }
}

#[test]
fn no_broadcast_at_all_is_reported_as_such_rather_than_as_a_crash() {
    let fake = FakeYoutube::start(vec![("GET /liveBroadcasts", 200, r#"{"items":[]}"#.into())]);
    let http = client();
    let api = YoutubeApi::with_base(&http, fake.base.clone());
    let err = api.active_broadcast("tok").unwrap_err();
    assert_eq!(err.code_str, "LL-YOUTUBE-003");
}

/// A transport that fails, to prove a dead network is a bot problem and not a
/// broadcast problem.
#[derive(Debug)]
struct DeadNetwork;
impl HttpClient for DeadNetwork {
    fn request(&self, _: &str, _: &str, _: &str, _: Option<serde_json::Value>) -> Result<(u16, String)> {
        Err(louver_core::error::LouverError::with_detail(
            louver_core::error::ErrorCode::YoutubeApiFailed,
            "connection refused",
        ))
    }
}

#[test]
fn an_unreachable_google_is_an_error_value_not_a_panic() {
    let api = YoutubeApi::new(&DeadNetwork);
    let err = api.send_chat_message("tok", "chat", "hi").unwrap_err();
    assert_eq!(err.code_str, "LL-YOUTUBE-004");
}

// --- token exchange -------------------------------------------------------

/// A stand-in for Google's token endpoint that records the form it was sent.
struct FakeTokenEndpoint {
    url: String,
    bodies: Arc<Mutex<Vec<String>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

/// Decides the response from the request body the fake received.
type Decide = Box<dyn Fn(&str) -> (u16, String) + Send + Sync>;

impl FakeTokenEndpoint {
    fn start(status: u16, payload: &'static str) -> Self {
        Self::answering(Box::new(move |_| (status, payload.to_string())))
    }

    /// A token endpoint that behaves the way Google's does for this desktop
    /// client: no `client_secret`, no token.
    fn requiring_a_secret() -> Self {
        Self::answering(Box::new(|body: &str| {
            if body.contains("client_secret=") {
                (200, r#"{"access_token":"at","refresh_token":"rt","expires_in":3599}"#.to_string())
            } else {
                (
                    400,
                    r#"{"error":"invalid_request","error_description":"client_secret is missing."}"#
                        .to_string(),
                )
            }
        }))
    }

    fn answering(decide: Decide) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let recorded = Arc::clone(&bodies);
        let stopping = Arc::clone(&stop);
        std::thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !stopping.load(std::sync::atomic::Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut reader = BufReader::new(stream.try_clone().unwrap());
                        let mut line = String::new();
                        let _ = reader.read_line(&mut line);
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
                        recorded.lock().unwrap().push(String::from_utf8_lossy(&body).into_owned());

                        let (status, payload) = decide(&String::from_utf8_lossy(&body));
                        let response = format!(
                            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                            payload.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self { url: format!("http://127.0.0.1:{port}/token"), bodies, stop }
    }

    fn last_body(&self) -> String {
        self.bodies.lock().unwrap().last().cloned().unwrap_or_default()
    }
}

impl Drop for FakeTokenEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

fn token_client(endpoint: &FakeTokenEndpoint) -> louver_core::youtube::http::UreqClient {
    let mut c = louver_core::youtube::http::UreqClient::new();
    c.token_endpoint = Some(endpoint.url.clone());
    c
}

#[test]
fn a_build_with_no_secret_sends_no_secret_field() {
    use louver_core::youtube::oauth::TokenEndpoint;
    let fake =
        FakeTokenEndpoint::start(200, r#"{"access_token":"at","refresh_token":"rt","expires_in":3599}"#);
    let http = token_client(&fake);

    let creds = louver_core::youtube::oauth::ClientCredentials {
        client_id: "cid.apps.googleusercontent.com".into(),
        client_secret: String::new(),
    };
    let tok = http.exchange_code(&creds, "4/code", "http://127.0.0.1:9000", "verifier-123").unwrap();
    assert_eq!(tok.refresh_token.as_deref(), Some("rt"));

    let body = fake.last_body();
    assert!(body.contains("code_verifier=verifier-123"), "PKCE verifier must be sent: {body}");
    assert!(body.contains("grant_type=authorization_code"));
    // An empty `client_secret=` is a different request from one without the
    // field, and Google reads the two differently.
    assert!(!body.contains("client_secret"), "{body}");
}

#[test]
fn the_exchange_succeeds_against_an_endpoint_that_demands_the_secret() {
    use louver_core::youtube::oauth::TokenEndpoint;
    // Google's real answer for this desktop client, reproduced: the exchange
    // is refused without a `client_secret` and accepted with one. This is the
    // case that failed on a real Mac.
    let fake = FakeTokenEndpoint::requiring_a_secret();
    let http = token_client(&fake);

    let without = louver_core::youtube::oauth::ClientCredentials {
        client_id: "cid.apps.googleusercontent.com".into(),
        client_secret: String::new(),
    };
    let refused = http.exchange_code(&without, "4/code", "http://127.0.0.1:9000", "v").unwrap_err();
    assert_eq!(refused.code_str, "LL-YOUTUBE-002");
    // Google's own words, kept whole, because they are what says how to fix it.
    let detail = refused.detail.unwrap_or_default();
    assert!(detail.contains("invalid_request"), "{detail}");
    assert!(detail.contains("client_secret is missing"), "{detail}");

    let with = louver_core::youtube::oauth::ClientCredentials {
        client_id: "cid.apps.googleusercontent.com".into(),
        client_secret: "GOCSPX-testsecret".into(),
    };
    let tok = http.exchange_code(&with, "4/code", "http://127.0.0.1:9000", "verifier-123").unwrap();
    assert_eq!(tok.refresh_token.as_deref(), Some("rt"));

    let body = fake.last_body();
    assert!(body.contains("client_secret=GOCSPX-testsecret"), "{body}");
    // And PKCE is still carried; the secret is in addition to it, not instead.
    assert!(body.contains("code_verifier=verifier-123"), "{body}");
}

#[test]
fn a_refresh_against_the_same_endpoint_also_carries_the_secret() {
    use louver_core::youtube::oauth::TokenEndpoint;
    // A refresh that omits it fails hours later, when nobody is watching.
    let fake = FakeTokenEndpoint::requiring_a_secret();
    let http = token_client(&fake);
    let creds = louver_core::youtube::oauth::ClientCredentials {
        client_id: "cid.apps.googleusercontent.com".into(),
        client_secret: "GOCSPX-testsecret".into(),
    };
    http.refresh(&creds, "1//refresh-token").unwrap();
    assert!(fake.last_body().contains("client_secret=GOCSPX-testsecret"));
}

#[test]
fn a_secret_is_sent_only_when_the_build_actually_has_one() {
    use louver_core::youtube::oauth::TokenEndpoint;
    let fake = FakeTokenEndpoint::start(200, r#"{"access_token":"at","expires_in":3599}"#);
    let http = token_client(&fake);
    let creds = louver_core::youtube::oauth::ClientCredentials {
        client_id: "cid".into(),
        client_secret: "GOCSPX-xyz".into(),
    };
    http.exchange_code(&creds, "4/code", "http://127.0.0.1:9000", "v").unwrap();
    assert!(fake.last_body().contains("client_secret=GOCSPX-xyz"));
}

#[test]
fn a_refusal_is_reported_in_googles_own_words() {
    // This is the evidence that decides whether a secret is needed at all, so
    // none of it may be summarised away.
    use louver_core::youtube::oauth::TokenEndpoint;
    let fake = FakeTokenEndpoint::start(
        400,
        r#"{"error":"invalid_request","error_description":"client_secret is missing."}"#,
    );
    let http = token_client(&fake);
    let creds = louver_core::youtube::oauth::ClientCredentials {
        client_id: "cid".into(),
        client_secret: String::new(),
    };
    let err = http.exchange_code(&creds, "4/code", "http://127.0.0.1:9000", "v").unwrap_err();
    let detail = err.detail.unwrap();
    assert!(detail.contains("HTTP 400"), "{detail}");
    assert!(detail.contains("invalid_request"), "{detail}");
    assert!(detail.contains("client_secret is missing."), "{detail}");
}

// --- staying inside the free quota ------------------------------------------

use louver_core::youtube::quota::{
    ApiMethod, MeteredClient, QuotaGuard, QuotaState, FREE_DAILY_UNITS, RESERVE_UNITS,
};

fn metered(inner: Arc<dyn HttpClient>, guard: Arc<QuotaGuard>) -> MeteredClient {
    MeteredClient::new(inner, guard)
}

#[test]
fn every_call_is_charged_against_the_days_allowance() {
    let fake = FakeYoutube::start(vec![
        ("GET /videos", 200, EXISTING_VIDEO.into()),
        ("PUT /videos", 200, r#"{"id":"bcast-1"}"#.into()),
    ]);
    let guard = Arc::new(QuotaGuard::unlimited_for_tests());
    let http = metered(Arc::new(client()) as Arc<dyn HttpClient>, Arc::clone(&guard));
    let api = YoutubeApi::with_base(&http, fake.base.clone());

    api.update_video_snippet("tok", "bcast-1", &meta()).unwrap();

    // One read and one write, priced as such.
    assert_eq!(guard.snapshot().spent, 51);
}

#[test]
fn the_day_stops_before_the_allowance_runs_out() {
    let fake = FakeYoutube::start(vec![("GET /videos", 200, EXISTING_VIDEO.into())]);
    let nearly_spent = QuotaState {
        day: louver_core::youtube::quota::quota_day(chrono::Utc::now()),
        spent: FREE_DAILY_UNITS - RESERVE_UNITS,
        ..Default::default()
    };
    let guard = Arc::new(QuotaGuard::new(nearly_spent, FREE_DAILY_UNITS, Box::new(|_| {})));
    let http = metered(Arc::new(client()) as Arc<dyn HttpClient>, Arc::clone(&guard));
    let api = YoutubeApi::with_base(&http, fake.base.clone());

    let e = api.video_snippet("tok", "bcast-1").unwrap_err();
    assert_eq!(e.code_str, "LL-YOUTUBE-005");
    // And it never reached Google, so it cost nothing at all.
    assert!(fake.requests().is_empty(), "the refusal must happen before the socket");
}

#[test]
fn googles_own_refusal_closes_the_day_whatever_the_local_count_says() {
    let fake = FakeYoutube::start(vec![(
        "GET /videos",
        403,
        r#"{"error":{"code":403,"message":"quota","errors":[{"reason":"quotaExceeded"}]}}"#.into(),
    )]);
    let guard = Arc::new(QuotaGuard::unlimited_for_tests());
    let http = metered(Arc::new(client()) as Arc<dyn HttpClient>, Arc::clone(&guard));
    let api = YoutubeApi::with_base(&http, fake.base.clone());

    assert!(!guard.is_exhausted());
    let e = api.video_snippet("tok", "bcast-1").unwrap_err();
    assert_eq!(e.code_str, "LL-YOUTUBE-005");

    // The local estimate thought there was plenty left. Google's answer wins,
    // and nothing else is sent today.
    assert!(guard.is_exhausted());
    let second = api.video_snippet("tok", "bcast-1").unwrap_err();
    assert_eq!(second.code_str, "LL-YOUTUBE-005");
    assert_eq!(fake.requests().len(), 1, "the second call must not be attempted");
}

#[test]
fn an_ordinary_failure_does_not_close_the_day() {
    let fake = FakeYoutube::start(vec![(
        "GET /videos",
        500,
        r#"{"error":{"code":500,"message":"backend"}}"#.into(),
    )]);
    let guard = Arc::new(QuotaGuard::unlimited_for_tests());
    let http = metered(Arc::new(client()) as Arc<dyn HttpClient>, Arc::clone(&guard));
    let api = YoutubeApi::with_base(&http, fake.base.clone());

    api.video_snippet("tok", "bcast-1").unwrap_err();
    assert!(!guard.is_exhausted(), "a 500 is Google's problem, not the day's allowance");
}

#[test]
fn a_days_worth_of_chat_fits_inside_the_free_allowance() {
    // The heaviest thing this product does: a message every 20 minutes for a
    // whole day, on top of one metadata apply.
    let guard = Arc::new(QuotaGuard::unlimited_for_tests());
    for _ in 0..(24 * 3) {
        guard.try_spend(ApiMethod::LiveChatMessagesInsert).unwrap();
    }
    for m in [
        ApiMethod::LiveBroadcastsList,
        ApiMethod::LiveBroadcastsUpdate,
        ApiMethod::VideosList,
        ApiMethod::VideosUpdate,
    ] {
        guard.try_spend(m).unwrap();
    }
    let s = guard.snapshot();
    assert!(!s.exhausted);
    assert!(s.spent < FREE_DAILY_UNITS - RESERVE_UNITS, "spent {} units", s.spent);
}

// --- provisioning a broadcast for a scheduled window ------------------------

const NO_UPCOMING: &str = r#"{"items":[]}"#;

fn created_broadcast(id: &str, start: &str) -> String {
    format!(
        r#"{{"id":"{id}","snippet":{{"title":"COLORIST","scheduledStartTime":"{start}"}},
        "status":{{"privacyStatus":"unlisted","lifeCycleStatus":"created"}},
        "contentDetails":{{"enableAutoStart":true,"enableAutoStop":true}}}}"#
    )
}

const MY_STREAMS: &str = r#"{"items":[
  {"id":"s-other","snippet":{"title":"Old"},"status":{"streamStatus":"inactive"},
   "cdn":{"ingestionInfo":{"streamName":"wrong-key-0000"}}},
  {"id":"s-mine","snippet":{"title":"Main"},"status":{"streamStatus":"inactive"},
   "cdn":{"ingestionInfo":{"streamName":"abcd-efgh-ijkl-mnop"}}}
]}"#;

#[test]
fn an_empty_channel_gets_a_broadcast_created_and_bound() {
    // The reported failure, end to end: a scheduled window arrives, the
    // channel has nothing on it, and the app builds what it needs rather than
    // telling a sleeping user to open YouTube Studio.
    let bound = r#"{"id":"b-new","snippet":{"title":"COLORIST","scheduledStartTime":"2026-09-20T11:58:00Z"},
        "status":{"privacyStatus":"unlisted","lifeCycleStatus":"ready"},
        "contentDetails":{"boundStreamId":"s-mine","enableAutoStart":true}}"#;
    let fake = FakeYoutube::start(vec![
        ("GET /liveBroadcasts", 200, NO_UPCOMING.into()),
        ("POST /liveBroadcasts/bind", 200, bound.into()),
        ("POST /liveBroadcasts", 200, created_broadcast("b-new", "2026-09-20T11:58:00Z")),
        ("GET /liveStreams", 200, MY_STREAMS.into()),
    ]);
    let http = client();
    let api = YoutubeApi::with_base(&http, fake.base.clone());

    // 1. nothing upcoming
    let upcoming = api.upcoming_broadcasts("tok").unwrap();
    assert!(upcoming.is_empty());

    // 2. create it for this window
    let created = api
        .create_broadcast("tok", &meta(), "2026-09-20T11:58:00Z", Some("2026-09-20T12:00:00Z"), true)
        .unwrap();
    assert_eq!(created.id, "b-new");

    let insert = fake
        .requests()
        .into_iter()
        .find(|r| r.method == "POST" && r.path.contains("liveBroadcasts?"))
        .unwrap();
    assert_eq!(insert.body["snippet"]["title"], meta().title);
    assert_eq!(insert.body["snippet"]["scheduledStartTime"], "2026-09-20T11:58:00Z");
    assert_eq!(insert.body["snippet"]["scheduledEndTime"], "2026-09-20T12:00:00Z");
    assert_eq!(insert.body["status"]["privacyStatus"], "public");
    assert_eq!(insert.body["contentDetails"]["enableAutoStart"], true);
    assert_eq!(insert.body["contentDetails"]["enableAutoStop"], true);

    // 3. bind it to the endpoint the saved key publishes to — not the first
    //    stream, which would leave YouTube waiting for video forever.
    let streams = api.my_streams("tok").unwrap();
    let mine = louver_core::youtube::provision::stream_for_key(&streams, "abcd-efgh-ijkl-mnop").unwrap();
    assert_eq!(mine.id, "s-mine");

    let after = api.bind_broadcast("tok", &created.id, &mine.id).unwrap();
    louver_core::youtube::provision::verify_bound(&after, "s-mine").unwrap();

    let bind_req = fake.requests().into_iter().find(|r| r.path.contains("/bind")).unwrap();
    assert!(bind_req.path.contains("id=b-new"), "{}", bind_req.path);
    assert!(bind_req.path.contains("streamId=s-mine"), "{}", bind_req.path);
}

#[test]
fn the_stream_key_never_reaches_the_wire_as_a_query_or_a_body() {
    // The key is matched in memory. It must not turn up in a URL, a body or
    // anything the fake recorded.
    let fake = FakeYoutube::start(vec![("GET /liveStreams", 200, MY_STREAMS.into())]);
    let http = client();
    let api = YoutubeApi::with_base(&http, fake.base.clone());

    let streams = api.my_streams("tok").unwrap();
    assert_eq!(
        louver_core::youtube::provision::stream_for_key(&streams, "abcd-efgh-ijkl-mnop").unwrap().id,
        "s-mine"
    );

    for r in fake.requests() {
        assert!(!r.path.contains("abcd-efgh-ijkl-mnop"), "key in path: {}", r.path);
        assert!(!r.body.to_string().contains("abcd-efgh-ijkl-mnop"), "key in body");
    }
}

#[test]
fn a_broadcast_already_prepared_for_the_window_is_reused_not_duplicated() {
    // A retry inside the window must not leave two broadcasts on the channel.
    let existing = r#"{"items":[{"id":"b-existing",
        "snippet":{"title":"COLORIST","scheduledStartTime":"2026-09-20T11:58:00Z"},
        "status":{"privacyStatus":"unlisted","lifeCycleStatus":"ready"},
        "contentDetails":{"boundStreamId":"s-mine","enableAutoStart":true}}]}"#;
    let fake = FakeYoutube::start(vec![("GET /liveBroadcasts", 200, existing.into())]);
    let http = client();
    let api = YoutubeApi::with_base(&http, fake.base.clone());

    let upcoming = api.upcoming_broadcasts("tok").unwrap();
    let window =
        chrono::DateTime::parse_from_rfc3339("2026-09-20T11:59:30Z").unwrap().with_timezone(&chrono::Utc);
    match louver_core::youtube::provision::choose_broadcast(
        &upcoming,
        window,
        louver_core::youtube::provision::REUSE_TOLERANCE_SECS,
    ) {
        louver_core::youtube::provision::BroadcastChoice::Reuse(b) => {
            assert_eq!(b.id, "b-existing");
            // Already bound, so no second bind call is needed either.
            assert_eq!(b.bound_stream_id.as_deref(), Some("s-mine"));
        }
        other => panic!("should have reused: {other:?}"),
    }
    // And nothing was created.
    assert!(fake.requests().iter().all(|r| r.method == "GET"));
}

#[test]
fn the_transition_waits_for_the_stream_and_then_asks() {
    use louver_core::youtube::provision::{go_live_step, GoLive};
    let inactive = r#"{"items":[{"id":"s-mine","snippet":{"title":"Main"},
        "status":{"streamStatus":"inactive"},"cdn":{"ingestionInfo":{"streamName":"k"}}}]}"#;
    let fake = FakeYoutube::start(vec![
        ("GET /liveStreams", 200, inactive.into()),
        (
            "GET /liveBroadcasts",
            200,
            r#"{"items":[{"id":"b1","snippet":{},
            "status":{"privacyStatus":"unlisted","lifeCycleStatus":"ready"},
            "contentDetails":{"boundStreamId":"s-mine","enableAutoStart":false}}]}"#
                .into(),
        ),
    ]);
    let http = client();
    let api = YoutubeApi::with_base(&http, fake.base.clone());

    let b = api.broadcast_by_id("tok", "b1").unwrap();
    let s = api.stream_by_id("tok", "s-mine").unwrap();
    // FFmpeg has not connected yet: asking YouTube to go live now is refused
    // with an error that reads like a permissions problem.
    assert_eq!(go_live_step(&b, &s), GoLive::WaitForStream);
}

#[test]
fn the_transition_carries_the_broadcast_id_and_the_target_status() {
    let live = r#"{"id":"b1","snippet":{},"status":{"privacyStatus":"unlisted","lifeCycleStatus":"live"},
        "contentDetails":{"boundStreamId":"s-mine"}}"#;
    let fake = FakeYoutube::start(vec![("POST /liveBroadcasts/transition", 200, live.into())]);
    let http = client();
    let api = YoutubeApi::with_base(&http, fake.base.clone());

    let after = api.transition_broadcast("tok", "b1", "live").unwrap();
    assert_eq!(after.life_cycle_status, "live");

    let req = &fake.requests()[0];
    assert!(req.path.contains("id=b1"), "{}", req.path);
    assert!(req.path.contains("broadcastStatus=live"), "{}", req.path);
}

#[test]
fn ending_a_window_completes_the_broadcast() {
    let done = r#"{"id":"b1","snippet":{},"status":{"privacyStatus":"unlisted","lifeCycleStatus":"complete"},
        "contentDetails":{}}"#;
    let fake = FakeYoutube::start(vec![("POST /liveBroadcasts/transition", 200, done.into())]);
    let http = client();
    let api = YoutubeApi::with_base(&http, fake.base.clone());

    let after = api.transition_broadcast("tok", "b1", "complete").unwrap();
    assert_eq!(after.life_cycle_status, "complete");
    // A broadcast left `live` with nothing publishing to it is exactly the
    // stale state the next window would try to reuse.
    assert!(fake.requests()[0].path.contains("broadcastStatus=complete"));
}

#[test]
fn provisioning_costs_what_the_quota_table_says() {
    use louver_core::youtube::quota::ApiMethod;
    // The whole scheduled start, priced: list upcoming (1), insert (50),
    // list streams (1), bind (50), then the metadata apply. Worth knowing
    // because it runs on every scheduled window, every day.
    let per_start = ApiMethod::LiveBroadcastsList.units()
        + ApiMethod::LiveBroadcastsInsert.units()
        + ApiMethod::LiveStreamsList.units()
        + ApiMethod::LiveBroadcastsBind.units()
        + ApiMethod::LiveBroadcastsUpdate.units()
        + ApiMethod::VideosList.units() * 2
        + ApiMethod::VideosUpdate.units()
        + ApiMethod::LiveBroadcastsList.units()
        + ApiMethod::LiveBroadcastsTransition.units() * 2;
    assert_eq!(per_start, 305);
    // Even one scheduled start an hour, all day, stays inside the free
    // allowance with room for the chat bot.
    assert!(per_start * 24 < FREE_DAILY_UNITS - RESERVE_UNITS, "{}", per_start * 24);
}
