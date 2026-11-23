//! What happens to a half-finished conversion when the server goes away.
//!
//! Converting an upload to the broadcast profile takes hours — the production
//! log shows 9,000 to 13,500 seconds of source at three or four times
//! realtime, one file at a time, because the box has two cores and the live
//! broadcasts come first. A deploy lands in the middle of that more often than
//! not.
//!
//! The record that a conversion was running lived in `Ingest::in_flight`, an
//! in-memory set. `media.state` in the database said `preparing`, and after a
//! restart those two disagreed: the row read as work in progress that no thread
//! was doing. Nothing looked again — `prepare` is called by the upload that
//! created the row and by `ensure_playlist_compatible` when a playlist is
//! saved, and neither happens on its own. One customer's screen said
//! 맞추는 중 for days.
//!
//! Two things are pinned here: the restart picks that work back up, and the
//! message stops promising a conversion that already failed.

use louver_cloud::{CloudDb, CloudError, MediaState};

/// A media row in a given state, with no file behind it: these tests are about
/// bookkeeping, not about bytes.
fn media_in(db: &CloudDb, user: &str, name: &str, state: MediaState) -> String {
    let m = db.create_media(user, name, 1024, &format!("key/{name}")).unwrap();
    db.set_media_state(&m.id, state).unwrap();
    m.id
}

/// What `playlist_shapes` reads. Written directly because the real writer is
/// the encoder, and these tests must run without one.
fn shape(db: &CloudDb, id: &str, mode: &str, signature: Option<&str>) {
    db.raw()
        .lock()
        .unwrap()
        .execute(
            "UPDATE media SET prepared_mode=?2, prepared_signature=?3 WHERE id=?1",
            rusqlite::params![id, mode, signature],
        )
        .unwrap();
}

fn user(db: &CloudDb) -> String {
    db.create_user("prep@x.com", "hash", "business").unwrap().id
}

// -- the restart picks the work back up ---------------------------------

#[test]
fn a_preparation_a_restart_interrupted_is_picked_up_again() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user(&db);
    let a = media_in(&db, &u, "a.mp4", MediaState::Preparing);
    let b = media_in(&db, &u, "b.mp4", MediaState::Analysing);

    let found = db.media_needing_preparation().unwrap();
    assert!(found.contains(&a), "preparing 은 아무도 하고 있지 않은 일입니다");
    assert!(found.contains(&b), "analysing 도 마찬가지입니다");
    assert_eq!(found.len(), 2);
}

#[test]
fn a_finished_or_abandoned_media_is_left_alone() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user(&db);
    media_in(&db, &u, "ready.mp4", MediaState::Ready);
    media_in(&db, &u, "failed.mp4", MediaState::Failed);
    media_in(&db, &u, "uploaded.mp4", MediaState::Uploaded);

    assert!(
        db.media_needing_preparation().unwrap().is_empty(),
        "끝난 것, 포기한 것, 아직 시작도 안 한 것은 이어서 할 일이 아닙니다"
    );
}

// -- the message stops lying --------------------------------------------

/// A playlist whose items cannot be joined: one native, one canonical.
fn mixed_playlist(db: &CloudDb, u: &str) -> (String, String, String) {
    let one = media_in(db, u, "one.mp4", MediaState::Ready);
    let two = media_in(db, u, "two.mp4", MediaState::Ready);
    shape(db, &one, "direct", Some("h264/1280x720/2fps"));
    shape(db, &two, "canonical", Some("h264/1920x1080/30fps"));
    let dest = db.create_destination(u, "채널", "rtmps://a/live2", "••••").unwrap();
    let b = db.create_broadcast(u, "LIVE", &one, &dest.id, true).unwrap();
    db.replace_items(
        u,
        &b.id,
        &[
            louver_cloud::db::NewItem { media_id: one.clone(), enabled: true, repeat_count: 1 },
            louver_cloud::db::NewItem { media_id: two.clone(), enabled: true, repeat_count: 1 },
        ],
    )
    .unwrap();
    (b.id, one, two)
}

#[test]
fn a_playlist_still_being_converted_says_it_is_being_converted() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user(&db);
    let (b, one, _) = mixed_playlist(&db, &u);
    db.set_media_state(&one, MediaState::Preparing).unwrap();

    match db.check_playlist_joinable(&b) {
        Err(CloudError::Invalid(m)) => {
            assert!(m.contains("맞추는 중"), "진행 중일 때는 기다리라고 말해야 합니다: {m}")
        }
        other => panic!("형식이 섞였으므로 거부되어야 합니다: {other:?}"),
    }
}

#[test]
fn a_playlist_whose_conversion_failed_says_that_instead() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user(&db);
    let (b, one, _) = mixed_playlist(&db, &u);
    db.record_media_failed(&one, "디스크가 가득 찼습니다").unwrap();

    match db.check_playlist_joinable(&b) {
        Err(CloudError::Invalid(m)) => {
            assert!(
                !m.contains("맞추는 중"),
                "실패한 변환을 두고 기다리라고 말하면 고객은 영원히 기다립니다: {m}"
            );
            assert!(m.contains("바꾸지 못했습니다"), "{m}");
            assert!(m.contains("디스크가 가득"), "원인을 그대로 보여 줘야 합니다: {m}");
        }
        other => panic!("실패한 영상이 있으면 거부되어야 합니다: {other:?}"),
    }
}

#[test]
fn a_joinable_playlist_is_still_allowed() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user(&db);
    let (b, one, two) = mixed_playlist(&db, &u);
    // Make them agree, which is what the conversion is for.
    shape(&db, &one, "canonical", Some("h264/1920x1080/30fps"));
    shape(&db, &two, "canonical", Some("h264/1920x1080/30fps"));
    db.check_playlist_joinable(&b).expect("둘 다 canonical 이면 이어붙일 수 있습니다");
}
