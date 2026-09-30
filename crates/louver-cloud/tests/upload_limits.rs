//! The 2026-09 upload relief: per-account storage and per-file ceilings.
//!
//! Basic 15GB / 10GB, Pro 30GB / 15GB, Business 60GB / 20GB, in the GiB the
//! rest of the product calls "GB". No fixture here is tens of gigabytes: usage
//! is a `size_bytes` row and an upload is a byte count, which is all the
//! entitlement check ever looks at.

use louver_cloud::entitlement::*;
use louver_cloud::{CloudDb, CloudError};

const GB: i64 = 1024 * 1024 * 1024;

fn user_on(db: &CloudDb, email: &str, plan: &str) -> String {
    db.create_user(email, "hash", plan).unwrap().id
}

/// Pretend this account already stores `bytes`.
fn stored(db: &CloudDb, uid: &str, bytes: i64) {
    db.create_media(uid, "already.mp4", bytes, &format!("{uid}/already.mp4")).unwrap();
}

fn refused_for(r: louver_cloud::Result<()>) -> Option<&'static str> {
    match r {
        Ok(()) => None,
        Err(CloudError::LimitReached { limit, .. }) => Some(limit),
        Err(other) => panic!("expected a plan limit, got {other:?}"),
    }
}

#[test]
fn the_seeded_plans_carry_the_new_ceilings() {
    let db = CloudDb::open_in_memory().unwrap();
    for (id, storage, upload) in
        [("basic", 15 * GB, 10 * GB), ("pro", 30 * GB, 15 * GB), ("business", 60 * GB, 20 * GB)]
    {
        let p = db.plan(id).unwrap();
        assert_eq!(p.limits.get(MAX_STORAGE_BYTES).copied(), Some(storage), "{id} 저장 한도");
        assert_eq!(p.limits.get(MAX_UPLOAD_BYTES).copied(), Some(upload), "{id} 파일당 한도");
    }
    // Still nothing for an account without a plan.
    let p = db.plan("none").unwrap();
    assert_eq!(p.limits.get(MAX_STORAGE_BYTES).copied(), Some(0));
    assert_eq!(p.limits.get(MAX_UPLOAD_BYTES).copied(), Some(0));
}

#[test]
fn basic_boundaries() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user_on(&db, "basic@x.com", "basic");
    assert_eq!(refused_for(db.check_upload_allowed(&u, 9 * GB)), None, "9GB");
    assert_eq!(refused_for(db.check_upload_allowed(&u, 10 * GB)), None, "10GB exactly");
    assert_eq!(
        refused_for(db.check_upload_allowed(&u, 10 * GB + 1)),
        Some(MAX_UPLOAD_BYTES),
        "10GB + 1 byte"
    );

    // 4GB used + 10GB = 14GB of 15GB.
    let a = user_on(&db, "basic-a@x.com", "basic");
    stored(&db, &a, 4 * GB);
    assert_eq!(refused_for(db.check_upload_allowed(&a, 10 * GB)), None, "4 + 10 = 14GB");

    // 6GB used + 10GB = 16GB of 15GB.
    let b = user_on(&db, "basic-b@x.com", "basic");
    stored(&db, &b, 6 * GB);
    assert_eq!(refused_for(db.check_upload_allowed(&b, 10 * GB)), Some(MAX_STORAGE_BYTES), "6 + 10 = 16GB");

    // Filling the account exactly is allowed; one byte past is not.
    let c = user_on(&db, "basic-c@x.com", "basic");
    stored(&db, &c, 5 * GB);
    assert_eq!(refused_for(db.check_upload_allowed(&c, 10 * GB)), None, "5 + 10 = 15GB exactly");
    stored(&db, &c, 10 * GB);
    assert_eq!(refused_for(db.check_upload_allowed(&c, 1)), Some(MAX_STORAGE_BYTES), "15GB + 1 byte");
}

#[test]
fn pro_boundaries() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user_on(&db, "pro@x.com", "pro");
    assert_eq!(refused_for(db.check_upload_allowed(&u, 15 * GB)), None, "15GB exactly");
    assert_eq!(refused_for(db.check_upload_allowed(&u, 15 * GB + 1)), Some(MAX_UPLOAD_BYTES));

    stored(&db, &u, 15 * GB);
    assert_eq!(refused_for(db.check_upload_allowed(&u, 15 * GB)), None, "15 + 15 = 30GB exactly");
    assert_eq!(refused_for(db.check_upload_allowed(&u, 15 * GB + 1)), Some(MAX_UPLOAD_BYTES));
    stored(&db, &u, 10 * GB);
    assert_eq!(refused_for(db.check_upload_allowed(&u, 5 * GB)), None, "25 + 5 = 30GB exactly");
    assert_eq!(
        refused_for(db.check_upload_allowed(&u, 5 * GB + 1)),
        Some(MAX_STORAGE_BYTES),
        "30GB + 1 byte"
    );
}

#[test]
fn business_boundaries() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user_on(&db, "biz@x.com", "business");
    assert_eq!(refused_for(db.check_upload_allowed(&u, 20 * GB)), None, "20GB exactly");
    assert_eq!(refused_for(db.check_upload_allowed(&u, 20 * GB + 1)), Some(MAX_UPLOAD_BYTES));

    stored(&db, &u, 40 * GB);
    assert_eq!(refused_for(db.check_upload_allowed(&u, 20 * GB)), None, "40 + 20 = 60GB exactly");
    stored(&db, &u, 1);
    assert_eq!(refused_for(db.check_upload_allowed(&u, 20 * GB)), Some(MAX_STORAGE_BYTES), "60GB + 1 byte");
}

#[test]
fn the_quota_is_per_account_not_per_plan() {
    // Two Basic accounts, each nearly full on its own. If the ceiling were
    // shared, the second would be refused; it is not.
    let db = CloudDb::open_in_memory().unwrap();
    let a = user_on(&db, "one@x.com", "basic");
    let b = user_on(&db, "two@x.com", "basic");
    stored(&db, &a, 14 * GB);
    stored(&db, &b, 14 * GB);
    assert_eq!(db.storage_used(&a).unwrap(), 14 * GB, "b's files are not a's");
    assert_eq!(refused_for(db.check_upload_allowed(&a, GB)), None);
    assert_eq!(refused_for(db.check_upload_allowed(&b, GB)), None);
    // And one account filling up refuses only that account.
    stored(&db, &a, GB);
    assert_eq!(refused_for(db.check_upload_allowed(&a, 1)), Some(MAX_STORAGE_BYTES));
    assert_eq!(refused_for(db.check_upload_allowed(&b, GB)), None);
}

#[test]
fn the_three_refusals_read_differently() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user_on(&db, "words@x.com", "basic");

    let too_big = db.check_upload_allowed(&u, 11 * GB).unwrap_err().to_string();
    assert!(too_big.contains("파일 크기가 Basic 플랜의 파일당 최대 용량 10GB를 초과했습니다"), "{too_big}");

    stored(&db, &u, 6 * GB);
    let full = db.check_upload_allowed(&u, 10 * GB).unwrap_err().to_string();
    assert!(full.contains("Basic 플랜 저장공간 15GB를 초과합니다"), "{full}");
    assert!(!full.contains("파일당"), "{full}");

    let disk = CloudError::OutOfSpace.to_string();
    assert!(disk.contains("현재 서버 저장공간이 부족하여 업로드할 수 없습니다"), "{disk}");
    assert!(!disk.contains("플랜"), "paying more does not fix a full server: {disk}");

    // No plan at all: a sentence about choosing one, not "0GB를 초과".
    let none = user_on(&db, "none@x.com", "none");
    let said = db.check_upload_allowed(&none, 1).unwrap_err().to_string();
    assert!(said.contains("요금제가 필요합니다"), "{said}");
    assert!(!said.contains("0GB"), "{said}");
}

// --- existing databases ----------------------------------------------------

fn set_storage(db: &CloudDb, plan: &str, storage: i64, upload: i64) {
    let mut limits = db.plan(plan).unwrap().limits;
    limits.insert(MAX_STORAGE_BYTES.into(), storage);
    limits.insert(MAX_UPLOAD_BYTES.into(), upload);
    db.raw()
        .lock()
        .unwrap()
        .execute(
            "UPDATE plans SET limits = ?2 WHERE id = ?1",
            rusqlite::params![plan, serde_json::to_string(&limits).unwrap()],
        )
        .unwrap();
}

fn storage_of(db: &CloudDb, plan: &str) -> (i64, i64) {
    let p = db.plan(plan).unwrap();
    (p.limits[MAX_STORAGE_BYTES], p.limits[MAX_UPLOAD_BYTES])
}

#[test]
fn an_existing_database_is_raised_once_and_only_upwards() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cloud.db");
    let (uid, sub_before, billing_before) = {
        let db = CloudDb::open(&path).unwrap();
        // The database as the previous release left it.
        set_storage(&db, "basic", 5 * GB, 2 * GB);
        set_storage(&db, "pro", 10 * GB, 99 * GB); // an operator's raise, above the new target
        set_storage(&db, "business", 20 * GB, 8 * GB);
        db.raw().lock().unwrap().execute("DROP TABLE cloud_migrations", []).unwrap();
        // A paying customer with the old ceiling.
        let uid = user_on(&db, "customer@x.com", "basic");
        assert_eq!(db.limit(&uid, MAX_UPLOAD_BYTES).unwrap(), 2 * GB);
        let s = db.subscription(&uid).unwrap();
        let sub = (s.plan_id, s.status, s.active);
        let billing: i64 = db
            .raw()
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM billing_subscriptions", [], |r| r.get(0))
            .unwrap();
        (uid, sub, billing)
    };

    let db = CloudDb::open(&path).unwrap();
    assert_eq!(storage_of(&db, "basic"), (15 * GB, 10 * GB));
    assert_eq!(storage_of(&db, "pro"), (30 * GB, 99 * GB), "a higher figure an operator set is kept");
    assert_eq!(storage_of(&db, "business"), (60 * GB, 20 * GB));
    // The existing customer gets it with no change to their own rows.
    assert_eq!(db.limit(&uid, MAX_UPLOAD_BYTES).unwrap(), 10 * GB);
    assert_eq!(db.limit(&uid, MAX_STORAGE_BYTES).unwrap(), 15 * GB);
    let s = db.subscription(&uid).unwrap();
    assert_eq!((s.plan_id, s.status, s.active), sub_before, "구독이 바뀌었습니다");
    let billing: i64 = db
        .raw()
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM billing_subscriptions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(billing, billing_before);
    // Prices and everything else in the row are untouched.
    for (id, price, streams) in [("basic", 19_900, 1), ("pro", 39_900, 2), ("business", 59_900, 3)] {
        let p = db.plan(id).unwrap();
        assert_eq!(p.monthly_price_krw, price, "{id} 가격");
        assert_eq!(p.max_concurrent_streams(), streams, "{id} 동시 송출");
        assert_eq!(p.limits.get("scheduling_enabled").copied(), Some(1), "{id}");
    }

    // An operator lowers Basic afterwards. A restart must not undo that.
    set_storage(&db, "basic", 12 * GB, 6 * GB);
    drop(db);
    let db = CloudDb::open(&path).unwrap();
    assert_eq!(storage_of(&db, "basic"), (12 * GB, 6 * GB), "부팅이 운영자의 한도를 되돌렸습니다");
}

#[test]
fn a_fresh_database_needs_no_raise_and_records_it_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cloud.db");
    drop(CloudDb::open(&path).unwrap());
    let db = CloudDb::open(&path).unwrap();
    let n: i64 = db
        .raw()
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM cloud_migrations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(storage_of(&db, "basic"), (15 * GB, 10 * GB));
}
