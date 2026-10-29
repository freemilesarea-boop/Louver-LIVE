//! Who may point a broadcast at a CCTV address.
//!
//! The feature is sold with Business, and the thing being proved here is the
//! half that is easy to get wrong: a free trial is handed out as a *grant*, and
//! a grant raises every other quota on purpose. If it opened this too, the
//! trial would be the product. So the save path asks the plan the account pays
//! for, and these tests pin both answers.

use louver_cloud::entitlement::{CCTV_ENABLED, MAX_CONCURRENT_STREAMS};
use louver_cloud::grants::{NewGrant, OnExisting, Term};
use louver_cloud::{BroadcastPatch, CloudDb, CloudError};

/// An IP literal, so `cctv::validate` resolves nothing and the test needs no
/// network. 93.184.216.34 is the documentation address and is public, which is
/// all the address rules care about.
const CAM: &str = "https://93.184.216.34/live/cam1.m3u8";

/// A user on `plan` with one broadcast ready to be edited.
fn user_with_broadcast(db: &CloudDb, email: &str, plan: &str) -> (String, String) {
    let u = db.create_user(email, "hash", plan).unwrap();
    let media = db.create_media(&u.id, "clip.mp4", 1024, "key/clip.mp4").unwrap();
    db.record_media_prepared(&media.id, "key/prepared.mp4", 60.0, 0, 2048).unwrap();
    let dest = db.create_destination(&u.id, "채널", "rtmps://a/live2", "••••").unwrap();
    let b = db.create_broadcast(&u.id, "LIVE", &media.id, &dest.id, true).unwrap();
    (u.id, b.id)
}

fn set_cctv(db: &CloudDb, uid: &str, bid: &str, url: &str) -> louver_cloud::Result<()> {
    let patch = BroadcastPatch { cctv_url: Some(url.to_string()), ..Default::default() };
    db.update_broadcast_owned(uid, bid, &patch).map(|_| ())
}

#[test]
fn business_may_point_a_broadcast_at_a_camera() {
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, bid) = user_with_broadcast(&db, "biz@x.com", "business");

    set_cctv(&db, &uid, &bid, CAM).expect("Business is what the feature is sold with");
    assert_eq!(db.broadcast(&bid).unwrap().cctv_url.as_deref(), Some(CAM));
}

#[test]
fn pro_may_not() {
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, bid) = user_with_broadcast(&db, "pro@x.com", "pro");

    match set_cctv(&db, &uid, &bid, CAM) {
        Err(CloudError::LimitReached { limit, ref plan_label, .. }) => {
            assert_eq!(limit, CCTV_ENABLED);
            assert_eq!(plan_label, "Pro", "the message names the plan they are on");
        }
        other => panic!("Pro must be refused, got {other:?}"),
    }
    assert!(db.broadcast(&bid).unwrap().cctv_url.is_none(), "nothing may be stored");
}

#[test]
fn basic_may_not() {
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, bid) = user_with_broadcast(&db, "basic@x.com", "basic");
    assert!(matches!(
        set_cctv(&db, &uid, &bid, CAM),
        Err(CloudError::LimitReached { limit: CCTV_ENABLED, .. })
    ));
}

/// The one that pays for the `paid_flag` split existing.
#[test]
fn a_free_trial_raises_quotas_but_does_not_open_cctv() {
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, bid) = user_with_broadcast(&db, "trial@x.com", "basic");

    // An operator hands out Business for a month — a free trial.
    let admin = db.create_user("boss@x.com", "hash", "business").unwrap();
    db.set_role("boss@x.com", louver_cloud::ROLE_ADMIN).unwrap();
    db.create_grants(
        std::slice::from_ref(&uid),
        &NewGrant { plan_id: "business", term: Term::Days(30), reason: "무료 체험" },
        OnExisting::Extend,
        &admin.id,
        "boss@x.com",
    )
    .unwrap();

    // The grant is live and does what a grant is for: the quota moved.
    assert_eq!(
        db.limit(&uid, MAX_CONCURRENT_STREAMS).unwrap(),
        3,
        "a grant must still raise the limits it has always raised"
    );
    // And the feature sold with the plan did not come with it.
    assert!(!db.paid_flag(&uid, CCTV_ENABLED).unwrap());
    assert!(matches!(
        set_cctv(&db, &uid, &bid, CAM),
        Err(CloudError::LimitReached { limit: CCTV_ENABLED, .. })
    ));
    assert!(db.broadcast(&bid).unwrap().cctv_url.is_none());
}

/// Clearing is not using. An account that leaves Business must be able to take
/// the address back out of a broadcast rather than be stuck with one it can no
/// longer run.
#[test]
fn clearing_a_camera_is_allowed_on_any_plan() {
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, bid) = user_with_broadcast(&db, "down@x.com", "business");
    set_cctv(&db, &uid, &bid, CAM).unwrap();

    db.set_plan(&uid, "basic").unwrap();
    set_cctv(&db, &uid, &bid, "").expect("clearing must not be gated");
    assert!(db.broadcast(&bid).unwrap().cctv_url.is_none());
}

/// The plan is asked before the address, so somebody without the feature is not
/// sent off to fix a URL that was never the problem.
#[test]
fn the_plan_is_refused_before_the_address_is_judged() {
    let db = CloudDb::open_in_memory().unwrap();
    let (uid, bid) = user_with_broadcast(&db, "order@x.com", "pro");

    // Malformed *and* not entitled. The answer must be about the plan.
    match set_cctv(&db, &uid, &bid, "not-a-url-at-all") {
        Err(CloudError::LimitReached { limit: CCTV_ENABLED, .. }) => {}
        other => panic!("the plan check must come first, got {other:?}"),
    }
}
