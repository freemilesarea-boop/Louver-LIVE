//! What happens when a camera's address stops working.
//!
//! This is the failure the Korean feeds guarantee rather than risk. ITS hands
//! out an address that carries a token and stops answering after about a day,
//! and nothing anywhere announces the moment it does. Three separate things
//! used to go wrong at once, and each has its own section here:
//!
//! 1. **A running broadcast never learned.** `broadcasts.cctv_url` is a copy of
//!    the address taken when the camera was picked. Re-import writes a fresh one
//!    onto the `cctv_sources` row, but the copy is what FFmpeg was launched
//!    with, and `Runtime::spawn_now` relaunches `last_args` — so the reconnect
//!    backoff walked 2, 5, 10, 20, 60 seconds onto the same dead address and
//!    then stayed at sixty for ever. The broadcast read as 재접속 중 and the
//!    channel was dark until somebody noticed by hand.
//! 2. **The catalogue kept advertising corpses.** `checked_ok` is a boolean with
//!    no expiry date, and the only thing that resets it is an import that sees
//!    the address *string* change. An import that is not running resets nothing,
//!    so a camera that passed last week was still offered today as verified.
//! 3. **A hand-typed address could be silently replaced.** Resolution prefers
//!    the catalogue row, so a stale link left on a broadcast whose address was
//!    then edited by hand would throw the edit away.

use louver_cloud::cctv::{CctvCheck, NewCctvSource};
use louver_cloud::CloudDb;

/// IP literals, so nothing here resolves a host or touches the network.
const OLD_ADDRESS: &str = "https://93.184.216.34/live/token-yesterday.m3u8";
const NEW_ADDRESS: &str = "https://93.184.216.34/live/token-today.m3u8";
const TYPED_ADDRESS: &str = "https://93.184.216.34/live/mine.m3u8";

fn row(region: &str, name: &str, url: &str) -> NewCctvSource {
    NewCctvSource {
        region: region.into(),
        provider: "한국도로공사".into(),
        name: name.into(),
        url: url.into(),
    }
}

fn ok_check() -> CctvCheck {
    CctvCheck { ok: true, message: "h264 1280x720".into(), ..Default::default() }
}

/// A camera in the catalogue, checked and on offer.
fn camera(db: &CloudDb, region: &str, url: &str) -> String {
    let out = db.import_cctv_sources(&[row(region, "경부선 서울요금소", url)]).unwrap();
    let id = out[0].id.clone().expect("the row was written");
    db.record_cctv_check(&id, &ok_check()).unwrap();
    id
}

/// Move a camera's last check into the past, which is the one thing a test
/// cannot do by waiting.
fn check_aged_hours(db: &CloudDb, id: &str, hours: i64) {
    db.raw()
        .lock()
        .unwrap()
        .execute(
            "UPDATE cctv_sources SET checked_at = datetime('now', ?2) WHERE id = ?1",
            rusqlite::params![id, format!("-{hours} hours")],
        )
        .unwrap();
}

/// Mark a broadcast as meant to be running.
///
/// Written straight to the column rather than through `claim_stream_slot`: this
/// file is about addresses, and dragging a plan, a subscription and an encoder
/// budget into it would mean a test that fails for reasons that are not its
/// subject.
fn wants_to_run(db: &CloudDb, id: &str) {
    db.raw()
        .lock()
        .unwrap()
        .execute("UPDATE broadcasts SET desired_state = 'running' WHERE id = ?1", [id])
        .unwrap();
}

/// A Business account with one broadcast, which is the only plan that may point
/// a broadcast at a camera at all.
fn account_with_broadcast(db: &CloudDb, email: &str) -> louver_cloud::Broadcast {
    let u = db.create_user(email, "hash", "business").unwrap();
    let media = db.create_media(&u.id, "clip.mp4", 1024, "key/clip.mp4").unwrap();
    db.record_media_prepared(&media.id, "key/prepared.mp4", 60.0, 0, 2048).unwrap();
    let dest = db.create_destination(&u.id, "채널", "rtmps://a/live2", "••••").unwrap();
    db.create_broadcast(&u.id, "LIVE", &media.id, &dest.id, true).unwrap()
}

/// A broadcast on `url`, linked to `source_id` when there is one.
///
/// The address and the link go in through the save path, so the test exercises
/// what the form actually sends rather than a shape only it can produce.
fn broadcast_on(db: &CloudDb, email: &str, source_id: Option<&str>, url: &str) -> louver_cloud::Broadcast {
    let b = account_with_broadcast(db, email);
    let patch = louver_cloud::BroadcastPatch {
        cctv_url: Some(url.to_string()),
        cctv_source_id: Some(source_id.unwrap_or("").to_string()),
        ..Default::default()
    };
    db.update_broadcast_owned(&b.user_id, &b.id, &patch).unwrap()
}

// -- 1. a running broadcast follows its camera ---------------------------

#[test]
fn a_broadcast_sends_to_the_address_its_camera_has_now_and_not_the_one_it_was_made_with() {
    let db = CloudDb::open_in_memory().unwrap();
    let id = camera(&db, "KR", OLD_ADDRESS);
    let b = broadcast_on(&db, "biz@x.com", Some(&id), OLD_ADDRESS);

    // Nothing has changed yet, so the two agree.
    assert_eq!(db.resolve_cctv_url(&b).as_deref(), Some(OLD_ADDRESS));

    // Re-import: same camera, new address. This is the one event the whole
    // mechanism exists for, and it does not touch the broadcast row.
    db.import_cctv_sources(&[row("KR", "경부선 서울요금소", NEW_ADDRESS)]).unwrap();

    let b = db.broadcast(&b.id).unwrap();
    assert_eq!(
        b.cctv_url.as_deref(),
        Some(OLD_ADDRESS),
        "the stored snapshot is untouched — it is the record of what the worker was launched with"
    );
    assert_eq!(
        db.resolve_cctv_url(&b).as_deref(),
        Some(NEW_ADDRESS),
        "but what it should be sending to is the camera's address today"
    );
}

#[test]
fn a_running_broadcast_whose_camera_moved_is_listed_for_a_restart_exactly_once() {
    let db = CloudDb::open_in_memory().unwrap();
    let id = camera(&db, "KR", OLD_ADDRESS);
    let b = broadcast_on(&db, "biz@x.com", Some(&id), OLD_ADDRESS);
    wants_to_run(&db, &b.id);

    assert!(db.cctv_urls_needing_refresh().unwrap().is_empty(), "nothing has moved");

    db.import_cctv_sources(&[row("KR", "경부선 서울요금소", NEW_ADDRESS)]).unwrap();
    assert_eq!(
        db.cctv_urls_needing_refresh().unwrap(),
        vec![(b.id.clone(), NEW_ADDRESS.to_string())],
        "one broadcast, one fresh address"
    );

    // Adopting the address is what the restart does, and it is what stops the
    // tick restarting the same broadcast every thirty seconds for ever.
    db.adopt_cctv_url(&b.id, NEW_ADDRESS).unwrap();
    assert!(
        db.cctv_urls_needing_refresh().unwrap().is_empty(),
        "once adopted the broadcast is not restarted again for the same change"
    );
}

#[test]
fn a_stopped_broadcast_is_not_restarted_for_a_moved_camera() {
    let db = CloudDb::open_in_memory().unwrap();
    let id = camera(&db, "KR", OLD_ADDRESS);
    let b = broadcast_on(&db, "biz@x.com", Some(&id), OLD_ADDRESS);
    // desired_state is `stopped`, which is what `create_broadcast` leaves.
    db.import_cctv_sources(&[row("KR", "경부선 서울요금소", NEW_ADDRESS)]).unwrap();

    assert!(
        db.cctv_urls_needing_refresh().unwrap().is_empty(),
        "a broadcast nobody asked to run has nothing to interrupt; it resolves the address when it starts"
    );
    // It still gets today's address the moment it does start.
    assert_eq!(db.resolve_cctv_url(&db.broadcast(&b.id).unwrap()).as_deref(), Some(NEW_ADDRESS));
}

#[test]
fn a_retired_camera_does_not_take_a_paying_broadcast_off_the_air() {
    let db = CloudDb::open_in_memory().unwrap();
    let id = camera(&db, "KR", OLD_ADDRESS);
    let b = broadcast_on(&db, "biz@x.com", Some(&id), OLD_ADDRESS);
    wants_to_run(&db, &b.id);

    db.set_cctv_source_active(&id, false).unwrap();

    let b = db.broadcast(&b.id).unwrap();
    assert_eq!(
        db.resolve_cctv_url(&b).as_deref(),
        Some(OLD_ADDRESS),
        "withdrawing a camera from the catalogue is not a reason to stop a broadcast: \
         the snapshot is the last address known to have worked"
    );
    assert!(
        db.cctv_urls_needing_refresh().unwrap().is_empty(),
        "and it is certainly not a reason to restart one"
    );
}

#[test]
fn a_broadcast_whose_camera_was_deleted_keeps_the_address_it_had() {
    let db = CloudDb::open_in_memory().unwrap();
    let id = camera(&db, "KR", OLD_ADDRESS);
    let b = broadcast_on(&db, "biz@x.com", Some(&id), OLD_ADDRESS);
    db.delete_cctv_source(&id).unwrap();

    let b = db.broadcast(&b.id).unwrap();
    assert_eq!(db.resolve_cctv_url(&b).as_deref(), Some(OLD_ADDRESS));
}

// -- 3. a typed address is the customer's, not the catalogue's -----------

#[test]
fn an_address_typed_by_hand_is_never_replaced_by_a_camera() {
    let db = CloudDb::open_in_memory().unwrap();
    camera(&db, "KR", OLD_ADDRESS);
    // No source id: this is what the form stores for a typed address.
    let b = broadcast_on(&db, "biz@x.com", None, TYPED_ADDRESS);
    wants_to_run(&db, &b.id);

    db.import_cctv_sources(&[row("KR", "경부선 서울요금소", NEW_ADDRESS)]).unwrap();

    assert_eq!(db.resolve_cctv_url(&db.broadcast(&b.id).unwrap()).as_deref(), Some(TYPED_ADDRESS));
    assert!(
        db.cctv_urls_needing_refresh().unwrap().is_empty(),
        "nobody refreshes an address its owner typed"
    );
}

#[test]
fn a_broadcast_with_no_live_source_resolves_to_nothing() {
    let db = CloudDb::open_in_memory().unwrap();
    let b = account_with_broadcast(&db, "plain@x.com");
    assert_eq!(db.resolve_cctv_url(&b), None, "an ordinary broadcast takes its picture from the playlist");
}

// -- 2. the catalogue stops advertising corpses --------------------------

#[test]
fn a_korean_camera_checked_too_long_ago_is_no_longer_offered() {
    let db = CloudDb::open_in_memory().unwrap();
    let id = camera(&db, "KR", OLD_ADDRESS);
    assert_eq!(db.cctv_sources_for_pick().unwrap().len(), 1, "checked just now");

    // Inside the window: a daily re-import takes about three hours to walk the
    // cameras, so yesterday's run finishing late must not blink the list out.
    check_aged_hours(&db, &id, 30);
    assert_eq!(db.cctv_sources_for_pick().unwrap().len(), 1, "30 hours is one ordinary day plus its run");

    // Past it: the address this row carries almost certainly stopped working,
    // and `checked_ok` is still 1 because nothing resets it.
    check_aged_hours(&db, &id, 40);
    assert!(
        db.cctv_sources_for_pick().unwrap().is_empty(),
        "a camera last opened 40 hours ago is not evidence of a working address"
    );
    assert_eq!(
        db.cctv_sources_all().unwrap().len(),
        1,
        "still in the table, because the operator console is where the failure has to be visible"
    );
    assert_eq!(db.cctv_stale_counts().unwrap(), vec![("KR".to_string(), 1)]);
}

#[test]
fn an_american_camera_is_not_hidden_by_age() {
    let db = CloudDb::open_in_memory().unwrap();
    let id = camera(&db, "US", OLD_ADDRESS);
    // The American feeds are public HLS with no token, and they are re-checked
    // by hand — so a window applied to them would empty the list rather than
    // keep it honest.
    check_aged_hours(&db, &id, 24 * 30);
    assert_eq!(
        db.cctv_sources_for_pick().unwrap().len(),
        1,
        "an address with no expiry does not go stale just because nobody looked"
    );
    assert!(db.cctv_stale_counts().unwrap().is_empty(), "and it is not counted as hidden");
}

#[test]
fn a_fresh_check_puts_a_hidden_korean_camera_back_on_offer() {
    let db = CloudDb::open_in_memory().unwrap();
    let id = camera(&db, "KR", OLD_ADDRESS);
    check_aged_hours(&db, &id, 40);
    assert!(db.cctv_sources_for_pick().unwrap().is_empty());

    // Which is all the daily re-import has to do for the list to come back.
    db.record_cctv_check(&id, &ok_check()).unwrap();
    assert_eq!(db.cctv_sources_for_pick().unwrap().len(), 1);
    assert!(db.cctv_stale_counts().unwrap().is_empty());
}
