//! Manual grants over HTTP: who may hand one out, and what a body may ask for.
//!
//! The entitlement arithmetic is proved in `louver-cloud`'s `manual_grants`.
//! What only this layer can get wrong is authorization — and in particular the
//! one shape this feature adds that the rest of the console does not have: a
//! request body that names *other people*. The ids in it are targets, and the
//! caller is still whoever the session cookie says, re-read from the database.

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

#[derive(Debug)]
struct Fake;

impl LauncherFactory for Fake {
    fn for_broadcast(&self, _db: &CloudDb, _id: &str) -> Arc<dyn StreamLauncher> {
        Arc::new(Fake)
    }
}

impl StreamLauncher for Fake {
    fn launch(&self, _s: &mut StreamSupervisor, _args: &[String]) -> CoreResult<Box<dyn ProcessHandle>> {
        Ok(Box::new(Idle(AtomicBool::new(false))))
    }
}

struct Idle(AtomicBool);

impl ProcessHandle for Idle {
    fn try_exited(&mut self) -> Option<bool> {
        self.0.load(Ordering::SeqCst).then_some(true)
    }
    fn terminate(&mut self) -> CoreResult<()> {
        self.0.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn pid(&self) -> Option<u32> {
        Some(4242)
    }
}

struct Server {
    _dir: tempfile::TempDir,
    app: App,
}

fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let db = CloudDb::open(&dir.path().join("cloud.db")).unwrap();
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
            machine: Arc::new(louver_server::state::Machine::default()),
        },
    }
}

struct Reply {
    status: StatusCode,
    body: String,
}

impl Reply {
    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
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
    let bytes = axum::body::to_bytes(res.into_body(), 4 * 1024 * 1024).await.unwrap();
    Reply { status, body: String::from_utf8_lossy(&bytes).to_string() }
}

async fn get(s: &Server, path: &str, token: &str) -> Reply {
    send(s, Method::GET, path, Some(token), None).await
}

async fn post(s: &Server, path: &str, token: &str, body: serde_json::Value) -> Reply {
    send(s, Method::POST, path, Some(token), Some(body)).await
}

async fn account(s: &Server, email: &str) -> String {
    let r = send(
        s,
        Method::POST,
        "/api/auth/register",
        None,
        Some(serde_json::json!({ "name": "사용자", "email": email, "password": "correct-horse-battery" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "register: {}", r.body);
    r.json()["id"].as_str().unwrap().to_string()
}

async fn token_for(s: &Server, email: &str) -> String {
    let res = louver_server::router(s.app.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "email": email, "password": "correct-horse-battery"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = res.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap();
    cookie.split(';').next().unwrap().trim_start_matches("louver_session=").to_string()
}

async fn admin(s: &Server, email: &str) -> String {
    account(s, email).await;
    s.app.db.set_role(email, "admin").unwrap();
    token_for(s, email).await
}

/// Every route this feature adds.
fn grant_routes(id: &str) -> Vec<(Method, String)> {
    vec![
        (Method::GET, "/api/admin/grants".to_string()),
        (Method::POST, "/api/admin/grants".to_string()),
        (Method::POST, format!("/api/admin/grants/{id}/extend")),
        (Method::POST, format!("/api/admin/grants/{id}/plan")),
        (Method::POST, format!("/api/admin/grants/{id}/revoke")),
    ]
}

// --- who may ----------------------------------------------------------------

/// Case 13 — nobody signed in reaches any of it.
#[tokio::test]
async fn without_a_session_every_grant_route_answers_401() {
    let s = server();
    for (method, path) in grant_routes("some-id") {
        let body = (method == Method::POST).then(|| serde_json::json!({}));
        let r = send(&s, method.clone(), &path, None, body).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{method} {path} answered {}", r.status);
    }
}

/// Case 12 — an ordinary member reaches none of it, and 403 rather than a
/// refusal that could be mistaken for a missing route.
#[tokio::test]
async fn an_ordinary_member_cannot_grant_anything() {
    let s = server();
    account(&s, "member@x.com").await;
    let token = token_for(&s, "member@x.com").await;
    let victim = account(&s, "victim@x.com").await;

    for (method, path) in grant_routes("some-id") {
        let body = (method == Method::POST).then(|| {
            serde_json::json!({
                "user_ids": [victim], "plan_id": "business", "days": 30,
                "reason": "스스로 지급", "days_": 1, "plan_id_": "business"
            })
        });
        let r = send(&s, method.clone(), &path, Some(&token), body).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{method} {path} answered {}", r.status);
    }

    // And nothing happened: the member did not grant themselves anything.
    let me = s.app.db.subscription(&s.app.db.user_by_email("member@x.com").unwrap().id).unwrap();
    assert!(!me.active, "a member granted themselves an entitlement");
    let rows: i64 = s
        .app
        .db
        .raw()
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM admin_grants", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0);
}

/// The body names targets, never the caller.
///
/// A member who learns an admin's id cannot become the granter by putting it in
/// a field: who is asking comes from the session, and the role is re-read from
/// the database on every request.
#[tokio::test]
async fn a_body_cannot_claim_to_be_an_operator() {
    let s = server();
    let admin_token = admin(&s, "boss@x.com").await;
    let boss_id = s.app.db.user_by_email("boss@x.com").unwrap().id;
    account(&s, "sneak@x.com").await;
    let sneak = token_for(&s, "sneak@x.com").await;
    let sneak_id = s.app.db.user_by_email("sneak@x.com").unwrap().id;

    // The member sends the admin's id in every field it could possibly mean
    // something in. Still 403.
    let r = post(
        &s,
        "/api/admin/grants",
        &sneak,
        serde_json::json!({
            "user_ids": [sneak_id],
            "plan_id": "business",
            "days": 30,
            "reason": "이벤트",
            "admin_id": boss_id,
            "granted_by": boss_id,
            "role": "admin"
        }),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);

    // The operator doing the same thing works, and the grant is recorded as
    // theirs rather than as whatever the body said.
    let r = post(
        &s,
        "/api/admin/grants",
        &admin_token,
        serde_json::json!({
            "user_ids": [sneak_id], "plan_id": "business", "days": 30,
            "reason": "6기 수강생 혜택", "granted_by": "nonsense"
        }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let who: String = s
        .app
        .db
        .raw()
        .lock()
        .unwrap()
        .query_row("SELECT granted_by_email FROM admin_grants LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(who, "boss@x.com", "the grant was attributed to the body, not the session");
}

/// A switched-off operator is refused like any other non-operator: the session
/// is gone and the login is blocked, so there is nothing left to hold.
#[tokio::test]
async fn a_disabled_operator_cannot_grant() {
    let s = server();
    let token = admin(&s, "ex@x.com").await;
    let target = account(&s, "t@x.com").await;
    let ex = s.app.db.user_by_email("ex@x.com").unwrap().id;

    // Works while they are an operator in good standing.
    let r = post(
        &s,
        "/api/admin/grants",
        &token,
        serde_json::json!({ "user_ids": [&target], "plan_id": "pro", "days": 7, "reason": "테스트" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);

    s.app.db.set_disabled(&ex, true).unwrap();

    let r = post(
        &s,
        "/api/admin/grants",
        &token,
        serde_json::json!({ "user_ids": [&target], "plan_id": "business", "days": 30, "reason": "또" }),
    )
    .await;
    assert_ne!(r.status, StatusCode::OK, "a disabled operator granted an entitlement: {}", r.body);
}

// --- what a body may ask for -----------------------------------------------

/// Case 10 and 11 over HTTP: many at once, and one bad id refuses the lot.
#[tokio::test]
async fn a_bulk_grant_is_all_or_nothing() {
    let s = server();
    let token = admin(&s, "boss@x.com").await;
    let a = account(&s, "a@x.com").await;
    let b = account(&s, "b@x.com").await;

    let r = post(
        &s,
        "/api/admin/grants",
        &token,
        serde_json::json!({
            "user_ids": [&a, &b, "no-such-user"],
            "plan_id": "business", "days": 30, "reason": "6기 수강생 혜택"
        }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    assert!(!s.app.db.subscription(&a).unwrap().active, "a refused batch granted something");

    // The same list without the bad id lands completely.
    let r = post(
        &s,
        "/api/admin/grants",
        &token,
        serde_json::json!({
            "user_ids": [&a, &b], "plan_id": "business", "days": 30,
            "reason": "6기 수강생 혜택"
        }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.json()["outcomes"].as_array().unwrap().len(), 2);
    for id in [&a, &b] {
        let sub = s.app.db.subscription(id).unwrap();
        assert!(sub.active);
        assert_eq!(sub.plan_id, "business");
    }
}

/// The per-request ceiling is enforced by the server, not the browser.
#[tokio::test]
async fn more_than_a_hundred_targets_is_refused() {
    let s = server();
    let token = admin(&s, "boss@x.com").await;
    let one = account(&s, "one@x.com").await;
    let many: Vec<&str> = (0..=louver_cloud::grants::MAX_BULK_TARGETS).map(|_| one.as_str()).collect();

    let r = post(
        &s,
        "/api/admin/grants",
        &token,
        serde_json::json!({ "user_ids": many, "plan_id": "pro", "days": 30, "reason": "대량" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    assert!(r.body.contains("100"), "the refusal should say the ceiling: {}", r.body);
}

/// A term has to be one shape or the other, and the dates have to be dates.
#[tokio::test]
async fn a_malformed_term_is_refused() {
    let s = server();
    let token = admin(&s, "boss@x.com").await;
    let u = account(&s, "u@x.com").await;
    let bad = [
        serde_json::json!({ "user_ids": [&u], "plan_id": "pro", "reason": "r" }),
        serde_json::json!({ "user_ids": [&u], "plan_id": "pro", "days": 30, "from": "2026-10-01", "to": "2026-10-30", "reason": "r" }),
        serde_json::json!({ "user_ids": [&u], "plan_id": "pro", "from": "2026-10-01", "reason": "r" }),
        serde_json::json!({ "user_ids": [&u], "plan_id": "pro", "from": "10/01/2026", "to": "2026-10-30", "reason": "r" }),
        serde_json::json!({ "user_ids": [&u], "plan_id": "pro", "from": "2026-10-30", "to": "2026-10-01", "reason": "r" }),
        serde_json::json!({ "user_ids": [&u], "plan_id": "pro", "days": 9999, "reason": "r" }),
        serde_json::json!({ "user_ids": [&u], "plan_id": "none", "days": 30, "reason": "r" }),
        serde_json::json!({ "user_ids": [&u], "plan_id": "pro", "days": 30, "reason": "   " }),
        serde_json::json!({ "user_ids": [], "plan_id": "pro", "days": 30, "reason": "r" }),
    ];
    for body in bad {
        let r = post(&s, "/api/admin/grants", &token, body.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "accepted {body}: {}", r.body);
    }
    let rows: i64 = s
        .app
        .db
        .raw()
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM admin_grants", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0);
}

/// An explicit window works, and lands on the Seoul day an operator typed.
#[tokio::test]
async fn an_explicit_window_is_stored_as_the_seoul_days_it_names() {
    let s = server();
    let token = admin(&s, "boss@x.com").await;
    let u = account(&s, "w@x.com").await;

    let r = post(
        &s,
        "/api/admin/grants",
        &token,
        serde_json::json!({
            "user_ids": [&u], "plan_id": "pro",
            "from": "2026-11-01", "to": "2026-11-30", "reason": "이벤트"
        }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);

    let (starts, expires): (String, String) = s
        .app
        .db
        .raw()
        .lock()
        .unwrap()
        .query_row("SELECT starts_at, expires_at FROM admin_grants LIMIT 1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    // Nine hours before Seoul midnight, both ends, and `to` inclusive.
    assert_eq!(starts, "2026-10-31 15:00:00", "start should be Seoul 11/01 00:00");
    assert_eq!(expires, "2026-11-30 15:00:00", "expiry should be Seoul 12/01 00:00");
}

// --- the rest of the life ---------------------------------------------------

/// Extend, change plan, revoke, and what the account is entitled to after each.
#[tokio::test]
async fn the_console_can_extend_change_and_revoke() {
    let s = server();
    let token = admin(&s, "boss@x.com").await;
    let u = account(&s, "life@x.com").await;
    s.app.db.set_plan(&u, "basic").unwrap();

    let r = post(
        &s,
        "/api/admin/grants",
        &token,
        serde_json::json!({ "user_ids": [&u], "plan_id": "business", "days": 30, "reason": "CS 보상" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let id = r.json()["outcomes"][0]["grant_id"].as_str().unwrap().to_string();
    assert_eq!(s.app.db.subscription(&u).unwrap().plan_id, "business");

    let r = post(
        &s,
        &format!("/api/admin/grants/{id}/extend"),
        &token,
        serde_json::json!({ "days": 30, "reason": "한 달 더" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let extended = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(s.app.db.subscription(&u).unwrap().plan_id, "business", "extending dropped the plan");

    let r = post(
        &s,
        &format!("/api/admin/grants/{extended}/plan"),
        &token,
        serde_json::json!({ "plan_id": "pro", "reason": "하향" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let changed = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(s.app.db.subscription(&u).unwrap().plan_id, "pro");

    // Revoked: straight back to the plan the account pays for, untouched.
    let r = post(
        &s,
        &format!("/api/admin/grants/{changed}/revoke"),
        &token,
        serde_json::json!({ "reason": "종료" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let sub = s.app.db.subscription(&u).unwrap();
    assert!(sub.active, "revoking a grant switched off a paying account");
    assert_eq!(sub.plan_id, "basic");

    // A revoke needs a reason.
    let r =
        post(&s, &format!("/api/admin/grants/{changed}/revoke"), &token, serde_json::json!({ "reason": "" }))
            .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

/// Case 14 — every action is in the audit log, with who and why, and the batch
/// can be found by its id.
#[tokio::test]
async fn every_grant_action_is_audited() {
    let s = server();
    let token = admin(&s, "boss@x.com").await;
    let a = account(&s, "aa@x.com").await;
    let b = account(&s, "bb@x.com").await;

    let r = post(
        &s,
        "/api/admin/grants",
        &token,
        serde_json::json!({
            "user_ids": [&a, &b], "plan_id": "business", "days": 30,
            "reason": "6기 수강생 혜택"
        }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let batch = r.json()["batch_id"].as_str().unwrap().to_string();
    let id = r.json()["outcomes"][0]["grant_id"].as_str().unwrap().to_string();

    post(
        &s,
        &format!("/api/admin/grants/{id}/extend"),
        &token,
        serde_json::json!({ "days": 7, "reason": "조금 더" }),
    )
    .await;

    let log = get(&s, "/api/admin/audit?limit=100", &token).await;
    assert_eq!(log.status, StatusCode::OK);
    let rows = log.json();
    let rows = rows.as_array().unwrap();
    let actions: Vec<&str> = rows.iter().filter_map(|r| r["action"].as_str()).collect();
    assert!(actions.contains(&"admin.manual_grant.bulk_create"), "{actions:?}");
    assert!(actions.contains(&"admin.manual_grant.create"), "{actions:?}");
    assert!(actions.contains(&"admin.manual_grant.extend"), "{actions:?}");

    // Who, and why, on every line.
    for r in rows.iter().filter(|r| r["action"].as_str().unwrap_or("").contains("manual_grant")) {
        assert_eq!(r["admin_email"].as_str(), Some("boss@x.com"));
        assert!(r["note"].as_str().is_some_and(|n| !n.is_empty()), "no reason on {r}");
    }
    // The batch line names the batch, so a bulk run can be found as one thing.
    let batch_line = rows
        .iter()
        .find(|r| r["action"] == "admin.manual_grant.bulk_create")
        .expect("the batch is its own line");
    assert_eq!(batch_line["target_id"].as_str(), Some(batch.as_str()));
    assert!(batch_line["after"].as_str().unwrap().contains("business"));
}

/// The console's own figures: a grant shows up where grants belong, and the
/// money is exactly where it was.
#[tokio::test]
async fn the_dashboard_keeps_grants_out_of_the_money() {
    let s = server();
    let token = admin(&s, "boss@x.com").await;
    let u = account(&s, "free@x.com").await;

    let before = get(&s, "/api/admin/dashboard", &token).await.json();

    post(
        &s,
        "/api/admin/grants",
        &token,
        serde_json::json!({ "user_ids": [&u], "plan_id": "business", "days": 3, "reason": "이벤트" }),
    )
    .await;

    let after = get(&s, "/api/admin/dashboard", &token).await.json();
    assert_eq!(after["revenue"], before["revenue"], "a grant moved revenue");
    assert_eq!(after["mrr_krw"], before["mrr_krw"], "a grant moved MRR");
    assert_eq!(
        after["subscriptions"]["paid_total"], before["subscriptions"]["paid_total"],
        "a grant was counted as a paying member",
    );
    // And it is counted, as its own thing.
    assert_eq!(after["grants"]["active"], 1);
    assert_eq!(after["grants"]["expiring_7d"], 1);
    assert_eq!(after["grants"]["by_plan"][0][0], "business");
}

/// The user list explains why an account is on the plan it is on, and the
/// filters find the accounts an operator is looking for.
#[tokio::test]
async fn the_user_list_shows_the_source_of_the_entitlement() {
    let s = server();
    let token = admin(&s, "boss@x.com").await;
    let paid = account(&s, "paid@x.com").await;
    s.app.db.set_plan(&paid, "basic").unwrap();
    let granted = account(&s, "granted@x.com").await;
    account(&s, "nobody@x.com").await;

    post(
        &s,
        "/api/admin/grants",
        &token,
        serde_json::json!({ "user_ids": [&granted, &paid], "plan_id": "business", "days": 20, "reason": "이벤트" }),
    )
    .await;

    let rows = get(&s, "/api/admin/users?limit=100", &token).await.json();
    let rows = rows.as_array().unwrap();
    let of = |email: &str| {
        rows.iter().find(|r| r["email"] == email).unwrap_or_else(|| panic!("{email} missing")).clone()
    };

    let g = of("granted@x.com");
    assert_eq!(g["plan_id"], "none", "the paid side must stay as it is");
    assert_eq!(g["effective_plan_id"], "business");
    assert_eq!(g["entitlement_source"], "grant");
    assert_eq!(g["grant_plan_id"], "business");
    assert!(g["grant_days_left"].as_i64().unwrap() >= 19);
    // The ceiling shown is the one in force.
    assert_eq!(g["storage_limit_bytes"].as_i64(), Some(60 * 1024 * 1024 * 1024));

    let p = of("paid@x.com");
    assert_eq!(p["plan_id"], "basic", "a grant rewrote the paid plan");
    assert_eq!(p["effective_plan_id"], "business");
    assert_eq!(p["entitlement_source"], "grant");

    let n = of("nobody@x.com");
    assert_eq!(n["effective_plan_id"], "none");
    assert_eq!(n["entitlement_source"], "none");
    assert!(n["grant_plan_id"].is_null());

    // Filters.
    let only_granted = get(&s, "/api/admin/users?filter=granted&limit=100", &token).await.json();
    let emails: Vec<&str> =
        only_granted.as_array().unwrap().iter().filter_map(|r| r["email"].as_str()).collect();
    assert_eq!(emails.len(), 2, "{emails:?}");
    assert!(emails.contains(&"granted@x.com") && emails.contains(&"paid@x.com"));

    let soon = get(&s, "/api/admin/users?filter=grant_30d&limit=100", &token).await.json();
    assert_eq!(soon.as_array().unwrap().len(), 2);
    let very_soon = get(&s, "/api/admin/users?filter=grant_7d&limit=100", &token).await.json();
    assert_eq!(very_soon.as_array().unwrap().len(), 0, "a 20-day grant is not ending within 7");

    let unsubscribed = get(&s, "/api/admin/users?filter=none&limit=100", &token).await.json();
    let emails: Vec<&str> =
        unsubscribed.as_array().unwrap().iter().filter_map(|r| r["email"].as_str()).collect();
    assert!(emails.contains(&"nobody@x.com"));
    assert!(!emails.contains(&"granted@x.com"), "an account with a live grant is not unsubscribed");
}

/// The member's own detail page carries the grant history, and no secret.
#[tokio::test]
async fn the_member_detail_explains_both_sources() {
    let s = server();
    let token = admin(&s, "boss@x.com").await;
    let u = account(&s, "both@x.com").await;
    s.app.db.set_plan(&u, "basic").unwrap();
    post(
        &s,
        "/api/admin/grants",
        &token,
        serde_json::json!({ "user_ids": [&u], "plan_id": "business", "days": 30, "reason": "6기 수강생 혜택" }),
    )
    .await;

    let d = get(&s, &format!("/api/admin/users/{u}"), &token).await;
    assert_eq!(d.status, StatusCode::OK, "{}", d.body);
    let v = d.json();
    assert_eq!(v["user"]["plan_id"], "basic");
    assert_eq!(v["user"]["effective_plan_id"], "business");
    let grants = v["grants"].as_array().unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0]["state"], "active");
    assert_eq!(grants[0]["plan_id"], "business");
    assert_eq!(grants[0]["granted_by_email"], "boss@x.com");
    assert_eq!(grants[0]["reason"], "6기 수강생 혜택");

    // Nothing sensitive anywhere in the body.
    for secret in ["password", "password_hash", "refresh_token", "access_token", "stream_key", "linkkey"] {
        assert!(!d.body.to_lowercase().contains(secret), "{secret} in the detail body");
    }
}

/// The grants page itself, and its filters.
#[tokio::test]
async fn the_grants_page_lists_and_filters() {
    let s = server();
    let token = admin(&s, "boss@x.com").await;
    let a = account(&s, "g1@x.com").await;
    let b = account(&s, "g2@x.com").await;
    post(
        &s,
        "/api/admin/grants",
        &token,
        serde_json::json!({ "user_ids": [&a], "plan_id": "business", "days": 3, "reason": "짧게" }),
    )
    .await;
    let r = post(
        &s,
        "/api/admin/grants",
        &token,
        serde_json::json!({ "user_ids": [&b], "plan_id": "pro", "days": 60, "reason": "길게" }),
    )
    .await;
    let long = r.json()["outcomes"][0]["grant_id"].as_str().unwrap().to_string();

    let all = get(&s, "/api/admin/grants", &token).await.json();
    assert_eq!(all.as_array().unwrap().len(), 2);

    let soon = get(&s, "/api/admin/grants?filter=expiring_7d", &token).await.json();
    assert_eq!(soon.as_array().unwrap().len(), 1);
    assert_eq!(soon[0]["email"], "g1@x.com");

    post(&s, &format!("/api/admin/grants/{long}/revoke"), &token, serde_json::json!({ "reason": "정리" }))
        .await;
    let revoked = get(&s, "/api/admin/grants?filter=revoked", &token).await.json();
    assert_eq!(revoked.as_array().unwrap().len(), 1);
    assert_eq!(revoked[0]["state"], "revoked");
    let active = get(&s, "/api/admin/grants?filter=active", &token).await.json();
    assert_eq!(active.as_array().unwrap().len(), 1);
}
