//! Enterprise: the plan whose size the customer chooses.
//!
//! Enterprise is priced per channel, which is the one thing the billing design
//! says cannot travel in a request — `checkout` reads the amount from a plan row
//! precisely so that no browser can name what it pays. The resolution is that a
//! *count* becomes a *row*: `ensure_enterprise_plan(n)` writes `enterprise_{n}`
//! with the price the server computed, and `checkout` then reads that row like
//! any other. So these tests are about the row, and about the two things that
//! must stay true of it: it is never listed for sale, and re-writing it never
//! changes what anybody is already being charged.

use louver_cloud::db::Signup;
use louver_cloud::entitlement::{CCTV_ENABLED, MAX_CONCURRENT_STREAMS, MAX_STORAGE_BYTES, MAX_UPLOAD_BYTES};
use louver_cloud::{enterprise, CloudDb, CloudError};

fn db() -> CloudDb {
    CloudDb::open_in_memory().unwrap()
}

fn user(db: &CloudDb, email: &str) -> String {
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
fn a_count_becomes_a_plan_row_priced_by_the_server() {
    let db = db();
    let plan = db.ensure_enterprise_plan(4).unwrap();

    assert_eq!(plan.id, "enterprise_4");
    assert_eq!(plan.monthly_price_krw, 76_000);
    assert_eq!(plan.label, "Enterprise 4채널");
    // 4 × 20GB, and the same per-file ceiling Business has.
    assert_eq!(plan.limits.get(MAX_CONCURRENT_STREAMS), Some(&4));
    assert_eq!(plan.limits.get(MAX_STORAGE_BYTES), Some(&(80 * 1024 * 1024 * 1024)));
    assert_eq!(plan.limits.get(MAX_UPLOAD_BYTES), Some(&(20 * 1024 * 1024 * 1024)));
    assert_eq!(plan.limits.get(CCTV_ENABLED), Some(&1));
}

#[test]
fn an_enterprise_row_is_never_on_the_price_list() {
    // `GET /api/plans` filters on `active`, so an active row per channel count
    // would put a card on the pricing page for every number anybody ever typed.
    let db = db();
    for n in [4, 7, 40] {
        let plan = db.ensure_enterprise_plan(n).unwrap();
        assert!(!plan.active, "enterprise_{n} is on the price list");
    }

    let for_sale: Vec<String> = db.plans_for_sale().unwrap().into_iter().map(|p| p.id).collect();
    assert_eq!(for_sale, ["basic", "pro", "business"]);
}

#[test]
fn it_is_nonetheless_buyable_by_id() {
    // Which is the whole point: not listed, and not a second class of plan that
    // `checkout` has to know about.
    let db = db();
    db.ensure_enterprise_plan(6).unwrap();
    let plan = db.purchasable_plan("enterprise_6").unwrap();
    assert_eq!(plan.monthly_price_krw, 114_000);
}

#[test]
fn asking_twice_is_the_same_row() {
    // Two customers buying 5 channels are one row, not two, and the second
    // checkout must not fail on a unique id.
    let db = db();
    let first = db.ensure_enterprise_plan(5).unwrap();
    let again = db.ensure_enterprise_plan(5).unwrap();
    assert_eq!(first.id, again.id);
    assert_eq!(first.monthly_price_krw, again.monthly_price_krw);

    let count: i64 = db
        .raw()
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM plans WHERE id LIKE 'enterprise_%'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn below_the_minimum_is_refused_and_writes_nothing() {
    let db = db();
    for n in [-1, 0, 1, 2, 3] {
        let refused = db.ensure_enterprise_plan(n);
        assert!(matches!(refused, Err(CloudError::Invalid(_))), "{n} channels was accepted");
    }
    let count: i64 = db
        .raw()
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM plans WHERE id LIKE 'enterprise_%'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0, "a refused count left a row behind");
    // And the message points the customer at what they can buy instead.
    let Err(CloudError::Invalid(why)) = db.ensure_enterprise_plan(2) else { panic!() };
    assert!(why.contains("4채널"), "{why}");
    assert!(why.contains("Business"), "{why}");
}

#[test]
fn a_plan_id_that_was_never_generated_is_not_buyable() {
    let db = db();
    // No row, so nothing to read a price from. Refused rather than created: only
    // `ensure_enterprise_plan` makes these, and it is reached by a route that
    // sends a count.
    for id in ["enterprise_9", "enterprise", "enterprise_", "enterprise_0", "enterprise_04", "enterprise_3"] {
        assert!(db.purchasable_plan(id).is_err(), "{id} was buyable");
    }
}

#[test]
fn the_unsubscribed_and_internal_plans_stay_unbuyable() {
    // `purchasable_plan` replaced a `plans_for_sale().find(...)`, so this is the
    // behaviour that had to survive the replacement.
    let db = db();
    for id in ["none", "owner", "", "NONE", "basic "] {
        assert!(db.purchasable_plan(id).is_err(), "{id} was buyable");
    }
    for id in ["basic", "pro", "business"] {
        assert!(db.purchasable_plan(id).is_ok(), "{id} stopped being buyable");
    }
    // Taken off sale by an operator, and immediately unbuyable.
    db.raw().lock().unwrap().execute("UPDATE plans SET active = 0 WHERE id = 'pro'", []).unwrap();
    assert!(db.purchasable_plan("pro").is_err());
}

#[test]
fn a_missing_plan_and_a_withdrawn_one_answer_the_same() {
    // So a checkout cannot be used to ask which plan ids exist.
    let db = db();
    db.raw().lock().unwrap().execute("UPDATE plans SET active = 0 WHERE id = 'pro'", []).unwrap();
    let Err(CloudError::Invalid(missing)) = db.purchasable_plan("no_such_plan") else { panic!() };
    let Err(CloudError::Invalid(withdrawn)) = db.purchasable_plan("pro") else { panic!() };
    assert_eq!(missing, withdrawn);
}

#[test]
fn restating_the_row_reprices_nobody() {
    // The row is rewritten on every checkout for that count, exactly as
    // `seed_plans` rewrites the ladder's prices at every start. That is safe
    // because a subscriber is charged the amount on *their own*
    // `billing_subscriptions` row — which is also what `handle_feedback` checks
    // each notification against. This test is the proof, because the day the two
    // are confused is the day a price change silently recharges every customer.
    let db = db();
    let uid = user(&db, "ent@example.com");
    db.ensure_enterprise_plan(4).unwrap();
    let record = db.open_billing_subscription(&uid, "enterprise_4", "payapp", 76_000).unwrap();

    // Pretend the rate moved: the row now says something else.
    db.raw()
        .lock()
        .unwrap()
        .execute("UPDATE plans SET monthly_price_krw = 999999 WHERE id = 'enterprise_4'", [])
        .unwrap();
    // The next checkout for 4 channels restores the server's own figure…
    assert_eq!(db.ensure_enterprise_plan(4).unwrap().monthly_price_krw, 76_000);
    // …and the existing subscriber's amount is untouched throughout.
    assert_eq!(db.billing_subscription(&record.id).unwrap().amount_krw, 76_000);
}

#[test]
fn a_limit_an_operator_raised_survives_the_next_checkout() {
    // The same split `seed_plans` makes: what the service charges is restated,
    // what an account is allowed is left alone. An operator who widened a
    // customer's ceiling must not have it undone by the next person who buys
    // that many channels.
    let db = db();
    db.ensure_enterprise_plan(4).unwrap();
    db.raw()
        .lock()
        .unwrap()
        .execute("UPDATE plans SET limits = '{\"max_concurrent_streams\":9}' WHERE id = 'enterprise_4'", [])
        .unwrap();

    let plan = db.ensure_enterprise_plan(4).unwrap();
    assert_eq!(plan.limits.get(MAX_CONCURRENT_STREAMS), Some(&9), "the raised limit was reset");
    assert_eq!(plan.monthly_price_krw, 76_000, "the price was not restated");
}

#[test]
fn an_activated_enterprise_account_is_entitled_to_what_it_bought() {
    // The last link: nothing in `entitlement` knows the word "enterprise". It
    // reads the limits by name, so a fourth plan is an INSERT and never an `if`.
    let db = db();
    let uid = user(&db, "big@example.com");
    db.ensure_enterprise_plan(12).unwrap();
    db.activate_subscription(&uid, "enterprise_12").unwrap();

    assert_eq!(db.limit(&uid, MAX_CONCURRENT_STREAMS).unwrap(), 12);
    assert_eq!(db.limit(&uid, MAX_STORAGE_BYTES).unwrap(), 240 * 1024 * 1024 * 1024);
    assert_eq!(db.limit(&uid, CCTV_ENABLED).unwrap(), 1);

    let sub = db.subscription(&uid).unwrap();
    assert!(sub.active);
    assert_eq!(sub.plan_label, "Enterprise 12채널");
}

#[test]
fn the_ladder_still_goes_up_in_total_and_down_per_channel() {
    // The reason Enterprise starts at 4: three channels are cheaper on Business,
    // and the fourth is where paying per channel starts to win.
    let db = db();
    let business = db.plan("business").unwrap();
    let four = db.ensure_enterprise_plan(enterprise::MIN_CHANNELS).unwrap();

    assert!(four.monthly_price_krw > business.monthly_price_krw);
    let per_channel_on_business = business.monthly_price_krw / 3;
    assert!(
        enterprise::PRICE_PER_CHANNEL_KRW < per_channel_on_business,
        "{} is not cheaper than {per_channel_on_business}",
        enterprise::PRICE_PER_CHANNEL_KRW
    );
}

#[test]
fn an_enterprise_customer_is_counted_among_the_people_paying() {
    // The admin dashboard's "paying accounts" is summed from its per-plan
    // breakdown, and that breakdown used to ask the plans table for what is on
    // sale. Enterprise is never on sale — it is an inactive row per channel
    // count — so asking that question would have made every Enterprise customer
    // invisible, in the list and in the total. It asks who has subscribers now.
    let db = db();
    let basic = user(&db, "small@example.com");
    let big = user(&db, "big@example.com");
    db.activate_subscription(&basic, "basic").unwrap();
    db.ensure_enterprise_plan(9).unwrap();
    db.activate_subscription(&big, "enterprise_9").unwrap();

    let counts = db.subscription_counts().unwrap();
    assert_eq!(counts.paid_total, 2, "{:?}", counts.by_plan);
    let row = counts
        .by_plan
        .iter()
        .find(|p| p.plan_id == "enterprise_9")
        .unwrap_or_else(|| panic!("no enterprise row in {:?}", counts.by_plan));
    assert_eq!((row.count, row.monthly_price_krw), (1, 171_000));
    assert_eq!(row.label, "Enterprise 9채널");

    // And the counts a count of nobody does not conjure rows: the sizes nobody
    // bought are not on the dashboard either.
    db.ensure_enterprise_plan(50).unwrap();
    let counts = db.subscription_counts().unwrap();
    assert!(
        !counts.by_plan.iter().any(|p| p.plan_id == "enterprise_50"),
        "an unbought size is on the dashboard: {:?}",
        counts.by_plan
    );
    // The three on sale are still listed whether or not anybody is on them.
    for id in ["basic", "pro", "business"] {
        assert!(counts.by_plan.iter().any(|p| p.plan_id == id), "{id} vanished");
    }
    // Neither of the two that are nobody's purchase.
    for id in ["none", "owner"] {
        assert!(!counts.by_plan.iter().any(|p| p.plan_id == id), "{id} is in the breakdown");
    }
}

#[test]
fn a_plan_taken_off_sale_keeps_the_customers_it_has() {
    // The same bug, in the shape it had before Enterprise existed: a plan an
    // operator retires still has the accounts that bought it, and they are still
    // paying.
    let db = db();
    let uid = user(&db, "loyal@example.com");
    db.activate_subscription(&uid, "business").unwrap();
    db.raw().lock().unwrap().execute("UPDATE plans SET active = 0 WHERE id = 'business'", []).unwrap();

    let counts = db.subscription_counts().unwrap();
    assert_eq!(counts.paid_total, 1, "{:?}", counts.by_plan);
    assert!(counts.by_plan.iter().any(|p| p.plan_id == "business" && p.count == 1));
}
