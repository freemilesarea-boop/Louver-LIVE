//! §18: the whole PayApp integration, against a fake PayApp.
//!
//! Nothing here reaches `api.payapp.kr`. The transport is a trait, and the fake
//! below records every field it was sent — so the assertions are about what
//! 247streams actually posted and what it did with the answer, not about our own
//! tables agreeing with themselves.
//!
//! The one thing these tests exist to pin down: **registering a recurring payment
//! must not grant anything.** Only a verified `pay_state=4` notification does.

use louver_cloud::billing::{
    self, cycle_month_for, one_month_after, pay_state, rebill_expire_from, Config, FeedbackOutcome, FormPost,
    Payapp,
};
use louver_cloud::db::Signup;
use louver_cloud::{BillingStatus, CloudDb, CloudError};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

// --- a fake PayApp ----------------------------------------------------------

#[derive(Debug, Default)]
struct FakePayapp {
    /// Every request, as a field map, in order.
    calls: Mutex<Vec<BTreeMap<String, String>>>,
    /// What to answer with next. Popped from the front; the default is a success.
    replies: Mutex<Vec<String>>,
    /// A fresh `rebill_no` per registration, as PayApp issues them. Reusing one
    /// would make this fake the only thing in the system that could.
    issued: Mutex<u32>,
}

const REBILL_NO: &str = "8891234";
const PAYURL: &str = "https://payapp.kr/pay/8891234abcdef";

impl FakePayapp {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn calls(&self) -> Vec<BTreeMap<String, String>> {
        self.calls.lock().unwrap().clone()
    }

    /// The one request with this `cmd`, or a panic naming what was there instead.
    fn call(&self, cmd: &str) -> BTreeMap<String, String> {
        let calls = self.calls();
        calls.iter().find(|c| c.get("cmd").map(String::as_str) == Some(cmd)).cloned().unwrap_or_else(|| {
            panic!("no {cmd} request; saw {:?}", calls.iter().map(|c| c.get("cmd")).collect::<Vec<_>>())
        })
    }

    fn answer_next_with(&self, body: &str) {
        self.replies.lock().unwrap().push(body.to_string());
    }
}

impl FormPost for FakePayapp {
    fn post_form(&self, _url: &str, fields: &[(&str, &str)]) -> louver_cloud::Result<String> {
        let map: BTreeMap<String, String> =
            fields.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let cmd = map.get("cmd").cloned().unwrap_or_default();
        self.calls.lock().unwrap().push(map);

        let scripted = {
            let mut r = self.replies.lock().unwrap();
            if r.is_empty() {
                None
            } else {
                Some(r.remove(0))
            }
        };
        Ok(scripted.unwrap_or_else(|| match cmd.as_str() {
            // URL-encoded key=value, as the documentation describes the reply.
            "rebillRegist" => {
                let mut n = self.issued.lock().unwrap();
                let no = if *n == 0 { REBILL_NO.to_string() } else { format!("{REBILL_NO}{n}") };
                *n += 1;
                format!("state=1&errno=00000&rebill_no={no}&payurl={}", billing::urlencode(PAYURL))
            }
            _ => "state=1&errno=00000".to_string(),
        }))
    }
}

// --- harness ----------------------------------------------------------------

const USERID: &str = "247streams";
const LINKKEY: &str = "link-key-for-tests-only";
const LINKVAL: &str = "link-val-for-tests-only";

struct Env {
    db: CloudDb,
    api: Arc<FakePayapp>,
    pay: Payapp,
    user: String,
}

fn env() -> Env {
    let db = CloudDb::open_in_memory().unwrap();
    let api = FakePayapp::new();
    let config = Config {
        userid: USERID.into(),
        linkkey: LINKKEY.into(),
        linkval: LINKVAL.into(),
        api_url: "https://fake.payapp.test/oapi/apiLoad.html".into(),
        public_url: "https://247streams.kr".into(),
    };
    let pay = Payapp::new(db.clone(), Arc::clone(&api) as Arc<dyn FormPost>, config);
    let user = db
        .register_user(&Signup {
            name: "홍길동",
            email: "dj@example.com",
            password_hash: "salt:hash",
            terms_version: "2026-09-27",
        })
        .unwrap()
        .id;
    Env { db, api, pay, user }
}

impl Env {
    /// A notification exactly as PayApp posts one, with fields overridden.
    fn feedback(&self, over: &[(&str, &str)]) -> BTreeMap<String, String> {
        let mut f: BTreeMap<String, String> = [
            ("userid", USERID),
            ("linkkey", LINKKEY),
            ("linkval", LINKVAL),
            ("goodname", "247streams Pro"),
            ("price", "39900"),
            ("recvphone", "01012345678"),
            ("memo", ""),
            ("reqdate", "2026-09-27 12:00:00"),
            ("pay_date", "2026-09-27 12:00:05"),
            ("pay_type", "card"),
            ("pay_state", pay_state::PAID),
            ("mul_no", "550001"),
            ("payurl", PAYURL),
            ("feedbacktype", "rebill"),
            ("rebill_no", REBILL_NO),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        for (k, v) in over {
            f.insert(k.to_string(), v.to_string());
        }
        f
    }

    fn billing(&self) -> louver_cloud::BillingSubscription {
        self.db.billing_subscriptions_for(&self.user).unwrap().into_iter().next().expect("a record")
    }
}

// --- checkout ---------------------------------------------------------------

#[test]
fn checkout_registers_a_recurring_payment_and_returns_the_providers_url() {
    let e = env();
    let out = e.pay.checkout(&e.user, "pro", "010-1234-5678").unwrap();

    assert_eq!(out.payurl, PAYURL, "the browser is sent to PayApp's URL, not ours");
    assert_eq!(out.plan_id, "pro");
    // The amount comes from the plans table, not from anything a caller said.
    assert_eq!(out.amount_krw, 39_900);

    let req = e.api.call("rebillRegist");
    assert_eq!(req.get("userid").unwrap(), USERID);
    assert_eq!(req.get("goodname").unwrap(), "247streams Pro");
    assert_eq!(req.get("goodprice").unwrap(), "39900", "the registration amount is `goodprice`");
    assert_eq!(req.get("recvphone").unwrap(), "01012345678", "dashes are stripped");
    assert_eq!(req.get("rebillCycleType").unwrap(), "Month");
    assert_eq!(req.get("openpaytype").unwrap(), "card");
    assert_eq!(req.get("recvemail").unwrap(), "dj@example.com");
    assert_eq!(req.get("feedbackurl").unwrap(), "https://247streams.kr/api/billing/payapp/feedback");
    assert_eq!(req.get("returnurl").unwrap(), "https://247streams.kr/billing/complete");
    assert_eq!(req.get("failurl").unwrap(), "https://247streams.kr/api/billing/payapp/failure");

    // `rebillExpire` is required and must be yyyy-mm-dd.
    let expire = req.get("rebillExpire").unwrap();
    assert!(chrono::NaiveDate::parse_from_str(expire, "%Y-%m-%d").is_ok(), "{expire}");

    // §15: `var1` is our opaque order id, and neither var carries a secret or an
    // address — they come back through a callback we do not control.
    assert_eq!(req.get("var1").unwrap(), &out.billing_id);
    assert_eq!(req.get("var2").unwrap(), "pro");
    for var in ["var1", "var2"] {
        let v = req.get(var).unwrap();
        assert!(!v.contains('@'), "{var} must not carry an email: {v}");
        assert_ne!(v, &e.user, "{var} must not be the user id");
        assert!(!v.contains(LINKKEY) && !v.contains(LINKVAL), "{var} must not carry a key");
    }
    // The registration request does not send linkval at all.
    assert!(!req.contains_key("linkval"), "rebillRegist takes no linkval");

    // Stored, pending, and with PayApp's reference against it.
    let record = e.billing();
    assert_eq!(record.provider, "payapp");
    assert_eq!(record.provider_subscription_id.as_deref(), Some(REBILL_NO));
    assert_eq!(record.status, BillingStatus::Pending);
    assert_eq!(record.amount_krw, 39_900);
}

#[test]
fn checkout_grants_nothing() {
    // The single most important assertion in this file. PayApp's documentation is
    // explicit that registering is not paying.
    let e = env();
    e.pay.checkout(&e.user, "business", "01012345678").unwrap();

    let sub = e.db.subscription(&e.user).unwrap();
    assert!(!sub.active, "a payment URL is not a payment");
    assert_eq!(sub.plan_id, louver_cloud::db::UNSUBSCRIBED_PLAN);
    assert_eq!(e.db.limit(&e.user, louver_cloud::entitlement::MAX_CONCURRENT_STREAMS).unwrap(), 0);
    assert!(matches!(e.db.require_active_subscription(&e.user), Err(CloudError::NoSubscription)));
}

#[test]
fn each_plan_is_registered_at_the_price_the_server_holds() {
    for (plan, price, label) in
        [("basic", "19900", "Basic"), ("pro", "39900", "Pro"), ("business", "59900", "Business")]
    {
        let e = env();
        let out = e.pay.checkout(&e.user, plan, "01012345678").unwrap();
        let req = e.api.call("rebillRegist");
        assert_eq!(req.get("goodprice").unwrap(), price, "{plan}");
        assert_eq!(req.get("goodname").unwrap(), &format!("247streams {label}"));
        assert_eq!(out.amount_krw.to_string(), price);
        assert_eq!(e.billing().amount_krw.to_string(), price);
    }
}

#[test]
fn a_plan_that_is_not_on_sale_cannot_be_checked_out() {
    let e = env();
    for plan in ["none", "", "enterprise", "NONE", "desktop"] {
        let refused = e.pay.checkout(&e.user, plan, "01012345678");
        assert!(refused.is_err(), "{plan} was accepted");
    }
    // An inactive plan is off the list too, so it stops being buyable the moment
    // an operator takes it down.
    e.db.raw().lock().unwrap().execute("UPDATE plans SET active = 0 WHERE id = 'pro'", []).unwrap();
    assert!(e.pay.checkout(&e.user, "pro", "01012345678").is_err());

    assert!(e.api.calls().is_empty(), "nothing may reach the provider for a refused plan");
    assert!(e.db.billing_subscriptions_for(&e.user).unwrap().is_empty(), "and no record is left");
}

#[test]
fn a_phone_number_that_could_not_receive_the_link_is_refused_before_the_provider_is_called() {
    let e = env();
    for phone in ["", "123", "0212345678", "010123456789", "hello", "+82 10 1234 5678"] {
        assert!(e.pay.checkout(&e.user, "pro", phone).is_err(), "{phone:?} was accepted");
    }
    assert!(e.api.calls().is_empty());
    // The one that is fine, however it was typed.
    assert!(e.pay.checkout(&e.user, "pro", " 010-1234-5678 ").is_ok());
}

#[test]
fn a_second_registration_is_refused_while_the_provider_still_holds_one() {
    // §14. Two registrations would be two charges a month, and PayApp has no idea
    // the first one exists.
    let e = env();
    e.pay.checkout(&e.user, "basic", "01012345678").unwrap();

    let same = e.pay.checkout(&e.user, "basic", "01012345678").unwrap_err();
    assert!(same.to_string().contains("이미 이 요금제의 정기결제가 등록되어"), "{same}");
    let other = e.pay.checkout(&e.user, "pro", "01012345678").unwrap_err();
    assert!(other.to_string().contains("현재 구독을 해지한 후"), "{other}");

    assert_eq!(e.db.billing_subscriptions_for(&e.user).unwrap().len(), 1);
    assert_eq!(e.api.calls().len(), 1, "the provider was called once");
}

#[test]
fn a_registration_the_provider_refuses_leaves_a_record_saying_so_and_no_entitlement() {
    let e = env();
    e.api.answer_next_with("state=0&errno=10001&errorMessage=%EA%B0%80%EB%A7%B9%EC%A0%90+%EC%98%A4%EB%A5%98");

    let failed = e.pay.checkout(&e.user, "pro", "01012345678").unwrap_err();
    // The provider's own message does not reach the user.
    assert!(failed.to_string().contains("거절되었습니다"), "{failed}");
    assert!(!failed.to_string().contains("가맹점"), "provider text must not be rendered: {failed}");

    assert!(!e.db.subscription(&e.user).unwrap().active);
    // And a second attempt is allowed, because a failed registration does not
    // hold the provider.
    assert_eq!(e.billing().status, BillingStatus::RegistrationFailed);
    assert!(e.pay.checkout(&e.user, "pro", "01012345678").is_ok());
}

// --- the notification -------------------------------------------------------

#[test]
fn a_verified_paid_notification_activates_exactly_the_plan_that_was_bought() {
    let e = env();
    let out = e.pay.checkout(&e.user, "pro", "01012345678").unwrap();

    let outcome = e.pay.handle_feedback(&e.feedback(&[("var1", &out.billing_id)]));
    assert_eq!(outcome, FeedbackOutcome::Activated("pro".into()));
    assert!(outcome.is_accepted(), "PayApp must be told SUCCESS");

    let sub = e.db.subscription(&e.user).unwrap();
    assert!(sub.active);
    assert_eq!(sub.plan_id, "pro");
    assert_eq!(e.db.limit(&e.user, louver_cloud::entitlement::MAX_CONCURRENT_STREAMS).unwrap(), 2);

    let record = e.billing();
    assert_eq!(record.status, BillingStatus::Active);
    assert!(record.activated_at.is_some());
    assert_eq!(record.last_paid_at.as_deref(), Some("2026-09-27 12:00:05"));
    // A month on from the payment, so a cancellation has something to measure
    // against.
    assert_eq!(record.current_period_end.as_deref(), Some("2026-10-27"));
}

#[test]
fn a_notification_cannot_upgrade_anybody_by_claiming_a_different_plan() {
    // The callback's own plan hint is ignored: the plan comes from the record.
    let e = env();
    let out = e.pay.checkout(&e.user, "basic", "01012345678").unwrap();

    let forged = e.feedback(&[
        ("var1", &out.billing_id),
        ("var2", "business"),
        ("goodname", "247streams Business"),
        ("price", "19900"),
    ]);
    assert_eq!(e.pay.handle_feedback(&forged), FeedbackOutcome::Activated("basic".into()));

    let sub = e.db.subscription(&e.user).unwrap();
    assert_eq!(sub.plan_id, "basic", "the plan is the one that was paid for");
    assert_eq!(e.db.limit(&e.user, louver_cloud::entitlement::MAX_CONCURRENT_STREAMS).unwrap(), 1);
}

#[test]
fn a_notification_with_the_wrong_credentials_is_refused() {
    let e = env();
    let out = e.pay.checkout(&e.user, "pro", "01012345678").unwrap();
    let id = out.billing_id.clone();

    for over in [
        vec![("userid", "somebody-else")],
        vec![("linkkey", "wrong-key")],
        vec![("linkval", "wrong-val")],
        vec![("linkkey", "")],
        vec![("linkval", "")],
        vec![("userid", "")],
        // A near-miss, in case a prefix comparison ever creeps in.
        vec![("linkkey", "link-key-for-tests-onl")],
        vec![("linkkey", "link-key-for-tests-only2")],
    ] {
        let mut f = e.feedback(&[("var1", &id)]);
        for (k, v) in &over {
            f.insert(k.to_string(), v.to_string());
        }
        let outcome = e.pay.handle_feedback(&f);
        assert_eq!(outcome, FeedbackOutcome::Rejected("인증 실패"), "{over:?}");
        // Not SUCCESS: a forgery must never be acknowledged.
        assert!(!outcome.is_accepted(), "{over:?}");
        assert!(!e.db.subscription(&e.user).unwrap().active, "{over:?} granted an entitlement");
    }
    // Nothing was written to the ledger either.
    assert!(e.db.billing_events_for(&e.user, 10).unwrap().is_empty());
}

#[test]
fn a_notification_for_an_order_we_never_made_is_refused() {
    let e = env();
    e.pay.checkout(&e.user, "pro", "01012345678").unwrap();

    for var1 in ["", "not-an-order", "../../etc/passwd", &e.user.clone()] {
        let outcome = e.pay.handle_feedback(&e.feedback(&[("var1", var1)]));
        assert_eq!(outcome, FeedbackOutcome::Rejected("알 수 없는 주문"), "{var1}");
        assert!(!e.db.subscription(&e.user).unwrap().active);
    }
}

#[test]
fn a_notification_whose_amount_is_not_the_one_we_stored_is_refused() {
    let e = env();
    let out = e.pay.checkout(&e.user, "pro", "01012345678").unwrap();

    for price in ["100", "0", "-39900", "", "39899", "399000", "not-a-number", "19900"] {
        let outcome = e.pay.handle_feedback(&e.feedback(&[("var1", &out.billing_id), ("price", price)]));
        assert_eq!(outcome, FeedbackOutcome::Rejected("금액 불일치"), "price={price}");
        assert!(!e.db.subscription(&e.user).unwrap().active, "price={price}");
    }
    // The real one, however it is punctuated.
    assert!(e
        .pay
        .handle_feedback(&e.feedback(&[("var1", &out.billing_id), ("price", "39,900")]))
        .is_accepted());
}

#[test]
fn a_notification_carrying_the_wrong_rebill_number_is_refused() {
    let e = env();
    let out = e.pay.checkout(&e.user, "pro", "01012345678").unwrap();

    for no in ["", "9999999", "8891235"] {
        let outcome = e.pay.handle_feedback(&e.feedback(&[("var1", &out.billing_id), ("rebill_no", no)]));
        assert_eq!(outcome, FeedbackOutcome::Rejected("정기결제 번호 불일치"), "rebill_no={no}");
        assert!(!e.db.subscription(&e.user).unwrap().active);
    }
}

#[test]
fn the_same_paid_notification_ten_times_activates_once() {
    // §4, and §7 of the earlier request. PayApp documents that it may call this
    // more than once.
    let e = env();
    let out = e.pay.checkout(&e.user, "pro", "01012345678").unwrap();
    let f = e.feedback(&[("var1", &out.billing_id)]);

    assert_eq!(e.pay.handle_feedback(&f), FeedbackOutcome::Activated("pro".into()));
    for i in 0..9 {
        let again = e.pay.handle_feedback(&f);
        assert_eq!(again, FeedbackOutcome::AlreadyHandled, "repeat {i}");
        // Still SUCCESS, so PayApp stops asking.
        assert!(again.is_accepted(), "repeat {i}");
    }

    // One ledger row, one activation, and the entitlement is what it should be.
    assert_eq!(e.db.billing_events_for(&e.user, 50).unwrap().len(), 1);
    let sub = e.db.subscription(&e.user).unwrap();
    assert!(sub.active);
    assert_eq!(sub.plan_id, "pro");
    assert_eq!(e.db.billing_subscriptions_for(&e.user).unwrap().len(), 1);
}

#[test]
fn next_months_renewal_is_a_different_event_and_is_recorded() {
    let e = env();
    let out = e.pay.checkout(&e.user, "basic", "01012345678").unwrap();
    let first = e.feedback(&[("var1", &out.billing_id), ("price", "19900")]);
    assert!(e.pay.handle_feedback(&first).is_accepted());

    // A new `mul_no` and a new date: PayApp's own id for the second payment.
    let second = e.feedback(&[
        ("var1", &out.billing_id),
        ("price", "19900"),
        ("mul_no", "550002"),
        ("pay_date", "2026-10-27 12:00:05"),
    ]);
    assert_eq!(e.pay.handle_feedback(&second), FeedbackOutcome::Activated("basic".into()));

    assert_eq!(e.db.billing_events_for(&e.user, 50).unwrap().len(), 2);
    let record = e.billing();
    assert_eq!(record.last_paid_at.as_deref(), Some("2026-10-27 12:00:05"));
    assert_eq!(record.current_period_end.as_deref(), Some("2026-11-27"));
}

#[test]
fn every_state_that_is_not_paid_is_recorded_and_grants_nothing() {
    // §3's table, one state at a time. Only `4` is an entitlement.
    for (state, expect) in [
        (pay_state::REQUESTED, "요청"),
        (pay_state::WAITING, "결제대기"),
        ("8", "요청취소"),
        ("32", "요청취소"),
        ("9", "승인취소"),
        ("64", "승인취소"),
        ("70", "부분취소"),
        ("71", "부분취소"),
        ("12345", "알 수 없는 상태"),
    ] {
        let e = env();
        let out = e.pay.checkout(&e.user, "pro", "01012345678").unwrap();
        let outcome = e.pay.handle_feedback(&e.feedback(&[("var1", &out.billing_id), ("pay_state", state)]));

        match &outcome {
            FeedbackOutcome::Recorded(what) => assert!(what.contains(expect), "{state}: {what}"),
            other => panic!("pay_state={state} should only be recorded, got {other:?}"),
        }
        // Verified, so PayApp is told SUCCESS — but nothing was granted.
        assert!(outcome.is_accepted(), "{state}");
        assert!(!e.db.subscription(&e.user).unwrap().active, "pay_state={state} granted an entitlement");
        assert_eq!(e.db.billing_events_for(&e.user, 10).unwrap().len(), 1, "{state}");
    }
}

#[test]
fn a_reversal_after_a_payment_is_recorded_without_taking_the_broadcast_off_air() {
    // §8: one failure is a card to re-enter, not a reason to drop somebody's 24/7
    // stream. Recorded, and the policy decision left to a person.
    let e = env();
    let out = e.pay.checkout(&e.user, "pro", "01012345678").unwrap();
    e.pay.handle_feedback(&e.feedback(&[("var1", &out.billing_id)]));
    assert!(e.db.subscription(&e.user).unwrap().active);

    let reversal = e.feedback(&[("var1", &out.billing_id), ("pay_state", "9"), ("mul_no", "550009")]);
    assert!(e.pay.handle_feedback(&reversal).is_accepted());

    assert_eq!(e.billing().status, BillingStatus::PaymentFailed);
    let sub = e.db.subscription(&e.user).unwrap();
    assert!(sub.active, "the entitlement is not removed by a failed renewal alone");
    assert_eq!(sub.plan_id, "pro");
}

#[test]
fn a_notification_with_no_payment_id_is_still_only_applied_once() {
    // `mul_no` is the idempotency key. Without one, a deterministic composite —
    // otherwise every repeat would look new and re-grant.
    let e = env();
    let out = e.pay.checkout(&e.user, "pro", "01012345678").unwrap();
    let f = e.feedback(&[("var1", &out.billing_id), ("mul_no", "")]);

    assert_eq!(e.pay.handle_feedback(&f), FeedbackOutcome::Activated("pro".into()));
    assert_eq!(e.pay.handle_feedback(&f), FeedbackOutcome::AlreadyHandled);
    assert_eq!(e.db.billing_events_for(&e.user, 10).unwrap().len(), 1);
}

#[test]
fn an_unknown_field_in_a_notification_changes_nothing() {
    // §2: PayApp may add fields, and a parser that refused would turn that into an
    // outage on a payment path.
    let e = env();
    let out = e.pay.checkout(&e.user, "pro", "01012345678").unwrap();
    let f = e.feedback(&[
        ("var1", &out.billing_id),
        ("some_new_field_payapp_added", "whatever"),
        ("another", "1"),
    ]);
    assert_eq!(e.pay.handle_feedback(&f), FeedbackOutcome::Activated("pro".into()));
}

// --- cancellation -----------------------------------------------------------

#[test]
fn cancelling_stops_the_next_charge_and_takes_the_entitlement_back_at_once() {
    // Test A. Basic → 미구독, immediately. The service policy is that cancelling
    // ends the paid features there and then; the remainder of the month is not
    // honoured, so nothing here may keep the plan alive.
    let e = env();
    let out = e.pay.checkout(&e.user, "basic", "01012345678").unwrap();
    e.pay.handle_feedback(&e.feedback(&[("var1", &out.billing_id), ("price", "19900")]));
    assert!(e.db.subscription(&e.user).unwrap().active);

    let after = e.pay.cancel(&e.user).unwrap();
    assert_eq!(after.status, BillingStatus::Cancelled);
    assert!(after.cancelled_at.is_some());

    // The request is exactly the four fields the documentation lists, and no
    // linkval.
    let req = e.api.call("rebillCancel");
    assert_eq!(req.get("userid").unwrap(), USERID);
    assert_eq!(req.get("rebill_no").unwrap(), REBILL_NO);
    assert_eq!(req.get("linkkey").unwrap(), LINKKEY);
    assert!(!req.contains_key("linkval"), "rebillCancel takes no linkval");
    assert_eq!(req.len(), 4, "no field beyond cmd/userid/rebill_no/linkkey: {req:?}");

    // And the entitlement is gone. This is the whole point.
    let sub = e.db.subscription(&e.user).unwrap();
    assert!(!sub.active, "cancelling revokes the paid features immediately");
    assert_eq!(sub.plan_id, louver_cloud::db::UNSUBSCRIBED_PLAN);
    assert_eq!(e.db.limit(&e.user, louver_cloud::entitlement::MAX_CONCURRENT_STREAMS).unwrap(), 0);
}

#[test]
fn cancelling_pro_revokes_both_of_its_concurrent_streams() {
    // Test B. The same for a plan that allowed more than one stream: the
    // concurrency allowance goes to zero, not down by one.
    let e = env();
    let out = e.pay.checkout(&e.user, "pro", "01012345678").unwrap();
    e.pay.handle_feedback(&e.feedback(&[("var1", &out.billing_id)]));
    assert_eq!(e.db.limit(&e.user, louver_cloud::entitlement::MAX_CONCURRENT_STREAMS).unwrap(), 2);

    e.pay.cancel(&e.user).unwrap();
    let sub = e.db.subscription(&e.user).unwrap();
    assert!(!sub.active);
    assert_eq!(e.db.limit(&e.user, louver_cloud::entitlement::MAX_CONCURRENT_STREAMS).unwrap(), 0);
}

#[test]
fn a_refused_rebill_cancel_changes_nothing_at_all() {
    // Test C, and the reason the provider is called before anything local is
    // written. If we revoked first and PayApp then refused, the account would go
    // on being charged every month with nothing to show for it — the one outcome
    // this ordering exists to make impossible.
    let e = env();
    let out = e.pay.checkout(&e.user, "pro", "01012345678").unwrap();
    e.pay.handle_feedback(&e.feedback(&[("var1", &out.billing_id)]));

    e.api.answer_next_with("state=0&errno=00009&errorMessage=%EC%B2%98%EB%A6%AC%EC%8B%A4%ED%8C%A8");
    let err = e.pay.cancel(&e.user).expect_err("a refused cancellation must not report success");
    let said = err.to_string();
    assert!(!said.contains(LINKKEY) && !said.contains(LINKVAL), "no credential in the message: {said}");

    // Still subscribed, still billable, still cancellable.
    let sub = e.db.subscription(&e.user).unwrap();
    assert!(sub.active, "the entitlement must survive a failed provider call");
    assert_eq!(sub.plan_id, "pro");
    let record = e.billing();
    assert_eq!(record.status, BillingStatus::Active, "the record must not be marked cancelled");
    assert!(record.cancelled_at.is_none());

    // Retryable: the next attempt reaches PayApp and this time it works.
    e.pay.cancel(&e.user).unwrap();
    assert!(!e.db.subscription(&e.user).unwrap().active);
}

#[test]
fn cancelling_is_confined_to_the_callers_own_billing_record() {
    // Test D. There is no billing id on the cancellation path — `cancel` takes a
    // user id and finds that user's own record — so another user's record cannot
    // be named. The operator repair path, which does take an id, refuses one that
    // is not a mismatch of its own.
    let e = env();
    let out = e.pay.checkout(&e.user, "pro", "01012345678").unwrap();
    e.pay.handle_feedback(&e.feedback(&[("var1", &out.billing_id)]));

    let other =
        e.db.register_user(&Signup {
            name: "이몽룡",
            email: "other@example.com",
            password_hash: "salt:hash",
            terms_version: "2026-09-27",
        })
        .unwrap()
        .id;

    // The other account has nothing to cancel, and saying so must not disturb the
    // first account's subscription.
    assert!(e.pay.cancel(&other).is_err());
    assert!(e.db.subscription(&e.user).unwrap().active);
    assert_eq!(e.billing().status, BillingStatus::Active);

    // And the audit's fix refuses a live record outright.
    assert!(e.db.fix_billing_mismatch(&out.billing_id).is_err());
    assert!(e.db.subscription(&e.user).unwrap().active);
}

#[test]
fn cancelling_twice_leaves_one_cancellation() {
    // Test E. PayApp is asked once; the second attempt finds nothing live and
    // says so, rather than writing the record again or revoking something else.
    let e = env();
    let out = e.pay.checkout(&e.user, "pro", "01012345678").unwrap();
    e.pay.handle_feedback(&e.feedback(&[("var1", &out.billing_id)]));

    let first = e.pay.cancel(&e.user).unwrap();
    assert!(e.pay.cancel(&e.user).is_err(), "there is nothing left to cancel");

    let cancels =
        e.api.calls().iter().filter(|c| c.get("cmd").map(String::as_str) == Some("rebillCancel")).count();
    assert_eq!(cancels, 1, "PayApp is not asked twice");
    let record = e.billing();
    assert_eq!(record.status, BillingStatus::Cancelled);
    assert_eq!(record.cancelled_at, first.cancelled_at, "the cancellation time is not rewritten");
    assert_eq!(e.db.billing_subscriptions_for(&e.user).unwrap().len(), 1);
    assert!(!e.db.subscription(&e.user).unwrap().active);
}

#[test]
fn an_operator_granted_plan_is_not_collateral_damage() {
    // Test F. The Business account an operator made with `--create-user` has no
    // billing record at all, and a cancellation on another account must not reach
    // it. The second half is the sharper case: an account that *does* have a
    // cancelled PayApp record but has since been moved to a plan an operator
    // granted keeps that grant, because the revocation only ever moves an account
    // off the plan the cancelled record itself paid for.
    let e = env();
    let operator =
        e.db.register_user(&Signup {
            name: "운영자",
            email: "ops@example.com",
            password_hash: "salt:hash",
            terms_version: "2026-09-27",
        })
        .unwrap()
        .id;
    e.db.activate_subscription(&operator, "business").unwrap();

    let out = e.pay.checkout(&e.user, "basic", "01012345678").unwrap();
    e.pay.handle_feedback(&e.feedback(&[("var1", &out.billing_id), ("price", "19900")]));
    // An operator lifts this account to Business by hand, outside PayApp.
    e.db.activate_subscription(&e.user, "business").unwrap();

    e.pay.cancel(&e.user).unwrap();

    let granted = e.db.subscription(&e.user).unwrap();
    assert!(granted.active, "the hand-granted plan is not PayApp's to take away");
    assert_eq!(granted.plan_id, "business");
    assert_eq!(e.billing().status, BillingStatus::Cancelled, "the recurring payment still stops");

    let untouched = e.db.subscription(&operator).unwrap();
    assert!(untouched.active);
    assert_eq!(untouched.plan_id, "business");
}

#[test]
fn a_registration_that_never_paid_takes_nothing_away() {
    // The other side of the same guard: cancelling a pending registration must not
    // move an account that is on a plan for some other reason.
    let e = env();
    e.db.activate_subscription(&e.user, "business").unwrap();
    e.pay.checkout(&e.user, "basic", "01012345678").unwrap();

    e.pay.cancel(&e.user).unwrap();
    let sub = e.db.subscription(&e.user).unwrap();
    assert!(sub.active);
    assert_eq!(sub.plan_id, "business");
}

#[test]
fn the_audit_finds_only_the_account_whose_billing_is_already_cancelled() {
    // Test H. The production case: an account that ran `rebillCancel` under the
    // old policy and still holds Basic. The audit must find it, must not list an
    // account that is simply on a plan, and fixing it must move that one account.
    let e = env();
    let out = e.pay.checkout(&e.user, "basic", "01012345678").unwrap();
    e.pay.handle_feedback(&e.feedback(&[("var1", &out.billing_id), ("price", "19900")]));
    // The shape the old code left behind: billing cancelled, entitlement live.
    e.db.set_billing_status(&out.billing_id, BillingStatus::CancelAtPeriodEnd).unwrap();

    let paying =
        e.db.register_user(&Signup {
            name: "김구독",
            email: "paying@example.com",
            password_hash: "salt:hash",
            terms_version: "2026-09-27",
        })
        .unwrap()
        .id;
    let live = e.pay.checkout(&paying, "basic", "01012345678").unwrap();
    e.pay.handle_feedback(&e.feedback(&[
        ("var1", &live.billing_id),
        ("price", "19900"),
        ("mul_no", "550002"),
        (
            "rebill_no",
            &e.db.billing_subscription(&live.billing_id).unwrap().provider_subscription_id.unwrap(),
        ),
    ]));
    assert!(e.db.subscription(&paying).unwrap().active);

    let found = e.db.billing_mismatches().unwrap();
    assert_eq!(found.len(), 1, "only the cancelled-but-entitled account: {found:?}");
    assert_eq!(found[0].email, "dj@example.com");

    e.db.fix_billing_mismatch(&out.billing_id).unwrap();
    assert!(!e.db.subscription(&e.user).unwrap().active);
    assert!(e.db.subscription(&paying).unwrap().active, "the paying account is untouched");
    assert!(e.db.billing_mismatches().unwrap().is_empty());
    // And the repair is not a second cancellation at PayApp: the audit repairs
    // our own records, it does not re-ask the provider.
    assert!(e.api.calls().iter().all(|c| c.get("cmd").map(String::as_str) != Some("rebillCancel")));
}

#[test]
fn a_paid_period_running_into_next_month_does_not_delay_the_revocation() {
    // Test I. The payment notification writes `current_period_end` a month out.
    // The column stays — it is what the account screen's "last paid" story is
    // built from, and dropping a column from a production table is not additive —
    // but no entitlement decision may read it.
    let e = env();
    let out = e.pay.checkout(&e.user, "pro", "01012345678").unwrap();
    e.pay.handle_feedback(&e.feedback(&[("var1", &out.billing_id)]));
    let before = e.billing();
    assert!(before.current_period_end.is_some(), "the period end is still recorded");

    e.pay.cancel(&e.user).unwrap();
    let after = e.billing();
    assert_eq!(after.current_period_end, before.current_period_end, "and is left as it was");
    assert!(!e.db.subscription(&e.user).unwrap().active, "but grants nothing after cancellation");
}

#[test]
fn cancelling_a_registration_that_was_never_paid_ends_it_outright() {
    let e = env();
    e.pay.checkout(&e.user, "pro", "01012345678").unwrap();

    let after = e.pay.cancel(&e.user).unwrap();
    assert_eq!(after.status, BillingStatus::Cancelled, "there is no paid period to keep");
    assert!(!e.db.subscription(&e.user).unwrap().active);
    // And the user may start again.
    assert!(e.pay.checkout(&e.user, "basic", "01012345678").is_ok());
}

#[test]
fn cancelling_touches_only_the_callers_own_subscription() {
    let e = env();
    let out = e.pay.checkout(&e.user, "pro", "01012345678").unwrap();
    e.pay.handle_feedback(&e.feedback(&[("var1", &out.billing_id)]));

    // A second account, with a registration of its own.
    let other =
        e.db.register_user(&Signup {
            name: "다른 사람",
            email: "other@example.com",
            password_hash: "salt:hash",
            terms_version: "2026-09-27",
        })
        .unwrap()
        .id;
    e.api.answer_next_with(&format!(
        "state=1&errno=00000&rebill_no=777&payurl={}",
        billing::urlencode(PAYURL)
    ));
    e.pay.checkout(&other, "basic", "01098765432").unwrap();

    e.pay.cancel(&other).unwrap();

    // Theirs ended; ours did not.
    assert_eq!(e.db.billing_subscriptions_for(&other).unwrap()[0].status, BillingStatus::Cancelled);
    assert_eq!(e.billing().status, BillingStatus::Active);
    assert!(e.db.subscription(&e.user).unwrap().active);
    // The cancel named their rebill number, not ours.
    assert_eq!(e.api.call("rebillCancel").get("rebill_no").unwrap(), "777");
}

#[test]
fn there_is_nothing_to_cancel_when_nothing_was_registered() {
    let e = env();
    assert!(e.pay.cancel(&e.user).is_err());
    assert!(e.api.calls().is_empty());
}

// --- no secret anywhere it should not be ------------------------------------

#[test]
fn no_credential_reaches_the_database_the_ledger_or_a_response() {
    let e = env();
    let out = e.pay.checkout(&e.user, "pro", "01012345678").unwrap();
    e.pay.handle_feedback(&e.feedback(&[("var1", &out.billing_id)]));
    e.pay.cancel(&e.user).unwrap();

    // Whatever is serialised to a browser.
    let record = serde_json::to_string(&e.billing()).unwrap();
    let ledger = serde_json::to_string(&e.db.billing_events_for(&e.user, 50).unwrap()).unwrap();
    let checkout = serde_json::to_string(&out).unwrap();
    for body in [&record, &ledger, &checkout] {
        for secret in [LINKKEY, LINKVAL] {
            assert!(!body.contains(secret), "a credential reached a response: {body}");
        }
    }

    // And the config cannot print one even when something formats it whole.
    let config = Config {
        userid: USERID.into(),
        linkkey: LINKKEY.into(),
        linkval: LINKVAL.into(),
        api_url: billing::DEFAULT_API_URL.into(),
        public_url: "https://247streams.kr".into(),
    };
    let debug = format!("{config:?}");
    assert!(!debug.contains(LINKKEY), "{debug}");
    assert!(!debug.contains(LINKVAL), "{debug}");
    assert!(debug.contains("[REDACTED]"), "{debug}");
    assert!(debug.contains(USERID), "the merchant id is not a secret and is worth seeing");
}

// --- the policies we chose, pinned down -------------------------------------

#[test]
fn the_charge_day_follows_the_day_the_subscription_started() {
    use chrono::NaiveDate;
    let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();

    // 1–28: that day of the month, so a signup on the 15th is charged the 15th.
    assert_eq!(cycle_month_for(d(2026, 9, 1)), 1);
    assert_eq!(cycle_month_for(d(2026, 9, 15)), 15);
    assert_eq!(cycle_month_for(d(2026, 9, 28)), 28);
    // 29–31: 말일, PayApp's 90. Choosing 31 would leave February undefined.
    assert_eq!(cycle_month_for(d(2026, 9, 29)), 90);
    assert_eq!(cycle_month_for(d(2026, 9, 30)), 90);
    assert_eq!(cycle_month_for(d(2026, 10, 31)), 90);
    assert_eq!(cycle_month_for(d(2024, 2, 29)), 90);
}

#[test]
fn the_registration_expiry_is_ten_years_out_and_always_a_real_date() {
    use chrono::NaiveDate;
    let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();

    assert_eq!(rebill_expire_from(d(2026, 9, 27)), "2036-09-27");
    assert_eq!(rebill_expire_from(d(2026, 1, 1)), "2036-01-01");
    // 29 February has no tenth anniversary; stepping back a day keeps it valid
    // rather than silently falling back to today.
    assert_eq!(rebill_expire_from(d(2024, 2, 29)), "2034-02-28");

    // Whatever the date, the format is the yyyy-mm-dd PayApp requires.
    for day in 1..=28 {
        let out = rebill_expire_from(d(2026, 2, day));
        assert!(NaiveDate::parse_from_str(&out, "%Y-%m-%d").is_ok(), "{out}");
    }
}

#[test]
fn a_period_end_is_a_month_on_or_absent_rather_than_guessed() {
    assert_eq!(one_month_after("2026-09-27 12:00:05").as_deref(), Some("2026-10-27"));
    assert_eq!(one_month_after("2026-09-27").as_deref(), Some("2026-10-27"));
    assert_eq!(one_month_after("20260927").as_deref(), Some("2026-10-27"));
    assert_eq!(one_month_after("2026/09/27").as_deref(), Some("2026-10-27"));
    // December rolls the year.
    assert_eq!(one_month_after("2026-12-15").as_deref(), Some("2027-01-15"));
    // A day the next month does not have lands on its last.
    assert_eq!(one_month_after("2026-01-31").as_deref(), Some("2026-02-28"));
    assert_eq!(one_month_after("2024-01-31").as_deref(), Some("2024-02-29"));
    // Anything unparseable is absent, not invented.
    for bad in ["", "   ", "not a date", "2026-13-40"] {
        assert_eq!(one_month_after(bad), None, "{bad:?}");
    }
}

#[test]
fn a_reply_is_parsed_the_way_payapp_writes_one() {
    // URL-encoded key=value, and tolerant of what it has not seen.
    let m = billing::parse_query("state=1&errno=00000&rebill_no=123&payurl=https%3A%2F%2Fpayapp.kr%2Fx");
    assert_eq!(m.get("state").unwrap(), "1");
    assert_eq!(m.get("payurl").unwrap(), "https://payapp.kr/x");

    // A Korean message, percent-encoded, and `+` for a space.
    let m = billing::parse_query("state=0&errorMessage=%EA%B0%80%EB%A7%B9%EC%A0%90+%EC%98%A4%EB%A5%98");
    assert_eq!(m.get("errorMessage").unwrap(), "가맹점 오류");

    // Shapes that must not panic.
    for body in ["", "?", "&&", "state", "=1", "a=b=c", "%"] {
        let _ = billing::parse_query(body);
    }
    assert_eq!(billing::parse_query("a=b=c").get("a").unwrap(), "b=c");
}
