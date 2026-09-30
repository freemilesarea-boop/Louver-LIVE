//! The admin console over HTTP: who may reach it, and what it refuses to say.
//!
//! The data-layer tests in `louver-cloud` prove the figures are right. These
//! prove the two things only the HTTP layer can get wrong: authorization, and
//! what ends up in a response body.

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

// --- a process that does nothing, so a broadcast can be "running" ----------

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

/// An ordinary account, signed in.
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
    let r = send(
        s,
        Method::POST,
        "/api/auth/login",
        None,
        Some(serde_json::json!({ "email": email, "password": "correct-horse-battery" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "login: {}", r.body);
    // The token travels in the cookie; the tests use the bearer form.
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

/// An operator: an ordinary account whose role was set the way the CLI sets it.
async fn admin(s: &Server, email: &str) -> String {
    account(s, email).await;
    s.app.db.set_role(email, "admin").unwrap();
    token_for(s, email).await
}

/// Every route the console uses.
const ADMIN_ROUTES: [&str; 7] = [
    "/api/admin/dashboard",
    "/api/admin/revenue",
    "/api/admin/users",
    "/api/admin/broadcasts",
    "/api/admin/billing",
    "/api/admin/system",
    "/api/admin/audit",
];

// --- A, B: who may reach it -----------------------------------------------

#[tokio::test]
async fn without_a_session_every_admin_route_answers_401() {
    // Test B. And deliberately 401 rather than 404: an anonymous caller must not
    // be able to map the admin surface by watching which paths 404.
    let s = server();
    for path in ADMIN_ROUTES {
        let r = send(&s, Method::GET, path, None, None).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path} answered {}", r.status);
    }
    let r = send(
        &s,
        Method::POST,
        "/api/admin/users/anyone/disabled",
        None,
        Some(serde_json::json!({"disabled": true})),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_ordinary_user_is_refused_every_admin_route() {
    // Test A, and test of the IDOR shape: knowing somebody else's id buys
    // nothing, because the refusal happens before any id is read.
    let s = server();
    account(&s, "dj@example.com").await;
    let theirs = account(&s, "victim@example.com").await;
    let token = token_for(&s, "dj@example.com").await;

    for path in ADMIN_ROUTES {
        let r = get(&s, path, &token).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path} answered {} {}", r.status, r.body);
    }
    for path in [format!("/api/admin/users/{theirs}"), format!("/api/admin/users/{theirs}/payments")] {
        assert_eq!(get(&s, &path, &token).await.status, StatusCode::FORBIDDEN, "{path}");
    }
    let r = post(
        &s,
        &format!("/api/admin/users/{theirs}/disabled"),
        &token,
        serde_json::json!({"disabled": true}),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // And the victim is untouched.
    assert!(!s.app.db.user(&theirs).unwrap().is_disabled());
}

#[tokio::test]
async fn there_is_no_route_through_which_a_user_can_make_themselves_an_admin() {
    // The role is a database column and nothing a request can write. Registering
    // with it, or sending it to the profile routes, must not take.
    let s = server();
    let sneaky = send(
        &s,
        Method::POST,
        "/api/auth/register",
        None,
        Some(serde_json::json!({
            "name": "관리자", "email": "sneak@example.com",
            "password": "correct-horse-battery", "role": "admin",
        })),
    )
    .await;
    assert_eq!(
        sneaky.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an unknown field was accepted: {} {}",
        sneaky.status,
        sneaky.body
    );

    account(&s, "sneak2@example.com").await;
    let token = token_for(&s, "sneak2@example.com").await;
    assert_eq!(get(&s, "/api/me", &token).await.json()["role"], "user");
    assert_eq!(get(&s, "/api/admin/dashboard", &token).await.status, StatusCode::FORBIDDEN);
    assert!(s.app.db.admins().unwrap().is_empty());
}

// --- C–J: the figures ------------------------------------------------------

/// A paid account, made the way a verified notification makes one.
fn pay(s: &Server, user_id: &str, plan: &str, amount: i64, key: &str) -> String {
    let record = s.app.db.open_billing_subscription(user_id, plan, "payapp", amount).unwrap();
    s.app.db.attach_provider_subscription(&record.id, &format!("rebill-{key}")).unwrap();
    s.app
        .db
        .record_billing_payment(&louver_cloud::db::BillingPayment {
            provider: "payapp",
            event_key: key,
            billing_id: &record.id,
            user_id,
            provider_subscription_id: Some(&format!("rebill-{key}")),
            pay_state: "4",
            amount_krw: amount,
            pay_date: Some("2026-09-29 12:00:00"),
            pay_type: Some("card"),
            outcome: "결제완료",
            status: Some(louver_cloud::BillingStatus::Active),
            paid_at: Some("2026-09-29 12:00:00"),
            period_end: Some("2026-10-29"),
        })
        .unwrap();
    s.app.db.activate_subscription(user_id, plan).unwrap();
    record.id
}

#[tokio::test]
async fn the_dashboard_counts_what_is_actually_in_the_database() {
    // Tests C, D, E, F, G, H, I.
    let s = server();
    let token = admin(&s, "ops@example.com").await;
    let basic = account(&s, "basic@example.com").await;
    let pro = account(&s, "pro@example.com").await;
    account(&s, "free@example.com").await;
    pay(&s, &basic, "basic", 19_900, "evt-1");
    let pro_record = pay(&s, &pro, "pro", 39_900, "evt-2");

    // A repeat of the same provider event, and a failed one. Neither is money.
    s.app
        .db
        .record_billing_payment(&louver_cloud::db::BillingPayment {
            provider: "payapp",
            event_key: "evt-2", // the same event, delivered again
            billing_id: &pro_record,
            user_id: &pro,
            provider_subscription_id: Some("rebill-evt-2"),
            pay_state: "4",
            amount_krw: 39_900,
            pay_date: Some("2026-09-29 12:00:00"),
            pay_type: Some("card"),
            outcome: "결제완료",
            status: Some(louver_cloud::BillingStatus::Active),
            paid_at: Some("2026-09-29 12:00:00"),
            period_end: Some("2026-10-29"),
        })
        .unwrap();
    s.app
        .db
        .record_billing_payment(&louver_cloud::db::BillingPayment {
            provider: "payapp",
            event_key: "evt-3",
            billing_id: &pro_record,
            user_id: &pro,
            provider_subscription_id: Some("rebill-evt-2"),
            pay_state: "9",
            amount_krw: 39_900,
            pay_date: None,
            pay_type: Some("card"),
            outcome: "승인취소",
            // What `handle_feedback` does with a reversal: the record stops
            // being one the provider is collecting on.
            status: Some(louver_cloud::BillingStatus::PaymentFailed),
            paid_at: None,
            period_end: None,
        })
        .unwrap();

    let d = get(&s, "/api/admin/dashboard", &token).await;
    assert_eq!(d.status, StatusCode::OK, "{}", d.body);
    let j = d.json();
    // C, D: four accounts exist — three plus the operator.
    assert_eq!(j["users"]["total"], 4);
    assert_eq!(j["users"]["today"], 4);
    // E: by plan, from the entitlement the service honours.
    let by_plan = j["subscriptions"]["by_plan"].as_array().unwrap();
    let count =
        |id: &str| by_plan.iter().find(|p| p["plan_id"] == id).map(|p| p["count"].as_i64().unwrap()).unwrap();
    assert_eq!((count("basic"), count("pro"), count("business")), (1, 1, 0));
    assert_eq!(j["subscriptions"]["paid_total"], 2);
    assert_eq!(j["subscriptions"]["unsubscribed"], 2, "the free account and the operator");
    // F, G, H: revenue is the two real payments. The repeat and the reversal are
    // not money.
    assert_eq!(j["revenue"]["all_time"], 59_800);
    assert_eq!(j["revenue"]["today"], 59_800);
    assert_eq!(j["subscriptions"]["failed_this_month"], 1);
    // I: MRR is the recurring registrations that are still live.
    assert_eq!(j["mrr_krw"], 19_900, "the reversed record must leave the MRR");
    assert_eq!(j["youtube_accounts"], 0);
    assert_eq!(j["storage_bytes"], 0);
}

#[tokio::test]
async fn a_cancelled_subscription_leaves_the_mrr_and_shows_up_in_billing() {
    // Test J.
    let s = server();
    let token = admin(&s, "ops@example.com").await;
    let dj = account(&s, "dj@example.com").await;
    let record = pay(&s, &dj, "pro", 39_900, "evt-1");
    assert_eq!(get(&s, "/api/admin/dashboard", &token).await.json()["mrr_krw"], 39_900);

    s.app.db.cancel_billing_and_revoke(&record).unwrap();

    assert_eq!(get(&s, "/api/admin/dashboard", &token).await.json()["mrr_krw"], 0);
    let billing = get(&s, "/api/admin/billing", &token).await;
    assert_eq!(billing.status, StatusCode::OK, "{}", billing.body);
    let cancelled = billing.json()["cancellations"].as_array().unwrap().clone();
    assert_eq!(cancelled.len(), 1);
    assert_eq!(cancelled[0]["email"], "dj@example.com");
    assert_eq!(cancelled[0]["entitlement_active"], false);
    // The money that already arrived is still money.
    assert_eq!(get(&s, "/api/admin/dashboard", &token).await.json()["revenue"]["all_time"], 39_900);
}

#[tokio::test]
async fn the_revenue_report_answers_a_range_and_a_grain() {
    // Test K, at the HTTP layer: the range is the operator's own dates.
    let s = server();
    let token = admin(&s, "ops@example.com").await;
    let dj = account(&s, "dj@example.com").await;
    pay(&s, &dj, "pro", 39_900, "evt-1");

    let r = get(&s, "/api/admin/revenue?from=2000-01-01&to=2100-01-01&grain=month", &token).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let j = r.json();
    assert_eq!(j["grain"], "month");
    assert_eq!(j["summary"]["all_time"], 39_900);
    assert_eq!(j["mrr_krw"], 39_900);
    assert_eq!(j["active_payers"], 1);
    assert_eq!(j["buckets"].as_array().unwrap().len(), 1);
    assert_eq!(j["buckets"][0]["new_krw"], 39_900);
    assert_eq!(j["by_plan"][0]["plan_id"], "pro");

    // A range with nothing in it is empty rather than invented.
    let empty = get(&s, "/api/admin/revenue?from=1990-01-01&to=1990-01-31", &token).await;
    assert_eq!(empty.json()["buckets"].as_array().unwrap().len(), 0);
    // A nonsense range falls back rather than producing a silent zero.
    let junk = get(&s, "/api/admin/revenue?from=drop-table&grain=nonsense", &token).await;
    assert_eq!(junk.status, StatusCode::OK);
    assert_eq!(junk.json()["grain"], "day");
}

// --- L, M, N: the lists ----------------------------------------------------

#[tokio::test]
async fn the_user_list_searches_filters_and_pages() {
    // Test L, M.
    let s = server();
    let token = admin(&s, "ops@example.com").await;
    let dj = account(&s, "dj@example.com").await;
    account(&s, "other@example.com").await;
    pay(&s, &dj, "pro", 39_900, "evt-1");

    let all = get(&s, "/api/admin/users", &token).await;
    assert_eq!(all.status, StatusCode::OK, "{}", all.body);
    assert_eq!(all.json().as_array().unwrap().len(), 3);

    let found = get(&s, "/api/admin/users?q=dj@", &token).await;
    assert_eq!(found.json().as_array().unwrap().len(), 1);
    assert_eq!(found.json()[0]["email"], "dj@example.com");
    assert_eq!(found.json()[0]["total_paid_krw"], 39_900);

    assert_eq!(get(&s, "/api/admin/users?filter=paid", &token).await.json().as_array().unwrap().len(), 1);
    assert_eq!(get(&s, "/api/admin/users?limit=1", &token).await.json().as_array().unwrap().len(), 1);
    // However many are asked for, a page is a page.
    assert!(get(&s, "/api/admin/users?limit=99999", &token).await.json().as_array().unwrap().len() <= 100);

    // Test M: the detail.
    let detail = get(&s, &format!("/api/admin/users/{dj}"), &token).await;
    assert_eq!(detail.status, StatusCode::OK, "{}", detail.body);
    assert_eq!(detail.json()["user"]["email"], "dj@example.com");
    assert_eq!(detail.json()["payment_count"], 1);
    assert_eq!(detail.json()["payments"][0]["amount_krw"], 39_900);
    assert_eq!(detail.json()["payments"][0]["is_first"], true);

    assert_eq!(get(&s, "/api/admin/users/nobody", &token).await.status, StatusCode::NOT_FOUND);
}

/// A broadcast that is on air, the way the API makes one.
async fn running_broadcast(s: &Server, user_id: &str, token: &str) -> String {
    s.app.db.activate_subscription(user_id, "business").unwrap();
    let dest = post(
        s,
        "/api/stream-destinations",
        token,
        serde_json::json!({
            "label": "내 채널", "rtmps_url": "rtmps://a.rtmps.youtube.com/live2",
            "stream_key": "abcd-1234-efgh-5678-ijkl",
        }),
    )
    .await;
    assert_eq!(dest.status, StatusCode::OK, "{}", dest.body);
    let src = s._dir.path().join("clip.mp4");
    std::fs::write(&src, b"prepared video bytes").unwrap();
    let key = s.app.storage.put_file(user_id, "clip.mp4", &src).unwrap();
    let m = s.app.db.create_media(user_id, "clip.mp4", 20, &key).unwrap();
    s.app.db.record_media_prepared(&m.id, &key, 60.0, 0, 20).unwrap();
    let b = post(
        s,
        "/api/broadcasts",
        token,
        serde_json::json!({
            "name": "밤 라디오", "media_ids": [m.id], "destination_id": dest.json()["id"],
        }),
    )
    .await;
    assert_eq!(b.status, StatusCode::OK, "{}", b.body);
    let id = b.json()["id"].as_str().unwrap().to_string();
    let started = post(s, &format!("/api/broadcasts/{id}/start"), token, serde_json::json!({})).await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.body);
    id
}

#[tokio::test]
async fn the_broadcast_list_shows_every_users_and_the_worker_state() {
    // Test N.
    let s = server();
    let ops = admin(&s, "ops@example.com").await;
    let dj = account(&s, "dj@example.com").await;
    let token = token_for(&s, "dj@example.com").await;
    let id = running_broadcast(&s, &dj, &token).await;

    let list = get(&s, "/api/admin/broadcasts", &ops).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    let rows = list.json();
    let row = rows.as_array().unwrap().iter().find(|b| b["id"] == id.as_str()).unwrap().clone();
    assert_eq!(row["email"], "dj@example.com", "the owner is joined in");
    assert_eq!(row["desired_state"], "running");
    assert_eq!(row["worker_alive"], true, "this process has a worker for it");
    assert_eq!(row["item_count"], 1);

    assert_eq!(
        get(&s, "/api/admin/broadcasts?filter=running", &ops).await.json().as_array().unwrap().len(),
        1
    );
    assert_eq!(
        get(&s, "/api/admin/broadcasts?filter=stopped", &ops).await.json().as_array().unwrap().len(),
        0
    );
    s.app.mgr.shutdown();
}

// --- O, P, Q: the actions --------------------------------------------------

#[tokio::test]
async fn a_force_stop_is_the_ordinary_stop_and_nothing_revives_it() {
    // Tests O and P. The console must not have its own idea of what stopping
    // means: the intent, the slot and the provider lifecycle all belong to the
    // manager, and an admin console that killed a process would leave all three
    // wrong.
    let s = server();
    let ops = admin(&s, "ops@example.com").await;
    let dj = account(&s, "dj@example.com").await;
    let token = token_for(&s, "dj@example.com").await;
    let id = running_broadcast(&s, &dj, &token).await;
    assert_eq!(s.app.db.active_stream_count(&dj).unwrap(), 1);

    let stopped = post(
        &s,
        &format!("/api/admin/broadcasts/{id}/stop"),
        &ops,
        serde_json::json!({ "note": "고객 요청" }),
    )
    .await;
    assert_eq!(stopped.status, StatusCode::OK, "{}", stopped.body);
    assert_eq!(stopped.json()["desired_state"], "stopped");

    // O: the intent, the runtime state and the slot, exactly as STOP leaves them.
    let row = s.app.db.broadcast(&id).unwrap();
    assert_eq!(row.desired_state, louver_cloud::DesiredState::Stopped);
    assert_eq!(row.runtime_state, louver_cloud::RuntimeState::Stopped);
    assert_eq!(s.app.db.active_stream_count(&dj).unwrap(), 0);
    assert!(!s.app.mgr.running_ids().contains(&id), "a worker outlived the force stop");

    // P: nothing brings it back — not the watchdog, not a boot.
    std::thread::sleep(std::time::Duration::from_millis(1200));
    assert!(!s.app.mgr.running_ids().contains(&id));
    assert_eq!(s.app.mgr.recover_all().unwrap(), 0, "recovery revived a force-stopped broadcast");

    // S: and it is in the audit log, with the reason.
    let log = get(&s, "/api/admin/audit", &ops).await;
    let entry = log.json()[0].clone();
    assert_eq!(entry["action"], "admin.broadcast.force_stop");
    assert_eq!(entry["target_id"], id.as_str());
    assert_eq!(entry["admin_email"], "ops@example.com");
    assert_eq!(entry["note"], "고객 요청");
    assert_eq!(entry["after"], "stopped/STOPPED");
    s.app.mgr.shutdown();
}

#[tokio::test]
async fn a_disabled_account_cannot_sign_in_or_get_back_on_air() {
    // Test Q. And nothing of theirs is deleted: the point of disabling rather
    // than deleting is that the payment history and the videos survive.
    let s = server();
    let ops = admin(&s, "ops@example.com").await;
    let dj = account(&s, "dj@example.com").await;
    let token = token_for(&s, "dj@example.com").await;
    let id = running_broadcast(&s, &dj, &token).await;
    pay(&s, &dj, "pro", 39_900, "evt-1");

    let off = post(
        &s,
        &format!("/api/admin/users/{dj}/disabled"),
        &ops,
        serde_json::json!({ "disabled": true, "note": "결제 분쟁" }),
    )
    .await;
    assert_eq!(off.status, StatusCode::OK, "{}", off.body);
    assert!(!off.json()["disabled_at"].is_null());

    // Their broadcast came off air with them.
    assert_eq!(s.app.db.broadcast(&id).unwrap().desired_state, louver_cloud::DesiredState::Stopped);
    assert!(!s.app.mgr.running_ids().contains(&id));
    // Their session is gone, and a new one cannot be had.
    assert_eq!(get(&s, "/api/me", &token).await.status, StatusCode::UNAUTHORIZED);
    let login = send(
        &s,
        Method::POST,
        "/api/auth/login",
        None,
        Some(serde_json::json!({ "email": "dj@example.com", "password": "correct-horse-battery" })),
    )
    .await;
    assert_eq!(login.status, StatusCode::FORBIDDEN, "a disabled account signed in: {}", login.body);
    assert!(login.body.contains("사용이 중지"), "{}", login.body);

    // Nothing was deleted.
    let detail = get(&s, &format!("/api/admin/users/{dj}"), &ops).await;
    assert_eq!(detail.json()["payment_count"], 1);
    assert_eq!(detail.json()["broadcasts"].as_array().unwrap().len(), 1);

    // And it can be undone.
    let on =
        post(&s, &format!("/api/admin/users/{dj}/disabled"), &ops, serde_json::json!({ "disabled": false }))
            .await;
    assert_eq!(on.status, StatusCode::OK);
    assert!(on.json()["disabled_at"].is_null());
    let again = send(
        &s,
        Method::POST,
        "/api/auth/login",
        None,
        Some(serde_json::json!({ "email": "dj@example.com", "password": "correct-horse-battery" })),
    )
    .await;
    assert_eq!(again.status, StatusCode::OK);

    // An operator cannot lock themselves out.
    let ops_id = s.app.db.user_by_email("ops@example.com").unwrap().id;
    let self_off = post(
        &s,
        &format!("/api/admin/users/{ops_id}/disabled"),
        &ops,
        serde_json::json!({ "disabled": true }),
    )
    .await;
    assert_eq!(self_off.status, StatusCode::BAD_REQUEST);
    s.app.mgr.shutdown();
}

// --- R, T: what a response may contain -------------------------------------

#[tokio::test]
async fn no_admin_answer_carries_a_secret() {
    // Test R. The console shows an operator everything about an account except
    // the four things that would let them *be* that account.
    let s = server();
    let ops = admin(&s, "ops@example.com").await;
    let dj = account(&s, "dj@example.com").await;
    let token = token_for(&s, "dj@example.com").await;
    let id = running_broadcast(&s, &dj, &token).await;
    pay(&s, &dj, "pro", 39_900, "evt-1");
    // A connected channel, with both tokens sealed under it.
    let account_id = s.app.db.upsert_youtube_account(&dj, "UC-1", "수강생 채널", None).unwrap().id;
    s.app.keys.set(&louver_cloud::youtube::refresh_account(&account_id), "1//super-secret-refresh").unwrap();
    s.app.keys.set(&louver_cloud::youtube::access_account(&account_id), "ya29.super-secret-access").unwrap();

    let secrets = [
        "abcd-1234-efgh-5678-ijkl", // the stream key
        "1//super-secret-refresh",
        "ya29.super-secret-access",
        "password_hash",
        "sealed",
    ];
    let mut checked = 0;
    for path in [
        "/api/admin/dashboard".to_string(),
        "/api/admin/revenue".to_string(),
        "/api/admin/users".to_string(),
        format!("/api/admin/users/{dj}"),
        format!("/api/admin/users/{dj}/payments"),
        "/api/admin/broadcasts".to_string(),
        "/api/admin/billing".to_string(),
        "/api/admin/system".to_string(),
        "/api/admin/audit".to_string(),
    ] {
        let r = get(&s, &path, &ops).await;
        assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.body);
        for secret in secrets {
            assert!(!r.body.contains(secret), "{path} leaked {secret}");
        }
        checked += 1;
    }
    assert_eq!(checked, 9);

    // The channel's *title* is shown — that is the point of the page. So is the
    // provider's own subscription reference, which the account screen already
    // shows its owner and which support asks for by name; it is not a
    // credential. In the payment *ledger*, where a page may hold a hundred
    // rows, it is shortened to its last four.
    let detail = get(&s, &format!("/api/admin/users/{dj}"), &ops).await;
    assert!(detail.body.contains("수강생 채널"));
    let ledger = get(&s, &format!("/api/admin/users/{dj}/payments"), &ops).await;
    assert_eq!(ledger.json()[0]["provider_ref"], "…vt-1");
    let _ = id;
    s.app.mgr.shutdown();
}

#[tokio::test]
async fn a_note_cannot_carry_a_credential_into_the_audit_log() {
    // Test T. The note is the one free-text field an operator fills in, and the
    // audit log is the table most likely to be pasted into a support thread.
    let s = server();
    let ops = admin(&s, "ops@example.com").await;
    let dj = account(&s, "dj@example.com").await;

    post(
        &s,
        &format!("/api/admin/users/{dj}/disabled"),
        &ops,
        serde_json::json!({ "disabled": true, "note": "키 GOCSPX-thisisaclientsecret 로 확인함" }),
    )
    .await;

    let log = get(&s, "/api/admin/audit", &ops).await;
    let note = log.json()[0]["note"].as_str().unwrap().to_string();
    assert!(note.contains("확인함"), "{note}");
    assert!(!note.contains("GOCSPX-"), "a credential reached the audit log: {note}");
}

#[tokio::test]
async fn the_system_page_reuses_the_healthcheck_and_bounds_what_it_returns() {
    let s = server();
    let ops = admin(&s, "ops@example.com").await;
    let r = get(&s, "/api/admin/system", &ops).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let j = r.json();
    // The same answer `/health` gives, so the two cannot disagree.
    assert!(j["health"]["checks"]["database"].as_bool().unwrap());
    assert_eq!(j["health"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(j["memory_total_mb"].as_u64().unwrap() > 0);
    assert_eq!(j["workers"], 0);
    assert!(j["problems"].as_array().unwrap().len() <= 50);
    assert_eq!(j["disk_floor_bytes"], louver_cloud::ingest::DISK_FLOOR_BYTES);
}
