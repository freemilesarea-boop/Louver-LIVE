//! §19's plan-limit and race tests.
//!
//! A limit only enforced in a browser is not a limit, and a limit enforced by
//! reading then writing is not a limit either — two requests can pass the read
//! together. These tests drive the database directly, without any HTTP layer,
//! because that is where the guarantee has to live.

use louver_cloud::entitlement::*;
use louver_cloud::{CloudDb, CloudError, DesiredState, RuntimeState};
use std::sync::{Arc, Barrier};

/// A user on `plan`, with `n` broadcasts ready to start.
fn user_with_broadcasts(db: &CloudDb, email: &str, plan: &str, n: usize) -> (String, Vec<String>) {
    let u = db.create_user(email, "hash", plan).unwrap();
    let media = db.create_media(&u.id, "clip.mp4", 1024, "key/clip.mp4").unwrap();
    db.record_media_prepared(&media.id, "key/prepared.mp4", 60.0, 2048).unwrap();
    let dest = db.create_destination(&u.id, "채널", "rtmps://a/live2", "••••").unwrap();

    let ids = (0..n)
        .map(|i| db.create_broadcast(&u.id, &format!("LIVE {i}"), &media.id, &dest.id, true).unwrap().id)
        .collect();
    (u.id, ids)
}

#[test]
fn basic_allows_one_stream_and_refuses_the_second() {
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, b) = user_with_broadcasts(&db, "basic@x.com", "basic", 3);

    db.claim_stream_slot(&uid, &b[0]).expect("the first must be allowed");
    let second = db.claim_stream_slot(&uid, &b[1]);
    match second {
        Err(CloudError::LimitReached { limit, used, allowed }) => {
            assert_eq!(limit, MAX_CONCURRENT_STREAMS);
            assert_eq!((used, allowed), (1, 1));
        }
        other => panic!("a Basic user's second stream must be refused, got {other:?}"),
    }
    assert_eq!(db.active_stream_count(&uid).unwrap(), 1);
}

#[test]
fn business_allows_three_and_refuses_the_fourth() {
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, b) = user_with_broadcasts(&db, "biz@x.com", "business", 4);

    for (i, id) in b.iter().take(3).enumerate() {
        db.claim_stream_slot(&uid, id).unwrap_or_else(|e| panic!("stream {i} refused: {e}"));
    }
    assert_eq!(db.active_stream_count(&uid).unwrap(), 3);

    assert!(
        matches!(db.claim_stream_slot(&uid, &b[3]), Err(CloudError::LimitReached { .. })),
        "the fourth stream must be refused",
    );
}

#[test]
fn pro_sits_between_them() {
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, b) = user_with_broadcasts(&db, "pro@x.com", "pro", 3);
    db.claim_stream_slot(&uid, &b[0]).unwrap();
    db.claim_stream_slot(&uid, &b[1]).unwrap();
    assert!(matches!(db.claim_stream_slot(&uid, &b[2]), Err(CloudError::LimitReached { .. })));
}

#[test]
fn stopping_one_frees_the_slot_for_another() {
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, b) = user_with_broadcasts(&db, "free@x.com", "basic", 2);
    db.claim_stream_slot(&uid, &b[0]).unwrap();
    assert!(db.claim_stream_slot(&uid, &b[1]).is_err());

    db.release_stream_slot(&uid, &b[0]).unwrap();
    assert_eq!(db.active_stream_count(&uid).unwrap(), 0);
    db.claim_stream_slot(&uid, &b[1]).expect("the freed slot must be reusable");
}

/// Starting the same broadcast twice must not spend two slots.
#[test]
fn a_repeated_start_does_not_consume_a_second_slot() {
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, b) = user_with_broadcasts(&db, "twice@x.com", "basic", 2);
    db.claim_stream_slot(&uid, &b[0]).unwrap();
    db.claim_stream_slot(&uid, &b[0]).expect("an already-running broadcast keeps its slot");
    assert_eq!(db.active_stream_count(&uid).unwrap(), 1);
    assert!(db.claim_stream_slot(&uid, &b[1]).is_err(), "the plan is still full");
}

/// §19's race condition, run for real — against separate connections.
///
/// The first version of this test shared one `CloudDb`, and passed even with a
/// deferred transaction: a `Mutex<Connection>` serialises everything inside one
/// process, so nothing collided and the test proved nothing. Each thread now
/// opens its **own** connection to the same file, which is what two server
/// processes, or a restart overlapping a request, actually look like. That is
/// where `BEGIN IMMEDIATE` earns its place. Swapping it for `Deferred` makes
/// this test fail: SQLite takes the write lock only at the first write, the
/// contending transactions collide, and the count comes out at 1 of 3 rather
/// than 3 — under-granting rather than over-granting, but wrong either way, and
/// a user told their plan is full when it is not.
#[test]
fn concurrent_starts_cannot_exceed_the_plan() {
    const THREADS: usize = 8;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cloud.db");

    let setup = CloudDb::open(&path).unwrap();
    let (uid, ids) = user_with_broadcasts(&setup, "race@x.com", "business", THREADS);
    drop(setup);

    let barrier = Arc::new(Barrier::new(THREADS));
    let granted = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let handles: Vec<_> = ids
        .into_iter()
        .map(|id| {
            let path = path.clone();
            let uid = uid.clone();
            let barrier = Arc::clone(&barrier);
            let granted = Arc::clone(&granted);
            std::thread::spawn(move || {
                // One connection per thread: no shared mutex to hide behind.
                let db = CloudDb::open(&path).unwrap();
                barrier.wait();
                if db.claim_stream_slot(&uid, &id).is_ok() {
                    granted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let n = granted.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(n, 3, "{THREADS} simultaneous starts granted {n} slots on a 3-stream plan");

    let db = CloudDb::open(&path).unwrap();
    assert_eq!(db.active_stream_count(&uid).unwrap(), 3, "the database disagrees with the grants");
}

#[test]
fn a_plan_is_read_by_limit_name_and_never_by_plan_name() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = db.create_user("named@x.com", "h", "basic").unwrap();

    // Raising one customer's ceiling is an UPDATE, not a code change.
    db.raw()
        .lock()
        .unwrap()
        .execute(
            "UPDATE plans SET limits='{\"max_concurrent_streams\":3,\"max_broadcasts\":5}' WHERE id='basic'",
            [],
        )
        .unwrap();
    assert_eq!(db.limit(&u.id, MAX_CONCURRENT_STREAMS).unwrap(), 3);

    // A limit that is absent fails closed rather than meaning "unlimited".
    assert_eq!(db.limit(&u.id, MAX_STORAGE_BYTES).unwrap(), 0);
}

#[test]
fn storage_and_upload_ceilings_are_enforced() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = db.create_user("disk@x.com", "h", "basic").unwrap();
    let per_file = db.limit(&u.id, MAX_UPLOAD_BYTES).unwrap();
    let total = db.limit(&u.id, MAX_STORAGE_BYTES).unwrap();

    db.check_upload_allowed(&u.id, 1024).expect("a small file is fine");
    assert!(
        matches!(db.check_upload_allowed(&u.id, per_file + 1), Err(CloudError::LimitReached { limit, .. }) if limit == MAX_UPLOAD_BYTES),
        "one oversized file must be refused",
    );

    // Fill the account, then try to add one more byte.
    db.create_media(&u.id, "big.mp4", total, "k/big.mp4").unwrap();
    assert_eq!(db.storage_used(&u.id).unwrap(), total);
    assert!(
        matches!(db.check_upload_allowed(&u.id, 1), Err(CloudError::LimitReached { limit, .. }) if limit == MAX_STORAGE_BYTES),
        "a full account must refuse the next upload",
    );
}

#[test]
fn the_broadcast_count_is_capped_too() {
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, _) = user_with_broadcasts(&db, "many@x.com", "basic", 3);
    assert_eq!(db.limit(&uid, MAX_BROADCASTS).unwrap(), 3);
    assert!(matches!(db.check_can_create_broadcast(&uid), Err(CloudError::LimitReached { .. })));
}

/// A reconnecting broadcast still holds its slot.
///
/// Counting only RUNNING would let a user start a fourth stream during a
/// three-second reconnect, and then have four when it came back.
#[test]
fn a_reconnecting_broadcast_still_occupies_its_slot() {
    assert!(RuntimeState::Reconnecting.occupies_a_slot());
    assert!(RuntimeState::Preparing.occupies_a_slot());
    assert!(RuntimeState::Starting.occupies_a_slot());
    assert!(RuntimeState::Running.occupies_a_slot());
    assert!(!RuntimeState::Stopped.occupies_a_slot());
    assert!(!RuntimeState::Failed.occupies_a_slot());
    assert!(!RuntimeState::Created.occupies_a_slot());

    let db = CloudDb::open_in_memory().unwrap();
    let (uid, b) = user_with_broadcasts(&db, "blip@x.com", "basic", 2);
    db.claim_stream_slot(&uid, &b[0]).unwrap();
    db.record_runtime(&b[0], RuntimeState::Reconnecting, 1, 30, 4_096).unwrap();
    assert_eq!(db.active_stream_count(&uid).unwrap(), 1);
    assert!(db.claim_stream_slot(&uid, &b[1]).is_err(), "a blip must not free a slot");
}

/// §6: recovery reads intent, and a deliberate stop is not intent.
#[test]
fn recovery_finds_only_what_was_meant_to_be_running() {
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, b) = user_with_broadcasts(&db, "boot@x.com", "business", 3);

    db.claim_stream_slot(&uid, &b[0]).unwrap();
    db.claim_stream_slot(&uid, &b[1]).unwrap();
    db.release_stream_slot(&uid, &b[1]).unwrap(); // the user stopped this one

    let wanted: Vec<String> = db.broadcasts_wanting_to_run().unwrap().into_iter().map(|x| x.id).collect();
    assert_eq!(wanted, vec![b[0].clone()], "a stopped broadcast must not be resurrected");
    assert_eq!(db.broadcast(&b[1]).unwrap().desired_state, DesiredState::Stopped);
    assert_eq!(db.broadcast(&b[2]).unwrap().desired_state, DesiredState::Stopped);
}

/// Giving up must not leave a broadcast that recovery will start again.
#[test]
fn a_broadcast_that_failed_for_good_is_not_retried_after_a_restart() {
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, b) = user_with_broadcasts(&db, "gaveup@x.com", "basic", 1);
    db.claim_stream_slot(&uid, &b[0]).unwrap();

    db.record_failure(&b[0], "10회 연속 재시작 실패").unwrap();
    db.give_up(&b[0]).unwrap();

    let got = db.broadcast(&b[0]).unwrap();
    assert_eq!(got.runtime_state, RuntimeState::Failed);
    assert_eq!(got.desired_state, DesiredState::Stopped);
    assert!(db.broadcasts_wanting_to_run().unwrap().is_empty());
    assert_eq!(db.active_stream_count(&uid).unwrap(), 0, "a failed stream must free its slot");
}
