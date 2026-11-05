//! A broadcast must come off air when the entitlement behind it runs out.
//!
//! The subscription gate was asked at the three moments something *starts* —
//! CREATE, START and boot recovery — and at the two moments entitlement is
//! taken away by a request: the provider's cancellation callback, and an
//! operator switching an account off.
//!
//! None of those is the case that cost money. A pass bought for one month is a
//! date, and a date passing calls nothing. A broadcast already running when its
//! grant lapsed kept running: FFmpeg does not consult the database, the
//! scheduler only looked at schedules, and nothing else was watching. On a
//! service whose whole product is 24/7 streaming, that is one paid month in
//! exchange for unlimited broadcasting — until a human noticed, or the server
//! happened to restart and recovery refused to bring it back.
//!
//! These tests drive `expire_lapsed` directly, which is what the scheduler tick
//! calls.

use louver_cloud::grants::{NewGrant, OnExisting, Term};
use louver_cloud::manager::{BroadcastManager, LauncherFactory};
use louver_cloud::storage::{LocalStorage, Storage};
use louver_cloud::{CloudDb, DesiredState};
use louver_core::error::Result as CoreResult;
use louver_core::runtime::StreamLauncher;
use louver_core::security::{MemorySecretStore, SecretStore};
use louver_core::streaming::ffmpeg::FfmpegTools;
use louver_core::streaming::supervisor::{ProcessHandle, StreamSupervisor};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// --- a process that starts and stops and does nothing else ------------------

#[derive(Debug, Default)]
struct Dead(AtomicBool);

impl ProcessHandle for Dead {
    fn try_exited(&mut self) -> Option<bool> {
        if self.0.load(Ordering::SeqCst) { Some(true) } else { None }
    }
    fn terminate(&mut self) -> CoreResult<()> {
        self.0.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn pid(&self) -> Option<u32> {
        Some(4242)
    }
}

#[derive(Debug)]
struct Fake;

impl StreamLauncher for Fake {
    fn launch(&self, _s: &mut StreamSupervisor, _args: &[String]) -> CoreResult<Box<dyn ProcessHandle>> {
        Ok(Box::new(Dead::default()))
    }
}

#[derive(Debug)]
struct Fakes;

impl LauncherFactory for Fakes {
    fn for_broadcast(&self, _db: &CloudDb, _id: &str) -> Arc<dyn StreamLauncher> {
        Arc::new(Fake)
    }
}

// --- harness ----------------------------------------------------------------

struct H {
    _dir: tempfile::TempDir,
    db: CloudDb,
    mgr: BroadcastManager,
    user: String,
    dest: String,
}

/// An account on `plan`, with no broadcasts yet.
fn account(plan: &str) -> H {
    let dir = tempfile::tempdir().unwrap();
    let db = CloudDb::open(&dir.path().join("cloud.db")).unwrap();
    let keys: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
    let mgr = BroadcastManager::new(
        db.clone(),
        Arc::new(LocalStorage::new(dir.path().join("media"))),
        dir.path().join("work"),
        FfmpegTools::new("ffmpeg", "ffprobe"),
        "libx264".into(),
        keys,
        Arc::new(Fakes),
    );

    let user = db.create_user("sung@x.com", "hash", plan).unwrap().id;
    let store = LocalStorage::new(dir.path().join("media"));
    let dest = db.create_destination(&user, "채널", "rtmps://a/live2", "••••").unwrap();
    mgr.secret_store()
        .set(&louver_cloud::credentials::destination_account(&dest.id), "aaaa-aaaa-aaaa-aaaa")
        .unwrap();

    let _ = store;
    H { _dir: dir, db, mgr, user, dest: dest.id }
}

/// Put `n` broadcasts on air.
///
/// Called **after** the entitlement is in place, because that is the only order
/// reality allows: an unsubscribed account cannot create a broadcast at all
/// (`max_broadcasts` is 0 on that plan). The bug under test is not a broadcast
/// that should never have existed — it is one that was perfectly legitimate
/// when it started and outlived the thing that paid for it.
fn on_air(h: &H, n: usize) {
    let store = LocalStorage::new(h._dir.path().join("media"));
    for i in 0..n {
        let src = h._dir.path().join(format!("in{i}.mp4"));
        std::fs::write(&src, b"prepared video bytes").unwrap();
        let key = store.put_file(&h.user, &format!("clip{i}.mp4"), &src).unwrap();
        let m = h.db.create_media(&h.user, &format!("clip{i}.mp4"), 20, &key).unwrap();
        h.db.record_media_prepared(&m.id, &key, 60.0, 0, 20).unwrap();
        let b = h
            .db
            .create_broadcast(&h.user, &format!("LIVE {i}"), &m.id, &h.dest, true)
            .unwrap();
        // Written to the column rather than started through the manager:
        // what is under test is the sweep over `desired_state = running`, not
        // the start path, which has its own tests and a real encoder.
        h.db.raw()
            .lock()
            .unwrap()
            .execute("UPDATE broadcasts SET desired_state='running' WHERE id=?1", [&b.id])
            .unwrap();
    }
}

/// A grant from now, for `days`.
fn grant(h: &H, plan: &str, days: i64) -> String {
    let admin = h.db.create_user("boss@x.com", "hash", "business").unwrap();
    h.db.set_role("boss@x.com", louver_cloud::ROLE_ADMIN).unwrap();
    let r = h
        .db
        .create_grants(
            &[h.user.clone()],
            &NewGrant { plan_id: plan, term: Term::Days(days), reason: "1달 이용권" },
            OnExisting::Extend,
            &admin.id,
            "boss@x.com",
        )
        .unwrap();
    r.outcomes.first().and_then(|o| o.grant_id.clone()).expect("a grant was written")
}

/// Move a grant's end into the past, which is how a month runs out.
///
/// Written straight to the column on purpose: revoking it would exercise the
/// path that already worked. What was never covered is the date simply passing,
/// with nobody told.
fn expire(h: &H, grant_id: &str) {
    let n = h
        .db
        .raw()
        .lock()
        .unwrap()
        .execute(
            "UPDATE admin_grants SET starts_at = datetime('now','-40 days'),
                                     expires_at = datetime('now','-1 day')
             WHERE id = ?1",
            [grant_id],
        )
        .unwrap();
    assert_eq!(n, 1, "the grant under test has to be the row that moved");
}

fn still_wants_to_run(h: &H) -> usize {
    h.db.broadcasts_for(&h.user)
        .unwrap()
        .into_iter()
        .filter(|b| b.desired_state == DesiredState::Running)
        .count()
}

// --- tests ------------------------------------------------------------------

/// The case that was costing money.
#[test]
fn a_broadcast_whose_one_month_pass_ran_out_is_taken_off_air() {
    let h = account(louver_cloud::db::UNSUBSCRIBED_PLAN);
    let g = grant(&h, "business", 30);
    on_air(&h, 1);
    assert_eq!(still_wants_to_run(&h), 1, "on air while the pass is good");

    expire(&h, &g);
    // Nothing has happened yet: the date passed, and a date calls nothing.
    assert_eq!(still_wants_to_run(&h), 1, "expiry alone changes nothing — that is the bug");

    assert_eq!(h.mgr.expire_lapsed(), 1, "the tick is what notices");
    assert_eq!(still_wants_to_run(&h), 0, "and takes it off air");
}

/// Every broadcast on the account, not just the first one found.
#[test]
fn all_of_a_lapsed_accounts_broadcasts_stop_together() {
    let h = account(louver_cloud::db::UNSUBSCRIBED_PLAN);
    let g = grant(&h, "business", 30);
    on_air(&h, 3);
    expire(&h, &g);

    assert_eq!(h.mgr.expire_lapsed(), 3);
    assert_eq!(still_wants_to_run(&h), 0);
}

/// The sweep must not touch somebody whose pass is still good.
#[test]
fn a_pass_still_in_force_keeps_the_broadcast_on_air() {
    let h = account(louver_cloud::db::UNSUBSCRIBED_PLAN);
    grant(&h, "business", 30);
    on_air(&h, 2);

    assert_eq!(h.mgr.expire_lapsed(), 0, "a live grant is entitlement");
    assert_eq!(still_wants_to_run(&h), 2);
}

/// Nor somebody who is simply paying.
#[test]
fn a_paying_account_is_left_alone() {
    let h = account("business");
    on_air(&h, 2);

    assert_eq!(h.mgr.expire_lapsed(), 0);
    assert_eq!(still_wants_to_run(&h), 2);
}

/// A cancellation whose callback never arrived is caught by the same sweep.
///
/// `billing` stops broadcasts when the provider calls back. A callback that is
/// lost, or a status an operator wrote by hand, left the stream up for exactly
/// the same reason expiry did.
#[test]
fn a_cancelled_subscription_is_caught_even_if_the_callback_was_missed() {
    let h = account("business");
    on_air(&h, 1);
    h.db.raw()
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO subscriptions (user_id, plan_id, status) VALUES (?1, 'business', ?2)
             ON CONFLICT(user_id) DO UPDATE SET status = ?2",
            rusqlite::params![&h.user, louver_cloud::db::SUBSCRIPTION_UNSUBSCRIBED],
        )
        .unwrap();

    assert_eq!(h.mgr.expire_lapsed(), 1);
    assert_eq!(still_wants_to_run(&h), 0);
}

/// Idempotent: a second tick finds nothing left to do.
#[test]
fn a_second_tick_does_not_stop_what_is_already_stopped() {
    let h = account(louver_cloud::db::UNSUBSCRIBED_PLAN);
    let g = grant(&h, "business", 30);
    on_air(&h, 2);
    expire(&h, &g);

    assert_eq!(h.mgr.expire_lapsed(), 2);
    assert_eq!(h.mgr.expire_lapsed(), 0, "the tick runs every thirty seconds; it must settle");
}
