//! §19's security cases, driven through the HTTP layer rather than past it.
//!
//! The database already refuses to answer for the wrong owner; these tests are
//! about the layer above it, where a forgotten `Caller` or a struct that
//! serialises one field too many would be the actual leak. They drive the real
//! router with a real request, so a route added without an owner check fails
//! here.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use louver_cloud::credentials::CredentialStore;
use louver_cloud::ingest::Ingest;
use louver_cloud::manager::{BroadcastManager, LauncherFactory};
use louver_cloud::storage::{LocalStorage, Storage};
use louver_cloud::CloudDb;
use louver_core::error::Result as CoreResult;
use louver_core::runtime::StreamLauncher;
use louver_core::security::SecretStore;
use louver_core::streaming::ffmpeg::FfmpegTools;
use louver_core::streaming::supervisor::{ProcessHandle, StreamSupervisor};
use louver_server::state::App;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tower::ServiceExt;

// --- a process that just sits there ----------------------------------------

#[derive(Debug)]
struct Idle {
    stopped: AtomicBool,
}

impl ProcessHandle for Idle {
    fn try_exited(&mut self) -> Option<bool> {
        self.stopped.load(Ordering::SeqCst).then_some(true)
    }
    fn terminate(&mut self) -> CoreResult<()> {
        self.stopped.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn pid(&self) -> Option<u32> {
        Some(4242)
    }
}

#[derive(Debug)]
struct Fake;

impl LauncherFactory for Fake {
    fn for_broadcast(&self, _db: &CloudDb, _id: &str) -> Arc<dyn StreamLauncher> {
        Arc::new(Fake)
    }
}

impl StreamLauncher for Fake {
    fn launch(&self, _s: &mut StreamSupervisor, _a: &[String]) -> CoreResult<Box<dyn ProcessHandle>> {
        Ok(Box::new(Idle { stopped: AtomicBool::new(false) }))
    }
}

// --- harness ---------------------------------------------------------------

struct Server {
    _dir: tempfile::TempDir,
    app: App,
}

fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let db = CloudDb::open(&dir.path().join("cloud.db")).unwrap();
    // A fixed key, so the sealing is the real thing and a test can prove the
    // stored bytes are not the plaintext.
    let keys: Arc<dyn SecretStore> = Arc::new(CredentialStore::new(db.raw(), [7u8; 32]));
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(dir.path().join("media")));
    let tools = FfmpegTools::new("ffmpeg", "ffprobe");
    let mgr = BroadcastManager::new(
        db.clone(),
        Arc::clone(&storage),
        dir.path().join("work"),
        tools.clone(),
        "libx264".into(),
        Arc::clone(&keys),
        Arc::new(Fake),
    );
    let ingest = Ingest::new(db.clone(), Arc::clone(&storage), tools, "libx264".into());
    let upload_tmp = dir.path().join("uploads");
    std::fs::create_dir_all(&upload_tmp).unwrap();
    Server { _dir: dir, app: App { db, mgr, ingest, storage, keys, upload_tmp } }
}

struct Reply {
    status: StatusCode,
    body: String,
    set_cookie: Option<String>,
}

impl Reply {
    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
    }
    fn id(&self) -> String {
        self.json()["id"].as_str().unwrap_or_default().to_string()
    }
}

async fn send(
    s: &Server,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> Reply {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = match body {
        Some(v) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&v).unwrap()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let res = louver_server::router(s.app.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let set_cookie = res.headers().get(header::SET_COOKIE).and_then(|v| v.to_str().ok()).map(str::to_string);
    let bytes = axum::body::to_bytes(res.into_body(), 4 * 1024 * 1024).await.unwrap();
    Reply { status, body: String::from_utf8_lossy(&bytes).to_string(), set_cookie }
}

async fn get(s: &Server, path: &str, token: &str) -> Reply {
    send(s, Method::GET, path, Some(token), None).await
}

async fn post(s: &Server, path: &str, token: &str, body: Option<serde_json::Value>) -> Reply {
    send(s, Method::POST, path, Some(token), body).await
}

/// Register, and return the session token the way a browser would receive it.
async fn account(s: &Server, email: &str) -> String {
    let r = send(
        s,
        Method::POST,
        "/api/auth/register",
        None,
        Some(serde_json::json!({ "email": email, "password": "correct-horse-battery" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "register failed: {}", r.body);
    let cookie = r.set_cookie.expect("register must set a session cookie");
    assert!(cookie.contains("HttpOnly"), "session cookie must be HttpOnly: {cookie}");
    assert!(!r.body.contains("louver_session"), "the token must not be in the body: {}", r.body);
    cookie.split(';').next().unwrap().trim_start_matches("louver_session=").to_string()
}

const A_REAL_LOOKING_KEY: &str = "abcd-1234-efgh-5678-ijkl";

/// A destination plus a broadcast that is ready to start.
async fn ready_broadcast(s: &Server, token: &str, user_id: &str, name: &str) -> (String, String) {
    let dest = post(
        s,
        "/api/stream-destinations",
        token,
        Some(serde_json::json!({
            "label": "내 채널",
            "rtmps_url": "rtmps://a.rtmps.youtube.com/live2",
            "stream_key": A_REAL_LOOKING_KEY,
        })),
    )
    .await;
    assert_eq!(dest.status, StatusCode::OK, "{}", dest.body);

    // Prepared media, put in place directly: what is under test here is the API
    // layer, not FFmpeg.
    let src = s._dir.path().join(format!("{name}.mp4"));
    std::fs::write(&src, b"prepared video bytes").unwrap();
    let key = s.app.storage.put_file(user_id, &format!("{name}.mp4"), &src).unwrap();
    let m = s.app.db.create_media(user_id, &format!("{name}.mp4"), 20, &key).unwrap();
    s.app.db.record_media_prepared(&m.id, &key, 60.0, 20).unwrap();

    let b = post(
        s,
        "/api/broadcasts",
        token,
        Some(serde_json::json!({
            "name": name,
            "media_id": m.id,
            "destination_id": dest.id(),
        })),
    )
    .await;
    assert_eq!(b.status, StatusCode::OK, "{}", b.body);
    (b.id(), m.id)
}

// --- tests -----------------------------------------------------------------

#[tokio::test]
async fn without_a_session_the_api_answers_nothing() {
    let s = server();
    for (method, path) in [
        (Method::GET, "/api/me"),
        (Method::GET, "/api/me/subscription"),
        (Method::GET, "/api/media"),
        (Method::GET, "/api/stream-destinations"),
        (Method::GET, "/api/broadcasts"),
        (Method::POST, "/api/broadcasts/anything/start"),
        (Method::POST, "/api/broadcasts/anything/stop"),
        (Method::GET, "/api/broadcasts/anything/logs"),
        (Method::GET, "/api/events"),
    ] {
        let r = send(&s, method.clone(), path, None, None).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{method} {path} answered {}", r.status);
    }

    // A made-up token is no better than none.
    let r = get(&s, "/api/me", "not-a-real-token").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn knowing_another_users_ids_buys_nothing() {
    let s = server();
    let a = account(&s, "a@example.com").await;
    let b = account(&s, "b@example.com").await;
    let b_id = get(&s, "/api/me", &b).await.id();
    let (b_broadcast, b_media) = ready_broadcast(&s, &b, &b_id, "B의 방송").await;
    let b_dest = get(&s, "/api/stream-destinations", &b).await.json()[0]["id"].as_str().unwrap().to_string();

    // A holds B's real ids. Every one of them is simply not found.
    for (method, path) in [
        (Method::GET, format!("/api/broadcasts/{b_broadcast}")),
        (Method::POST, format!("/api/broadcasts/{b_broadcast}/start")),
        (Method::POST, format!("/api/broadcasts/{b_broadcast}/stop")),
        (Method::POST, format!("/api/broadcasts/{b_broadcast}/restart")),
        (Method::GET, format!("/api/broadcasts/{b_broadcast}/logs")),
        (Method::DELETE, format!("/api/broadcasts/{b_broadcast}")),
        (Method::GET, format!("/api/media/{b_media}")),
        (Method::DELETE, format!("/api/media/{b_media}")),
        (Method::DELETE, format!("/api/stream-destinations/{b_dest}")),
    ] {
        let r = send(&s, method.clone(), &path, Some(&a), None).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{method} {path} answered {}", r.status);
        // Not "forbidden", not "this belongs to someone else": the answer must
        // not confirm that the id exists at all.
        assert!(!r.body.contains("permitted"), "{}", r.body);
    }

    // And nothing A did touched B's broadcast.
    let still = get(&s, &format!("/api/broadcasts/{b_broadcast}"), &b).await;
    assert_eq!(still.status, StatusCode::OK);
    assert_eq!(still.json()["desired_state"], "stopped");
    // §2's state names, as the spec writes them.
    assert_eq!(still.json()["runtime_state"], "CREATED");

    // A's own listings show only A's things, which is to say nothing yet.
    assert_eq!(get(&s, "/api/media", &a).await.json().as_array().unwrap().len(), 0);
    assert_eq!(get(&s, "/api/stream-destinations", &a).await.json().as_array().unwrap().len(), 0);
    assert_eq!(get(&s, "/api/broadcasts", &a).await.json()["broadcasts"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn no_response_and_no_log_ever_carries_the_stream_key() {
    let s = server();
    let token = account(&s, "keys@example.com").await;
    let uid = get(&s, "/api/me", &token).await.id();
    let (broadcast, _) = ready_broadcast(&s, &token, &uid, "키 검사").await;
    post(&s, &format!("/api/broadcasts/{broadcast}/start"), &token, None).await;

    for path in [
        "/api/stream-destinations".to_string(),
        "/api/broadcasts".to_string(),
        format!("/api/broadcasts/{broadcast}"),
        format!("/api/broadcasts/{broadcast}/logs"),
        "/api/me".to_string(),
        "/api/me/subscription".to_string(),
    ] {
        let r = get(&s, &path, &token).await;
        assert!(!r.body.contains(A_REAL_LOOKING_KEY), "{path} leaked the stream key: {}", r.body);
    }

    // What the UI gets instead is a mask.
    let listed = get(&s, "/api/stream-destinations", &token).await;
    assert_eq!(listed.json()[0]["key_masked"], "••••••••••••");
    assert!(listed.json()[0].get("key").is_none(), "the response must have no key field");

    // The key is in the database, but not as text anyone could read out of it.
    let dump = std::fs::read(s._dir.path().join("cloud.db")).unwrap();
    assert!(
        !String::from_utf8_lossy(&dump).contains(A_REAL_LOOKING_KEY),
        "the stream key is stored in plaintext"
    );

    // And the broadcast's own event log, which a user can read, says nothing.
    let logs = get(&s, &format!("/api/broadcasts/{broadcast}/logs"), &token).await;
    assert!(!logs.body.contains(A_REAL_LOOKING_KEY), "{}", logs.body);
    assert!(!logs.body.contains("live2?"), "an ingest URL with a key: {}", logs.body);
}

#[tokio::test]
async fn the_plan_limit_is_the_servers_answer_not_the_browsers() {
    let s = server();
    let token = account(&s, "basic@example.com").await;
    let uid = get(&s, "/api/me", &token).await.id();
    // The default plan allows one concurrent stream.
    let (first, _) = ready_broadcast(&s, &token, &uid, "첫 방송").await;
    let (second, _) = ready_broadcast(&s, &token, &uid, "둘째 방송").await;

    let ok = post(&s, &format!("/api/broadcasts/{first}/start"), &token, None).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);

    let refused = post(&s, &format!("/api/broadcasts/{second}/start"), &token, None).await;
    assert_eq!(refused.status, StatusCode::PAYMENT_REQUIRED, "{}", refused.body);
    assert!(refused.body.contains("max_concurrent_streams"), "{}", refused.body);

    // The refusal left nothing half-started.
    let still = get(&s, &format!("/api/broadcasts/{second}"), &token).await;
    assert_eq!(still.json()["desired_state"], "stopped");
    let dash = get(&s, "/api/broadcasts", &token).await;
    assert_eq!(dash.json()["active"], 1);
    assert_eq!(dash.json()["allowed"], 1);

    // Stopping the first makes room, and the second then starts. The limit is a
    // limit, not a one-time refusal.
    post(&s, &format!("/api/broadcasts/{first}/stop"), &token, None).await;
    let now = post(&s, &format!("/api/broadcasts/{second}/start"), &token, None).await;
    assert_eq!(now.status, StatusCode::OK, "{}", now.body);
    s.app.mgr.shutdown();
}

#[tokio::test]
async fn an_upload_over_the_plans_ceiling_is_refused_before_it_is_stored() {
    let s = server();
    let token = account(&s, "small@example.com").await;
    let uid = get(&s, "/api/me", &token).await.id();

    // A plan with a tiny per-file ceiling. Plans are rows, so this needs no new
    // code and no plan name in a condition.
    s.app
        .db
        .raw()
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO plans (id, label, limits) VALUES ('tiny','Tiny',?1)",
            [r#"{"max_concurrent_streams":1,"max_broadcasts":1,"max_storage_bytes":1000,"max_upload_bytes":64}"#],
        )
        .unwrap();
    s.app.db.set_plan(&uid, "tiny").unwrap();

    let boundary = "----louvertest";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"big.mp4\"\r\nContent-Type: video/mp4\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&vec![b'x'; 4096]);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/media/upload")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"))
        .body(Body::from(body))
        .unwrap();
    let res = louver_server::router(s.app.clone()).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::PAYMENT_REQUIRED);

    // No row, and nothing left in the upload directory.
    assert_eq!(s.app.db.media_for(&uid).unwrap().len(), 0);
    let leftovers = std::fs::read_dir(&s.app.upload_tmp).unwrap().count();
    assert_eq!(leftovers, 0, "a refused upload left its temp file behind");
}

#[tokio::test]
async fn logging_out_ends_the_session_on_the_server() {
    let s = server();
    let token = account(&s, "bye@example.com").await;
    assert_eq!(get(&s, "/api/me", &token).await.status, StatusCode::OK);

    let out = post(&s, "/api/auth/logout", &token, None).await;
    assert_eq!(out.status, StatusCode::OK);
    assert!(out.set_cookie.unwrap().contains("Max-Age=0"));

    // The token is not merely forgotten by the browser; it no longer works.
    assert_eq!(get(&s, "/api/me", &token).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_account_cannot_be_registered_twice_or_with_a_weak_password() {
    let s = server();
    account(&s, "dup@example.com").await;

    let again = send(
        &s,
        Method::POST,
        "/api/auth/register",
        None,
        Some(serde_json::json!({ "email": "DUP@example.com", "password": "correct-horse-battery" })),
    )
    .await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.body);

    let weak = send(
        &s,
        Method::POST,
        "/api/auth/register",
        None,
        Some(serde_json::json!({ "email": "weak@example.com", "password": "short" })),
    )
    .await;
    assert_eq!(weak.status, StatusCode::BAD_REQUEST);

    // A wrong password and an unknown address answer identically, so the
    // endpoint cannot be used to find out who has an account.
    let wrong = send(
        &s,
        Method::POST,
        "/api/auth/login",
        None,
        Some(serde_json::json!({ "email": "dup@example.com", "password": "not-the-password" })),
    )
    .await;
    let unknown = send(
        &s,
        Method::POST,
        "/api/auth/login",
        None,
        Some(serde_json::json!({ "email": "nobody@example.com", "password": "not-the-password" })),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown.status, unknown.status);
    assert_eq!(wrong.body, unknown.body);
}
