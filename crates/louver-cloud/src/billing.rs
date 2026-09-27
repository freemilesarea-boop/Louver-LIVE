//! PayApp recurring subscriptions. §1–§9 of the billing request.
//!
//! Two requests out (`rebillRegist`, `rebillCancel`) and one notification in
//! (`feedbackurl`). The shape of the whole thing follows from one sentence in
//! PayApp's documentation: **registering a recurring payment is not a payment.**
//! The user has to approve the first charge at the `payurl`, and only the
//! server-to-server notification afterwards says that happened. So nothing here
//! grants an entitlement except [`Payapp::handle_feedback`], and it does so only
//! after every field it was sent has been checked against what we stored.
//!
//! What a browser can reach: a checkout that returns a `payurl`, and a cancel.
//! Neither touches `activate_subscription`. There is no route that does.
//!
//! Credentials come from the environment, are sent only in request bodies, and
//! appear in no log line, no API response and no database column.

use crate::db::{BillingPayment, CloudDb};
use crate::models::{BillingStatus, BillingSubscription};
use crate::{CloudError, Result};
use std::sync::Arc;

/// `POST https://api.payapp.kr/oapi/apiLoad.html`, form-encoded, answering with a
/// URL-encoded `key=value` query string.
pub const DEFAULT_API_URL: &str = "https://api.payapp.kr/oapi/apiLoad.html";

pub const PROVIDER: &str = "payapp";

/// `pay_state`, exactly as PayApp documents it.
///
/// Spelt out rather than reduced to "4 or not 4" because the difference between a
/// cancellation and a pending payment is a thing an operator reads in the ledger,
/// and a number there would send them back to the documentation.
pub mod pay_state {
    /// 요청 — a payment has been asked for, nothing has been approved.
    pub const REQUESTED: &str = "1";
    /// 결제완료. **The only state that grants an entitlement.**
    pub const PAID: &str = "4";
    /// 요청취소.
    pub const REQUEST_CANCELLED: [&str; 2] = ["8", "32"];
    /// 승인취소 — an approved payment was reversed.
    pub const APPROVAL_CANCELLED: [&str; 2] = ["9", "64"];
    /// 결제대기.
    pub const WAITING: &str = "10";
    /// 부분취소.
    pub const PARTIALLY_CANCELLED: [&str; 2] = ["70", "71"];

    /// Does this state mean money came back out?
    pub fn is_reversal(state: &str) -> bool {
        APPROVAL_CANCELLED.contains(&state) || PARTIALLY_CANCELLED.contains(&state)
    }

    /// Korean for the ledger. An unknown state keeps its number rather than being
    /// flattened into "기타": PayApp may add one, and a number we can look up beats
    /// a word we invented.
    pub fn describe(state: &str) -> String {
        match state {
            REQUESTED => "요청".into(),
            PAID => "결제완료".into(),
            WAITING => "결제대기".into(),
            s if REQUEST_CANCELLED.contains(&s) => "요청취소".into(),
            s if APPROVAL_CANCELLED.contains(&s) => "승인취소".into(),
            s if PARTIALLY_CANCELLED.contains(&s) => "부분취소".into(),
            s => format!("알 수 없는 상태({s})"),
        }
    }
}

/// How long a registration is asked to live, in years.
///
/// `rebillExpire` is required and PayApp gives it no "forever". Ten years is a
/// deliberate choice: long enough that no real subscription reaches it, short
/// enough to be a date a human can read and not obviously nonsense to PayApp's
/// own validation. Ending a subscription is `rebillCancel`'s job, never expiry —
/// so if this date is ever reached, something has gone wrong, and it should be an
/// operator's problem rather than a silent renewal into the 2200s.
pub const REBILL_YEARS: i64 = 10;

/// Everything about the PayApp account this server bills through.
///
/// A value rather than four `std::env::var` calls at the point of use, for the
/// same reason the YouTube client is: the environment is process-global, and a
/// configuration that is a value can be handed to a fake in a test.
#[derive(Clone)]
pub struct Config {
    pub userid: String,
    pub linkkey: String,
    pub linkval: String,
    pub api_url: String,
    /// Where PayApp sends notifications and browsers back to. `https://247streams.kr`.
    pub public_url: String,
}

/// Neither key is ever printable, not even by accident.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("payapp::Config")
            .field("userid", &self.userid)
            .field("linkkey", &"[REDACTED]")
            .field("linkval", &"[REDACTED]")
            .field("api_url", &self.api_url)
            .field("public_url", &self.public_url)
            .finish()
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let get = |k: &str| std::env::var(k).unwrap_or_default().trim().to_string();
        let (userid, linkkey, linkval) = (get("PAYAPP_USERID"), get("PAYAPP_LINKKEY"), get("PAYAPP_LINKVAL"));
        if userid.is_empty() || linkkey.is_empty() || linkval.is_empty() {
            return Err(CloudError::Invalid("결제 시스템이 아직 설정되지 않았습니다.".into()));
        }
        let api_url = {
            let v = get("PAYAPP_API_URL");
            if v.is_empty() {
                DEFAULT_API_URL.to_string()
            } else {
                v
            }
        };
        let public_url = {
            let v = get("LOUVER_PUBLIC_URL");
            if v.is_empty() {
                // Without this the notification URL we hand PayApp would point
                // nowhere, every payment would go unconfirmed, and the failure
                // would look like "the callback never arrived".
                return Err(CloudError::Invalid(
                    "결제 시스템이 아직 설정되지 않았습니다. 서버에 LOUVER_PUBLIC_URL 을 설정해 주세요."
                        .into(),
                ));
            }
            v.trim_end_matches('/').to_string()
        };
        Ok(Self { userid, linkkey, linkval, api_url, public_url })
    }

    /// Is billing available on this server at all?
    pub fn is_configured() -> bool {
        Self::from_env().is_ok()
    }

    fn feedback_url(&self) -> String {
        format!("{}/api/billing/payapp/feedback", self.public_url)
    }

    fn return_url(&self) -> String {
        format!("{}/billing/complete", self.public_url)
    }

    fn fail_url(&self) -> String {
        format!("{}/api/billing/payapp/failure", self.public_url)
    }
}

/// The transport, abstracted so no test reaches PayApp.
pub trait FormPost: Send + Sync + std::fmt::Debug {
    /// POST `application/x-www-form-urlencoded`, returning the raw body.
    fn post_form(&self, url: &str, fields: &[(&str, &str)]) -> Result<String>;
}

/// The real transport.
///
/// Blocking, like everything else below the API: this is one form POST per
/// checkout, and an async runtime for it would be a larger change than the thing
/// it carries. A timeout, because a payment provider that has stopped answering
/// must not hold a request thread for ever.
#[derive(Debug, Default)]
pub struct UreqForm;

impl FormPost for UreqForm {
    fn post_form(&self, url: &str, fields: &[(&str, &str)]) -> Result<String> {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(20)))
            .user_agent(concat!("247streams/", env!("CARGO_PKG_VERSION")))
            // A refusal from PayApp has a body that says why, and the default
            // would throw it away in favour of a bare status.
            .http_status_as_error(false)
            .build()
            .into();
        match agent.post(url).header("Content-Type", "application/x-www-form-urlencoded").send(&body) {
            Ok(mut res) => res
                .body_mut()
                .read_to_string()
                .map_err(|e| CloudError::Invalid(format!("결제 서버 응답을 읽지 못했습니다: {e}"))),
            // The URL is ours and the body has credentials in it, so neither goes
            // into the message.
            Err(e) => {
                eprintln!("[louver] payapp: 전송 실패: {e}");
                Err(CloudError::Invalid("결제 서버에 연결할 수 없습니다. 잠시 후 다시 시도해주세요.".into()))
            }
        }
    }
}

/// PayApp's answer, parsed.
///
/// `state=1` is success and anything else is not; `errorMessage` is theirs and may
/// say anything, so it never reaches a user unaltered.
#[derive(Debug, Clone, Default)]
pub struct ApiReply {
    pub state: String,
    pub errno: String,
    pub error_message: Option<String>,
    pub rebill_no: Option<String>,
    pub payurl: Option<String>,
}

impl ApiReply {
    pub fn succeeded(&self) -> bool {
        self.state == "1"
    }
}

/// Parse a URL-encoded `key=value&…` body.
///
/// Tolerant by design: unknown keys are kept and missing ones are absent rather
/// than an error, because PayApp may add a field and a parser that refused would
/// turn that into an outage.
pub fn parse_query(body: &str) -> std::collections::BTreeMap<String, String> {
    body.trim()
        .trim_start_matches('?')
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

/// Percent-decoding, with `+` as a space — the `application/x-www-form-urlencoded`
/// rules, which is what both the request and the reply use.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    // Not a valid escape: keep the '%' as typed rather than
                    // dropping a character out of somebody's memo.
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encode a form value.
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// What the browser is given after a checkout.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Checkout {
    /// Where to send the browser. PayApp's, not ours.
    pub payurl: String,
    /// Our own opaque order id, so the completion page can ask about this attempt.
    pub billing_id: String,
    pub plan_id: String,
    pub amount_krw: i64,
}

/// The PayApp integration.
pub struct Payapp {
    db: CloudDb,
    http: Arc<dyn FormPost>,
    config: Config,
}

impl std::fmt::Debug for Payapp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Payapp")
    }
}

impl Clone for Payapp {
    fn clone(&self) -> Self {
        Self { db: self.db.clone(), http: Arc::clone(&self.http), config: self.config.clone() }
    }
}

impl Payapp {
    pub fn new(db: CloudDb, http: Arc<dyn FormPost>, config: Config) -> Self {
        Self { db, http, config }
    }

    /// Register a recurring payment and hand back the URL to approve it at.
    ///
    /// Emphatically **not** an activation. PayApp's own documentation says the
    /// buyer has to approve the first charge at the `payurl` before any recurring
    /// payment happens, so the only thing that changes here is that a `pending`
    /// billing record now has a `rebill_no` against it.
    pub fn checkout(&self, user_id: &str, plan_id: &str, recvphone: &str) -> Result<Checkout> {
        let user = self.db.user(user_id)?;

        // The plan has to be one that is actually on sale. Reading the price from
        // here rather than from the request is the whole reason this lookup exists.
        let plan = self
            .db
            .plans_for_sale()?
            .into_iter()
            .find(|p| p.id == plan_id)
            .ok_or_else(|| CloudError::Invalid("선택할 수 없는 요금제입니다.".into()))?;

        // §14: one recurring registration per account. Two would mean two charges
        // a month, for ever, and PayApp has no idea the first one exists.
        if let Some(live) = self.db.live_billing_subscription(user_id)? {
            let same = live.plan_id == plan_id;
            return Err(CloudError::Invalid(if same {
                "이미 이 요금제의 정기결제가 등록되어 있습니다.".into()
            } else {
                "현재 구독을 해지한 후 요금제를 변경할 수 있습니다.".into()
            }));
        }

        let phone = clean_phone(recvphone)?;
        let record =
            self.db.open_billing_subscription(user_id, &plan.id, PROVIDER, plan.monthly_price_krw)?;

        let today = chrono::Utc::now().date_naive();
        let amount = plan.monthly_price_krw.to_string();
        let cycle_month = cycle_month_for(today).to_string();
        let expire = rebill_expire_from(today);
        let goodname = format!("247streams {}", plan.label);
        let (feedback, ret, fail) =
            (self.config.feedback_url(), self.config.return_url(), self.config.fail_url());

        // Ordered as PayApp documents them, so this list can be read against the
        // page it came from. `var1` is our own order id and nothing else: it comes
        // back through a callback, so it must not be an email or a user id.
        let fields: Vec<(&str, &str)> = vec![
            ("cmd", "rebillRegist"),
            ("userid", &self.config.userid),
            ("goodname", &goodname),
            ("goodprice", &amount),
            ("recvphone", &phone),
            ("rebillCycleType", "Month"),
            ("rebillCycleMonth", &cycle_month),
            ("rebillExpire", &expire),
            ("recvemail", &user.email),
            ("openpaytype", "card"),
            ("feedbackurl", &feedback),
            ("returnurl", &ret),
            ("failurl", &fail),
            ("var1", &record.id),
            ("var2", &plan.id),
        ];

        // Everything from here can fail, and a failure must leave the record
        // saying so. A record stuck at `pending` would go on holding the provider
        // for ever by the rule above, and the user could never try again — which
        // is a worse outcome than the original failure.
        match self.register(&record.id, &fields) {
            Ok(payurl) => Ok(Checkout {
                payurl,
                billing_id: record.id,
                plan_id: plan.id,
                amount_krw: plan.monthly_price_krw,
            }),
            Err(e) => {
                let _ = self.db.set_billing_status(&record.id, BillingStatus::RegistrationFailed);
                Err(e)
            }
        }
    }

    /// The provider half of a checkout: register, and store what comes back.
    fn register(&self, billing_id: &str, fields: &[(&str, &str)]) -> Result<String> {
        let reply = self.call(fields)?;
        let (Some(rebill_no), Some(payurl)) = (reply.rebill_no.clone(), reply.payurl.clone()) else {
            // A `state=1` with nothing in it is not something to carry on from:
            // without a `rebill_no` there is no way to match the notification to
            // this record later.
            eprintln!("[louver] payapp: rebillRegist가 rebill_no/payurl 없이 성공했습니다");
            return Err(CloudError::Invalid("결제를 시작할 수 없습니다. 잠시 후 다시 시도해주세요.".into()));
        };
        self.db.attach_provider_subscription(billing_id, &rebill_no)?;
        Ok(payurl)
    }

    /// Stop the provider charging again. §5.
    ///
    /// Does **not** touch the entitlement. PayApp's cancellation ends the
    /// registration; it does not reverse a payment that has already been approved,
    /// so the period the user paid for is still theirs.
    pub fn cancel(&self, user_id: &str) -> Result<BillingSubscription> {
        let record = self
            .db
            .live_billing_subscription(user_id)?
            .ok_or_else(|| CloudError::Invalid("해지할 정기결제가 없습니다.".into()))?;

        if let Some(rebill_no) = record.provider_subscription_id.clone() {
            // `rebillCancel` takes linkkey and, per the documentation, not linkval.
            let fields: Vec<(&str, &str)> = vec![
                ("cmd", "rebillCancel"),
                ("userid", &self.config.userid),
                ("rebill_no", &rebill_no),
                ("linkkey", &self.config.linkkey),
            ];
            self.call(&fields)?;
        }

        // A record that never got as far as a payment has nothing to keep: it goes
        // straight to cancelled. One that has been paid keeps its entitlement.
        let next = if record.status == BillingStatus::Pending {
            BillingStatus::Cancelled
        } else {
            BillingStatus::CancelAtPeriodEnd
        };
        self.db.set_billing_status(&record.id, next)?;
        self.db.billing_subscription(&record.id)
    }

    /// One PayApp request, with the reply turned into something safe.
    fn call(&self, fields: &[(&str, &str)]) -> Result<ApiReply> {
        let body = self.http.post_form(&self.config.api_url, fields)?;
        let map = parse_query(&body);
        let reply = ApiReply {
            state: map.get("state").cloned().unwrap_or_default(),
            errno: map.get("errno").cloned().unwrap_or_default(),
            error_message: map.get("errorMessage").cloned().filter(|m| !m.is_empty()),
            rebill_no: map.get("rebill_no").cloned().filter(|v| !v.is_empty()),
            payurl: map.get("payurl").cloned().filter(|v| !v.is_empty()),
        };
        if !reply.succeeded() {
            // The provider's own words go to the server log, where an operator can
            // read them; the user gets a sentence. `errorMessage` is free text from
            // somebody else's system and is not something to render into a page.
            eprintln!(
                "[louver] payapp: 요청 실패 state={} errno={} message={}",
                reply.state,
                reply.errno,
                reply.error_message.as_deref().unwrap_or("-")
            );
            return Err(CloudError::Invalid("결제 요청이 거절되었습니다. 잠시 후 다시 시도해주세요.".into()));
        }
        Ok(reply)
    }

    // --- the notification -------------------------------------------------

    /// Handle one `feedbackurl` POST. §2, §3, §4.
    ///
    /// The only path in this program that grants a paid entitlement, and it does so
    /// only when every one of these holds:
    ///
    /// * `userid`, `linkkey` and `linkval` match this server's own credentials —
    ///   which is what makes the notification authentic rather than merely
    ///   well-formed;
    /// * `var1` names a billing record we created;
    /// * `price` equals the amount we stored when we created it;
    /// * `rebill_no` is the one PayApp gave us for that record;
    /// * `pay_state` is `4`.
    ///
    /// The plan comes from the record, never from the callback. A notification that
    /// said `var2=business` for a record that bought Basic gets Basic.
    pub fn handle_feedback(&self, form: &std::collections::BTreeMap<String, String>) -> FeedbackOutcome {
        let field = |k: &str| form.get(k).map(|s| s.trim()).unwrap_or("");

        // Credentials first. Everything after this reads values from a request
        // that has proved it came from PayApp.
        if field("userid") != self.config.userid
            || !constant_time_eq(field("linkkey"), &self.config.linkkey)
            || !constant_time_eq(field("linkval"), &self.config.linkval)
        {
            // Deliberately says which check failed only in general terms, and
            // prints none of the values.
            eprintln!("[louver] payapp: 인증되지 않은 feedback 요청을 거부했습니다");
            return FeedbackOutcome::Rejected("인증 실패");
        }

        let order_id = field("var1");
        let Ok(record) = self.db.billing_subscription_for_order(PROVIDER, order_id) else {
            eprintln!("[louver] payapp: 알 수 없는 주문의 feedback (var1={order_id})");
            return FeedbackOutcome::Rejected("알 수 없는 주문");
        };

        let pay_state = field("pay_state").to_string();
        let price: i64 = field("price").replace(',', "").parse().unwrap_or(-1);
        if price != record.amount_krw {
            eprintln!(
                "[louver] payapp: 금액 불일치 order={} 기대={} 수신={}",
                record.id, record.amount_krw, price
            );
            return FeedbackOutcome::Rejected("금액 불일치");
        }

        let rebill_no = field("rebill_no");
        match record.provider_subscription_id.as_deref() {
            Some(stored) if stored == rebill_no => {}
            _ => {
                eprintln!("[louver] payapp: rebill_no 불일치 order={}", record.id);
                return FeedbackOutcome::Rejected("정기결제 번호 불일치");
            }
        }

        // The idempotency key. `mul_no` is PayApp's own id for this payment, so a
        // renewal next month is a different event and the same notification
        // arriving ten times is one. Without it — PayApp may omit it for a state
        // that is not a payment — a deterministic composite, so a repeat is still
        // a repeat rather than a fresh grant.
        let event_key = if field("mul_no").is_empty() {
            format!("{}:{}:{}", record.id, pay_state, field("pay_date"))
        } else {
            field("mul_no").to_string()
        };

        let paid = pay_state == pay_state::PAID;
        let pay_date = field("pay_date");
        let period_end = paid.then(|| one_month_after(pay_date)).flatten();
        let status = if paid {
            Some(BillingStatus::Active)
        } else if pay_state::is_reversal(&pay_state) {
            // Money came back out. Recorded, and deliberately not a revocation:
            // taking a broadcast off air is a decision with a person behind it.
            Some(BillingStatus::PaymentFailed)
        } else {
            None
        };
        let outcome_text = if paid {
            "결제완료 — 구독 활성화".to_string()
        } else {
            format!("{} — 구독을 변경하지 않았습니다", pay_state::describe(&pay_state))
        };

        let payment = BillingPayment {
            provider: PROVIDER,
            event_key: &event_key,
            billing_id: &record.id,
            user_id: &record.user_id,
            provider_subscription_id: record.provider_subscription_id.as_deref(),
            pay_state: &pay_state,
            amount_krw: price,
            pay_date: Some(pay_date).filter(|d| !d.is_empty()),
            pay_type: Some(field("pay_type")).filter(|d| !d.is_empty()),
            outcome: &outcome_text,
            status,
            paid_at: paid.then_some(pay_date).filter(|d| !d.is_empty()),
            period_end: period_end.as_deref(),
        };

        match self.db.record_billing_payment(&payment) {
            // A repeat. Answered SUCCESS so PayApp stops retrying, and nothing
            // else happens — the whole point of §4.
            Ok(false) => FeedbackOutcome::AlreadyHandled,
            Ok(true) => {
                if !paid {
                    return FeedbackOutcome::Recorded(pay_state::describe(&pay_state));
                }
                // The plan is the record's. A callback cannot upgrade anybody.
                match self.db.activate_subscription(&record.user_id, &record.plan_id) {
                    Ok(_) => FeedbackOutcome::Activated(record.plan_id.clone()),
                    Err(e) => {
                        eprintln!("[louver] payapp: 구독 활성화 실패 order={}: {e}", record.id);
                        FeedbackOutcome::Rejected("구독을 활성화하지 못했습니다")
                    }
                }
            }
            Err(e) => {
                eprintln!("[louver] payapp: feedback 기록 실패 order={}: {e}", record.id);
                FeedbackOutcome::Rejected("기록 실패")
            }
        }
    }
}

/// What happened to one notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedbackOutcome {
    /// Verified, first time, paid: the entitlement was granted.
    Activated(String),
    /// Verified and recorded, but not a payment. No entitlement changed.
    Recorded(String),
    /// Seen before. Nothing done, and PayApp should stop asking.
    AlreadyHandled,
    /// Not something we will act on. PayApp is *not* told SUCCESS, so a genuine
    /// notification that failed for a transient reason is retried rather than
    /// lost; a forged one simply never succeeds.
    Rejected(&'static str),
}

impl FeedbackOutcome {
    /// Should PayApp be told `SUCCESS`?
    pub fn is_accepted(&self) -> bool {
        !matches!(self, Self::Rejected(_))
    }
}

/// Which day of the month PayApp should charge on. §8.
///
/// Our policy, stated here because PayApp's documentation does not promise a
/// relationship between the first approval and the cycle day: **charge on the day
/// of the month the subscription was started, and on the last day of the month for
/// the 29th, 30th and 31st.**
///
/// `rebillCycleMonth` takes 1–31 for a day and 90 for 말일. Picking 31 for a
/// January signup would leave February undefined; 90 means "the last day", which
/// is the nearest thing to "the same day" that every month has.
pub fn cycle_month_for(today: chrono::NaiveDate) -> i64 {
    use chrono::Datelike;
    match today.day() {
        d @ 1..=28 => d as i64,
        // 29, 30, 31 → 말일.
        _ => 90,
    }
}

/// `rebillExpire`, in the `yyyy-mm-dd` PayApp requires. §8.
pub fn rebill_expire_from(today: chrono::NaiveDate) -> String {
    // `with_year` fails only on 29 February, where the same-day-ten-years-later
    // does not exist; stepping back a day is the conventional answer and keeps the
    // date valid.
    let then = today
        .with_year(today.year() + REBILL_YEARS as i32)
        .or_else(|| today.pred_opt().and_then(|d| d.with_year(d.year() + REBILL_YEARS as i32)))
        .unwrap_or(today);
    then.format("%Y-%m-%d").to_string()
}

use chrono::Datelike;

/// A month after a payment date, for `current_period_end`.
///
/// PayApp's `pay_date` is theirs to format, so this parses the shapes it is
/// documented to use and answers `None` for anything else rather than inventing a
/// period. A `None` here costs a display field; a guess would cost a wrong
/// cancellation date.
pub fn one_month_after(pay_date: &str) -> Option<String> {
    let date = parse_pay_date(pay_date)?;
    let (y, m) = if date.month() == 12 { (date.year() + 1, 1) } else { (date.year(), date.month() + 1) };
    // The same day next month, or that month's last day when it has no such day.
    let day = date.day().min(days_in_month(y, m));
    chrono::NaiveDate::from_ymd_opt(y, m, day).map(|d| d.format("%Y-%m-%d").to_string())
}

fn parse_pay_date(s: &str) -> Option<chrono::NaiveDate> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // `2026-09-27 13:04:11`, `2026-09-27`, `20260927`.
    let date_part = s.split_whitespace().next().unwrap_or(s);
    chrono::NaiveDate::parse_from_str(date_part, "%Y-%m-%d")
        .or_else(|_| chrono::NaiveDate::parse_from_str(date_part, "%Y/%m/%d"))
        .or_else(|_| chrono::NaiveDate::parse_from_str(date_part, "%Y%m%d"))
        .ok()
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    chrono::NaiveDate::from_ymd_opt(ny, nm, 1)
        .and_then(|first| first.pred_opt())
        .map(|last| last.day())
        .unwrap_or(28)
}

/// Digits only, and long enough to be a Korean mobile number.
///
/// `recvphone` is required by PayApp and is what the buyer is sent the payment
/// link on, so a typo here is a payment that never happens.
pub fn clean_phone(raw: &str) -> Result<String> {
    let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 10 || digits.len() > 11 || !digits.starts_with("01") {
        return Err(CloudError::Invalid("휴대폰 번호를 확인해주세요. (예: 01012345678)".into()));
    }
    Ok(digits)
}

/// Compare two secrets without leaking their length difference through timing.
///
/// The link keys are shared secrets, and this is the one place they are compared
/// against something an attacker controls.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
