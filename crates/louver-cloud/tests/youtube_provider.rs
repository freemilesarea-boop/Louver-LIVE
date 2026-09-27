//! §18 A–T: the whole YouTube provider, against a fake Google.
//!
//! Nothing here reaches `googleapis.com`. Both halves of Google — the API host
//! and the token endpoint — are traits the provider is constructed with, and the
//! fakes below record every request they receive, so the assertions are about
//! what 247streams actually sent rather than about our own tables.
//!
//! The configuration is a value, not an environment variable. That is deliberate
//! and the reason these tests can run in parallel at all: `set_var` is
//! process-global and would race across the threads cargo uses.

use louver_cloud::db::NewItem;
use louver_cloud::youtube::{Config, Youtube};
use louver_cloud::{CloudDb, Privacy};
use louver_core::error::{ErrorCode, LouverError, Result as CoreResult};
use louver_core::security::{MemorySecretStore, SecretStore};
use louver_core::youtube::oauth::{ClientCredentials, TokenEndpoint, TokenResponse};
use louver_core::youtube::HttpClient;
use std::sync::{Arc, Mutex};

// --- a fake Google ---------------------------------------------------------

#[derive(Debug, Clone)]
struct Call {
    method: String,
    url: String,
    bearer: String,
    body: Option<serde_json::Value>,
}

/// What the fake answers with, and what it was asked.
#[derive(Debug, Default)]
struct Google {
    calls: Mutex<Vec<Call>>,
    /// `life_cycle_status` of the broadcast, as this fake currently reports it.
    lifecycle: Mutex<String>,
    /// `status.streamStatus` of the ingestion stream.
    stream_status: Mutex<String>,
    /// Does the broadcast report `enableAutoStart`?
    auto_start: Mutex<bool>,
    /// Does `liveBroadcasts?…&id=` report a bound stream?
    bound: Mutex<bool>,
    /// Access tokens this fake will refuse with a 401 before accepting one.
    stale: Mutex<Vec<String>>,
    /// A canned failure for the next matching request: (url fragment, status, body).
    fail: Mutex<Option<(String, u16, String)>>,
}

const STREAM_NAME: &str = "abcd-1234-efgh-5678-ijkl";
const INGEST: &str = "rtmps://a.rtmps.youtube.com/live2";

impl Google {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            lifecycle: Mutex::new("ready".into()),
            stream_status: Mutex::new("inactive".into()),
            auto_start: Mutex::new(false),
            bound: Mutex::new(true),
            ..Default::default()
        })
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    /// Every request whose URL contains this fragment.
    fn matching(&self, fragment: &str) -> Vec<Call> {
        self.calls().into_iter().filter(|c| c.url.contains(fragment)).collect()
    }

    fn broadcast_json(&self) -> serde_json::Value {
        let mut content = serde_json::json!({
            "enableAutoStart": *self.auto_start.lock().unwrap(),
            "enableAutoStop": *self.auto_start.lock().unwrap(),
        });
        if *self.bound.lock().unwrap() {
            content["boundStreamId"] = serde_json::json!("stream-1");
        }
        serde_json::json!({
            "id": "bcast-1",
            "snippet": { "title": "t", "description": "d" },
            "status": { "lifeCycleStatus": *self.lifecycle.lock().unwrap(), "privacyStatus": "unlisted" },
            "contentDetails": content,
        })
    }
}

impl HttpClient for Google {
    fn request(
        &self,
        method: &str,
        url: &str,
        bearer: &str,
        body: Option<serde_json::Value>,
    ) -> CoreResult<(u16, String)> {
        self.calls.lock().unwrap().push(Call {
            method: method.into(),
            url: url.into(),
            bearer: bearer.into(),
            body: body.clone(),
        });

        // A token this fake has been told is stale: 401, once each.
        {
            let mut stale = self.stale.lock().unwrap();
            if let Some(i) = stale.iter().position(|t| t == bearer) {
                stale.remove(i);
                return Ok((
                    401,
                    r#"{"error":{"code":401,"message":"Invalid Credentials","errors":[{"reason":"authError"}]}}"#
                        .into(),
                ));
            }
        }

        // A scripted failure, for the 403 cases.
        {
            let mut fail = self.fail.lock().unwrap();
            if let Some((fragment, status, text)) = fail.clone() {
                if url.contains(&fragment) {
                    *fail = None;
                    return Ok((status, text));
                }
            }
        }

        let answer = if url.contains("/channels?") {
            serde_json::json!({
                "items": [{
                    "id": "UC-channel-1",
                    "snippet": {
                        "title": "COLORISTE",
                        "thumbnails": { "default": { "url": "https://yt/thumb.jpg" } },
                    },
                }]
            })
        } else if url.contains("/liveStreams?") && method == "POST" {
            serde_json::json!({
                "id": "stream-1",
                "snippet": { "title": "247streams / x" },
                "cdn": {
                    "ingestionInfo": {
                        "ingestionAddress": "rtmp://a.rtmp.youtube.com/live2",
                        "rtmpsIngestionAddress": INGEST,
                        "streamName": STREAM_NAME,
                    },
                },
                "status": { "streamStatus": "inactive" },
            })
        } else if url.contains("/liveStreams?") {
            serde_json::json!({
                "items": [{
                    "id": "stream-1",
                    "snippet": { "title": "247streams / x" },
                    "cdn": { "ingestionInfo": { "streamName": STREAM_NAME } },
                    "status": { "streamStatus": *self.stream_status.lock().unwrap() },
                }]
            })
        } else if url.contains("/liveBroadcasts/bind") {
            *self.bound.lock().unwrap() = true;
            self.broadcast_json()
        } else if url.contains("/liveBroadcasts/transition") {
            let to = url.split("broadcastStatus=").nth(1).unwrap_or("").split('&').next().unwrap_or("");
            *self.lifecycle.lock().unwrap() = to.to_string();
            self.broadcast_json()
        } else if url.contains("/liveBroadcasts?") {
            // insert and update answer with the resource; list wraps it.
            match method {
                "POST" | "PUT" => self.broadcast_json(),
                _ => serde_json::json!({ "items": [self.broadcast_json()] }),
            }
        } else {
            return Err(LouverError::with_detail(
                ErrorCode::YoutubeApiFailed,
                format!("이 fake 는 {method} {url} 을 모릅니다"),
            ));
        };
        Ok((200, answer.to_string()))
    }
}

/// The token endpoint, scripted.
#[derive(Debug, Default)]
struct Tokens {
    /// What `exchange_code` returns.
    on_exchange: Mutex<Option<serde_json::Value>>,
    /// What each successive `refresh` returns.
    on_refresh: Mutex<Vec<serde_json::Value>>,
    exchanges: Mutex<Vec<(String, String, String)>>,
    refreshes: Mutex<Vec<String>>,
    /// The client secret each call was given, so a test can prove it travelled.
    secrets: Mutex<Vec<String>>,
}

fn token_json(access: &str, refresh: Option<&str>, expires_in: u64) -> serde_json::Value {
    let mut v = serde_json::json!({ "access_token": access, "expires_in": expires_in });
    if let Some(r) = refresh {
        v["refresh_token"] = serde_json::json!(r);
    }
    v
}

fn as_tokens(v: &serde_json::Value) -> TokenResponse {
    serde_json::from_value(v.clone()).expect("a token response the deserializer accepts")
}

impl TokenEndpoint for Tokens {
    fn exchange_code(
        &self,
        creds: &ClientCredentials,
        code: &str,
        redirect_uri: &str,
        code_verifier: &str,
    ) -> CoreResult<TokenResponse> {
        self.exchanges.lock().unwrap().push((code.into(), redirect_uri.into(), code_verifier.into()));
        self.secrets.lock().unwrap().push(creds.client_secret.clone());
        let scripted = self.on_exchange.lock().unwrap().clone();
        match scripted {
            Some(v) => Ok(as_tokens(&v)),
            None => Ok(as_tokens(&token_json("access-1", Some("refresh-1"), 3600))),
        }
    }

    fn refresh(&self, creds: &ClientCredentials, refresh_token: &str) -> CoreResult<TokenResponse> {
        self.refreshes.lock().unwrap().push(refresh_token.into());
        self.secrets.lock().unwrap().push(creds.client_secret.clone());
        let next = {
            let mut list = self.on_refresh.lock().unwrap();
            if list.is_empty() {
                None
            } else {
                Some(list.remove(0))
            }
        };
        match next {
            Some(v) => Ok(as_tokens(&v)),
            // Google's ordinary answer: a new access token and no refresh token.
            None => Ok(as_tokens(&token_json("access-2", None, 3600))),
        }
    }
}

// --- harness ---------------------------------------------------------------

const CLIENT_SECRET: &str = "GOCSPX-this-is-only-a-test-secret";

struct Env {
    dir: tempfile::TempDir,
    db: CloudDb,
    keys: Arc<dyn SecretStore>,
    api: Arc<Google>,
    tokens: Arc<Tokens>,
    yt: Youtube,
    user: String,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let db = CloudDb::open(&dir.path().join("cloud.db")).unwrap();
    let keys: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
    let api = Google::new();
    let tokens = Arc::new(Tokens::default());
    let config = Config {
        credentials: ClientCredentials {
            client_id: "test-client.apps.googleusercontent.com".into(),
            client_secret: CLIENT_SECRET.into(),
        },
        redirect_uri: "https://live.example.com/api/youtube/oauth/callback".into(),
        api_base: Some("https://fake.googleapis.test/youtube/v3".into()),
    };
    let yt = Youtube::new(
        db.clone(),
        Arc::clone(&keys),
        Arc::clone(&api) as Arc<dyn HttpClient>,
        Arc::clone(&tokens) as Arc<dyn TokenEndpoint>,
        config,
    );
    let user = db.create_user("dj@example.com", "hash", "business").unwrap().id;
    Env { dir, db, keys, api, tokens, yt, user }
}

impl Env {
    /// A connected account, the way a user who finished consent has one.
    fn connect(&self) -> String {
        let url = self.yt.consent_url(&self.user).unwrap();
        let state = state_of(&url);
        self.yt.complete_consent(&state, "auth-code-1").unwrap().id
    }

    fn video(&self, name: &str) -> String {
        use louver_cloud::storage::Storage;
        let store = louver_cloud::storage::LocalStorage::new(self.dir.path().join("media"));
        let src = self.dir.path().join(name);
        std::fs::write(&src, format!("bytes of {name}")).unwrap();
        let key = store.put_file(&self.user, name, &src).unwrap();
        let m = self.db.create_media(&self.user, name, 42, &key).unwrap();
        self.db.record_media_prepared(&m.id, &key, 60.0, 42).unwrap();
        m.id
    }

    /// A broadcast pointed at a reserved YouTube destination, as the API's
    /// create handler builds one before calling `provision`.
    fn broadcast(&self, account_id: &str, name: &str) -> String {
        let media = self.video("one.mp4");
        let dest = self.db.reserve_youtube_destination(&self.user, account_id).unwrap();
        let b = self.db.create_broadcast(&self.user, name, &media, &dest.id, true).unwrap();
        self.db
            .replace_items(&self.user, &b.id, &[NewItem { media_id: media, enabled: true, repeat_count: 1 }])
            .unwrap();
        b.id
    }

    /// A broadcast on a pasted stream key, which nothing here may disturb.
    fn manual_broadcast(&self, name: &str) -> (String, String) {
        let media = self.video("manual.mp4");
        let dest = self.db.create_destination(&self.user, "손으로 넣은 키", INGEST, "••••").unwrap();
        self.keys.set(&louver_cloud::credentials::destination_account(&dest.id), "manual-key-9999").unwrap();
        let b = self.db.create_broadcast(&self.user, name, &media, &dest.id, true).unwrap();
        (b.id, dest.id)
    }

    fn set_metadata(&self, id: &str, title: &str, description: &str, privacy: Privacy) {
        let patch = louver_cloud::BroadcastPatch {
            title: Some(title.into()),
            description: Some(description.into()),
            privacy: Some(privacy),
            ..Default::default()
        };
        self.db.update_broadcast_owned(&self.user, id, &patch).unwrap();
    }

    /// Every line this broadcast has recorded, for the "no secret ever appears"
    /// assertions.
    fn log(&self, id: &str) -> String {
        self.db.events_for(id, 200).unwrap().into_iter().map(|e| e.message).collect::<Vec<_>>().join("\n")
    }
}

fn state_of(url: &str) -> String {
    url.split("state=").nth(1).unwrap().split('&').next().unwrap().to_string()
}

fn query(url: &str, key: &str) -> Option<String> {
    url.split(&format!("{key}=")).nth(1).map(|r| r.split('&').next().unwrap_or("").to_string())
}

// --- A: the consent URL ----------------------------------------------------

#[test]
fn a_the_consent_url_asks_for_exactly_what_the_flow_needs() {
    let e = env();
    let url = e.yt.consent_url(&e.user).unwrap();

    assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"), "{url}");
    // force-ssl is the scope that can create and manage a live broadcast.
    assert!(url.contains("youtube.force-ssl"), "{url}");
    // Without offline access Google issues no refresh token, and the connection
    // would work for an hour and then stop.
    assert_eq!(query(&url, "access_type").as_deref(), Some("offline"));
    // Without this, an account that has consented before gets no refresh token
    // at all on a re-connect.
    assert_eq!(query(&url, "prompt").as_deref(), Some("consent"));
    assert_eq!(query(&url, "response_type").as_deref(), Some("code"));
    assert_eq!(query(&url, "code_challenge_method").as_deref(), Some("S256"));
    assert!(
        url.contains("redirect_uri=https%3A%2F%2Flive.example.com%2Fapi%2Fyoutube%2Foauth%2Fcallback"),
        "{url}"
    );
    assert!(!url.contains(CLIENT_SECRET), "the client secret must never be in a URL: {url}");
    assert!(!state_of(&url).is_empty());
}

// --- B, C, D: the state is the CSRF defence --------------------------------

#[test]
fn b_a_state_can_only_be_used_once() {
    let e = env();
    let state = state_of(&e.yt.consent_url(&e.user).unwrap());
    e.yt.complete_consent(&state, "code-1").unwrap();

    let again = e.yt.complete_consent(&state, "code-1");
    assert!(again.is_err(), "a replayed state must be refused");
    // And nothing was exchanged the second time.
    assert_eq!(e.tokens.exchanges.lock().unwrap().len(), 1);
}

#[test]
fn c_a_state_this_server_never_issued_is_refused() {
    let e = env();
    assert!(e.yt.complete_consent("not-a-state-we-made", "code-1").is_err());
    assert!(e.tokens.exchanges.lock().unwrap().is_empty(), "nothing may be exchanged for an unknown state");
}

#[test]
fn d_an_expired_state_is_refused() {
    let e = env();
    let state = state_of(&e.yt.consent_url(&e.user).unwrap());
    // Age it past the window the provider allows.
    e.db.raw()
        .lock()
        .unwrap()
        .execute(
            "UPDATE oauth_states SET created_at = datetime('now', '-2 hours') WHERE state = ?1",
            [&state],
        )
        .unwrap();

    assert!(e.yt.complete_consent(&state, "code-1").is_err());
    assert!(e.tokens.exchanges.lock().unwrap().is_empty());
}

// --- E: the account, and where its tokens are -----------------------------

#[test]
fn e_connecting_stores_the_channel_and_seals_both_tokens() {
    let e = env();
    let url = e.yt.consent_url(&e.user).unwrap();
    let account_id = e.yt.complete_consent(&state_of(&url), "auth-code-1").unwrap().id;

    let accounts = e.db.youtube_accounts_for(&e.user).unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].channel_id, "UC-channel-1");
    assert_eq!(accounts[0].channel_title, "COLORISTE");
    assert_eq!(accounts[0].thumbnail_url.as_deref(), Some("https://yt/thumb.jpg"));
    assert!(accounts[0].token_expiry.is_some(), "the cached token's expiry has to be known");

    // Sealed, under this account's own names.
    let refresh = e.keys.get(&louver_cloud::youtube::refresh_account(&account_id)).unwrap();
    assert_eq!(refresh.as_deref(), Some("refresh-1"));

    // §3: not in the database as plaintext. The whole file, because a token
    // could have reached any table.
    let bytes = std::fs::read(e.dir.path().join("cloud.db")).unwrap();
    assert!(!contains(&bytes, b"refresh-1"), "the refresh token must not be in the database file");
    assert!(!contains(&bytes, CLIENT_SECRET.as_bytes()), "the client secret must never be stored at all");

    // The code went to the token endpoint with the redirect URI and the verifier.
    let exchanges = e.tokens.exchanges.lock().unwrap();
    assert_eq!(exchanges[0].0, "auth-code-1");
    assert_eq!(exchanges[0].1, "https://live.example.com/api/youtube/oauth/callback");
    assert!(!exchanges[0].2.is_empty(), "PKCE's verifier has to travel with the code");
}

#[test]
fn e2_google_not_returning_a_refresh_token_on_consent_is_an_error_not_a_half_connection() {
    let e = env();
    *e.tokens.on_exchange.lock().unwrap() = Some(token_json("access-1", None, 3600));
    let state = state_of(&e.yt.consent_url(&e.user).unwrap());

    assert!(e.yt.complete_consent(&state, "code-1").is_err());
    assert!(
        e.db.youtube_accounts_for(&e.user).unwrap().is_empty(),
        "an account that cannot be refreshed must not be stored as connected"
    );
}

// --- F, G: refreshing ------------------------------------------------------

#[test]
fn f_a_refresh_that_returns_no_refresh_token_keeps_the_one_we_have() {
    let e = env();
    let account = e.connect();
    // Google's ordinary answer on refresh: an access token and nothing else.
    e.db.set_youtube_token_expiry(&account, -60).unwrap();

    let token = e.yt.access_token(&account).unwrap();
    assert_eq!(token, "access-2");
    assert_eq!(
        e.keys.get(&louver_cloud::youtube::refresh_account(&account)).unwrap().as_deref(),
        Some("refresh-1"),
        "overwriting the refresh token with the absent one disconnects the account for good"
    );
    assert_eq!(e.tokens.refreshes.lock().unwrap().as_slice(), ["refresh-1"]);
}

#[test]
fn g_a_rotated_refresh_token_is_stored() {
    let e = env();
    let account = e.connect();
    *e.tokens.on_refresh.lock().unwrap() = vec![token_json("access-2", Some("refresh-2"), 3600)];
    e.db.set_youtube_token_expiry(&account, -60).unwrap();

    e.yt.access_token(&account).unwrap();
    assert_eq!(
        e.keys.get(&louver_cloud::youtube::refresh_account(&account)).unwrap().as_deref(),
        Some("refresh-2")
    );
}

#[test]
fn h_an_unexpired_access_token_is_reused_rather_than_refreshed() {
    let e = env();
    let account = e.connect();

    assert_eq!(e.yt.access_token(&account).unwrap(), "access-1");
    assert!(
        e.tokens.refreshes.lock().unwrap().is_empty(),
        "a token good for another hour must not cost a refresh"
    );

    e.db.set_youtube_token_expiry(&account, -1).unwrap();
    assert_eq!(e.yt.access_token(&account).unwrap(), "access-2");
    assert_eq!(e.tokens.refreshes.lock().unwrap().len(), 1);
}

// --- I: what provision actually sends -------------------------------------

#[test]
fn i_the_broadcast_insert_carries_the_metadata_247streams_holds() {
    let e = env();
    let account = e.connect();
    let id = e.broadcast(&account, "밤의 플레이리스트");
    e.set_metadata(&id, "COLORISTE 24/7", "밤새 흐르는 음악", Privacy::Unlisted);

    e.yt.provision(&e.user, &id, &account).unwrap();

    let insert = e.api.matching("/liveBroadcasts?").into_iter().find(|c| c.method == "POST").unwrap();
    let body = insert.body.unwrap();
    assert_eq!(body["snippet"]["title"], "COLORISTE 24/7");
    assert_eq!(body["snippet"]["description"], "밤새 흐르는 음악");
    assert_eq!(body["status"]["privacyStatus"], "unlisted");
    // §5: required by YouTube on insert, and required to be answered explicitly
    // rather than left to a default.
    assert_eq!(body["status"]["selfDeclaredMadeForKids"], false);
    // §5: a valid scheduledStartTime even for a broadcast that starts now.
    let start = body["snippet"]["scheduledStartTime"].as_str().unwrap();
    assert!(
        chrono::DateTime::parse_from_rfc3339(start).is_ok(),
        "scheduledStartTime has to be a timestamp YouTube accepts: {start}"
    );
    assert_eq!(body["contentDetails"]["enableAutoStart"], true);
    assert_eq!(body["contentDetails"]["recordFromStart"], true);
}

#[test]
fn i2_a_stream_of_its_own_is_created_before_the_broadcast_is_bound() {
    let e = env();
    let account = e.connect();
    let id = e.broadcast(&account, "밤의 플레이리스트");
    e.yt.provision(&e.user, &id, &account).unwrap();

    // §6: one liveStreams.insert per broadcast, titled so that it can be found
    // in YouTube Studio.
    let insert = e.api.matching("/liveStreams?").into_iter().find(|c| c.method == "POST").unwrap();
    let body = insert.body.unwrap();
    assert_eq!(body["snippet"]["title"], "247streams / 밤의 플레이리스트");
    assert_eq!(body["cdn"]["ingestionType"], "rtmp");
    assert_eq!(body["cdn"]["resolution"], "variable");
    assert_eq!(body["cdn"]["frameRate"], "variable");

    // The bind names both resources, and the ids are the ones stored.
    let bind = e.api.matching("/liveBroadcasts/bind").into_iter().next().unwrap();
    assert!(bind.url.contains("id=bcast-1"), "{}", bind.url);
    assert!(bind.url.contains("streamId=stream-1"), "{}", bind.url);

    let b = e.db.broadcast_owned(&e.user, &id).unwrap();
    assert_eq!(b.youtube.account_id.as_deref(), Some(account.as_str()));
    assert_eq!(b.youtube.broadcast_id.as_deref(), Some("bcast-1"));
    assert_eq!(b.youtube.stream_id.as_deref(), Some("stream-1"));
    assert_eq!(b.youtube.status.as_deref(), Some("waiting_for_ingest"));
}

// --- J: the stream name is a secret ---------------------------------------

#[test]
fn j_the_stream_name_is_sealed_and_never_appears_anywhere_else() {
    let e = env();
    let account = e.connect();
    let id = e.broadcast(&account, "밤의 플레이리스트");
    e.yt.provision(&e.user, &id, &account).unwrap();

    let b = e.db.broadcast_owned(&e.user, &id).unwrap();
    let dest = e.db.destination_owned(&e.user, &b.destination_id).unwrap();

    // The row holds the address and a mask, never the key — exactly as a pasted
    // destination does, which is what keeps the sending path one path.
    assert_eq!(dest.rtmps_url, INGEST);
    assert_eq!(dest.kind, louver_cloud::DestinationKind::YoutubeAccount);
    assert!(!dest.key_masked.contains(STREAM_NAME));
    assert!(!serde_json::to_string(&dest).unwrap().contains(STREAM_NAME), "not in an API response");

    // It is in the sealed store, under the name the worker reads.
    assert_eq!(
        e.keys.get(&louver_cloud::credentials::destination_account(&dest.id)).unwrap().as_deref(),
        Some(STREAM_NAME)
    );

    // Not in the event log, and not in the database file.
    assert!(!e.log(&id).contains(STREAM_NAME), "the stream name must not be logged:\n{}", e.log(&id));
    let bytes = std::fs::read(e.dir.path().join("cloud.db")).unwrap();
    assert!(!contains(&bytes, STREAM_NAME.as_bytes()), "the stream name must not be stored as plaintext");
}

#[test]
fn j2_the_url_ffmpeg_is_given_is_the_address_and_the_stream_name() {
    let e = env();
    let account = e.connect();
    let id = e.broadcast(&account, "밤의 플레이리스트");
    e.yt.provision(&e.user, &id, &account).unwrap();

    let b = e.db.broadcast_owned(&e.user, &id).unwrap();
    let dest = e.db.destination_owned(&e.user, &b.destination_id).unwrap();
    let key = e.keys.get(&louver_cloud::credentials::destination_account(&dest.id)).unwrap().unwrap();

    // The same function the engine uses, so this cannot drift from what is sent.
    assert_eq!(
        louver_core::security::build_ingest_url(&dest.rtmps_url, &key),
        format!("rtmps://a.rtmps.youtube.com/live2/{STREAM_NAME}")
    );
}

// --- K: ownership ---------------------------------------------------------

#[test]
fn k_one_account_cannot_use_another_accounts_connected_channel() {
    let e = env();
    let mine = e.connect();
    let id = e.broadcast(&mine, "내 방송");

    // A second user, with a broadcast of their own.
    let other = e.db.create_user("thief@example.com", "hash", "business").unwrap().id;

    // Knowing the id buys nothing: the row is simply not found for them.
    assert!(e.db.youtube_account_owned(&other, &mine).is_err());
    assert!(e.yt.disconnect(&other, &mine).is_err());
    assert!(e.yt.provision(&other, &id, &mine).is_err());
    assert!(e.yt.sync_metadata(&other, &id).is_err());
    assert!(e.db.reserve_youtube_destination(&other, &mine).is_err());

    // And the victim's account is still connected and still refreshable.
    assert_eq!(e.db.youtube_accounts_for(&e.user).unwrap().len(), 1);
    assert_eq!(
        e.keys.get(&louver_cloud::youtube::refresh_account(&mine)).unwrap().as_deref(),
        Some("refresh-1")
    );
}

#[test]
fn k2_disconnecting_removes_the_row_and_both_sealed_tokens() {
    let e = env();
    let account = e.connect();
    e.yt.disconnect(&e.user, &account).unwrap();

    assert!(e.db.youtube_accounts_for(&e.user).unwrap().is_empty());
    assert_eq!(e.keys.get(&louver_cloud::youtube::refresh_account(&account)).unwrap(), None);
    assert_eq!(e.keys.get(&louver_cloud::youtube::access_account(&account)).unwrap(), None);
}

// --- L: 401 is a token to replace, not a failure to report -----------------

#[test]
fn l_a_401_refreshes_the_token_and_retries_once() {
    let e = env();
    let account = e.connect();
    let id = e.broadcast(&account, "밤의 플레이리스트");
    // The cached access token has been revoked, but our expiry says it is fine.
    e.api.stale.lock().unwrap().push("access-1".into());

    e.yt.provision(&e.user, &id, &account).unwrap();

    assert_eq!(e.tokens.refreshes.lock().unwrap().len(), 1, "exactly one refresh");
    // The retried request carried the new token.
    let inserts: Vec<_> =
        e.api.matching("/liveBroadcasts?").into_iter().filter(|c| c.method == "POST").collect();
    assert_eq!(inserts.len(), 2, "the refused request is the one that is retried");
    assert_eq!(inserts[0].bearer, "access-1");
    assert_eq!(inserts[1].bearer, "access-2");
    assert_eq!(e.db.broadcast_owned(&e.user, &id).unwrap().youtube.broadcast_id.as_deref(), Some("bcast-1"));
}

#[test]
fn l2_a_second_401_is_reported_rather_than_retried_for_ever() {
    let e = env();
    let account = e.connect();
    let id = e.broadcast(&account, "밤의 플레이리스트");
    e.api.stale.lock().unwrap().push("access-1".into());
    e.api.stale.lock().unwrap().push("access-2".into());

    let failed = e.yt.provision(&e.user, &id, &account);
    assert!(failed.is_err(), "a grant that is gone has to surface, not loop");
    assert_eq!(e.tokens.refreshes.lock().unwrap().len(), 1, "one refresh, not a retry storm");
}

// --- M: a 403 says what a user can do about it ----------------------------

#[test]
fn m_googles_refusals_arrive_as_something_a_user_can_act_on() {
    for (reason, expect) in [
        ("quotaExceeded", "사용량"),
        ("insufficientPermissions", "liveBroadcasts.insert"),
        ("liveStreamingNotEnabled", "liveBroadcasts.insert"),
    ] {
        let e = env();
        let account = e.connect();
        let id = e.broadcast(&account, "밤의 플레이리스트");
        *e.api.fail.lock().unwrap() = Some((
            "/liveBroadcasts?".into(),
            403,
            format!(
                r#"{{"error":{{"code":403,"message":"nope","errors":[{{"reason":"{reason}","message":"nope"}}]}}}}"#
            ),
        ));

        let err = e.yt.provision(&e.user, &id, &account).unwrap_err().to_string();
        // Both halves matter: the sentence a user reads, and Google's own
        // reason, which is the only thing that tells three identical-looking
        // 403s apart.
        assert!(err.contains(expect), "a {reason} refusal has to explain itself, got: {err}");
        assert!(err.contains(reason), "Google's reason has to survive, got: {err}");
        assert!(err.contains("403"), "the status has to survive, got: {err}");
        // §15: no token, no secret, no stream name in what the user is shown.
        assert!(!err.contains("access-1"), "{err}");
        assert!(!err.contains("refresh-1"), "{err}");
        assert!(!err.contains(CLIENT_SECRET), "{err}");
        assert!(!err.contains(STREAM_NAME), "{err}");

        // And the failure is where the dashboard can show it.
        let b = e.db.broadcast_owned(&e.user, &id).unwrap();
        assert!(b.youtube.last_error.is_some(), "the step that failed has to be recorded");
        assert!(!e.log(&id).contains("access-1"));
    }
}

// --- N: metadata sync ------------------------------------------------------

#[test]
fn n_renaming_a_broadcast_renames_it_on_youtube() {
    let e = env();
    let account = e.connect();
    let id = e.broadcast(&account, "밤의 플레이리스트");
    e.yt.provision(&e.user, &id, &account).unwrap();

    e.set_metadata(&id, "zzz", "새 설명", Privacy::Private);
    e.yt.sync_metadata(&e.user, &id).unwrap();

    let update = e.api.matching("/liveBroadcasts?").into_iter().rfind(|c| c.method == "PUT").unwrap();
    let body = update.body.unwrap();
    assert_eq!(body["id"], "bcast-1");
    assert_eq!(body["snippet"]["title"], "zzz");
    assert_eq!(body["snippet"]["description"], "새 설명");
    assert_eq!(body["status"]["privacyStatus"], "private");
    // liveBroadcasts.update replaces the part it is given, so omitting this is
    // how a broadcast silently loses its schedule.
    assert!(body["snippet"]["scheduledStartTime"].is_string());
}

#[test]
fn n2_a_pasted_key_broadcast_causes_no_api_call_at_all() {
    let e = env();
    let account = e.connect();
    let calls_before = e.api.calls().len();
    let (id, _) = e.manual_broadcast("수동 방송");

    // Both hooks are called for every broadcast; neither may touch Google for
    // one that has no connected account.
    e.yt.sync_metadata(&e.user, &id).unwrap();
    e.yt.before_start(&id).unwrap();
    assert_eq!(e.yt.poll_status(&id).unwrap(), None);
    e.yt.after_stop(&id);

    assert_eq!(e.api.calls().len(), calls_before, "manual RTMPS must not reach the YouTube API");
    let _ = account;
}

// --- O: a broadcast YouTube has ended is not started again ----------------

#[test]
fn o_a_completed_youtube_broadcast_refuses_to_start() {
    let e = env();
    let account = e.connect();
    let id = e.broadcast(&account, "밤의 플레이리스트");
    e.yt.provision(&e.user, &id, &account).unwrap();
    *e.api.lifecycle.lock().unwrap() = "complete".into();

    assert!(e.yt.before_start(&id).is_err(), "§14: this must not be restarted for ever");
    assert_eq!(e.db.broadcast_owned(&e.user, &id).unwrap().youtube.status.as_deref(), Some("complete"));
}

#[test]
fn o2_a_lost_binding_is_restored_before_the_worker_starts() {
    let e = env();
    let account = e.connect();
    let id = e.broadcast(&account, "밤의 플레이리스트");
    e.yt.provision(&e.user, &id, &account).unwrap();

    // What recovery finds after a restart: the resources exist, the binding does
    // not.
    *e.api.bound.lock().unwrap() = false;
    let binds_before = e.api.matching("/liveBroadcasts/bind").len();
    e.yt.before_start(&id).unwrap();

    assert_eq!(e.api.matching("/liveBroadcasts/bind").len(), binds_before + 1);
    assert_eq!(
        e.db.broadcast_owned(&e.user, &id).unwrap().youtube.status.as_deref(),
        Some("waiting_for_ingest")
    );
}

// --- P: YouTube's state, not FFmpeg's ------------------------------------

#[test]
fn p_the_status_follows_youtube_and_transitions_only_when_it_has_to() {
    let e = env();
    let account = e.connect();
    let id = e.broadcast(&account, "밤의 플레이리스트");
    e.yt.provision(&e.user, &id, &account).unwrap();

    // Nothing arriving yet.
    assert_eq!(e.yt.poll_status(&id).unwrap().as_deref(), Some("waiting_for_ingest"));

    // Video is arriving, and this broadcast will not start itself.
    *e.api.stream_status.lock().unwrap() = "active".into();
    assert_eq!(e.yt.poll_status(&id).unwrap().as_deref(), Some("live"));
    assert_eq!(e.api.matching("broadcastStatus=live").len(), 1, "one transition, not one per poll");
    assert_eq!(e.db.broadcast_owned(&e.user, &id).unwrap().youtube.status.as_deref(), Some("live"));
}

#[test]
fn p2_a_broadcast_that_starts_itself_is_not_transitioned() {
    let e = env();
    let account = e.connect();
    let id = e.broadcast(&account, "밤의 플레이리스트");
    e.yt.provision(&e.user, &id, &account).unwrap();
    *e.api.auto_start.lock().unwrap() = true;
    *e.api.stream_status.lock().unwrap() = "active".into();

    assert_eq!(e.yt.poll_status(&id).unwrap().as_deref(), Some("ready"));
    assert!(
        e.api.matching("broadcastStatus=live").is_empty(),
        "enableAutoStart means YouTube does this itself; asking anyway reads as a permissions error"
    );
}

// --- Q: stopping -----------------------------------------------------------

#[test]
fn q_stopping_a_live_broadcast_completes_it_on_youtube() {
    let e = env();
    let account = e.connect();
    let id = e.broadcast(&account, "밤의 플레이리스트");
    e.yt.provision(&e.user, &id, &account).unwrap();
    *e.api.lifecycle.lock().unwrap() = "live".into();

    e.yt.after_stop(&id);
    assert_eq!(e.api.matching("broadcastStatus=complete").len(), 1);
    assert_eq!(e.db.broadcast_owned(&e.user, &id).unwrap().youtube.status.as_deref(), Some("complete"));
}

#[test]
fn q2_stopping_a_broadcast_that_never_went_live_asks_for_no_transition() {
    let e = env();
    let account = e.connect();
    let id = e.broadcast(&account, "밤의 플레이리스트");
    e.yt.provision(&e.user, &id, &account).unwrap();

    // `enableAutoStop` may already have ended it; either way there is nothing
    // to transition, and a failure here must not leave 247streams thinking the
    // broadcast is still running.
    e.yt.after_stop(&id);
    assert!(e.api.matching("broadcastStatus=complete").is_empty());
    assert_eq!(e.db.broadcast_owned(&e.user, &id).unwrap().youtube.status.as_deref(), Some("complete"));
}

// --- R: FFmpeg being alive is not "라이브 중" -----------------------------

#[test]
fn r_a_running_worker_does_not_make_youtube_say_live() {
    let e = env();
    let account = e.connect();
    let id = e.broadcast(&account, "밤의 플레이리스트");
    e.yt.provision(&e.user, &id, &account).unwrap();

    // The engine's own report, exactly as the manager writes it.
    e.db.record_runtime(&id, louver_cloud::RuntimeState::Running, 0, 30, 4_000_000, Some(4242)).unwrap();

    let b = e.db.broadcast_owned(&e.user, &id).unwrap();
    assert_eq!(b.runtime_state, louver_cloud::RuntimeState::Running);
    assert_eq!(
        b.youtube.status.as_deref(),
        Some("waiting_for_ingest"),
        "§8: YouTube's state comes from YouTube, never from a live process"
    );
}

// --- S: the path that is on air -------------------------------------------

#[test]
fn s_a_pasted_key_broadcast_is_untouched_by_any_of_this() {
    let e = env();
    let (id, dest) = e.manual_broadcast("수동 방송");
    let before = e.db.broadcast_owned(&e.user, &id).unwrap();

    e.connect();
    e.yt.sync_metadata(&e.user, &id).unwrap();
    e.yt.before_start(&id).unwrap();
    e.yt.after_stop(&id);

    let after = e.db.broadcast_owned(&e.user, &id).unwrap();
    assert_eq!(after.destination_id, before.destination_id);
    assert!(after.youtube.account_id.is_none());
    assert!(after.youtube.broadcast_id.is_none());
    assert!(after.youtube.status.is_none(), "a manual broadcast has no YouTube state to show");

    // The key it sends with is the one it had, byte for byte.
    assert_eq!(
        e.keys.get(&louver_cloud::credentials::destination_account(&dest)).unwrap().as_deref(),
        Some("manual-key-9999")
    );
    let d = e.db.destination_owned(&e.user, &dest).unwrap();
    assert_eq!(d.kind, louver_cloud::DestinationKind::ManualRtmps);
    assert!(!d.kind.can_publish_metadata(), "a pasted key still cannot set a title");
}

// --- T: multiple channels, and reconnecting -------------------------------

#[test]
fn t_a_user_can_connect_more_than_one_channel() {
    let e = env();
    let first = e.connect();
    // A second consent, reporting a different channel.
    e.api.calls.lock().unwrap().clear();
    let second = {
        let url = e.yt.consent_url(&e.user).unwrap();
        // The fake answers `/channels` with one channel, so point it at another
        // by swapping what it reports through the scripted failure slot's
        // sibling: a second Google, sharing nothing but the user.
        let other = Google::new();
        let yt = Youtube::new(
            e.db.clone(),
            Arc::clone(&e.keys),
            Arc::clone(&other) as Arc<dyn HttpClient>,
            Arc::clone(&e.tokens) as Arc<dyn TokenEndpoint>,
            Config {
                credentials: ClientCredentials {
                    client_id: "test-client.apps.googleusercontent.com".into(),
                    client_secret: CLIENT_SECRET.into(),
                },
                redirect_uri: "https://live.example.com/api/youtube/oauth/callback".into(),
                api_base: Some("https://fake.googleapis.test/youtube/v3".into()),
            },
        );
        // Same channel id: this is a re-connect of the same channel, which must
        // update the row rather than make a second one.
        yt.complete_consent(&state_of(&url), "code-2").unwrap().id
    };

    assert_eq!(first, second, "re-connecting the same channel keeps its id, so sealed tokens stay valid");
    assert_eq!(e.db.youtube_accounts_for(&e.user).unwrap().len(), 1);
}

#[test]
fn t2_two_broadcasts_on_one_channel_get_two_different_ingestion_streams() {
    let e = env();
    let account = e.connect();
    let a = e.broadcast(&account, "첫 번째");
    let b = e.broadcast(&account, "두 번째");
    e.yt.provision(&e.user, &a, &account).unwrap();
    e.yt.provision(&e.user, &b, &account).unwrap();

    // §6: liveStreams.insert once per broadcast, not one shared key.
    assert_eq!(e.api.matching("/liveStreams?").iter().filter(|c| c.method == "POST").count(), 2);
    let da = e.db.broadcast_owned(&e.user, &a).unwrap().destination_id;
    let db_ = e.db.broadcast_owned(&e.user, &b).unwrap().destination_id;
    assert_ne!(da, db_, "each broadcast sends to its own destination row");
}

#[test]
fn t3_provisioning_twice_reuses_the_destination_rather_than_piling_them_up() {
    let e = env();
    let account = e.connect();
    let id = e.broadcast(&account, "밤의 플레이리스트");
    e.yt.provision(&e.user, &id, &account).unwrap();
    let first = e.db.broadcast_owned(&e.user, &id).unwrap().destination_id;

    e.yt.provision(&e.user, &id, &account).unwrap();
    let second = e.db.broadcast_owned(&e.user, &id).unwrap().destination_id;

    assert_eq!(first, second);
    assert_eq!(
        e.db.destinations_for(&e.user).unwrap().len(),
        1,
        "re-provisioning must not leave orphan destinations behind"
    );
}

/// Is `needle` anywhere in `haystack`? For "the plaintext is not in the file".
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
