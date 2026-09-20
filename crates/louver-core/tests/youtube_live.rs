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
fn updating_tags_preserves_everything_else_on_the_video() {
    // The rule §3 is explicit about: read the resource, merge, then write.
    // Asserted on the bytes that actually went over the socket.
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
    // The fields that would have been destroyed by a naive update:
    assert_eq!(sent["title"], "ROOM. 24/7");
    assert_eq!(sent["description"], "original description");
    assert_eq!(sent["categoryId"], "10");
    assert_eq!(sent["defaultLanguage"], "ko");
    assert_eq!(sent["channelId"], "UC-room");
    assert!(sent["thumbnails"].is_object(), "thumbnails survived");
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
