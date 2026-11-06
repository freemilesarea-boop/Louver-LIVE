//! Entitlement an operator handed out, and the money it must never touch.
//!
//! Two things are being proved here. The first is the ladder: several sources of
//! entitlement may apply at once and the strongest wins, so a grant can lift an
//! account and never lower one. The second is the wall: a grant is not a
//! payment, and every figure the business runs on has to be blind to it.

use louver_cloud::entitlement::{MAX_CONCURRENT_STREAMS, MAX_STORAGE_BYTES, MAX_UPLOAD_BYTES};
use louver_cloud::grants::{NewGrant, OnExisting, Term, MAX_BULK_TARGETS, MAX_GRANT_DAYS};
use louver_cloud::{CloudDb, CloudError};

const GB: i64 = 1024 * 1024 * 1024;

fn db() -> (tempfile::TempDir, CloudDb) {
    let d = tempfile::tempdir().unwrap();
    let db = CloudDb::open(&d.path().join("cloud.db")).unwrap();
    (d, db)
}

fn user(db: &CloudDb, email: &str, plan: &str) -> String {
    let u = db.create_user(email, "hash", plan).unwrap();
    u.id
}

fn admin(db: &CloudDb) -> (String, String) {
    let id = user(db, "boss@x.com", "business");
    db.set_role("boss@x.com", louver_cloud::ROLE_ADMIN).unwrap();
    (id, "boss@x.com".to_string())
}

/// A grant running from now for `days`.
fn grant(db: &CloudDb, who: &[&str], plan: &str, days: i64) -> louver_cloud::grants::BulkGrantResult {
    grant_as(db, who, plan, days, OnExisting::Extend)
}

fn grant_as(
    db: &CloudDb,
    who: &[&str],
    plan: &str,
    days: i64,
    on_existing: OnExisting,
) -> louver_cloud::grants::BulkGrantResult {
    let (aid, amail) = admin_of(db);
    let ids: Vec<String> = who.iter().map(|s| s.to_string()).collect();
    db.create_grants(
        &ids,
        &NewGrant { plan_id: plan, term: Term::Days(days), reason: "6기 수강생 혜택" },
        on_existing,
        &aid,
        &amail,
    )
    .unwrap()
}

/// The admin row every test writes its grants as, made once.
fn admin_of(db: &CloudDb) -> (String, String) {
    match db.admins() {
        Ok(list) if !list.is_empty() => {
            let email = list[0].clone();
            let id = db.user_by_email(&email).unwrap().id;
            (id, email)
        }
        _ => admin(db),
    }
}

/// A subscription whose status lapsed while the plan stayed on the row.
///
/// Written with SQL because no helper does exactly this: the product's own
/// cancel path moves the plan to unsubscribed as well, and what this needs is
/// the other shape — a card that stopped working, which is how an account ends
/// up with `plan_id = basic` and a status that is not active.
fn lapse(db: &CloudDb, user_id: &str) {
    db.raw()
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO subscriptions (user_id, plan_id, status)
             SELECT id, plan_id, 'unsubscribed' FROM users WHERE id = ?1
             ON CONFLICT(user_id) DO UPDATE SET status = 'unsubscribed'",
            [user_id],
        )
        .unwrap();
}

// --- the ladder -------------------------------------------------------------

/// Case 1 — nothing paid, a Business grant: Business.
#[test]
fn an_unsubscribed_account_with_a_grant_gets_the_granted_plan() {
    let (_d, db) = db();
    let u = user(&db, "a@x.com", "none");
    assert!(!db.subscription(&u).unwrap().active, "a new account starts unsubscribed");
    assert_eq!(db.limit(&u, MAX_CONCURRENT_STREAMS).unwrap(), 0);

    grant(&db, &[&u], "business", 30);

    let sub = db.subscription(&u).unwrap();
    assert!(sub.active, "a live grant must let an account broadcast");
    assert_eq!(sub.plan_id, "business");
    assert_eq!(db.limit(&u, MAX_CONCURRENT_STREAMS).unwrap(), 3);
    assert_eq!(db.limit(&u, MAX_STORAGE_BYTES).unwrap(), 60 * GB);
    assert_eq!(db.limit(&u, MAX_UPLOAD_BYTES).unwrap(), 20 * GB);
    db.require_active_subscription(&u).expect("the one gate every START goes through");
}

/// Case 2 — paying for Basic, granted Business: Business while it lasts.
#[test]
fn a_grant_stronger_than_the_subscription_takes_over() {
    let (_d, db) = db();
    let u = user(&db, "b@x.com", "basic");
    assert_eq!(db.limit(&u, MAX_CONCURRENT_STREAMS).unwrap(), 1);

    grant(&db, &[&u], "business", 30);

    assert_eq!(db.subscription(&u).unwrap().plan_id, "business");
    assert_eq!(db.limit(&u, MAX_CONCURRENT_STREAMS).unwrap(), 3);
    // And the payment is untouched underneath it.
    assert_eq!(db.user(&u).unwrap().plan_id, "basic", "the paid plan was overwritten");
}

/// Case 3 — the same account once the grant has run out: back to Basic, on its
/// own, with nothing having had to run.
#[test]
fn an_expired_grant_falls_back_to_the_subscription() {
    let (_d, db) = db();
    let u = user(&db, "c@x.com", "basic");
    let (aid, amail) = admin_of(&db);
    db.create_grants(
        std::slice::from_ref(&u),
        &NewGrant {
            plan_id: "business",
            term: Term::Between { from: "2026-08-01".into(), to: "2026-08-31".into() },
            reason: "지난 이벤트",
        },
        OnExisting::Extend,
        &aid,
        &amail,
    )
    .unwrap();

    assert_eq!(db.subscription(&u).unwrap().plan_id, "basic");
    assert_eq!(db.limit(&u, MAX_CONCURRENT_STREAMS).unwrap(), 1);
    assert!(db.active_grant_plan(&u).unwrap().is_none(), "an ended grant must grant nothing");
}

/// Case 4 — paying for Business, granted Basic: still Business. A grant may
/// never take anything away.
#[test]
fn a_weaker_grant_cannot_lower_a_paid_plan() {
    let (_d, db) = db();
    let u = user(&db, "d@x.com", "business");
    grant(&db, &[&u], "basic", 30);

    let sub = db.subscription(&u).unwrap();
    assert_eq!(sub.plan_id, "business", "a Basic grant pulled a Business customer down");
    assert_eq!(db.limit(&u, MAX_CONCURRENT_STREAMS).unwrap(), 3);
    assert_eq!(db.limit(&u, MAX_STORAGE_BYTES).unwrap(), 60 * GB);
}

/// Case 15 — a grant that has not started yet grants nothing either.
#[test]
fn a_grant_that_has_not_started_grants_nothing() {
    let (_d, db) = db();
    let u = user(&db, "e@x.com", "none");
    let (aid, amail) = admin_of(&db);
    db.create_grants(
        std::slice::from_ref(&u),
        &NewGrant {
            plan_id: "business",
            term: Term::Between { from: "2099-01-01".into(), to: "2099-01-31".into() },
            reason: "예약 지급",
        },
        OnExisting::Extend,
        &aid,
        &amail,
    )
    .unwrap();

    assert!(!db.subscription(&u).unwrap().active);
    assert_eq!(db.limit(&u, MAX_CONCURRENT_STREAMS).unwrap(), 0);
    let g = &db.grants_for(&u).unwrap()[0];
    assert_eq!(g.state, louver_cloud::grants::GrantState::Scheduled);
}

// --- revoking ---------------------------------------------------------------

/// Case 5 — revoked with nothing paid: unsubscribed again.
#[test]
fn revoking_with_no_subscription_leaves_the_account_unsubscribed() {
    let (_d, db) = db();
    let u = user(&db, "f@x.com", "none");
    let out = grant(&db, &[&u], "business", 30);
    assert!(db.subscription(&u).unwrap().active);

    let (aid, amail) = admin_of(&db);
    let id = out.outcomes[0].grant_id.clone().unwrap();
    let g = db.revoke_grant(&id, &aid, &amail, "테스트 종료").unwrap();
    assert_eq!(g.state, louver_cloud::grants::GrantState::Revoked);
    assert!(g.revoked_at.is_some());
    assert_eq!(g.revoke_reason.as_deref(), Some("테스트 종료"));

    assert!(!db.subscription(&u).unwrap().active);
    assert_eq!(db.limit(&u, MAX_CONCURRENT_STREAMS).unwrap(), 0);
    assert!(matches!(db.require_active_subscription(&u), Err(CloudError::NoSubscription)));
}

/// Case 6 — revoked with a Basic subscription underneath: straight back to it.
#[test]
fn revoking_returns_an_account_to_what_it_pays_for() {
    let (_d, db) = db();
    let u = user(&db, "g@x.com", "basic");
    let out = grant(&db, &[&u], "business", 30);
    assert_eq!(db.subscription(&u).unwrap().plan_id, "business");

    let (aid, amail) = admin_of(&db);
    db.revoke_grant(&out.outcomes[0].grant_id.clone().unwrap(), &aid, &amail, "CS 종결").unwrap();

    let sub = db.subscription(&u).unwrap();
    assert!(sub.active, "a paying account must not be switched off by a revoke");
    assert_eq!(sub.plan_id, "basic");
    assert_eq!(db.limit(&u, MAX_CONCURRENT_STREAMS).unwrap(), 1);
}

/// A revoke needs a reason, and cannot happen twice.
#[test]
fn a_revoke_is_recorded_once_and_needs_a_reason() {
    let (_d, db) = db();
    let u = user(&db, "h@x.com", "none");
    let out = grant(&db, &[&u], "pro", 30);
    let id = out.outcomes[0].grant_id.clone().unwrap();
    let (aid, amail) = admin_of(&db);

    assert!(db.revoke_grant(&id, &aid, &amail, "   ").is_err(), "an empty reason was accepted");
    db.revoke_grant(&id, &aid, &amail, "사유").unwrap();
    assert!(db.revoke_grant(&id, &aid, &amail, "또").is_err(), "a grant was revoked twice");
}

// --- the wall between a grant and the money --------------------------------

/// Cases 7, 8 and 9 — a grant's whole life leaves the money untouched.
#[test]
fn a_grant_never_appears_in_the_money() {
    let (_d, db) = db();
    let paying = user(&db, "pays@x.com", "basic");
    // A real payment, so the figures are not all zero and a change would show.
    let bill = db.open_billing_subscription(&paying, "basic", "payapp", 19900).unwrap();
    db.attach_provider_subscription(&bill.id, "rebill-pays").unwrap();
    db.record_billing_payment(&louver_cloud::db::BillingPayment {
        provider: "payapp",
        event_key: "evt-pays-1",
        billing_id: &bill.id,
        user_id: &paying,
        provider_subscription_id: Some("rebill-pays"),
        pay_state: "4",
        amount_krw: 19900,
        pay_date: Some("2026-10-01 10:00:00"),
        pay_type: Some("card"),
        outcome: "결제완료",
        status: Some(louver_cloud::BillingStatus::Active),
        paid_at: Some("2026-10-01 10:00:00"),
        period_end: Some("2026-11-01"),
    })
    .unwrap();
    db.activate_subscription(&paying, "basic").unwrap();

    let before_mrr = db.mrr().unwrap();
    let before_rev = db.revenue_summary().unwrap();
    let before_payers = db.active_payers().unwrap();
    let before_events: i64 =
        db.raw().lock().unwrap().query_row("SELECT COUNT(*) FROM billing_events", [], |r| r.get(0)).unwrap();
    let before_subs: i64 = db
        .raw()
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM billing_subscriptions", [], |r| r.get(0))
        .unwrap();
    let before_billing = db.billing_subscriptions_for(&paying).unwrap();

    // Grant, extend, change plan, revoke: the whole life of one.
    let free = user(&db, "free@x.com", "none");
    let out = grant(&db, &[&paying, &free], "business", 30);
    let id = out.outcomes[0].grant_id.clone().unwrap();
    let (aid, amail) = admin_of(&db);
    let extended = db.extend_grant(&id, 30, &aid, &amail, "연장").unwrap();
    let changed = db.change_grant_plan(&extended.id, "pro", &aid, &amail, "하향 조정").unwrap();
    db.revoke_grant(&changed.id, &aid, &amail, "종료").unwrap();

    // Case 9: MRR is the sum of active recurring records and nothing else.
    assert_eq!(db.mrr().unwrap(), before_mrr, "a grant moved MRR");
    // Case 8: the ledger did not gain a row.
    let after_events: i64 =
        db.raw().lock().unwrap().query_row("SELECT COUNT(*) FROM billing_events", [], |r| r.get(0)).unwrap();
    assert_eq!(after_events, before_events, "a grant wrote to billing_events");
    let after_subs: i64 = db
        .raw()
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM billing_subscriptions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after_subs, before_subs, "a grant wrote to billing_subscriptions");
    // Revenue, every bucket of it.
    let after_rev = db.revenue_summary().unwrap();
    assert_eq!(after_rev.all_time, before_rev.all_time, "a grant moved revenue");
    assert_eq!(after_rev.this_month, before_rev.this_month);
    assert_eq!(after_rev.today, before_rev.today);
    assert_eq!(db.active_payers().unwrap(), before_payers, "a grant was counted as a payer");
    // Case 7: the recurring payment is exactly where it was, term included.
    let after_billing = db.billing_subscriptions_for(&paying).unwrap();
    assert_eq!(after_billing.len(), before_billing.len());
    for (a, b) in after_billing.iter().zip(before_billing.iter()) {
        assert_eq!(a.status, b.status, "a grant changed a PayApp record's status");
        assert_eq!(a.plan_id, b.plan_id, "a grant changed a PayApp record's plan");
        assert_eq!(a.amount_krw, b.amount_krw);
        assert_eq!(a.current_period_end, b.current_period_end, "a grant moved the paid term");
        assert_eq!(a.cancelled_at, b.cancelled_at);
        assert_eq!(a.provider_subscription_id, b.provider_subscription_id);
    }
    assert_eq!(db.user(&paying).unwrap().plan_id, "basic", "a grant rewrote users.plan_id");

    // And the free account is a grant, not a customer.
    assert_eq!(db.payments(Some(&free), 50, None).unwrap().len(), 0);
}

/// The grant figures are their own, and they count what they should.
#[test]
fn the_dashboard_counts_grants_separately() {
    let (_d, db) = db();
    let a = user(&db, "i@x.com", "none");
    let b = user(&db, "j@x.com", "none");
    let c = user(&db, "k@x.com", "basic");
    grant(&db, &[&a, &b], "business", 30);
    grant(&db, &[&c], "pro", 3);

    let counts = db.grant_counts().unwrap();
    assert_eq!(counts.active, 3);
    assert_eq!(counts.expiring_7d, 1, "only the three-day grant ends within a week");
    assert_eq!(counts.scheduled, 0);
    let by: std::collections::HashMap<&str, i64> =
        counts.by_plan.iter().map(|(id, _, n)| (id.as_str(), *n)).collect();
    assert_eq!(by.get("business"), Some(&2));
    assert_eq!(by.get("pro"), Some(&1));
}

// --- bulk -------------------------------------------------------------------

/// Case 10 — many accounts at once.
#[test]
fn a_bulk_grant_reaches_every_account_in_the_list() {
    let (_d, db) = db();
    let ids: Vec<String> = (0..12).map(|n| user(&db, &format!("bulk{n}@x.com"), "none")).collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let out = grant(&db, &refs, "pro", 30);

    assert_eq!(out.outcomes.len(), 12);
    assert!(out.outcomes.iter().all(|o| o.action == "created"));
    assert_eq!(out.plan_label, "Pro");
    for id in &ids {
        assert_eq!(db.subscription(id).unwrap().plan_id, "pro");
    }
    // One batch id ties the run together.
    assert!(out.outcomes.iter().all(|o| o.grant_id.is_some()));
    let batched: i64 = db
        .raw()
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM admin_grants WHERE batch_id = ?1", [&out.batch_id], |r| r.get(0))
        .unwrap();
    assert_eq!(batched, 12);
}

/// Case 11 — one bad id in the list writes nothing at all.
#[test]
fn a_bulk_grant_with_one_bad_target_writes_nothing() {
    let (_d, db) = db();
    let good = user(&db, "good@x.com", "none");
    let (aid, amail) = admin_of(&db);

    let err = db
        .create_grants(
            &[good.clone(), "no-such-user".to_string()],
            &NewGrant { plan_id: "business", term: Term::Days(30), reason: "이벤트" },
            OnExisting::Extend,
            &aid,
            &amail,
        )
        .unwrap_err();
    assert!(matches!(err, CloudError::Invalid(_)), "{err:?}");

    // All or nothing: the good account did not get half a grant.
    assert!(!db.subscription(&good).unwrap().active, "a refused batch granted something");
    let rows: i64 =
        db.raw().lock().unwrap().query_row("SELECT COUNT(*) FROM admin_grants", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 0);
}

/// A disabled account is refused, and refuses the whole batch with it.
#[test]
fn a_disabled_account_cannot_be_granted() {
    let (_d, db) = db();
    let off = user(&db, "off@x.com", "none");
    db.set_disabled(&off, true).unwrap();
    let (aid, amail) = admin_of(&db);
    let err = db
        .create_grants(
            std::slice::from_ref(&off),
            &NewGrant { plan_id: "business", term: Term::Days(30), reason: "이벤트" },
            OnExisting::Extend,
            &aid,
            &amail,
        )
        .unwrap_err();
    assert!(matches!(err, CloudError::Invalid(_)), "{err:?}");
}

/// The input guards: a list too long, a term too long, a window the wrong way
/// round, a plan that grants nothing, and no reason.
#[test]
fn the_input_is_checked_before_anything_is_written() {
    let (_d, db) = db();
    let u = user(&db, "l@x.com", "none");
    let (aid, amail) = admin_of(&db);
    let mk = |plan: &'static str, term: Term, reason: &'static str| NewGrant { plan_id: plan, term, reason };

    let too_many: Vec<String> = (0..=MAX_BULK_TARGETS).map(|_| u.clone()).collect();
    assert!(db
        .create_grants(&too_many, &mk("pro", Term::Days(1), "r"), OnExisting::Extend, &aid, &amail)
        .is_err());

    assert!(db
        .create_grants(
            std::slice::from_ref(&u),
            &mk("pro", Term::Days(MAX_GRANT_DAYS + 1), "r"),
            OnExisting::Extend,
            &aid,
            &amail
        )
        .is_err());

    assert!(
        db.create_grants(
            std::slice::from_ref(&u),
            &mk("pro", Term::Between { from: "2026-10-10".into(), to: "2026-10-01".into() }, "r"),
            OnExisting::Extend,
            &aid,
            &amail
        )
        .is_err(),
        "a window the wrong way round was accepted"
    );
    assert!(
        db.create_grants(
            std::slice::from_ref(&u),
            &mk("pro", Term::Between { from: "2026-02-31".into(), to: "2026-03-05".into() }, "r"),
            OnExisting::Extend,
            &aid,
            &amail
        )
        .is_err(),
        "an impossible date was accepted"
    );
    assert!(
        db.create_grants(
            std::slice::from_ref(&u),
            &mk("pro", Term::Between { from: "oct 1".into(), to: "2026-10-05".into() }, "r"),
            OnExisting::Extend,
            &aid,
            &amail
        )
        .is_err(),
        "a free-text date was accepted"
    );

    assert!(db
        .create_grants(
            std::slice::from_ref(&u),
            &mk("none", Term::Days(1), "r"),
            OnExisting::Extend,
            &aid,
            &amail
        )
        .is_err());

    assert!(db
        .create_grants(
            std::slice::from_ref(&u),
            &mk("pro", Term::Days(1), "  "),
            OnExisting::Extend,
            &aid,
            &amail
        )
        .is_err());

    assert!(db
        .create_grants(&[u], &mk("nope", Term::Days(1), "r"), OnExisting::Extend, &aid, &amail)
        .is_err());

    let rows: i64 =
        db.raw().lock().unwrap().query_row("SELECT COUNT(*) FROM admin_grants", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 0, "a refused request wrote a row");
}

// --- an account that already holds one -------------------------------------

/// Extending adds to the end of the term that is running, and keeps the row it
/// replaces so the history reads as an extension.
#[test]
fn extending_adds_to_the_end_and_keeps_the_old_row() {
    let (_d, db) = db();
    let u = user(&db, "m@x.com", "none");
    let first = grant(&db, &[&u], "business", 30);
    let first_end = first.outcomes[0].expires_at.clone().unwrap();

    let again = grant_as(&db, &[&u], "business", 30, OnExisting::Extend);
    assert_eq!(again.outcomes[0].action, "extended");
    let second_end = again.outcomes[0].expires_at.clone().unwrap();
    assert!(second_end > first_end, "{second_end} should be after {first_end}");

    // Both rows are there: one superseded, one live.
    let all = db.grants_for(&u).unwrap();
    assert_eq!(all.len(), 2);
    let live: Vec<_> = all.iter().filter(|g| g.state.is_active()).collect();
    assert_eq!(live.len(), 1, "exactly one grant should be in force");
    let old = all.iter().find(|g| g.superseded_by.is_some()).expect("the replaced row is kept");
    assert_eq!(old.superseded_by.as_deref(), live[0].id.as_str().into());
    assert_eq!(db.subscription(&u).unwrap().plan_id, "business");
}

/// An extension must not open a gap.
///
/// The obvious implementation — start the new row where the old one ends —
/// leaves the account with a superseded row and a row that has not begun, so
/// the entitlement disappears from now until the old expiry. It is the opposite
/// of what the operator asked for and it would be invisible until a customer
/// said their stream stopped.
#[test]
fn extending_never_leaves_the_account_without_entitlement() {
    let (_d, db) = db();
    let u = user(&db, "gap@x.com", "none");
    grant(&db, &[&u], "business", 30);
    assert!(db.subscription(&u).unwrap().active);

    grant_as(&db, &[&u], "business", 30, OnExisting::Extend);

    let sub = db.subscription(&u).unwrap();
    assert!(sub.active, "extending a live grant switched the account off");
    assert_eq!(sub.plan_id, "business");
    assert!(db.active_grant_plan(&u).unwrap().is_some());
    db.require_active_subscription(&u).expect("an extended grant must still be in force now");
}

/// Resetting throws the remaining term away and starts again from now.
#[test]
fn resetting_starts_the_term_again_from_today() {
    let (_d, db) = db();
    let u = user(&db, "n@x.com", "none");
    let first = grant(&db, &[&u], "business", 90);
    let long_end = first.outcomes[0].expires_at.clone().unwrap();

    let out = grant_as(&db, &[&u], "business", 30, OnExisting::Reset);
    assert_eq!(out.outcomes[0].action, "reset");
    let new_end = out.outcomes[0].expires_at.clone().unwrap();
    assert!(new_end < long_end, "a reset should shorten a 90-day term to 30");
    assert_eq!(db.grants_for(&u).unwrap().iter().filter(|g| g.state.is_active()).count(), 1);
}

/// Skipping leaves the account exactly as it was and says so.
#[test]
fn skipping_leaves_an_existing_grant_alone() {
    let (_d, db) = db();
    let held = user(&db, "o@x.com", "none");
    let fresh = user(&db, "p@x.com", "none");
    let first = grant(&db, &[&held], "pro", 30);
    let end = first.outcomes[0].expires_at.clone().unwrap();

    let out = grant_as(&db, &[&held, &fresh], "business", 30, OnExisting::Skip);
    let by: std::collections::HashMap<&str, &str> =
        out.outcomes.iter().map(|o| (o.user_id.as_str(), o.action.as_str())).collect();
    assert_eq!(by.get(held.as_str()), Some(&"skipped"));
    assert_eq!(by.get(fresh.as_str()), Some(&"created"));

    // Untouched: still Pro, still ending when it did.
    assert_eq!(db.subscription(&held).unwrap().plan_id, "pro");
    let live = db.grants_for(&held).unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].expires_at, end);
    assert_eq!(db.subscription(&fresh).unwrap().plan_id, "business");
}

/// Changing the plan keeps the term and the trail.
#[test]
fn changing_the_plan_keeps_the_term() {
    let (_d, db) = db();
    let u = user(&db, "q@x.com", "none");
    let out = grant(&db, &[&u], "basic", 30);
    let id = out.outcomes[0].grant_id.clone().unwrap();
    let end = out.outcomes[0].expires_at.clone().unwrap();
    let (aid, amail) = admin_of(&db);

    let now = db.change_grant_plan(&id, "business", &aid, &amail, "CS 보상 상향").unwrap();
    assert_eq!(now.plan_id, "business");
    assert_eq!(now.expires_at, end, "the term moved when only the plan should have");
    assert_eq!(db.subscription(&u).unwrap().plan_id, "business");
    assert_eq!(db.grants_for(&u).unwrap().len(), 2, "the replaced row should be kept");
}

// --- regression -------------------------------------------------------------

/// Case 16 — an account with no grant behaves exactly as it did, and case 18 —
/// the ceilings on sale are the ones the plans carry.
#[test]
fn accounts_without_a_grant_are_untouched() {
    let (_d, db) = db();
    for (plan, streams, storage, upload) in
        [("basic", 1, 15 * GB, 10 * GB), ("pro", 2, 30 * GB, 15 * GB), ("business", 3, 60 * GB, 20 * GB)]
    {
        let u = user(&db, &format!("{plan}@x.com"), plan);
        assert_eq!(db.limit(&u, MAX_CONCURRENT_STREAMS).unwrap(), streams, "{plan} concurrency");
        assert_eq!(db.limit(&u, MAX_STORAGE_BYTES).unwrap(), storage, "{plan} storage");
        assert_eq!(db.limit(&u, MAX_UPLOAD_BYTES).unwrap(), upload, "{plan} per file");
        let sub = db.subscription(&u).unwrap();
        assert!(sub.active);
        assert_eq!(sub.plan_id, plan);
    }
    let none = user(&db, "none@x.com", "none");
    assert!(!db.subscription(&none).unwrap().active);
    assert_eq!(db.limit(&none, MAX_CONCURRENT_STREAMS).unwrap(), 0);

    // A cancelled subscription still behaves as it always has: the limits stay
    // on the row and the gate refuses. A grant is not involved either way.
    let lapsed = user(&db, "lapsed@x.com", "basic");
    lapse(&db, &lapsed);
    assert!(matches!(db.require_active_subscription(&lapsed), Err(CloudError::NoSubscription)));
    assert_eq!(db.limit(&lapsed, MAX_STORAGE_BYTES).unwrap(), 15 * GB);
}

/// A lapsed subscription with a live grant still works until the grant ends.
#[test]
fn a_grant_carries_an_account_whose_subscription_lapsed() {
    let (_d, db) = db();
    let u = user(&db, "r@x.com", "basic");
    lapse(&db, &u);
    assert!(matches!(db.require_active_subscription(&u), Err(CloudError::NoSubscription)));

    grant(&db, &[&u], "basic", 30);

    let sub = db.subscription(&u).unwrap();
    assert!(sub.active, "a grant should carry an account whose card lapsed");
    assert_eq!(sub.plan_id, "basic");
    db.require_active_subscription(&u).expect("the grant is a source of entitlement on its own");
    // And the lapse is still recorded as a lapse.
    assert_eq!(db.user(&u).unwrap().plan_id, "basic");
}
