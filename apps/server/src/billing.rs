//! The billing routes. §4, §6, §9, §11, §12.
//!
//! Four of them, and the interesting thing is which one grants an entitlement:
//! only `feedback`, and only after the PayApp provider has checked every field
//! against what we stored. `checkout` hands out a payment URL and changes nothing
//! a user can broadcast with; `cancel` stops the next charge and leaves the
//! current period alone.
//!
//! `feedback` and `failure` take no session. They are server-to-server POSTs from
//! PayApp, which has no cookie of ours — what authenticates them is the link keys
//! in the body, matched against this server's own.

use crate::auth::Caller;
use crate::error::ApiError;
use crate::state::App;
use axum::extract::{Form, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use louver_cloud::billing::{self, FeedbackOutcome, Payapp};
use louver_cloud::CloudError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

type Out<T> = std::result::Result<Json<T>, ApiError>;

/// The same answer everywhere when this server has no PayApp credentials.
///
/// A deployment without them is supported, not broken: everything except paying
/// works, and this is the only thing that has to say so.
fn provider(app: &App) -> std::result::Result<Payapp, ApiError> {
    app.payapp
        .clone()
        .ok_or_else(|| ApiError(CloudError::Invalid("결제 시스템이 아직 설정되지 않았습니다.".into())))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckoutRequest {
    pub plan_id: String,
    /// PayApp requires it, and it is where the buyer is sent the payment link.
    pub recvphone: String,
}

/// Start a recurring payment. §4.
///
/// Note what is **not** in the request: an amount. The price is read from the
/// `plans` table, so a body claiming ₩100 buys nothing — there is no field through
/// which it could.
pub async fn checkout(
    State(app): State<App>,
    Caller(uid): Caller,
    Json(body): Json<CheckoutRequest>,
) -> Out<billing::Checkout> {
    let payapp = provider(&app)?;
    Ok(Json(crate::blocking(move || payapp.checkout(&uid, &body.plan_id, &body.recvphone)).await?))
}

/// What the account screen shows. §12.
#[derive(Serialize)]
pub struct BillingStatusResponse {
    /// The provider's view, when there is one.
    pub subscription: Option<louver_cloud::BillingSubscription>,
    /// The entitlement's view. The two can differ legitimately — a cancelled
    /// billing subscription still has a paid-for period to run.
    pub plan: louver_cloud::Subscription,
    pub provider: &'static str,
    /// Can this deployment take a payment at all?
    pub configured: bool,
}

pub async fn status(State(app): State<App>, Caller(uid): Caller) -> Out<BillingStatusResponse> {
    let configured = app.payapp.is_some();
    Ok(Json(
        crate::blocking(move || {
            Ok(BillingStatusResponse {
                // The newest record, whatever its state, so a pending or failed
                // attempt is visible rather than looking like nothing happened.
                subscription: app.db.billing_subscriptions_for(&uid)?.into_iter().next(),
                plan: app.db.subscription(&uid)?,
                provider: billing::PROVIDER,
                configured,
            })
        })
        .await?,
    ))
}

/// Stop the next charge. §9.
///
/// Deliberately does not call `cancel_subscription`: PayApp's cancellation ends the
/// registration and does not reverse an approved payment, so the period already
/// paid for stays the user's.
pub async fn cancel(State(app): State<App>, Caller(uid): Caller) -> Out<louver_cloud::BillingSubscription> {
    let payapp = provider(&app)?;
    Ok(Json(crate::blocking(move || payapp.cancel(&uid)).await?))
}

/// PayApp's payment notification. §6.
///
/// Unauthenticated in the session sense and authenticated in the only sense that
/// matters here: the body carries this server's own link keys, and the provider
/// checks them before reading anything else out of it.
///
/// Answers the literal string `SUCCESS` — no JSON, no redirect — because that is
/// what PayApp looks for, and anything else makes it retry. Which is the behaviour
/// wanted for a genuine notification that failed transiently, and harmless for a
/// forged one, since a forgery never succeeds however many times it is sent.
pub async fn feedback(
    State(app): State<App>,
    Form(form): Form<BTreeMap<String, String>>,
) -> impl IntoResponse {
    let Some(payapp) = app.payapp.clone() else {
        eprintln!("[louver] payapp: feedback가 도착했지만 결제 설정이 없습니다");
        return (StatusCode::SERVICE_UNAVAILABLE, "NOT CONFIGURED");
    };

    let outcome = match tokio::task::spawn_blocking(move || payapp.handle_feedback(&form)).await {
        Ok(o) => o,
        Err(_) => FeedbackOutcome::Rejected("처리 중단"),
    };

    match &outcome {
        FeedbackOutcome::Activated(plan) => println!("[louver] payapp: 결제 확인 → {plan} 구독 활성화"),
        FeedbackOutcome::Recorded(what) => println!("[louver] payapp: {what} 기록"),
        // Not noise: a repeat is the documented behaviour, and seeing them is how
        // an operator knows retries are being absorbed rather than reapplied.
        FeedbackOutcome::AlreadyHandled => println!("[louver] payapp: 이미 처리된 feedback (무시)"),
        FeedbackOutcome::Rejected(why) => eprintln!("[louver] payapp: feedback 거부 — {why}"),
    }

    if outcome.is_accepted() {
        (StatusCode::OK, "SUCCESS")
    } else {
        // Not SUCCESS, so PayApp may try again. A forged request gains nothing by
        // being retried.
        (StatusCode::BAD_REQUEST, "FAIL")
    }
}

/// PayApp's failure notification. §6, §8.
///
/// PayApp documents this as *"결제실패 Noti URL (1회차 승인 실패는 Noti되지 않습니다)"*,
/// so it is about later renewals and cannot be relied on to hear about a first
/// payment that was never approved. That is why a first payment is only ever known
/// to have happened by a `pay_state=4` notification, and never by its absence here.
///
/// Recorded, and it does **not** take the entitlement away. One failed renewal is
/// a card that needs re-entering, not a reason to drop somebody's 24/7 broadcast;
/// the record is what a policy decision would act on later.
pub async fn failure(
    State(app): State<App>,
    Form(form): Form<BTreeMap<String, String>>,
) -> impl IntoResponse {
    let Some(payapp) = app.payapp.clone() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "NOT CONFIGURED");
    };
    let outcome = match tokio::task::spawn_blocking(move || payapp.handle_feedback(&form)).await {
        Ok(o) => o,
        Err(_) => FeedbackOutcome::Rejected("처리 중단"),
    };
    match &outcome {
        FeedbackOutcome::Rejected(why) => eprintln!("[louver] payapp: 실패 알림 거부 — {why}"),
        other => println!("[louver] payapp: 결제 실패 알림 처리 — {other:?}"),
    }
    if outcome.is_accepted() {
        (StatusCode::OK, "SUCCESS")
    } else {
        (StatusCode::BAD_REQUEST, "FAIL")
    }
}
