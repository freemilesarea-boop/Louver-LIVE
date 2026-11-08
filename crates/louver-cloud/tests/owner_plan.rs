//! The account that runs the service, rather than buys it.
//!
//! The operator pays for the machine. Capping that account at three concurrent
//! streams and 60GB — the ceilings that exist to price a product — was an
//! accident of everyone sharing one plan table.
//!
//! Two things are being pinned here, and the second matters as much as the
//! first. One: the owner plan really has no policy ceiling, including the
//! feature that is sold rather than metered. Two: **nothing moves for anybody
//! else.** A plan table is shared, so a change for one account is a change for
//! all of them until a test says otherwise.

use louver_cloud::entitlement::{
    BILLING_EXEMPT, CCTV_ENABLED, MAX_BROADCASTS, MAX_CONCURRENT_STREAMS, MAX_STORAGE_BYTES,
    MAX_UPLOAD_BYTES, UNLIMITED_BYTES, UNLIMITED_COUNT,
};
use louver_cloud::{CloudDb, CloudError};

const GB: i64 = 1024 * 1024 * 1024;

fn user_on(db: &CloudDb, email: &str, plan: &str) -> String {
    db.create_user(email, "hash", plan).unwrap().id
}

/// Say the `subscriptions` row out loud, which is what a customer's entitlement
/// hangs on.
fn set_subscription(db: &CloudDb, user_id: &str, plan_id: &str, status: &str) {
    db.raw()
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO subscriptions (user_id, plan_id, status) VALUES (?1, ?2, ?3)
             ON CONFLICT(user_id) DO UPDATE SET status = excluded.status",
            rusqlite::params![user_id, plan_id, status],
        )
        .unwrap();
}

// -- the owner account has no policy ceiling ----------------------------

#[test]
fn the_owner_plan_lifts_every_metered_limit() {
    let db = CloudDb::open_in_memory().unwrap();
    let o = user_on(&db, "owner@x.com", "owner");

    assert_eq!(db.limit(&o, MAX_CONCURRENT_STREAMS).unwrap(), UNLIMITED_COUNT);
    assert_eq!(db.limit(&o, MAX_BROADCASTS).unwrap(), UNLIMITED_COUNT);
    assert_eq!(db.limit(&o, MAX_STORAGE_BYTES).unwrap(), UNLIMITED_BYTES);
    assert_eq!(db.limit(&o, MAX_UPLOAD_BYTES).unwrap(), UNLIMITED_BYTES);
}

#[test]
fn the_owner_may_use_the_feature_that_is_sold_rather_than_metered() {
    let db = CloudDb::open_in_memory().unwrap();
    let o = user_on(&db, "owner@x.com", "owner");
    // `paid_flag` and not `flag`: CCTV is read from the plan the account pays
    // for, so a grant cannot open it. The owner plan has to carry it itself.
    assert!(db.paid_flag(&o, CCTV_ENABLED).unwrap(), "운영자 계정은 CCTV 를 쓸 수 있어야 합니다");
}

#[test]
fn an_upload_far_past_every_paid_ceiling_is_allowed() {
    let db = CloudDb::open_in_memory().unwrap();
    let o = user_on(&db, "owner@x.com", "owner");

    // 500GB in one file: more than eight times the Business per-file ceiling
    // and more than the whole Business account ceiling.
    assert!(db.check_upload_allowed(&o, 500 * GB).is_ok());

    // And again with 10TB already stored, which is past the disk — the point
    // being that the *plan* does not refuse it. The disk does, elsewhere, and
    // that check is deliberately left alone.
    let m = db.create_media(&o, "big.mp4", 10_000 * GB, "key/big.mp4").unwrap();
    db.record_media_prepared(&m.id, "key/prepared.mp4", 60.0, 0, 0).unwrap();
    assert!(db.check_upload_allowed(&o, 500 * GB).is_ok());
}

#[test]
fn the_owner_outranks_business_so_no_grant_can_displace_it() {
    let db = CloudDb::open_in_memory().unwrap();
    let o = user_on(&db, "owner@x.com", "owner");
    // `effective_plan` takes the stronger of the paid plan and a live grant.
    // A Business grant handed to the owner must not lower anything.
    let admin = user_on(&db, "admin@x.com", "business");
    db.create_grants(
        &[o.clone()],
        &louver_cloud::grants::NewGrant {
            plan_id: "business",
            term: louver_cloud::grants::Term::Days(30),
            reason: "시험",
        },
        louver_cloud::grants::OnExisting::Reset,
        &admin,
        "admin@x.com",
    )
    .expect("grant 지급");
    assert_eq!(
        db.limit(&o, MAX_STORAGE_BYTES).unwrap(),
        UNLIMITED_BYTES,
        "grant 는 올리기만 할 수 있고, 운영자 요금제보다 높은 것은 없습니다"
    );
}

// -- and it does not depend on a payment --------------------------------

#[test]
fn the_owner_stays_entitled_when_the_subscription_row_says_otherwise() {
    let db = CloudDb::open_in_memory().unwrap();
    let o = user_on(&db, "owner@x.com", "owner");
    set_subscription(&db, &o, "owner", "unsubscribed");

    let sub = db.subscription(&o).unwrap();
    assert!(sub.active, "운영자 계정은 결제 상태로 꺼지지 않습니다");
    assert!(db.require_active_subscription(&o).is_ok());
    assert!(db.flag(&o, BILLING_EXEMPT).unwrap());
}

#[test]
fn a_customer_is_still_switched_off_when_their_subscription_lapses() {
    let db = CloudDb::open_in_memory().unwrap();
    let b = user_on(&db, "biz@x.com", "business");
    set_subscription(&db, &b, "business", "unsubscribed");

    // The whole point of the exemption is that it is the owner's alone. If this
    // ever passes, a month of service has stopped being a month of service.
    assert!(!db.subscription(&b).unwrap().active);
    assert!(matches!(db.require_active_subscription(&b), Err(CloudError::NoSubscription)));
    assert!(!db.flag(&b, BILLING_EXEMPT).unwrap());
}

// -- nothing moves for anybody else -------------------------------------

#[test]
fn the_three_sold_plans_are_untouched() {
    let db = CloudDb::open_in_memory().unwrap();
    for (plan, conc, broadcasts, storage, upload) in [
        ("basic", 1, 3, 15 * GB, 10 * GB),
        ("pro", 2, 10, 30 * GB, 15 * GB),
        ("business", 3, 30, 60 * GB, 20 * GB),
    ] {
        let u = user_on(&db, &format!("{plan}@x.com"), plan);
        assert_eq!(db.limit(&u, MAX_CONCURRENT_STREAMS).unwrap(), conc, "{plan} 동시 송출");
        assert_eq!(db.limit(&u, MAX_BROADCASTS).unwrap(), broadcasts, "{plan} 방송 수");
        assert_eq!(db.limit(&u, MAX_STORAGE_BYTES).unwrap(), storage, "{plan} 저장");
        assert_eq!(db.limit(&u, MAX_UPLOAD_BYTES).unwrap(), upload, "{plan} 한 파일");
    }
}

#[test]
fn the_owner_plan_is_not_for_sale() {
    let db = CloudDb::open_in_memory().unwrap();
    let offered: Vec<String> = db.plans_for_sale().unwrap().into_iter().map(|p| p.id).collect();
    assert_eq!(
        offered,
        vec!["basic".to_string(), "pro".to_string(), "business".to_string()],
        "요금제 페이지와 /api/plans 는 파는 것 셋만 보여야 합니다"
    );
    // It exists all the same, which is what `--create-user --plan owner` needs.
    assert!(db.plan_ids().unwrap().iter().any(|p| p == "owner"));
}

#[test]
fn an_unsubscribed_account_still_gets_nothing() {
    let db = CloudDb::open_in_memory().unwrap();
    let n = user_on(&db, "none@x.com", "none");
    assert_eq!(db.limit(&n, MAX_UPLOAD_BYTES).unwrap(), 0);
    assert_eq!(db.limit(&n, MAX_CONCURRENT_STREAMS).unwrap(), 0);
    assert!(!db.subscription(&n).unwrap().active, "요금제 없음은 결제 면제와 무관합니다");
}
