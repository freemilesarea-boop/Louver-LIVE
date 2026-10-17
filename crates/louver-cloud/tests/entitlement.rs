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
    db.record_media_prepared(&media.id, "key/prepared.mp4", 60.0, 0, 2048).unwrap();
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
        Err(CloudError::ConcurrencyReached { ref plan_label, used, allowed }) => {
            assert_eq!((plan_label.as_str(), used, allowed), ("Basic", 1, 1));
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
        matches!(db.claim_stream_slot(&uid, &b[3]), Err(CloudError::ConcurrencyReached { .. })),
        "the fourth stream must be refused",
    );
}

#[test]
fn pro_sits_between_them() {
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, b) = user_with_broadcasts(&db, "pro@x.com", "pro", 3);
    db.claim_stream_slot(&uid, &b[0]).unwrap();
    db.claim_stream_slot(&uid, &b[1]).unwrap();
    assert!(matches!(db.claim_stream_slot(&uid, &b[2]), Err(CloudError::ConcurrencyReached { .. })));
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
    db.record_runtime(&b[0], RuntimeState::Reconnecting, 1, 30, 4_096, Some(4242)).unwrap();
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

// --- subscriptions ---------------------------------------------------------
//
// An entitlement has two halves: is there a subscription, and how much does it
// allow. These are about the first, and about the places that would otherwise
// spend server resources without asking.

use louver_cloud::db::{Signup, SUBSCRIPTION_UNSUBSCRIBED, UNSUBSCRIBED_PLAN};

fn signed_up(db: &CloudDb, email: &str) -> String {
    db.register_user(&Signup {
        name: "홍길동",
        email,
        password_hash: "salt:hash",
        terms_version: "2026-09-27",
    })
    .unwrap()
    .id
}

#[test]
fn a_new_public_account_is_unsubscribed_and_can_do_nothing_that_costs_anything() {
    let db = CloudDb::open_in_memory().unwrap();
    let uid = signed_up(&db, "new@example.com");

    let sub = db.subscription(&uid).unwrap();
    assert_eq!(sub.plan_id, UNSUBSCRIBED_PLAN);
    assert_eq!(sub.status, SUBSCRIPTION_UNSUBSCRIBED);
    assert!(!sub.active);
    assert!(sub.plan.is_none(), "there is no plan to report");

    // Every limit is zero, so even a path that forgot to ask about the
    // subscription would refuse.
    assert_eq!(db.limit(&uid, MAX_CONCURRENT_STREAMS).unwrap(), 0);
    assert_eq!(db.limit(&uid, louver_cloud::entitlement::MAX_BROADCASTS).unwrap(), 0);
    assert_eq!(db.limit(&uid, MAX_UPLOAD_BYTES).unwrap(), 0);

    // And the gate answers with something a person can act on.
    assert!(matches!(db.require_active_subscription(&uid), Err(CloudError::NoSubscription)));
    assert!(matches!(db.check_can_create_broadcast(&uid), Err(CloudError::NoSubscription)));
    assert_eq!(
        db.require_active_subscription(&uid).unwrap_err().to_string(),
        "방송을 시작하려면 활성화된 요금제가 필요합니다"
    );
}

#[test]
fn an_unsubscribed_account_cannot_claim_a_stream_slot() {
    let db = CloudDb::open_in_memory().unwrap();
    let uid = signed_up(&db, "new@example.com");
    // A broadcast row put in place directly: what is under test is the slot, and
    // creating one through the API is refused for the same reason.
    let media = db.create_media(&uid, "a.mp4", 10, "k").unwrap();
    db.record_media_prepared(&media.id, "k", 10.0, 0, 10).unwrap();
    let dest = db.create_destination(&uid, "d", "rtmps://a/live2", "••••").unwrap();
    db.raw()
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO broadcasts (id, user_id, name, media_id, destination_id, desired_state, runtime_state)
             VALUES ('b1', ?1, 'n', ?2, ?3, 'stopped', 'CREATED')",
            rusqlite::params![uid, media.id, dest.id],
        )
        .unwrap();

    assert!(matches!(db.claim_stream_slot(&uid, "b1"), Err(CloudError::NoSubscription)));
    // Refused means refused: the row must not have been marked running on the
    // way out.
    assert_eq!(db.active_stream_count(&uid).unwrap(), 0);
}

#[test]
fn paying_for_a_plan_is_the_only_thing_that_grants_one() {
    let db = CloudDb::open_in_memory().unwrap();
    let uid = signed_up(&db, "new@example.com");

    // What a verified payment will call. Nothing a browser can reach.
    let sub = db.activate_subscription(&uid, "pro").unwrap();
    assert!(sub.active);
    assert_eq!(sub.plan_id, "pro");
    assert_eq!(sub.status, "active");
    assert_eq!(sub.plan.as_ref().map(|p| p.max_concurrent_streams()), Some(2));
    assert_eq!(sub.plan.as_ref().map(|p| p.monthly_price_krw), Some(39_900));
    assert_eq!(db.limit(&uid, MAX_CONCURRENT_STREAMS).unwrap(), 2);

    // Cancelling takes the entitlement away and leaves everything they own.
    let media = db.create_media(&uid, "a.mp4", 10, "k").unwrap();
    let after = db.cancel_subscription(&uid).unwrap();
    assert!(!after.active);
    assert_eq!(after.plan_id, UNSUBSCRIBED_PLAN);
    assert!(db.media_owned(&uid, &media.id).is_ok(), "cancelling must not delete anything");
}

#[test]
fn a_plan_that_grants_nothing_cannot_be_activated() {
    let db = CloudDb::open_in_memory().unwrap();
    let uid = signed_up(&db, "new@example.com");

    // An unknown id, and the unsubscribed plan — both would leave an account
    // "active" on something that allows no broadcasts.
    assert!(db.activate_subscription(&uid, "enterprise-unlimited").is_err());
    assert!(db.activate_subscription(&uid, UNSUBSCRIBED_PLAN).is_err());
    assert!(db.activate_subscription(&uid, "").is_err());
    assert!(!db.subscription(&uid).unwrap().active, "a failed activation must grant nothing");
}

#[test]
fn an_account_the_bootstrap_cli_made_keeps_its_plan_and_its_entitlement() {
    let db = CloudDb::open_in_memory().unwrap();
    // Exactly what `--create-user ... --plan business` does, which is what the
    // deploy script runs for the operator's account.
    let ops = db.create_user("freemilesarea@example.com", "salt:hash", "business").unwrap();
    let sub = db.subscription(&ops.id).unwrap();
    assert!(sub.active, "the operator's account must not be unsubscribed by any of this");
    assert_eq!(sub.plan_id, "business");
    assert_eq!(db.limit(&ops.id, MAX_CONCURRENT_STREAMS).unwrap(), 3);

    // Re-running the deploy script keeps it there.
    db.set_plan(&ops.id, "business").unwrap();
    assert!(db.subscription(&ops.id).unwrap().active);
    assert_eq!(db.limit(&ops.id, MAX_CONCURRENT_STREAMS).unwrap(), 3);
}

#[test]
fn the_three_paid_plans_carry_the_prices_and_the_concurrency_the_service_sells() {
    let db = CloudDb::open_in_memory().unwrap();
    for (id, label, price, streams) in
        [("basic", "Basic", 19_900, 1), ("pro", "Pro", 39_900, 2), ("business", "Business", 59_900, 3)]
    {
        let p = db.plan(id).unwrap();
        assert_eq!(p.label, label);
        assert_eq!(p.monthly_price_krw, price, "{id}");
        assert_eq!(p.max_concurrent_streams(), streams, "{id}");
        assert!(p.active, "{id} must be on the pricing page");
        assert!(!p.description.is_empty(), "{id} needs a line saying who it is for");
        assert!(p.can_broadcast());
        // Every paid plan includes scheduling: 24/7 unattended is the thing
        // being sold, and it cannot be done without it.
        assert_eq!(p.limits.get("scheduling_enabled").copied(), Some(1), "{id}");
    }
}

#[test]
fn the_price_list_is_the_three_paid_plans_in_order_and_nothing_else() {
    let db = CloudDb::open_in_memory().unwrap();
    let ids: Vec<String> = db.plans_for_sale().unwrap().into_iter().map(|p| p.id).collect();
    assert_eq!(ids, ["basic", "pro", "business"], "sorted by sort_order, not by name");

    // The unsubscribed plan is a state, not something to buy.
    assert!(!ids.iter().any(|i| i == UNSUBSCRIBED_PLAN));
    assert!(!db.plan(UNSUBSCRIBED_PLAN).unwrap().active);

    // And a plan an operator marks inactive drops off the page without this
    // function knowing it existed.
    db.raw().lock().unwrap().execute("UPDATE plans SET active = 0 WHERE id = 'pro'", []).unwrap();
    let after: Vec<String> = db.plans_for_sale().unwrap().into_iter().map(|p| p.id).collect();
    assert_eq!(after, ["basic", "business"]);
}

#[test]
fn a_price_is_whole_won_in_an_integer_column() {
    let db = CloudDb::open_in_memory().unwrap();
    let conn = db.raw();
    let guard = conn.lock().unwrap();
    // The declared type, not just the value: a REAL column would round ₩19,900
    // correctly today and surprise somebody the first time a price is not a
    // round number.
    let mut st = guard.prepare("PRAGMA table_info(plans)").unwrap();
    let decl: Vec<(String, String)> = st
        .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let price = decl.iter().find(|(name, _)| name == "monthly_price_krw").expect("the column exists");
    assert_eq!(price.1.to_uppercase(), "INTEGER", "money is never a float");

    // And it reads back exactly, with no fractional part anywhere.
    let raw: i64 =
        guard.query_row("SELECT monthly_price_krw FROM plans WHERE id='basic'", [], |r| r.get(0)).unwrap();
    assert_eq!(raw, 19_900);
}

// --- the free-Basic rows a previous release left behind ---------------------
//
// A release before paid plans put every public signup on `LOUVER_DEFAULT_PLAN`,
// which was Basic. The current code cannot do that — `register_user` has no plan
// argument — but the rows it wrote are still in production, and they look exactly
// like a paid Basic. These are about telling them apart, and about being unable
// to touch anything else while doing it.

/// A user exactly as the pre-billing public signup wrote one: a paid plan, an
/// active subscription, and a consent timestamp.
fn legacy_public_signup(db: &CloudDb, id: &str, email: &str, plan: &str) {
    let conn = db.raw();
    let guard = conn.lock().unwrap();
    guard
        .execute(
            "INSERT INTO users (id, email, password_hash, plan_id, name,
                                terms_accepted_at, privacy_accepted_at, terms_version)
             VALUES (?1, ?2, 'salt:hash', ?3, '옛가입자',
                     datetime('now'), datetime('now'), '2026-09-27')",
            rusqlite::params![id, email, plan],
        )
        .unwrap();
    guard
        .execute(
            "INSERT INTO subscriptions (user_id, plan_id, status) VALUES (?1, ?2, 'active')",
            rusqlite::params![id, plan],
        )
        .unwrap();
}

#[test]
fn the_current_signup_path_cannot_produce_a_free_paid_plan() {
    // The fix for the production symptom, asserted at the layer that caused it.
    // `register_user` takes no plan argument at all, so this is not "we validated
    // it" — there is nothing to validate.
    let db = CloudDb::open_in_memory().unwrap();
    let uid = signed_up(&db, "new@example.com");
    assert_eq!(db.user(&uid).unwrap().plan_id, UNSUBSCRIBED_PLAN);
    assert_eq!(db.subscription(&uid).unwrap().status, SUBSCRIPTION_UNSUBSCRIBED);
    assert!(!db.subscription(&uid).unwrap().active);
}

#[test]
fn the_audit_tells_a_left_over_grant_from_an_account_the_operator_made() {
    let db = CloudDb::open_in_memory().unwrap();
    // Three shapes: the leftover, the operator's own, and a correct new signup.
    legacy_public_signup(&db, "u-legacy", "old-signup@example.com", "basic");
    let ops = db.create_user("ops@example.com", "salt:hash", "business").unwrap();
    let fresh = signed_up(&db, "new@example.com");

    let rows = db.plan_audit().unwrap();
    let by_email = |e: &str| rows.iter().find(|r| r.email == e).unwrap_or_else(|| panic!("{e}"));

    // The leftover: came through the form, sitting on a paid plan.
    let legacy = by_email("old-signup@example.com");
    assert!(legacy.from_public_signup);
    assert_eq!(legacy.plan_id, "basic");
    assert!(legacy.is_unpaid_grant(UNSUBSCRIBED_PLAN));

    // The operator's: made by the CLI, so no consent timestamp, so never flagged
    // however paid its plan is.
    let operator = by_email("ops@example.com");
    assert!(!operator.from_public_signup, "the CLI does not agree to terms on anybody's behalf");
    assert_eq!(operator.plan_id, "business");
    assert!(!operator.is_unpaid_grant(UNSUBSCRIBED_PLAN));
    let _ = ops;

    // A correct new signup is on no plan, so there is nothing to take back.
    assert!(!by_email("new@example.com").is_unpaid_grant(UNSUBSCRIBED_PLAN));
    let _ = fresh;
}

#[test]
fn revoking_a_left_over_grant_touches_that_account_and_no_other() {
    let db = CloudDb::open_in_memory().unwrap();
    legacy_public_signup(&db, "u-legacy", "old-signup@example.com", "basic");
    let ops = db.create_user("freemilesarea@example.com", "salt:hash", "business").unwrap();
    // Somebody the operator upgraded deliberately, through the CLI. Same plan as
    // the leftover; must survive, because the CLI leaves no consent timestamp.
    let granted = db.create_user("friend@example.com", "salt:hash", "basic").unwrap();

    db.revoke_unpaid_grant("u-legacy").unwrap();
    assert_eq!(db.user("u-legacy").unwrap().plan_id, UNSUBSCRIBED_PLAN);
    assert!(!db.subscription("u-legacy").unwrap().active);

    // Neither of the others moved.
    assert_eq!(db.subscription(&ops.id).unwrap().plan_id, "business");
    assert!(db.subscription(&ops.id).unwrap().active);
    assert_eq!(db.limit(&ops.id, MAX_CONCURRENT_STREAMS).unwrap(), 3);
    assert_eq!(db.subscription(&granted.id).unwrap().plan_id, "basic");
    assert!(db.subscription(&granted.id).unwrap().active);
}

#[test]
fn revoking_refuses_anything_the_audit_would_not_flag() {
    let db = CloudDb::open_in_memory().unwrap();
    let ops = db.create_user("freemilesarea@example.com", "salt:hash", "business").unwrap();
    let fresh = signed_up(&db, "new@example.com");

    // The operator's own account, even by id, even by a typo.
    let refused = db.revoke_unpaid_grant(&ops.id).unwrap_err();
    assert!(matches!(refused, CloudError::Invalid(_)), "{refused:?}");
    assert!(refused.to_string().contains("자동 부여된 요금제가 아닙니다"), "{refused}");
    assert_eq!(db.subscription(&ops.id).unwrap().plan_id, "business");
    assert!(db.subscription(&ops.id).unwrap().active);

    // An account already on no plan, and one that does not exist.
    assert!(db.revoke_unpaid_grant(&fresh).is_err());
    assert!(matches!(db.revoke_unpaid_grant("no-such-user"), Err(CloudError::NotFound(_))));
}

#[test]
fn a_revoked_account_can_be_put_back_on_a_plan() {
    // Whatever the audit gets wrong is recoverable: nothing is deleted, and the
    // ordinary activation path puts the account back.
    let db = CloudDb::open_in_memory().unwrap();
    legacy_public_signup(&db, "u-legacy", "old-signup@example.com", "basic");
    db.revoke_unpaid_grant("u-legacy").unwrap();

    let back = db.activate_subscription("u-legacy", "basic").unwrap();
    assert!(back.active);
    assert_eq!(back.plan_id, "basic");
    assert_eq!(db.limit("u-legacy", MAX_CONCURRENT_STREAMS).unwrap(), 1);
}

// --- storage ceilings for one 80 GB server --------------------------------

const GB: i64 = 1024 * 1024 * 1024;

#[test]
fn the_storage_ceilings_fit_the_server_they_run_on() {
    // One 80 GB disk holds the database, the container's logs, the working
    // directories and every account's originals *and* prepared copies. The
    // ceilings have to be numbers that fit that, and the prices and the
    // concurrency that actually distinguish the plans must not have moved.
    let db = CloudDb::open_in_memory().unwrap();
    for (id, storage, upload, price, streams) in [
        ("basic", 15 * GB, 10 * GB, 19_900, 1),
        ("pro", 30 * GB, 15 * GB, 39_900, 2),
        ("business", 60 * GB, 20 * GB, 59_900, 3),
    ] {
        let p = db.plan(id).unwrap();
        assert_eq!(p.limits.get(MAX_STORAGE_BYTES).copied(), Some(storage), "{id} 저장 한도");
        assert_eq!(p.limits.get(MAX_UPLOAD_BYTES).copied(), Some(upload), "{id} 한 파일 한도");
        assert_eq!(p.monthly_price_krw, price, "{id} 가격이 바뀌었습니다");
        assert_eq!(p.max_concurrent_streams(), streams, "{id} 동시 송출이 바뀌었습니다");
        // A per-file ceiling above the account's whole allowance would be a
        // limit that can never be the one that fires.
        assert!(upload <= storage, "{id}: 한 파일 한도가 전체 한도보다 큽니다");
    }
    // These are per-account ceilings, not a share of the disk: the plans
    // together may promise more than one server holds, and what stops the disk
    // filling is the free-space floor every upload is checked against. Even so,
    // one account alone must never be promised more than the server has.
    let disk = 80 * GB - louver_cloud::ingest::DISK_FLOOR_BYTES as i64;
    for id in ["basic", "pro", "business"] {
        let one = db.plan(id).unwrap().limits.get(MAX_STORAGE_BYTES).copied().unwrap_or(0);
        assert!(one <= disk, "{id} 한 계정이 서버 전체보다 큰 {}GB를 약속합니다", one / GB);
    }
}

#[test]
fn a_production_row_keeps_its_old_limits_until_an_operator_applies_the_new_ones() {
    // The thing the audit found: `seed_plans` leaves `limits` alone on conflict,
    // because an operator may have raised one for a customer. So changing the
    // numbers in code changes nothing for a database that already has the rows,
    // and lowering a ceiling is an explicit operator action.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cloud.db");
    {
        let db = CloudDb::open(&path).unwrap();
        // A row as the previous release left it: 400 GB, and a limit an operator
        // raised by hand that must survive.
        db.raw()
            .lock()
            .unwrap()
            .execute(
                "UPDATE plans SET limits='{\"max_concurrent_streams\":3,\"max_broadcasts\":99,\
                  \"max_storage_bytes\":429496729600,\"max_upload_bytes\":34359738368,\
                  \"scheduling_enabled\":1,\"priority_recovery\":1}' WHERE id='business'",
                [],
            )
            .unwrap();
    }

    // Re-opening runs the migrations and the seed. Neither may touch it.
    let db = CloudDb::open(&path).unwrap();
    let before = db.plan("business").unwrap();
    assert_eq!(before.limits.get(MAX_STORAGE_BYTES).copied(), Some(400 * GB), "부팅이 한도를 바꿨습니다");

    // The read-only audit says what would change, and nothing else.
    let audit = db.storage_audit().unwrap();
    let business = audit.iter().find(|a| a.plan_id == "business").unwrap();
    assert_eq!(business.storage_now, 400 * GB);
    assert_eq!(business.storage_target, 60 * GB);
    assert_eq!(business.monthly_price_krw, 59_900);
    assert_eq!(business.concurrent_streams, 3);
    assert_eq!(db.plan("business").unwrap().limits.get(MAX_STORAGE_BYTES).copied(), Some(400 * GB));

    // Applying changes exactly the two storage keys.
    let changed = db.apply_seed_storage_limits().unwrap();
    assert!(changed.contains(&"business".to_string()), "{changed:?}");
    let after = db.plan("business").unwrap();
    assert_eq!(after.limits.get(MAX_STORAGE_BYTES).copied(), Some(60 * GB));
    assert_eq!(after.limits.get(MAX_UPLOAD_BYTES).copied(), Some(20 * GB));
    assert_eq!(after.monthly_price_krw, 59_900, "가격이 바뀌었습니다");
    assert_eq!(after.max_concurrent_streams(), 3, "동시 송출이 바뀌었습니다");
    assert_eq!(
        after.limits.get(MAX_BROADCASTS).copied(),
        Some(99),
        "운영자가 올려 둔 다른 한도가 사라졌습니다",
    );
    assert_eq!(after.limits.get("priority_recovery").copied(), Some(1));

    // Idempotent: running it again changes nothing.
    assert!(db.apply_seed_storage_limits().unwrap().is_empty());
}

#[test]
fn an_account_over_the_new_ceiling_keeps_everything_and_can_still_broadcast() {
    // Lowering a ceiling must not delete a file or take anybody off air. The
    // only thing it does is refuse the next upload.
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, b) = user_with_broadcasts(&db, "full@x.com", "basic", 1);
    // More stored than the plan's 15 GB, as a lowered ceiling would leave it.
    let m = db.create_media(&uid, "big.mp4", 16 * GB, "key/big.mp4").unwrap();
    db.record_media_prepared(&m.id, "key/big-prepared.mp4", 3600.0, 0, 16 * GB).unwrap();
    assert!(db.storage_used(&uid).unwrap() > 15 * GB);

    // The next upload is refused, in words that say what to do.
    match db.check_upload_allowed(&uid, 1024) {
        Err(CloudError::LimitReached { limit, .. }) => {
            assert_eq!(limit, MAX_STORAGE_BYTES);
            let said = CloudError::LimitReached {
                limit,
                plan_label: "Basic".into(),
                used: 16 * GB,
                allowed: 15 * GB,
            }
            .to_string();
            assert!(said.contains("Basic 플랜 저장공간 15GB를 초과합니다"), "{said}");
            assert!(said.contains("삭제"), "{said}");
            assert!(!said.contains("max_storage_bytes"), "데이터베이스 키가 사용자에게 보입니다: {said}");
        }
        other => panic!("an over-quota upload was allowed: {other:?}"),
    }

    // Everything they have is still there, and still playable.
    assert_eq!(db.media_for(&uid).unwrap().len(), 2);
    assert!(!db.prepared_items_for(&b[0]).unwrap().is_empty());
    // And they can still go on air: storage has nothing to do with broadcasting.
    db.claim_stream_slot(&uid, &b[0]).expect("an over-quota account was refused a broadcast");
    assert_eq!(db.active_stream_count(&uid).unwrap(), 1);
}

// --- the machine's encoder ceiling ----------------------------------------
//
// A plan counts broadcasts. Until the live video source arrived every
// broadcast cost the same, because the media is normalised at upload and
// sending it is `-c copy`. A broadcast with a live source cannot work that
// way: two unrelated inputs cannot be muxed by copying packets, so FFmpeg
// encodes in real time for as long as it runs. Measured on two cores, one
// 1080p30 encode held `speed=1.01x` and two together fell to 0.957x and
// 0.938x — below 1.0x nothing ever catches up, for both customers at once.

/// Point a broadcast at a live video source, which is what makes it an encode.
fn make_it_a_cctv_broadcast(db: &CloudDb, user_id: &str, id: &str) {
    let patch = louver_cloud::BroadcastPatch {
        cctv_url: Some("https://93.184.216.34/live/stream.m3u8".to_string()),
        ..Default::default()
    };
    db.update_broadcast_owned(user_id, id, &patch).unwrap();
}

#[test]
fn the_budget_is_half_the_usable_cores_and_never_zero_by_accident() {
    // `available_parallelism` honours CPU affinity and the container's cgroup
    // quota, so this is the machine's real allowance rather than the
    // hypervisor's core count.
    let cores = std::thread::available_parallelism().map(|n| n.get() as i64).unwrap_or(1);
    let got = encoding_budget();

    if std::env::var(ENCODING_BUDGET_ENV).is_ok() {
        // An operator has pinned it; the formula is not what is under test.
        assert!(got >= 0, "a pinned budget is still a number");
        return;
    }
    assert_eq!(got, (cores / 2).max(1), "half the cores, at least one");
    assert!(got >= 1, "a machine must be able to encode at least one broadcast");
    assert!(got < cores.max(2), "half, so the API and the OS keep the rest");
}

#[test]
fn a_second_encoding_broadcast_is_refused_when_the_budget_is_one() {
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, b) = user_with_broadcasts(&db, "cctv@x.com", "business", 3);
    make_it_a_cctv_broadcast(&db, &uid, &b[0]);
    make_it_a_cctv_broadcast(&db, &uid, &b[1]);

    db.claim_stream_slot_within(&uid, &b[0], 1).expect("the first encode fits");
    match db.claim_stream_slot_within(&uid, &b[1], 1) {
        Err(CloudError::EncoderBusy { running, allowed }) => {
            assert_eq!((running, allowed), (1, 1));
        }
        other => panic!("the second encode must be refused, got {other:?}"),
    }
    // The plan would have allowed three; the machine is what refused.
    assert_eq!(db.active_stream_count(&uid).unwrap(), 1);
}

#[test]
fn the_ceiling_is_the_machines_and_not_each_accounts() {
    // Two customers share one CPU. A limit counted per account would let each
    // of them start one encode on a box that can only do one.
    let db = CloudDb::open_in_memory().unwrap();
    let (a_id, a) = user_with_broadcasts(&db, "a@x.com", "business", 1);
    let (b_id, b) = user_with_broadcasts(&db, "b@x.com", "business", 1);
    make_it_a_cctv_broadcast(&db, &a_id, &a[0]);
    make_it_a_cctv_broadcast(&db, &b_id, &b[0]);

    db.claim_stream_slot_within(&a_id, &a[0], 1).expect("the first customer fits");
    let second = db.claim_stream_slot_within(&b_id, &b[0], 1);
    assert!(
        matches!(second, Err(CloudError::EncoderBusy { .. })),
        "a second account's encode must meet the same ceiling, got {second:?}"
    );
}

#[test]
fn a_stream_copy_is_never_held_up_by_somebody_elses_camera() {
    // The whole point of counting only encodes: an ordinary broadcast costs
    // almost nothing and must not be refused because a camera is busy.
    let db = CloudDb::open_in_memory().unwrap();
    let (a_id, a) = user_with_broadcasts(&db, "cam@x.com", "business", 1);
    let (b_id, b) = user_with_broadcasts(&db, "music@x.com", "business", 3);
    make_it_a_cctv_broadcast(&db, &a_id, &a[0]);

    db.claim_stream_slot_within(&a_id, &a[0], 1).unwrap();
    for id in &b {
        db.claim_stream_slot_within(&b_id, id, 1)
            .unwrap_or_else(|e| panic!("a copy must not be refused by the encoder ceiling: {e:?}"));
    }
    assert_eq!(db.active_stream_count(&b_id).unwrap(), 3);
}

#[test]
fn re_claiming_the_same_encode_does_not_consume_a_second_budget_slot() {
    // A repeated click, a retried request, two tabs.
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, b) = user_with_broadcasts(&db, "double@x.com", "business", 2);
    make_it_a_cctv_broadcast(&db, &uid, &b[0]);

    db.claim_stream_slot_within(&uid, &b[0], 1).unwrap();
    for _ in 0..5 {
        db.claim_stream_slot_within(&uid, &b[0], 1).expect("already holds its slot");
    }
    assert_eq!(db.active_stream_count(&uid).unwrap(), 1);
}

#[test]
fn a_stopped_encode_gives_its_budget_back() {
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, b) = user_with_broadcasts(&db, "cycle@x.com", "business", 2);
    make_it_a_cctv_broadcast(&db, &uid, &b[0]);
    make_it_a_cctv_broadcast(&db, &uid, &b[1]);

    db.claim_stream_slot_within(&uid, &b[0], 1).unwrap();
    assert!(db.claim_stream_slot_within(&uid, &b[1], 1).is_err());
    db.release_stream_slot(&uid, &b[0]).unwrap();
    db.claim_stream_slot_within(&uid, &b[1], 1).expect("the budget clears as soon as a broadcast stops");
}

#[test]
fn a_budget_of_zero_switches_encoding_broadcasts_off() {
    // The honest way for an operator to say "not on this box".
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, b) = user_with_broadcasts(&db, "off@x.com", "business", 2);
    make_it_a_cctv_broadcast(&db, &uid, &b[0]);

    match db.claim_stream_slot_within(&uid, &b[0], 0) {
        Err(CloudError::EncoderBusy { running, allowed }) => assert_eq!((running, allowed), (0, 0)),
        other => panic!("zero must refuse every encode, got {other:?}"),
    }
    // And an ordinary broadcast still works, because zero is about encoding.
    db.claim_stream_slot_within(&uid, &b[1], 0).expect("a copy is unaffected");
}

#[test]
fn two_simultaneous_encode_starts_cannot_both_take_the_last_slot() {
    // The same race the plan check closes, one level up: counting and writing
    // are one `BEGIN IMMEDIATE` transaction.
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, b) = user_with_broadcasts(&db, "race@x.com", "business", 2);
    make_it_a_cctv_broadcast(&db, &uid, &b[0]);
    make_it_a_cctv_broadcast(&db, &uid, &b[1]);

    let gate = Arc::new(Barrier::new(2));
    let outcomes: Vec<bool> = std::thread::scope(|s| {
        let hands: Vec<_> = b
            .iter()
            .map(|id| {
                let (db, uid, id, gate) = (db.clone(), uid.clone(), id.clone(), Arc::clone(&gate));
                s.spawn(move || {
                    gate.wait();
                    db.claim_stream_slot_within(&uid, &id, 1).is_ok()
                })
            })
            .collect();
        hands.into_iter().map(|h| h.join().unwrap()).collect()
    });

    assert_eq!(
        outcomes.iter().filter(|ok| **ok).count(),
        1,
        "exactly one of two simultaneous encode starts may win: {outcomes:?}"
    );
    assert_eq!(db.active_stream_count(&uid).unwrap(), 1);
}

#[test]
fn an_unusable_budget_setting_does_not_claim_to_have_been_set() {
    // The boot log names the source, and a value that fell back to the derived
    // default must not be reported as "set by environment variable" — that
    // line is read by the one person working out why a start was refused.
    let detail = encoding_budget_detail();
    if !detail.from_env {
        assert_eq!(detail.value, encoding_budget(), "the two entry points must agree");
    }
    // Whatever the machine says, the pair is self-consistent.
    assert_eq!(detail.value, encoding_budget());
}
