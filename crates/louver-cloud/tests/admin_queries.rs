//! Every admin aggregation, against a real database with real rows.
//!
//! These exist because an aggregation is SQL, and SQL that references a column
//! that does not exist compiles perfectly and fails in production at the moment
//! an operator opens the page. Each query below is executed.

use louver_cloud::db::{NewItem, Signup};
use louver_cloud::{CloudDb, CloudError};

fn db() -> CloudDb {
    CloudDb::open_in_memory().unwrap()
}

fn user(db: &CloudDb, email: &str) -> String {
    db.register_user(&Signup {
        name: "테스트",
        email,
        password_hash: "salt:hash",
        terms_version: "2026-09-27",
    })
    .unwrap()
    .id
}

/// A paid subscription, the way a verified notification makes one.
fn paying(db: &CloudDb, email: &str, plan: &str, amount: i64) -> (String, String) {
    let uid = user(db, email);
    let record = db.open_billing_subscription(&uid, plan, "payapp", amount).unwrap();
    db.attach_provider_subscription(&record.id, &format!("rebill-{email}")).unwrap();
    db.record_billing_payment(&louver_cloud::db::BillingPayment {
        provider: "payapp",
        event_key: &format!("evt-{email}-1"),
        billing_id: &record.id,
        user_id: &uid,
        provider_subscription_id: Some(&format!("rebill-{email}")),
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
    db.activate_subscription(&uid, plan).unwrap();
    (uid, record.id)
}

#[test]
fn every_dashboard_aggregation_runs_and_counts_what_is_there() {
    let db = db();
    let (_, _) = paying(&db, "basic@x.com", "basic", 19_900);
    let (pro, _) = paying(&db, "pro@x.com", "pro", 39_900);
    user(&db, "free@x.com");

    let users = db.user_counts().unwrap();
    assert_eq!(users.total, 3, "three accounts exist");
    assert_eq!(users.today, 3, "all three were made today");
    assert_eq!(users.disabled, 0);

    let subs = db.subscription_counts().unwrap();
    assert_eq!(subs.paid_total, 2);
    assert_eq!(subs.unsubscribed, 1);
    let basic = subs.by_plan.iter().find(|p| p.plan_id == "basic").unwrap();
    assert_eq!((basic.count, basic.monthly_price_krw), (1, 19_900));
    assert_eq!(subs.new_this_month, 2, "both payments are first payments");
    assert_eq!(subs.renewed_this_month, 0);

    // Revenue is the ledger, not the headcount times the price.
    let r = db.revenue_summary().unwrap();
    assert_eq!(r.all_time, 59_800);
    assert_eq!(r.today, 59_800);
    assert_eq!(r.this_month, 59_800);
    assert_eq!(r.last_month, 0);
    assert_eq!(r.month_change_pct, None, "nothing last month is not a 100% rise");

    // MRR is what the provider will charge again, which is the same two records.
    assert_eq!(db.mrr().unwrap(), 59_800);
    assert_eq!(db.active_payers().unwrap(), 2);

    assert_eq!(db.broadcast_counts().unwrap().total, 0);
    assert_eq!(db.total_storage_bytes().unwrap(), 0);
    let _ = pro;
}

#[test]
fn a_duplicate_notification_is_one_payment() {
    // The whole idempotency story, seen from the revenue side: PayApp may
    // deliver the same notification ten times and the money arrived once.
    let db = db();
    let (uid, billing) = paying(&db, "dj@x.com", "pro", 39_900);
    for _ in 0..9 {
        let applied = db
            .record_billing_payment(&louver_cloud::db::BillingPayment {
                provider: "payapp",
                event_key: "evt-dj@x.com-1", // the same provider event
                billing_id: &billing,
                user_id: &uid,
                provider_subscription_id: Some("rebill-dj@x.com"),
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
        assert!(!applied, "a repeat must not be applied twice");
    }
    assert_eq!(db.revenue_summary().unwrap().all_time, 39_900, "a repeat became revenue");
    assert_eq!(db.payments(None, 50, None).unwrap().len(), 1);
}

#[test]
fn a_failed_payment_is_recorded_and_is_not_revenue() {
    let db = db();
    let (uid, billing) = paying(&db, "dj@x.com", "pro", 39_900);
    db.record_billing_payment(&louver_cloud::db::BillingPayment {
        provider: "payapp",
        event_key: "evt-reversal",
        billing_id: &billing,
        user_id: &uid,
        provider_subscription_id: Some("rebill-dj@x.com"),
        // 9 is 승인취소: the money came back out.
        pay_state: "9",
        amount_krw: 39_900,
        pay_date: Some("2026-09-29 13:00:00"),
        pay_type: Some("card"),
        outcome: "승인취소 — 구독을 변경하지 않았습니다",
        status: Some(louver_cloud::BillingStatus::PaymentFailed),
        paid_at: None,
        period_end: None,
    })
    .unwrap();

    assert_eq!(db.revenue_summary().unwrap().all_time, 39_900, "a reversal counted as revenue");
    assert_eq!(db.subscription_counts().unwrap().failed_this_month, 1);
    let failed = db.payments(None, 50, Some("failed")).unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].kind, "reversal");
    // And the record is no longer charging, so it leaves the MRR.
    assert_eq!(db.mrr().unwrap(), 0);
}

#[test]
fn a_cancelled_subscription_leaves_the_mrr_and_appears_in_the_cancellation_list() {
    let db = db();
    let (uid, billing) = paying(&db, "dj@x.com", "pro", 39_900);
    assert_eq!(db.mrr().unwrap(), 39_900);

    db.cancel_billing_and_revoke(&billing).unwrap();

    assert_eq!(db.mrr().unwrap(), 0, "a cancelled subscription is still counted as recurring");
    assert_eq!(db.active_payers().unwrap(), 0);
    // The money that already arrived is still money.
    assert_eq!(db.revenue_summary().unwrap().all_time, 39_900);

    let gone = db.cancellations(50).unwrap();
    assert_eq!(gone.len(), 1);
    assert_eq!(gone[0].user_id, uid);
    assert!(!gone[0].entitlement_active, "the entitlement is revoked with the cancellation");
    assert_eq!(gone[0].amount_krw, 39_900);
}

#[test]
fn the_revenue_report_buckets_by_day_and_splits_new_from_renewing() {
    let db = db();
    let (uid, billing) = paying(&db, "dj@x.com", "pro", 39_900);
    // Next month's charge on the same record: a renewal.
    db.record_billing_payment(&louver_cloud::db::BillingPayment {
        provider: "payapp",
        event_key: "evt-renewal",
        billing_id: &billing,
        user_id: &uid,
        provider_subscription_id: Some("rebill-dj@x.com"),
        pay_state: "4",
        amount_krw: 39_900,
        pay_date: Some("2026-10-29 12:00:00"),
        pay_type: Some("card"),
        outcome: "결제완료",
        status: Some(louver_cloud::BillingStatus::Active),
        paid_at: Some("2026-10-29 12:00:00"),
        period_end: Some("2026-11-29"),
    })
    .unwrap();

    let report = db.revenue_report("2000-01-01", "2100-01-01", "day").unwrap();
    assert_eq!(report.summary.all_time, 79_800, "the range total");
    assert_eq!(report.buckets.len(), 1, "both were recorded today: {:?}", report.buckets);
    let day = &report.buckets[0];
    assert_eq!((day.krw, day.payments), (79_800, 2));
    assert_eq!(day.new_krw, 39_900, "the first payment on the record is new");
    assert_eq!(day.renewal_krw, 39_900);
    assert_eq!(report.by_plan[0].plan_id, "pro");
    assert_eq!(report.reversals, 0);
    assert_eq!(report.active_payers, 1);
    // Every grain runs.
    for grain in ["day", "week", "month"] {
        assert!(!db.revenue_report("2000-01-01", "2100-01-01", grain).unwrap().buckets.is_empty());
    }
}

#[test]
fn the_days_boundary_is_the_operators_day_not_utc() {
    // A payment at 23:00 UTC is 08:00 the next morning in Seoul, and belongs to
    // that day. Writing the ledger row directly is the only way to place an
    // event at a chosen instant.
    let db = db();
    let (uid, billing) = paying(&db, "dj@x.com", "pro", 39_900);
    db.raw()
        .lock()
        .unwrap()
        .execute("UPDATE billing_events SET processed_at = date('now','-1 day') || ' 23:30:00'", [])
        .unwrap();
    let _ = (uid, billing);

    // Yesterday 23:30 UTC is today 08:30 in Seoul.
    let r = db.revenue_summary().unwrap();
    assert_eq!(r.today, 39_900, "a late-evening UTC payment belongs to the Korean day after it");
    assert_eq!(r.yesterday, 0);
}

#[test]
fn the_user_list_searches_filters_sorts_and_pages() {
    let db = db();
    paying(&db, "paid@x.com", "pro", 39_900);
    let free = user(&db, "free@x.com");

    let all = db.admin_users(None, None, None, 50, 0).unwrap();
    assert_eq!(all.len(), 2);
    let paid = all.iter().find(|u| u.email == "paid@x.com").unwrap();
    assert_eq!(paid.plan_id, "pro");
    assert_eq!(paid.plan_label, "Pro");
    assert_eq!(paid.total_paid_krw, 39_900);
    assert_eq!(paid.billing_status.as_deref(), Some("active"));
    assert!(paid.storage_limit_bytes > 0, "the plan's ceiling is carried with the row");

    assert_eq!(db.admin_users(Some("paid"), None, None, 50, 0).unwrap().len(), 1);
    assert_eq!(db.admin_users(None, Some("none"), None, 50, 0).unwrap()[0].id, free);
    assert_eq!(db.admin_users(None, Some("paid"), None, 50, 0).unwrap().len(), 1);
    assert_eq!(db.admin_users(None, None, Some("storage"), 1, 0).unwrap().len(), 1, "the limit holds");
    assert_eq!(db.admin_users(None, None, None, 1, 1).unwrap().len(), 1, "the offset holds");
    // Asking for a thousand rows gets a hundred at most.
    assert!(db.admin_users(None, None, None, 1000, 0).unwrap().len() <= 100);

    // Every sort runs against real SQL.
    for sort in ["created", "storage", "plan", "email", "active"] {
        db.admin_users(None, None, Some(sort), 10, 0).unwrap();
    }
    for filter in
        ["all", "paid", "none", "basic", "pro", "business", "disabled", "enabled", "youtube", "live"]
    {
        db.admin_users(None, Some(filter), None, 10, 0).unwrap();
    }
}

#[test]
fn the_user_detail_carries_payments_channels_and_broadcasts() {
    let db = db();
    let (uid, _) = paying(&db, "dj@x.com", "pro", 39_900);
    let media = db.create_media(&uid, "clip.mp4", 1024, "key/clip.mp4").unwrap();
    db.record_media_prepared(&media.id, "key/prepared.mp4", 60.0, 2048).unwrap();
    let dest = db.create_destination(&uid, "채널", "rtmps://a/live2", "••••").unwrap();
    let b = db.create_broadcast(&uid, "밤 라디오", &media.id, &dest.id, true).unwrap();
    db.replace_items(&uid, &b.id, &[NewItem { media_id: media.id, enabled: true, repeat_count: 1 }]).unwrap();

    let detail = db.admin_user(&uid).unwrap();
    assert_eq!(detail.user.email, "dj@x.com");
    assert_eq!(detail.media_count, 1);
    assert_eq!(detail.payment_count, 1);
    assert!(detail.first_paid_at.is_some());
    assert_eq!(detail.payments.len(), 1);
    assert_eq!(detail.payments[0].amount_krw, 39_900);
    assert_eq!(detail.payments[0].is_first, Some(true));
    assert_eq!(detail.billing.len(), 1);
    assert_eq!(detail.broadcasts.len(), 1);
    assert_eq!(detail.broadcasts[0].name, "밤 라디오");
    assert_eq!(detail.broadcasts[0].item_count, 1);

    // An id that is not an account is not found, rather than an empty page.
    assert!(matches!(db.admin_user("nobody").unwrap_err(), CloudError::NotFound("user")));
}

#[test]
fn the_broadcast_list_shows_everybodys_and_filters_by_state() {
    let db = db();
    let (uid, _) = paying(&db, "dj@x.com", "pro", 39_900);
    let media = db.create_media(&uid, "clip.mp4", 1024, "key/clip.mp4").unwrap();
    db.record_media_prepared(&media.id, "key/prepared.mp4", 60.0, 2048).unwrap();
    let dest = db.create_destination(&uid, "채널", "rtmps://a/live2", "••••").unwrap();
    let b = db.create_broadcast(&uid, "밤 라디오", &media.id, &dest.id, true).unwrap();

    let all = db.admin_broadcasts(None, None, 50, 0).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].email, "dj@x.com", "the list joins the owner in");
    assert_eq!(all[0].desired_state, "stopped");
    assert!(all[0].youtube_channel.is_none());

    db.claim_stream_slot(&uid, &b.id).unwrap();
    assert_eq!(db.admin_broadcasts(None, Some("running"), 50, 0).unwrap().len(), 1);
    assert_eq!(db.admin_broadcasts(None, Some("stopped"), 50, 0).unwrap().len(), 0);
    assert_eq!(db.broadcast_counts().unwrap().running, 1);
    for filter in ["all", "running", "scheduled", "stopped", "failed"] {
        db.admin_broadcasts(None, Some(filter), 10, 0).unwrap();
    }
}

#[test]
fn storage_and_problems_come_from_the_rows_the_service_already_keeps() {
    let db = db();
    let (uid, _) = paying(&db, "dj@x.com", "pro", 39_900);
    let media = db.create_media(&uid, "clip.mp4", 5_000_000, "key/clip.mp4").unwrap();
    db.record_media_prepared(&media.id, "key/prepared.mp4", 60.0, 5_000_000).unwrap();
    let dest = db.create_destination(&uid, "채널", "rtmps://a/live2", "••••").unwrap();
    let b = db.create_broadcast(&uid, "밤 라디오", &media.id, &dest.id, true).unwrap();
    db.record_failure(&b.id, "FFmpeg가 종료되었습니다").unwrap();

    let leaders = db.storage_leaders(10).unwrap();
    assert_eq!(leaders.len(), 1);
    assert_eq!(leaders[0].email, "dj@x.com");
    assert_eq!(leaders[0].bytes, 5_000_000);
    assert!(leaders[0].limit_bytes > 0);
    assert_eq!(db.total_storage_bytes().unwrap(), 5_000_000);

    let problems = db.recent_problems(10).unwrap();
    assert!(problems.iter().any(|p| p.message.contains("FFmpeg가 종료되었습니다")), "{problems:?}");
    assert_eq!(problems[0].email, "dj@x.com");
}

#[test]
fn a_role_is_set_by_the_database_and_nothing_else() {
    let db = db();
    let uid = user(&db, "ops@x.com");
    assert_eq!(db.user(&uid).unwrap().role, "user");
    assert!(!db.user(&uid).unwrap().is_admin());

    let promoted = db.set_role("OPS@X.COM", "admin").unwrap();
    assert!(promoted.is_admin(), "the lookup must not care about case");
    assert_eq!(db.admins().unwrap(), vec!["ops@x.com".to_string()]);

    db.set_role("ops@x.com", "user").unwrap();
    assert!(!db.user(&uid).unwrap().is_admin());
    assert!(db.set_role("ops@x.com", "root").is_err(), "only two roles exist");
    assert!(db.set_role("nobody@x.com", "admin").is_err());
}

#[test]
fn disabling_an_account_keeps_its_rows_and_ends_its_sessions() {
    let db = db();
    let (uid, _) = paying(&db, "dj@x.com", "pro", 39_900);
    db.create_auth_session(&uid, "token-hash", 30).unwrap();
    assert!(db.user_for_token("token-hash").is_ok());

    let off = db.set_disabled(&uid, true).unwrap();
    assert!(off.is_disabled());
    assert!(matches!(db.require_enabled(&uid), Err(CloudError::Disabled)));
    assert!(db.user_for_token("token-hash").is_err(), "an open session outlived the disable");
    // Nothing was deleted.
    assert_eq!(db.revenue_summary().unwrap().all_time, 39_900);
    assert_eq!(db.admin_user(&uid).unwrap().payment_count, 1);

    let on = db.set_disabled(&uid, false).unwrap();
    assert!(!on.is_disabled());
    assert!(db.require_enabled(&uid).is_ok());
}

#[test]
fn the_audit_log_records_what_was_done_and_redacts_what_it_must() {
    let db = db();
    let admin = user(&db, "ops@x.com");
    db.set_role("ops@x.com", "admin").unwrap();
    let (target, _) = paying(&db, "dj@x.com", "pro", 39_900);

    db.record_admin_action(&louver_cloud::admin::AuditEntry {
        admin_id: &admin,
        admin_email: "ops@x.com",
        action: "admin.user.disable",
        target_type: "user",
        target_id: &target,
        before: Some("enabled".into()),
        after: Some("disabled".into()),
        note: Some("환불 요청, 키는 GOCSPX-thisisasecretvalue 였음".into()),
    })
    .unwrap();

    let log = db.audit_log(50, None).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].action, "admin.user.disable");
    assert_eq!(log[0].admin_email, "ops@x.com");
    assert_eq!(log[0].target_id, target);
    let note = log[0].note.clone().unwrap();
    assert!(note.contains("환불 요청"), "{note}");
    assert!(!note.contains("GOCSPX-"), "a credential reached the audit log: {note}");

    // Paging is by id, newest first.
    db.record_admin_action(&louver_cloud::admin::AuditEntry {
        admin_id: &admin,
        admin_email: "ops@x.com",
        action: "admin.user.enable",
        target_type: "user",
        target_id: &target,
        before: None,
        after: None,
        note: None,
    })
    .unwrap();
    let page = db.audit_log(1, None).unwrap();
    assert_eq!(page[0].action, "admin.user.enable");
    let older = db.audit_log(10, Some(page[0].id)).unwrap();
    assert_eq!(older[0].action, "admin.user.disable");
}
