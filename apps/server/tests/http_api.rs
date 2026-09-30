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
    let ingest = Ingest::new(db.clone(), Arc::clone(&storage), tools.clone(), "libx264".into());
    let upload_tmp = dir.path().join("uploads");
    std::fs::create_dir_all(&upload_tmp).unwrap();
    Server {
        _dir: dir,
        app: App {
            db,
            mgr,
            ingest,
            storage,
            keys,
            upload_tmp,
            tools,
            youtube: None,
            payapp: None,
            machine: std::sync::Arc::new(louver_server::state::Machine::default()),
        },
    }
}

struct Reply {
    status: StatusCode,
    body: String,
    set_cookie: Option<String>,
    /// Where a redirect points. The OAuth callback answers with one rather than
    /// a body, because what arrives there is a person looking at a browser.
    location: Option<String>,
    /// So a test can check that a reply is data rather than a document.
    content_type: Option<String>,
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
    let location = res.headers().get(header::LOCATION).and_then(|v| v.to_str().ok()).map(str::to_string);
    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or(v).trim().to_string());
    let bytes = axum::body::to_bytes(res.into_body(), 4 * 1024 * 1024).await.unwrap();
    Reply { status, body: String::from_utf8_lossy(&bytes).to_string(), set_cookie, location, content_type }
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
        Some(serde_json::json!({
            "name": "테스트 사용자",
            "email": email,
            "password": "correct-horse-battery",
        })),
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
///
/// Puts the account on a plan first. Owning a broadcast is a paid entitlement
/// now, so every test that needs one needs a subscription to get there — Business
/// by default, because these tests are about ownership, metrics and stream keys
/// rather than about ceilings. A test that cares which plan sets its own with
/// `activate_subscription` before calling this.
async fn ready_broadcast(s: &Server, token: &str, user_id: &str, name: &str) -> (String, String) {
    if !s.app.db.subscription(user_id).unwrap().active {
        s.app.db.activate_subscription(user_id, "business").unwrap();
    }
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
    s.app.db.record_media_prepared(&m.id, &key, 60.0, 0, 20).unwrap();

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
    let a_id = get(&s, "/api/me", &a).await.id();
    let b_id = get(&s, "/api/me", &b).await.id();
    // A is a fully paid account. Otherwise `start` would refuse on the
    // subscription before it ever looked at whose broadcast this is, and the
    // test would be passing for the wrong reason — what is under test is
    // ownership, not billing.
    s.app.db.activate_subscription(&a_id, "business").unwrap();
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
    // Basic, whose entitlement is one concurrent stream.
    s.app.db.activate_subscription(&uid, "basic").unwrap();
    let (first, _) = ready_broadcast(&s, &token, &uid, "첫 방송").await;
    let (second, _) = ready_broadcast(&s, &token, &uid, "둘째 방송").await;

    let ok = post(&s, &format!("/api/broadcasts/{first}/start"), &token, None).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);

    let refused = post(&s, &format!("/api/broadcasts/{second}/start"), &token, None).await;
    assert_eq!(refused.status, StatusCode::PAYMENT_REQUIRED, "{}", refused.body);
    assert_eq!(
        refused.json()["error"],
        "Basic 요금제에서는 동시에 1개의 방송을 송출할 수 있습니다",
        "the refusal a user reads must name their plan, not a database key: {}",
        refused.body
    );

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
    // Says it was the one file, and on which plan — not the account total.
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
    let said =
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["error"].as_str().unwrap().to_string();
    assert!(said.contains("Tiny 플랜의 파일당 최대 용량"), "{said}");

    // No row, and nothing left in the upload directory.
    assert_eq!(s.app.db.media_for(&uid).unwrap().len(), 0);
    let leftovers = std::fs::read_dir(&s.app.upload_tmp).unwrap().count();
    assert_eq!(leftovers, 0, "a refused upload left its temp file behind");
}

#[tokio::test]
async fn an_upload_that_fits_one_file_but_not_the_account_says_so() {
    let s = server();
    let token = account(&s, "full@example.com").await;
    let uid = get(&s, "/api/me", &token).await.id();

    // Room for this file under the per-file ceiling, none left in the account.
    s.app
        .db
        .raw()
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO plans (id, label, limits) VALUES ('small','Small',?1)",
            [r#"{"max_concurrent_streams":1,"max_broadcasts":1,"max_storage_bytes":5000,"max_upload_bytes":8192}"#],
        )
        .unwrap();
    s.app.db.set_plan(&uid, "small").unwrap();
    s.app.db.create_media(&uid, "old.mp4", 2000, "k/old.mp4").unwrap();

    let boundary = "----louvertest";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"next.mp4\"\r\nContent-Type: video/mp4\r\n\r\n"
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
    assert_eq!(res.status(), StatusCode::PAYMENT_REQUIRED, "same status as before: the API contract holds");
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
    let said =
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["error"].as_str().unwrap().to_string();
    assert!(said.contains("Small 플랜 저장공간"), "{said}");
    assert!(!said.contains("파일당"), "an account-full refusal must not blame the file: {said}");

    assert_eq!(s.app.db.media_for(&uid).unwrap().len(), 1, "only the file that was already there");
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
        Some(serde_json::json!({
            "name": "중복",
            "email": "DUP@example.com",
            "password": "correct-horse-battery",
        })),
    )
    .await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.body);

    let weak = send(
        &s,
        Method::POST,
        "/api/auth/register",
        None,
        Some(serde_json::json!({ "name": "약함", "email": "weak@example.com", "password": "short" })),
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

#[tokio::test]
async fn health_answers_without_a_session_and_names_what_is_broken() {
    let s = server();
    let r = send(&s, Method::GET, "/health", None, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.json()["status"], "ok");
    // The four things the operator asked for, each its own answer.
    for check in ["api", "database", "ffmpeg", "ffmpeg_rtmps", "storage"] {
        assert_eq!(r.json()["checks"][check], true, "{check} failed: {}", r.body);
    }
    // And where this is running, which is what decides whether closing a
    // laptop ends a broadcast.
    assert!(["local", "cloud"].contains(&r.json()["deployment"].as_str().unwrap()));

    // A server whose FFmpeg is missing says so, and says it with a 503 so an
    // orchestrator does not have to read the body.
    let broken = Server {
        _dir: tempfile::tempdir().unwrap(),
        app: App { tools: FfmpegTools::new("/nonexistent/ffmpeg", "/nonexistent/ffprobe"), ..s.app.clone() },
    };
    let r = send(&broken, Method::GET, "/health", None, None).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.json()["status"], "degraded");
    assert_eq!(r.json()["checks"]["ffmpeg"], false);
    assert_eq!(r.json()["checks"]["ffmpeg_rtmps"], false, "a missing FFmpeg cannot speak rtmps");
    assert_eq!(r.json()["checks"]["database"], true, "one failure must not mask the rest");
}

#[tokio::test]
async fn metrics_are_per_account_and_carry_what_a_long_test_needs() {
    let s = server();
    let a = account(&s, "metrics-a@example.com").await;
    let b = account(&s, "metrics-b@example.com").await;
    let a_id = get(&s, "/api/me", &a).await.id();
    let (broadcast, _) = ready_broadcast(&s, &a, &a_id, "지표 확인").await;
    post(&s, &format!("/api/broadcasts/{broadcast}/start"), &a, None).await;

    let mine = get(&s, "/api/metrics", &a).await;
    assert_eq!(mine.status, StatusCode::OK, "{}", mine.body);
    let row = &mine.json()["broadcasts"][0];
    assert_eq!(row["id"], broadcast.as_str());
    for field in
        ["uptime_secs", "bytes_sent", "average_bitrate_bps", "restart_count", "last_error", "ffmpeg_pid"]
    {
        assert!(row.get(field).is_some(), "{field} missing from {}", mine.body);
    }
    assert!(mine.json()["server"]["memory_total_bytes"].as_u64().unwrap_or(0) > 0);

    // The other account sees its own nothing, not someone else's broadcast.
    let theirs = get(&s, "/api/metrics", &b).await;
    assert_eq!(theirs.json()["broadcasts"].as_array().unwrap().len(), 0);
    assert!(!theirs.body.contains(&broadcast));

    s.app.mgr.shutdown();
}

// --- §2, §15: the YouTube routes -------------------------------------------
//
// The provider itself is exercised against a fake Google in
// `crates/louver-cloud/tests/youtube_provider.rs`. What is under test here is
// the layer above it: who each route believes, and what it answers on a server
// that has no Google credentials — which is every server until an operator sets
// two environment variables, and therefore the state these routes are most
// likely to be met in.

#[tokio::test]
async fn the_youtube_routes_need_a_caller_like_every_other_route() {
    let s = server();
    for (method, path) in [
        (Method::GET, "/api/youtube"),
        (Method::GET, "/api/youtube/oauth/start"),
        (Method::GET, "/api/youtube/accounts"),
        (Method::DELETE, "/api/youtube/accounts/whatever"),
    ] {
        let r = send(&s, method.clone(), path, None, None).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{method} {path} answered {}: {}", r.status, r.body);
    }
}

#[tokio::test]
async fn an_unconfigured_server_says_so_rather_than_failing() {
    let s = server();
    let a = account(&s, "dj@example.com").await;

    let what = get(&s, "/api/youtube", &a).await;
    assert_eq!(what.status, StatusCode::OK, "{}", what.body);
    assert_eq!(what.json()["configured"], false);
    // The exact URI to register in the Google console, which is the single most
    // common thing to get wrong.
    assert!(what.json()["redirect_uri"].as_str().unwrap().ends_with("/api/youtube/oauth/callback"));

    // Starting a flow there is a 400 with an instruction, not a 500.
    let start = get(&s, "/api/youtube/oauth/start", &a).await;
    assert_eq!(start.status, StatusCode::BAD_REQUEST, "{}", start.body);
    assert!(start.body.contains("YOUTUBE_CLIENT_ID"), "{}", start.body);

    // And no account can exist yet.
    let list = get(&s, "/api/youtube/accounts", &a).await;
    assert_eq!(list.status, StatusCode::OK);
    assert_eq!(list.json().as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn the_callback_always_sends_the_browser_back_into_the_app() {
    let s = server();
    // Deliberately unauthenticated: the session cookie is SameSite=Strict, so a
    // browser arriving from accounts.google.com does not send it. A callback
    // that required one would fail for every real user.
    for (query, expect) in [
        ("?error=access_denied", "youtube=error"),
        ("?code=abc", "youtube=error"),  // no state
        ("?state=abc", "youtube=error"), // no code
        ("?code=abc&state=never-issued", "youtube=error"),
    ] {
        let r = send(&s, Method::GET, &format!("/api/youtube/oauth/callback{query}"), None, None).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{query} answered {}: {}", r.status, r.body);
        let to = location(&r);
        assert!(to.starts_with('/'), "the redirect has to stay on this host: {to}");
        assert!(to.contains(expect), "{query} → {to}");
        // A code is a credential; it must not be handed back to the browser.
        assert!(!to.contains("code=abc"), "{to}");
    }
}

#[tokio::test]
async fn one_account_cannot_disconnect_anothers_channel() {
    let s = server();
    let a = account(&s, "dj@example.com").await;
    let b = account(&s, "other@example.com").await;
    let me = s.app.db.user_by_email("dj@example.com").unwrap().id;

    // A connected channel, put in place directly: the consent flow is tested
    // against a fake Google elsewhere, and what matters here is the owner check.
    let mine = s.app.db.upsert_youtube_account(&me, "UC-1", "COLORISTE", None).unwrap().id;
    s.app.keys.set(&louver_cloud::youtube::refresh_account(&mine), "refresh-1").unwrap();

    // The other account cannot see it…
    let theirs = get(&s, "/api/youtube/accounts", &b).await;
    assert_eq!(theirs.json().as_array().unwrap().len(), 0);
    assert!(!theirs.body.contains(&mine));

    // …and knowing the id buys nothing.
    let stolen = send(&s, Method::DELETE, &format!("/api/youtube/accounts/{mine}"), Some(&b), None).await;
    assert_eq!(stolen.status, StatusCode::NOT_FOUND, "{}", stolen.body);
    assert_eq!(
        s.app.keys.get(&louver_cloud::youtube::refresh_account(&mine)).unwrap().as_deref(),
        Some("refresh-1"),
        "a refused request must not have deleted the owner's token"
    );

    // The owner can, and the token goes with the row.
    let gone = send(&s, Method::DELETE, &format!("/api/youtube/accounts/{mine}"), Some(&a), None).await;
    assert_eq!(gone.status, StatusCode::OK, "{}", gone.body);
    assert_eq!(s.app.keys.get(&louver_cloud::youtube::refresh_account(&mine)).unwrap(), None);
}

#[tokio::test]
async fn the_account_list_carries_no_token_and_no_key() {
    let s = server();
    let a = account(&s, "dj@example.com").await;
    let me = s.app.db.user_by_email("dj@example.com").unwrap().id;
    let id = s.app.db.upsert_youtube_account(&me, "UC-1", "COLORISTE", Some("https://yt/t.jpg")).unwrap().id;
    s.app.keys.set(&louver_cloud::youtube::refresh_account(&id), "refresh-1").unwrap();
    s.app.keys.set(&louver_cloud::youtube::access_account(&id), "access-1").unwrap();

    let list = get(&s, "/api/youtube/accounts", &a).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    let row = &list.json()[0];
    assert_eq!(row["channel_title"], "COLORISTE");
    assert_eq!(row["channel_id"], "UC-1");
    // §3: there is no field that could carry one, and this is the test that
    // fails if somebody adds one.
    for secret in ["refresh-1", "access-1", "refresh_token", "access_token"] {
        assert!(!list.body.contains(secret), "{secret} reached the browser: {}", list.body);
    }
}

#[tokio::test]
async fn asking_for_a_youtube_broadcast_on_an_unconfigured_server_leaves_nothing_behind() {
    let s = server();
    let a = account(&s, "dj@example.com").await;
    let me = s.app.db.user_by_email("dj@example.com").unwrap().id;
    let (_, media) = ready_broadcast(&s, &a, &me, "one").await;
    let account_id = s.app.db.upsert_youtube_account(&me, "UC-1", "COLORISTE", None).unwrap().id;

    let before = s.app.db.destinations_for(&me).unwrap().len();
    let made = post(
        &s,
        "/api/broadcasts",
        &a,
        Some(serde_json::json!({
            "name": "유튜브 방송",
            "youtube_account_id": account_id,
            "media_ids": [media],
        })),
    )
    .await;
    assert_eq!(made.status, StatusCode::BAD_REQUEST, "{}", made.body);

    // Nothing half-made: no broadcast, and no destination pointing at an
    // address YouTube never gave us.
    let list = get(&s, "/api/broadcasts", &a).await;
    assert_eq!(list.json()["broadcasts"].as_array().unwrap().len(), 1, "{}", list.body);
    assert_eq!(s.app.db.destinations_for(&me).unwrap().len(), before);
}

fn location(r: &Reply) -> String {
    r.location.clone().unwrap_or_default()
}

// --- signup ----------------------------------------------------------------
//
// Validation lives twice: once in the browser so the form can answer without a
// round trip, and once here because a request does not have to come from the
// form. These test the copy that decides.

/// The signup body a browser sends, with one field overridden.
fn signup(over: serde_json::Value) -> serde_json::Value {
    let mut body = serde_json::json!({
        "name": "홍길동",
        "email": "new@example.com",
        "password": "correct-horse-battery",
    });
    for (k, v) in over.as_object().expect("an object").clone() {
        body[k] = v;
    }
    body
}

async fn register(s: &Server, body: serde_json::Value) -> Reply {
    send(s, Method::POST, "/api/auth/register", None, Some(body)).await
}

#[tokio::test]
async fn signing_up_stores_the_name_leaves_you_unsubscribed_and_signs_you_in() {
    let s = server();
    let r = register(&s, signup(serde_json::json!({}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);

    // The reply carries the name, and no credential of any kind.
    assert_eq!(r.json()["name"], "홍길동");
    assert_eq!(r.json()["email"], "new@example.com");
    assert_eq!(r.json()["plan_id"], louver_cloud::db::UNSUBSCRIBED_PLAN);
    for secret in ["correct-horse-battery", "password", "token", "louver_session"] {
        assert!(!r.body.contains(secret), "{secret} reached the browser: {}", r.body);
    }

    // Signed in already: the cookie works on the next request.
    let token = r.set_cookie.clone().expect("a session cookie");
    let cookie = token.split(';').next().unwrap().trim_start_matches("louver_session=").to_string();
    let me = get(&s, "/api/me", &cookie).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.body);
    assert_eq!(me.json()["name"], "홍길동");

    // A subscription row exists — a user without one is a user whose limits come
    // from a fallback — and it is an unsubscribed one. Signing up is not a way to
    // get a paid entitlement.
    let uid = s.app.db.user_by_email("new@example.com").unwrap().id;
    let sub = s.app.db.subscription(&uid).unwrap();
    assert_eq!(sub.plan_id, s.app.db.user(&uid).unwrap().plan_id);
    assert_eq!(sub.status, "unsubscribed");
    assert!(!sub.active);

    // §6: the consent timestamps are the server's, and they are set.
    let user = s.app.db.user(&uid).unwrap();
    assert!(user.terms_accepted_at.is_some(), "agreeing has to be recorded");
    assert!(user.privacy_accepted_at.is_some());
}

#[tokio::test]
async fn a_name_is_stored_trimmed() {
    let s = server();
    let r = register(&s, signup(serde_json::json!({ "name": "  홍길동  " }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.json()["name"], "홍길동");
    assert_eq!(s.app.db.user_by_email("new@example.com").unwrap().name.as_deref(), Some("홍길동"));
}

#[tokio::test]
async fn a_name_that_is_not_one_is_refused_and_no_account_is_made() {
    let s = server();
    for name in [serde_json::json!(""), serde_json::json!("    "), serde_json::json!("가".repeat(61))] {
        let r = register(&s, signup(serde_json::json!({ "name": name }))).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{} → {}", name, r.body);
        assert!(r.set_cookie.is_none(), "a refused signup must not sign anybody in");
        assert!(s.app.db.user_by_email("new@example.com").is_err(), "an account was made anyway");
    }
}

#[tokio::test]
async fn an_unusable_email_or_a_short_password_is_refused_before_a_row_exists() {
    let s = server();
    let cases = [
        (serde_json::json!({ "email": "not-an-email" }), "이메일"),
        (serde_json::json!({ "email": "me@example" }), "이메일"),
        (serde_json::json!({ "password": "짧아요" }), "10자"),
        (serde_json::json!({ "password": "123456789" }), "10자"),
    ];
    for (over, expect) in cases {
        let r = register(&s, signup(over.clone())).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{over} → {}", r.body);
        assert!(r.body.contains(expect), "{over} → {}", r.body);
        // Nothing internal reaches the browser.
        for leak in ["SQLite", "sqlite", "UNIQUE", "constraint", "panicked", "/home/"] {
            assert!(!r.body.contains(leak), "{leak} leaked: {}", r.body);
        }
    }
    assert_eq!(s.app.db.all_user_ids().unwrap().len(), 0, "a refused signup left a row behind");
}

#[tokio::test]
async fn the_same_email_cannot_be_registered_twice() {
    let s = server();
    assert_eq!(register(&s, signup(serde_json::json!({}))).await.status, StatusCode::OK);

    // Same address, different case and padding: the column is NOCASE and the
    // value is normalised, so this is the same account.
    let again = register(&s, signup(serde_json::json!({ "email": " NEW@Example.com " }))).await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.body);
    assert!(again.json()["error"].as_str().unwrap().contains("이메일"), "{}", again.body);
    assert!(again.set_cookie.is_none());
    // No raw database words, and exactly one account.
    assert!(!again.body.contains("UNIQUE"), "{}", again.body);
    assert_eq!(s.app.db.all_user_ids().unwrap().len(), 1);
}

#[tokio::test]
async fn two_simultaneous_signups_for_one_address_produce_one_account() {
    let s = server();
    // The UNIQUE index decides, not a SELECT before the insert — which is the
    // only arrangement that survives this.
    let both = tokio::join!(
        register(&s, signup(serde_json::json!({}))),
        register(&s, signup(serde_json::json!({}))),
    );
    let statuses = [both.0.status, both.1.status];
    assert!(statuses.contains(&StatusCode::OK), "neither signup succeeded: {statuses:?}");
    assert!(statuses.contains(&StatusCode::CONFLICT), "both signups succeeded: {statuses:?}");
    assert_eq!(s.app.db.all_user_ids().unwrap().len(), 1);
}

#[tokio::test]
async fn signing_up_cannot_choose_a_plan_or_grant_itself_anything() {
    let s = server();
    for smuggled in [
        serde_json::json!({ "plan_id": "business" }),
        serde_json::json!({ "plan": "business" }),
        serde_json::json!({ "is_admin": true }),
        serde_json::json!({ "terms_accepted_at": "1999-01-01 00:00:00" }),
    ] {
        let mut body = signup(serde_json::json!({}));
        for (k, v) in smuggled.as_object().unwrap() {
            body[k] = v.clone();
        }
        let r = register(&s, body).await;
        // Refused outright rather than ignored, so a future reader cannot be
        // left guessing whether some path started honouring it. 422 is what axum
        // answers for a body that does not match the type — `deny_unknown_fields`
        // rejects before the handler runs, which is the point.
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{smuggled} → {} {}", r.status, r.body);
        assert!(s.app.db.user_by_email("new@example.com").is_err());
    }

    // And an honest signup gets no plan at all: the only way onto a paid one is
    // `activate_subscription`, which no route reaches.
    assert_eq!(register(&s, signup(serde_json::json!({}))).await.status, StatusCode::OK);
    let user = s.app.db.user_by_email("new@example.com").unwrap();
    assert_eq!(user.plan_id, louver_cloud::db::UNSUBSCRIBED_PLAN);
    assert!(!s.app.db.subscription(&user.id).unwrap().active);
}

#[tokio::test]
async fn an_account_made_before_signup_asked_for_a_name_still_signs_in() {
    let s = server();
    // Exactly what the bootstrap CLI and every earlier release wrote: no name,
    // no consent timestamps.
    let hash = louver_cloud::credentials::hash_password("correct-horse-battery").unwrap();
    let legacy = s.app.db.create_user("owner@example.com", &hash, "business").unwrap();
    assert!(legacy.name.is_none(), "create_user must keep writing a nameless account");
    assert!(legacy.terms_accepted_at.is_none(), "a script cannot agree on somebody's behalf");

    let r = send(
        &s,
        Method::POST,
        "/api/auth/login",
        None,
        Some(serde_json::json!({ "email": "owner@example.com", "password": "correct-horse-battery" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "a legacy account must still sign in: {}", r.body);
    // `name` is present and null rather than missing, so a client can tell the
    // difference between "no name" and "an older server".
    assert!(r.json()["name"].is_null(), "{}", r.body);
    assert_eq!(r.json()["plan_id"], "business", "and keeps the plan it was given");

    let cookie =
        r.set_cookie.unwrap().split(';').next().unwrap().trim_start_matches("louver_session=").to_string();
    let me = get(&s, "/api/me", &cookie).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.body);
    assert!(me.json()["name"].is_null());
}

#[tokio::test]
async fn a_name_containing_markup_is_stored_as_typed_and_escaped_in_json() {
    let s = server();
    // React renders text, never HTML, so the defence is at the point of
    // rendering. What this checks is the layer below: the value is not mangled
    // on the way in, and it leaves as a JSON string rather than as markup.
    let r = register(&s, signup(serde_json::json!({ "name": "<script>alert(1)</script>" }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    // Stored and returned exactly as typed — mangling somebody's name would be a
    // bug, and truncating it at `<` would be a worse one.
    assert_eq!(r.json()["name"], "<script>alert(1)</script>");
    assert_eq!(
        s.app.db.user_by_email("new@example.com").unwrap().name.as_deref(),
        Some("<script>alert(1)</script>")
    );
    // It leaves as JSON, not as a document a browser would parse as markup.
    assert_eq!(r.content_type.as_deref(), Some("application/json"));
    // And it survives a round trip through the parser, which is what React is
    // handed — React renders it as text, so this is where the defence ends.
    assert!(r.json().is_object(), "{}", r.body);
}

// --- plans and subscriptions ------------------------------------------------

#[tokio::test]
async fn the_price_list_is_public_and_is_the_only_place_prices_come_from() {
    let s = server();
    // No session: a price list nobody can read before signing up is not one.
    let r = send(&s, Method::GET, "/api/plans", None, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);

    let plans = r.json();
    let rows = plans.as_array().expect("an array");
    assert_eq!(rows.len(), 3, "{}", r.body);

    let expected =
        [("basic", "Basic", 19_900, 1), ("pro", "Pro", 39_900, 2), ("business", "Business", 59_900, 3)];
    for (i, (id, label, price, streams)) in expected.iter().enumerate() {
        let p = &rows[i];
        assert_eq!(p["id"], *id, "order: {}", r.body);
        assert_eq!(p["label"], *label);
        assert_eq!(p["monthly_price_krw"], *price, "{id}");
        assert_eq!(p["limits"]["max_concurrent_streams"], *streams, "{id}");
        assert!(p["description"].as_str().is_some_and(|d| !d.is_empty()), "{id}");
        // Whole won: a price that arrived as 19900.0 would round correctly today
        // and surprise somebody later.
        assert!(p["monthly_price_krw"].is_i64(), "money must not be a float: {}", r.body);
    }

    // The unsubscribed plan is a state, not something to buy.
    assert!(!r.body.contains("\"none\""), "{}", r.body);
    assert!(!r.body.contains("요금제 없음"), "{}", r.body);
}

#[tokio::test]
async fn a_plan_an_operator_takes_off_sale_disappears_from_the_public_list() {
    let s = server();
    s.app.db.raw().lock().unwrap().execute("UPDATE plans SET active = 0 WHERE id = 'business'", []).unwrap();

    let r = send(&s, Method::GET, "/api/plans", None, None).await;
    let json = r.json();
    let ids: Vec<&str> = json.as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["basic", "pro"], "{}", r.body);
}

#[tokio::test]
async fn a_new_account_is_unsubscribed_and_is_told_so() {
    let s = server();
    let a = account(&s, "dj@example.com").await;

    let sub = get(&s, "/api/me/subscription", &a).await;
    assert_eq!(sub.status, StatusCode::OK, "{}", sub.body);
    assert_eq!(sub.json()["status"], "unsubscribed");
    assert_eq!(sub.json()["active"], false);
    assert!(sub.json()["plan"].is_null(), "there is no plan to report: {}", sub.body);

    // The dashboard says the same thing, so the banner needs no second request.
    let dash = get(&s, "/api/broadcasts", &a).await;
    assert_eq!(dash.json()["subscribed"], false, "{}", dash.body);
    assert_eq!(dash.json()["allowed"], 0);
}

#[tokio::test]
async fn an_unsubscribed_account_can_look_around_and_cannot_spend_anything() {
    let s = server();
    let a = account(&s, "dj@example.com").await;
    let me = s.app.db.user_by_email("dj@example.com").unwrap().id;

    // §8: signing in, the dashboard, the price list and the account all work.
    for path in ["/api/me", "/api/me/subscription", "/api/broadcasts", "/api/plans", "/api/media"] {
        assert_eq!(get(&s, path, &a).await.status, StatusCode::OK, "{path}");
    }

    // Making a broadcast is refused, with a sentence and a 402 rather than a
    // limit of zero out of zero.
    let src = s._dir.path().join("x.mp4");
    std::fs::write(&src, b"prepared video bytes").unwrap();
    let key = s.app.storage.put_file(&me, "x.mp4", &src).unwrap();
    let m = s.app.db.create_media(&me, "x.mp4", 20, &key).unwrap();
    s.app.db.record_media_prepared(&m.id, &key, 60.0, 0, 20).unwrap();
    let dest = post(
        &s,
        "/api/stream-destinations",
        &a,
        Some(serde_json::json!({
            "label": "내 채널",
            "rtmps_url": "rtmps://a.rtmps.youtube.com/live2",
            "stream_key": A_REAL_LOOKING_KEY,
        })),
    )
    .await;
    assert_eq!(dest.status, StatusCode::OK, "{}", dest.body);

    let made = post(
        &s,
        "/api/broadcasts",
        &a,
        Some(serde_json::json!({ "name": "밤 라디오", "media_id": m.id, "destination_id": dest.id() })),
    )
    .await;
    assert_eq!(made.status, StatusCode::PAYMENT_REQUIRED, "{}", made.body);
    assert!(made.body.contains("요금제"), "{}", made.body);
}

#[tokio::test]
async fn an_unsubscribed_account_cannot_start_a_broadcast_it_already_owns() {
    let s = server();
    let a = account(&s, "dj@example.com").await;
    let me = s.app.db.user_by_email("dj@example.com").unwrap().id;

    // Paid for, made, then cancelled — a lapsed card, not a new signup.
    s.app.db.activate_subscription(&me, "basic").unwrap();
    let (broadcast, _) = ready_broadcast(&s, &a, &me, "one").await;
    s.app.db.cancel_subscription(&me).unwrap();

    let start = post(&s, &format!("/api/broadcasts/{broadcast}/start"), &a, None).await;
    assert_eq!(start.status, StatusCode::PAYMENT_REQUIRED, "{}", start.body);
    assert!(start.body.contains("활성화된 요금제가 필요합니다"), "{}", start.body);
    // Nothing started, and the broadcast is not left wanting to run.
    assert_eq!(s.app.db.broadcast(&broadcast).unwrap().desired_state, louver_cloud::DesiredState::Stopped);
    // The broadcast and the video are still theirs: cancelling took the
    // entitlement, not the work.
    assert_eq!(get(&s, "/api/broadcasts", &a).await.json()["broadcasts"].as_array().unwrap().len(), 1);

    s.app.mgr.shutdown();
}

#[tokio::test]
async fn the_concurrency_refusal_names_the_plan_and_the_number() {
    let s = server();
    let a = account(&s, "dj@example.com").await;
    let me = s.app.db.user_by_email("dj@example.com").unwrap().id;
    s.app.db.activate_subscription(&me, "basic").unwrap();

    let (first, media) = ready_broadcast(&s, &a, &me, "one").await;
    let second = post(
        &s,
        "/api/broadcasts",
        &a,
        Some(serde_json::json!({ "name": "둘", "media_id": media, "destination_id": s.app.db.destinations_for(&me).unwrap()[0].id })),
    )
    .await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.body);

    assert_eq!(post(&s, &format!("/api/broadcasts/{first}/start"), &a, None).await.status, StatusCode::OK);
    let refused = post(&s, &format!("/api/broadcasts/{}/start", second.id()), &a, None).await;
    assert_eq!(refused.status, StatusCode::PAYMENT_REQUIRED, "{}", refused.body);
    assert_eq!(
        refused.json()["error"],
        "Basic 요금제에서는 동시에 1개의 방송을 송출할 수 있습니다",
        "{}",
        refused.body
    );

    s.app.mgr.shutdown();
}

#[tokio::test]
async fn there_is_no_route_through_which_a_user_can_give_themselves_a_plan() {
    let s = server();
    let a = account(&s, "dj@example.com").await;
    let me = s.app.db.user_by_email("dj@example.com").unwrap().id;

    // Every shape somebody would try. None of these routes exists, and the point
    // of the test is that adding one would fail here.
    let attempts: &[(Method, &str)] = &[
        (Method::POST, "/api/me/subscription"),
        (Method::PUT, "/api/me/subscription"),
        (Method::PATCH, "/api/me/subscription"),
        (Method::POST, "/api/me/plan"),
        (Method::PUT, "/api/me/plan"),
        (Method::POST, "/api/plans"),
        (Method::POST, "/api/subscriptions"),
        (Method::POST, "/api/set-plan"),
        (Method::POST, "/api/billing/activate"),
    ];
    for (method, path) in attempts {
        let r = send(
            &s,
            method.clone(),
            path,
            Some(&a),
            Some(serde_json::json!({ "plan_id": "business", "status": "active" })),
        )
        .await;
        assert!(
            r.status == StatusCode::NOT_FOUND || r.status == StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path} answered {} — a user must not be able to grant themselves a plan: {}",
            r.status,
            r.body
        );
    }

    // And after all of that they are still unsubscribed.
    assert!(!s.app.db.subscription(&me).unwrap().active);
    assert_eq!(get(&s, "/api/me/subscription", &a).await.json()["status"], "unsubscribed");
}

#[tokio::test]
async fn a_paid_account_is_told_which_plan_and_how_many_streams() {
    let s = server();
    let a = account(&s, "dj@example.com").await;
    let me = s.app.db.user_by_email("dj@example.com").unwrap().id;
    // What a verified payment will do. Not reachable from a browser.
    s.app.db.activate_subscription(&me, "pro").unwrap();

    let sub = get(&s, "/api/me/subscription", &a).await;
    assert_eq!(sub.json()["status"], "active");
    assert_eq!(sub.json()["active"], true);
    assert_eq!(sub.json()["plan"]["id"], "pro");
    assert_eq!(sub.json()["plan"]["label"], "Pro");
    assert_eq!(sub.json()["plan"]["limits"]["max_concurrent_streams"], 2);

    let dash = get(&s, "/api/broadcasts", &a).await;
    assert_eq!(dash.json()["subscribed"], true);
    assert_eq!(dash.json()["plan_label"], "Pro");
    assert_eq!(dash.json()["allowed"], 2);
}

#[tokio::test]
async fn the_operators_business_account_survives_and_keeps_its_three_streams() {
    let s = server();
    // Exactly what the deploy script runs: `--create-user … --plan business`.
    let hash = louver_cloud::credentials::hash_password("correct-horse-battery").unwrap();
    let ops = s.app.db.create_user("freemilesarea@example.com", &hash, "business").unwrap();

    let sub = s.app.db.subscription(&ops.id).unwrap();
    assert!(sub.active, "the operator's account must not be unsubscribed by any of this");
    assert_eq!(sub.plan_id, "business");
    assert_eq!(sub.plan.as_ref().map(|p| p.max_concurrent_streams()), Some(3));

    // And it signs in and reads its plan over HTTP.
    let r = send(
        &s,
        Method::POST,
        "/api/auth/login",
        None,
        Some(serde_json::json!({
            "email": "freemilesarea@example.com",
            "password": "correct-horse-battery",
        })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let cookie =
        r.set_cookie.unwrap().split(';').next().unwrap().trim_start_matches("louver_session=").to_string();
    let dash = get(&s, "/api/broadcasts", &cookie).await;
    assert_eq!(dash.json()["plan_label"], "Business");
    assert_eq!(dash.json()["allowed"], 3);
    assert_eq!(dash.json()["subscribed"], true);
}

// --- billing over HTTP ------------------------------------------------------
//
// The provider itself is exercised against a fake PayApp in
// `crates/louver-cloud/tests/payapp.rs`. What is under test here is the layer
// above: who each route believes, what a server with no PayApp credentials
// answers, and — the one that matters most — that no route a browser can reach
// grants a paid entitlement.

/// A PayApp that answers the way the documentation says, and records what it got.
#[derive(Debug, Default)]
struct FakePayapp {
    calls: std::sync::Mutex<Vec<std::collections::BTreeMap<String, String>>>,
    /// Scripted answers, popped from the front. Empty means "agree".
    replies: std::sync::Mutex<Vec<String>>,
}

impl louver_cloud::billing::FormPost for FakePayapp {
    fn post_form(&self, _url: &str, fields: &[(&str, &str)]) -> louver_cloud::Result<String> {
        self.calls.lock().unwrap().push(fields.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect());
        let scripted = {
            let mut r = self.replies.lock().unwrap();
            if r.is_empty() {
                None
            } else {
                Some(r.remove(0))
            }
        };
        Ok(scripted.unwrap_or_else(|| {
            format!(
                "state=1&errno=00000&rebill_no=778899&payurl={}",
                louver_cloud::billing::urlencode("https://payapp.kr/pay/778899")
            )
        }))
    }
}

const PAY_USERID: &str = "247streams";
const PAY_LINKKEY: &str = "http-test-link-key";
const PAY_LINKVAL: &str = "http-test-link-val";

/// The same server, with billing configured.
fn billing_server() -> (Server, Arc<FakePayapp>) {
    let s = server();
    let api = Arc::new(FakePayapp::default());
    let config = louver_cloud::billing::Config {
        userid: PAY_USERID.into(),
        linkkey: PAY_LINKKEY.into(),
        linkval: PAY_LINKVAL.into(),
        api_url: "https://fake.payapp.test/oapi/apiLoad.html".into(),
        public_url: "https://247streams.kr".into(),
    };
    let payapp = louver_cloud::billing::Payapp::new(
        s.app.db.clone(),
        Arc::clone(&api) as Arc<dyn louver_cloud::billing::FormPost>,
        config,
    );
    (Server { _dir: s._dir, app: App { payapp: Some(payapp), ..s.app } }, api)
}

/// POST a form body, as PayApp's server-to-server notification does.
async fn post_form(s: &Server, path: &str, fields: &[(&str, &str)]) -> Reply {
    let body = fields
        .iter()
        .map(|(k, v)| {
            format!("{}={}", louver_cloud::billing::urlencode(k), louver_cloud::billing::urlencode(v))
        })
        .collect::<Vec<_>>()
        .join("&");
    let req = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = louver_server::router(s.app.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    Reply {
        status,
        body: String::from_utf8_lossy(&bytes).to_string(),
        set_cookie: None,
        location: None,
        content_type: None,
    }
}

/// A verified notification for one order, with fields overridden.
fn notification(billing_id: &str, over: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut f: Vec<(String, String)> = [
        ("userid", PAY_USERID),
        ("linkkey", PAY_LINKKEY),
        ("linkval", PAY_LINKVAL),
        ("goodname", "247streams Pro"),
        ("price", "39900"),
        ("recvphone", "01012345678"),
        ("pay_date", "2026-09-27 12:00:05"),
        ("pay_type", "card"),
        ("pay_state", "4"),
        ("mul_no", "990001"),
        ("rebill_no", "778899"),
        ("var1", billing_id),
        ("var2", "pro"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    for (k, v) in over {
        match f.iter_mut().find(|(key, _)| key == k) {
            Some(slot) => slot.1 = v.to_string(),
            None => f.push((k.to_string(), v.to_string())),
        }
    }
    f
}

fn borrowed(fields: &[(String, String)]) -> Vec<(&str, &str)> {
    fields.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

#[tokio::test]
async fn the_billing_routes_need_a_caller_except_the_two_payapp_posts_to() {
    let (s, _) = billing_server();
    for (method, path) in [(Method::GET, "/api/billing/status"), (Method::POST, "/api/billing/cancel")] {
        let r = send(&s, method.clone(), path, None, None).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{method} {path}: {}", r.body);
    }
    let r = send(
        &s,
        Method::POST,
        "/api/billing/checkout",
        None,
        Some(serde_json::json!({ "plan_id": "pro", "recvphone": "01012345678" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.body);
}

#[tokio::test]
async fn a_server_without_payapp_credentials_says_so_and_still_works() {
    // The `server()` harness has no provider, which is every deployment until an
    // operator sets three environment variables.
    let s = server();
    let a = account(&s, "dj@example.com").await;

    let checkout = post(
        &s,
        "/api/billing/checkout",
        &a,
        Some(serde_json::json!({ "plan_id": "pro", "recvphone": "01012345678" })),
    )
    .await;
    assert_eq!(checkout.status, StatusCode::BAD_REQUEST, "{}", checkout.body);
    assert!(checkout.body.contains("결제 시스템이 아직 설정되지 않았습니다"), "{}", checkout.body);

    // And everything else answers.
    let status = get(&s, "/api/billing/status", &a).await;
    assert_eq!(status.status, StatusCode::OK, "{}", status.body);
    assert_eq!(status.json()["configured"], false);
    assert!(status.json()["subscription"].is_null());
    assert_eq!(get(&s, "/api/plans", &a).await.status, StatusCode::OK);
}

#[tokio::test]
async fn checkout_returns_a_payurl_and_grants_nothing() {
    let (s, api) = billing_server();
    let a = account(&s, "dj@example.com").await;
    let me = s.app.db.user_by_email("dj@example.com").unwrap().id;

    let r = post(
        &s,
        "/api/billing/checkout",
        &a,
        Some(serde_json::json!({ "plan_id": "pro", "recvphone": "010-1234-5678" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.json()["payurl"], "https://payapp.kr/pay/778899");
    assert_eq!(r.json()["amount_krw"], 39900);
    assert_eq!(r.json()["plan_id"], "pro");

    // The provider was asked, with the price from the plans table.
    let call = api.calls.lock().unwrap()[0].clone();
    assert_eq!(call.get("cmd").unwrap(), "rebillRegist");
    assert_eq!(call.get("goodprice").unwrap(), "39900");

    // §5: nothing is entitled yet.
    assert!(!s.app.db.subscription(&me).unwrap().active);
    assert_eq!(get(&s, "/api/me/subscription", &a).await.json()["status"], "unsubscribed");
    assert_eq!(get(&s, "/api/broadcasts", &a).await.json()["allowed"], 0);

    // §15: no credential in the response.
    for secret in [PAY_LINKKEY, PAY_LINKVAL] {
        assert!(!r.body.contains(secret), "{}", r.body);
    }
}

#[tokio::test]
async fn a_checkout_body_cannot_name_its_own_price() {
    let (s, _) = billing_server();
    let a = account(&s, "dj@example.com").await;
    let me = s.app.db.user_by_email("dj@example.com").unwrap().id;

    // Refused outright rather than ignored: `deny_unknown_fields` means a future
    // reader cannot wonder whether some path started honouring it.
    for smuggled in [
        serde_json::json!({ "plan_id": "business", "recvphone": "01012345678", "amount_krw": 100 }),
        serde_json::json!({ "plan_id": "business", "recvphone": "01012345678", "goodprice": 100 }),
        serde_json::json!({ "plan_id": "business", "recvphone": "01012345678", "price": 0 }),
        serde_json::json!({ "plan_id": "business", "recvphone": "01012345678", "user_id": "someone" }),
    ] {
        let r = post(&s, "/api/billing/checkout", &a, Some(smuggled.clone())).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{smuggled} → {}", r.body);
    }

    // And an honest one is charged what the server says.
    let ok = post(
        &s,
        "/api/billing/checkout",
        &a,
        Some(serde_json::json!({ "plan_id": "business", "recvphone": "01012345678" })),
    )
    .await;
    assert_eq!(ok.json()["amount_krw"], 59900);
    assert!(!s.app.db.subscription(&me).unwrap().active);
}

#[tokio::test]
async fn a_plan_nobody_can_buy_cannot_be_checked_out() {
    let (s, api) = billing_server();
    let a = account(&s, "dj@example.com").await;

    for plan in ["none", "desktop", "enterprise", ""] {
        let r = post(
            &s,
            "/api/billing/checkout",
            &a,
            Some(serde_json::json!({ "plan_id": plan, "recvphone": "01012345678" })),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{plan} → {}", r.body);
    }
    assert!(api.calls.lock().unwrap().is_empty(), "nothing may reach the provider");
}

#[tokio::test]
async fn only_a_verified_notification_activates_a_subscription() {
    let (s, _) = billing_server();
    let a = account(&s, "dj@example.com").await;
    let me = s.app.db.user_by_email("dj@example.com").unwrap().id;
    let order = post(
        &s,
        "/api/billing/checkout",
        &a,
        Some(serde_json::json!({ "plan_id": "pro", "recvphone": "01012345678" })),
    )
    .await;
    let billing_id = order.json()["billing_id"].as_str().unwrap().to_string();

    // A forged notification, in each of the ways somebody would try it.
    for over in [
        vec![("linkkey", "wrong")],
        vec![("linkval", "wrong")],
        vec![("userid", "someone-else")],
        vec![("price", "100")],
        vec![("rebill_no", "000000")],
        vec![("var1", "not-an-order")],
    ] {
        let fields = notification(&billing_id, &over);
        let r = post_form(&s, "/api/billing/payapp/feedback", &borrowed(&fields)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{over:?} → {}", r.body);
        assert_eq!(r.body, "FAIL", "a forgery must not be acknowledged");
        assert!(!s.app.db.subscription(&me).unwrap().active, "{over:?} granted an entitlement");
    }

    // The real thing.
    let fields = notification(&billing_id, &[]);
    let ok = post_form(&s, "/api/billing/payapp/feedback", &borrowed(&fields)).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    // Exactly `SUCCESS`, no JSON, no redirect.
    assert_eq!(ok.body, "SUCCESS");

    let sub = get(&s, "/api/me/subscription", &a).await;
    assert_eq!(sub.json()["status"], "active");
    assert_eq!(sub.json()["plan"]["id"], "pro");
    assert_eq!(get(&s, "/api/broadcasts", &a).await.json()["allowed"], 2);
}

#[tokio::test]
async fn the_same_notification_ten_times_is_answered_success_and_applied_once() {
    let (s, _) = billing_server();
    let a = account(&s, "dj@example.com").await;
    let me = s.app.db.user_by_email("dj@example.com").unwrap().id;
    let order = post(
        &s,
        "/api/billing/checkout",
        &a,
        Some(serde_json::json!({ "plan_id": "basic", "recvphone": "01012345678" })),
    )
    .await;
    let fields = notification(order.json()["billing_id"].as_str().unwrap(), &[("price", "19900")]);

    for i in 0..10 {
        let r = post_form(&s, "/api/billing/payapp/feedback", &borrowed(&fields)).await;
        assert_eq!(r.status, StatusCode::OK, "call {i}: {}", r.body);
        assert_eq!(r.body, "SUCCESS", "call {i}");
    }

    assert_eq!(s.app.db.billing_events_for(&me, 50).unwrap().len(), 1, "one payment, one row");
    assert_eq!(s.app.db.billing_subscriptions_for(&me).unwrap().len(), 1);
    let sub = s.app.db.subscription(&me).unwrap();
    assert!(sub.active);
    assert_eq!(sub.plan_id, "basic");
    assert_eq!(s.app.db.limit(&me, louver_cloud::entitlement::MAX_CONCURRENT_STREAMS).unwrap(), 1);
}

#[tokio::test]
async fn arriving_at_the_return_url_activates_nothing() {
    // §11: a browser coming back from PayApp is not evidence of a payment.
    let (s, _) = billing_server();
    let a = account(&s, "dj@example.com").await;
    let me = s.app.db.user_by_email("dj@example.com").unwrap().id;
    post(
        &s,
        "/api/billing/checkout",
        &a,
        Some(serde_json::json!({ "plan_id": "pro", "recvphone": "01012345678" })),
    )
    .await;

    // Whatever the browser does on its way back, the answer is the same.
    for path in ["/api/me/subscription", "/api/billing/status"] {
        let r = get(&s, path, &a).await;
        assert_eq!(r.status, StatusCode::OK, "{path}");
    }
    assert_eq!(get(&s, "/api/me/subscription", &a).await.json()["status"], "unsubscribed");
    assert!(!s.app.db.subscription(&me).unwrap().active);
    // The record is visible as pending, which is what the completion page shows.
    let status = get(&s, "/api/billing/status", &a).await;
    assert_eq!(status.json()["subscription"]["status"], "pending");
    assert_eq!(status.json()["provider"], "payapp");
}

#[tokio::test]
async fn cancelling_revokes_the_paid_plan_straight_away() {
    let (s, api) = billing_server();
    let a = account(&s, "dj@example.com").await;
    let me = s.app.db.user_by_email("dj@example.com").unwrap().id;
    let order = post(
        &s,
        "/api/billing/checkout",
        &a,
        Some(serde_json::json!({ "plan_id": "pro", "recvphone": "01012345678" })),
    )
    .await;
    let fields = notification(order.json()["billing_id"].as_str().unwrap(), &[]);
    post_form(&s, "/api/billing/payapp/feedback", &borrowed(&fields)).await;
    assert!(s.app.db.subscription(&me).unwrap().active);

    let cancelled = post(&s, "/api/billing/cancel", &a, None).await;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.body);
    assert_eq!(cancelled.json()["status"], "cancelled");

    // PayApp was asked, with the four fields it documents.
    let call = api.calls.lock().unwrap().last().cloned().unwrap();
    assert_eq!(call.get("cmd").unwrap(), "rebillCancel");
    assert_eq!(call.get("rebill_no").unwrap(), "778899");
    assert!(call.contains_key("linkkey"));
    assert!(!call.contains_key("linkval"));

    // The entitlement is gone the moment the provider agreed, and the account
    // screen says so on its next question rather than on some later date.
    let sub = get(&s, "/api/me/subscription", &a).await;
    assert_eq!(sub.json()["status"], "unsubscribed");
    assert_eq!(sub.json()["active"], false);
    assert!(sub.json()["plan"].is_null());
    assert_eq!(get(&s, "/api/broadcasts", &a).await.json()["allowed"], 0);

    // And the billing panel's own endpoint agrees: 해지됨, with no plan behind it.
    let billing = get(&s, "/api/billing/status", &a).await;
    assert_eq!(billing.json()["subscription"]["status"], "cancelled");
    assert_eq!(billing.json()["plan"]["active"], false);
}

#[tokio::test]
async fn a_provider_that_refuses_the_cancellation_leaves_the_subscription_running() {
    // The user-facing half of the ordering rule: a failed `rebillCancel` is an
    // error, and the account is still paying and still able to broadcast.
    let (s, api) = billing_server();
    let a = account(&s, "dj@example.com").await;
    let me = s.app.db.user_by_email("dj@example.com").unwrap().id;
    let order = post(
        &s,
        "/api/billing/checkout",
        &a,
        Some(serde_json::json!({ "plan_id": "pro", "recvphone": "01012345678" })),
    )
    .await;
    let fields = notification(order.json()["billing_id"].as_str().unwrap(), &[]);
    post_form(&s, "/api/billing/payapp/feedback", &borrowed(&fields)).await;

    api.replies.lock().unwrap().push("state=0&errno=00009".into());
    let refused = post(&s, "/api/billing/cancel", &a, None).await;
    assert_ne!(refused.status, StatusCode::OK, "{}", refused.body);
    assert!(!refused.body.contains("link-"), "no credential in the answer: {}", refused.body);

    assert!(s.app.db.subscription(&me).unwrap().active);
    assert_eq!(get(&s, "/api/broadcasts", &a).await.json()["allowed"], 2);
    assert_eq!(get(&s, "/api/billing/status", &a).await.json()["subscription"]["status"], "active");

    // Retryable.
    assert_eq!(post(&s, "/api/billing/cancel", &a, None).await.status, StatusCode::OK);
    assert!(!s.app.db.subscription(&me).unwrap().active);
}

#[tokio::test]
async fn one_account_cannot_cancel_anothers_subscription() {
    let (s, api) = billing_server();
    let a = account(&s, "a@example.com").await;
    let b = account(&s, "b@example.com").await;
    let a_id = s.app.db.user_by_email("a@example.com").unwrap().id;
    let b_id = s.app.db.user_by_email("b@example.com").unwrap().id;

    // B pays; A has nothing.
    let order = post(
        &s,
        "/api/billing/checkout",
        &b,
        Some(serde_json::json!({ "plan_id": "pro", "recvphone": "01098765432" })),
    )
    .await;
    let fields = notification(order.json()["billing_id"].as_str().unwrap(), &[]);
    post_form(&s, "/api/billing/payapp/feedback", &borrowed(&fields)).await;
    assert!(s.app.db.subscription(&b_id).unwrap().active);
    let calls_before = api.calls.lock().unwrap().len();

    // A cancelling reaches only A's own, of which there is none.
    let r = post(&s, "/api/billing/cancel", &a, None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    assert!(r.body.contains("해지할 정기결제가 없습니다"), "{}", r.body);
    assert_eq!(api.calls.lock().unwrap().len(), calls_before, "the provider was not called");

    // B is untouched.
    assert!(s.app.db.subscription(&b_id).unwrap().active);
    assert_eq!(
        s.app.db.billing_subscriptions_for(&b_id).unwrap()[0].status,
        louver_cloud::BillingStatus::Active
    );
    // And A cannot see B's billing record.
    let a_status = get(&s, "/api/billing/status", &a).await;
    assert!(a_status.json()["subscription"].is_null(), "{}", a_status.body);
    let _ = a_id;
}

#[tokio::test]
async fn a_failed_renewal_is_recorded_without_taking_the_entitlement_away() {
    let (s, _) = billing_server();
    let a = account(&s, "dj@example.com").await;
    let me = s.app.db.user_by_email("dj@example.com").unwrap().id;
    let order = post(
        &s,
        "/api/billing/checkout",
        &a,
        Some(serde_json::json!({ "plan_id": "pro", "recvphone": "01012345678" })),
    )
    .await;
    let billing_id = order.json()["billing_id"].as_str().unwrap().to_string();
    post_form(&s, "/api/billing/payapp/feedback", &borrowed(&notification(&billing_id, &[]))).await;

    // A reversal next month, through the failure URL PayApp is given.
    let failed = notification(&billing_id, &[("pay_state", "9"), ("mul_no", "990009")]);
    let r = post_form(&s, "/api/billing/payapp/failure", &borrowed(&failed)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body, "SUCCESS");

    assert_eq!(
        s.app.db.billing_subscriptions_for(&me).unwrap()[0].status,
        louver_cloud::BillingStatus::PaymentFailed
    );
    // §8: recorded, and the broadcast stays on air.
    assert!(s.app.db.subscription(&me).unwrap().active);
    assert_eq!(get(&s, "/api/broadcasts", &a).await.json()["allowed"], 2);
}

#[tokio::test]
async fn a_notification_before_any_payment_never_grants_a_plan() {
    // §8's other half: a first payment that was never approved must not become an
    // entitlement by way of some other state arriving.
    let (s, _) = billing_server();
    let a = account(&s, "dj@example.com").await;
    let me = s.app.db.user_by_email("dj@example.com").unwrap().id;
    let order = post(
        &s,
        "/api/billing/checkout",
        &a,
        Some(serde_json::json!({ "plan_id": "business", "recvphone": "01012345678" })),
    )
    .await;
    let billing_id = order.json()["billing_id"].as_str().unwrap().to_string();

    for (i, state) in ["1", "10", "8", "32", "9", "64", "70", "71"].iter().enumerate() {
        let fields = notification(
            &billing_id,
            &[("pay_state", state), ("price", "59900"), ("mul_no", &format!("99{i}"))],
        );
        let r = post_form(&s, "/api/billing/payapp/feedback", &borrowed(&fields)).await;
        // Verified, so acknowledged — and nothing granted.
        assert_eq!(r.body, "SUCCESS", "pay_state={state}");
        assert!(!s.app.db.subscription(&me).unwrap().active, "pay_state={state} granted a plan");
    }
    assert_eq!(get(&s, "/api/broadcasts", &a).await.json()["allowed"], 0);
}

#[tokio::test]
async fn there_is_still_no_route_that_activates_a_subscription() {
    let (s, _) = billing_server();
    let a = account(&s, "dj@example.com").await;
    let me = s.app.db.user_by_email("dj@example.com").unwrap().id;

    // Every shape somebody would reach for, including the billing ones added here.
    let attempts: &[(Method, &str)] = &[
        (Method::POST, "/api/me/subscription"),
        (Method::PUT, "/api/me/subscription"),
        (Method::POST, "/api/me/plan"),
        (Method::POST, "/api/plans"),
        (Method::POST, "/api/set-plan"),
        (Method::POST, "/api/billing/activate"),
        (Method::POST, "/api/billing/subscriptions"),
        (Method::PUT, "/api/billing/status"),
        (Method::POST, "/api/subscriptions"),
    ];
    for (method, path) in attempts {
        let r = send(
            &s,
            method.clone(),
            path,
            Some(&a),
            Some(serde_json::json!({ "plan_id": "business", "status": "active" })),
        )
        .await;
        assert!(
            r.status == StatusCode::NOT_FOUND || r.status == StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path} answered {}: {}",
            r.status,
            r.body
        );
    }

    // The two notification routes exist, and a browser cannot use them to grant
    // itself anything: without the link keys they are refused.
    let r = post_form(&s, "/api/billing/payapp/feedback", &[("var1", "anything"), ("pay_state", "4")]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.body, "FAIL");

    assert!(!s.app.db.subscription(&me).unwrap().active);
    assert_eq!(get(&s, "/api/me/subscription", &a).await.json()["status"], "unsubscribed");
}

#[tokio::test]
async fn no_billing_response_carries_a_credential() {
    let (s, _) = billing_server();
    let a = account(&s, "dj@example.com").await;
    let order = post(
        &s,
        "/api/billing/checkout",
        &a,
        Some(serde_json::json!({ "plan_id": "pro", "recvphone": "01012345678" })),
    )
    .await;
    let fields = notification(order.json()["billing_id"].as_str().unwrap(), &[]);
    let feedback = post_form(&s, "/api/billing/payapp/feedback", &borrowed(&fields)).await;
    let status = get(&s, "/api/billing/status", &a).await;
    let cancelled = post(&s, "/api/billing/cancel", &a, None).await;

    for r in [&order, &feedback, &status, &cancelled] {
        for secret in [PAY_LINKKEY, PAY_LINKVAL, "linkkey", "linkval"] {
            assert!(!r.body.contains(secret), "{secret} reached a response: {}", r.body);
        }
    }
}

#[tokio::test]
async fn a_burst_of_password_guesses_is_refused_before_the_hash_is_computed() {
    // The CPU, not the account. Verifying a password is 600,000 PBKDF2 rounds by
    // design; on a two-core server a stream of guesses is a way to take the
    // machine away from the broadcasts on it without guessing anything.
    let s = server();
    account(&s, "dj@example.com").await;
    louver_server::throttle::shared().clear();

    let attempt = |ip: &'static str, password: &'static str, app: louver_server::state::App| async move {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/auth/login")
            .header("x-forwarded-for", ip)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "email": "dj@example.com", "password": password,
                }))
                .unwrap(),
            ))
            .unwrap();
        louver_server::router(app).oneshot(req).await.unwrap().status()
    };

    let limit = louver_server::throttle::MAX_ATTEMPTS;
    for i in 0..limit {
        let got = attempt("203.0.113.7", "not-the-password", s.app.clone()).await;
        assert_eq!(got, StatusCode::UNAUTHORIZED, "attempt {i} answered {got}");
    }
    assert_eq!(
        attempt("203.0.113.7", "not-the-password", s.app.clone()).await,
        StatusCode::TOO_MANY_REQUESTS,
        "the burst was never cut off",
    );
    // Even the right password waits its turn — the limit is on the caller, not
    // on being wrong.
    assert_eq!(
        attempt("203.0.113.7", "correct-horse-battery", s.app.clone()).await,
        StatusCode::TOO_MANY_REQUESTS,
    );
    // And somebody else signing in from another address is unaffected.
    assert_eq!(attempt("198.51.100.4", "correct-horse-battery", s.app.clone()).await, StatusCode::OK,);
    louver_server::throttle::shared().clear();
}
