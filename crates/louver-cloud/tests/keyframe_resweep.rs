//! Fixing a video that is already on air.
//!
//! The keyframe guard runs at import, and until the measurement was fixed it
//! could not see the worst files at all: one keyframe inside the probe window
//! read as "not measured", which the planner took as no objection, so a file
//! with a keyframe every two minutes was stream-copied to YouTube exactly as it
//! arrived. YouTube cuts HLS segments on keyframes; two minutes apart, a viewer
//! gets play, stall, buffer, play.
//!
//! Those files are Ready, in playlists, and broadcasting right now. Fixing the
//! guard does nothing for them — nothing asks about a Ready file again, and a
//! 24/7 broadcast has no next start to pick a new file up at. Two things close
//! that, and both are pinned here:
//!
//! 1. every file on a stream-copy path is measured once per start, and one
//!    outside the limit goes back into the preparation queue;
//! 2. when a preparation moves the pointer, the broadcasts playing that file
//!    are flagged, and the scheduler restarts them so the new file reaches air.

use louver_cloud::db::NewItem;
use louver_cloud::{CloudDb, MediaState};

fn user(db: &CloudDb, email: &str) -> String {
    db.create_user(email, "hash", "business").unwrap().id
}

/// A Ready media with a prepared pointer and a recorded mode.
///
/// The mode is written directly because the real writer is the encoder, and
/// these tests are about which rows are selected, not about bytes.
fn ready_media(db: &CloudDb, user: &str, name: &str, mode: &str) -> String {
    let m = db.create_media(user, name, 1024, &format!("src/{name}")).unwrap();
    db.record_media_prepared(&m.id, &format!("prep/{name}"), 60.0, 1024, 2048).unwrap();
    db.raw()
        .lock()
        .unwrap()
        .execute("UPDATE media SET prepared_mode=?2 WHERE id=?1", rusqlite::params![&m.id, mode])
        .unwrap();
    m.id
}

/// The prepared shape `playlist_shapes` compares. Written directly for the same
/// reason as the mode: the real writer is a probe of a file that does not exist
/// in these tests.
fn signature(db: &CloudDb, id: &str, sig: &str) {
    db.raw()
        .lock()
        .unwrap()
        .execute("UPDATE media SET prepared_signature=?2 WHERE id=?1", rusqlite::params![id, sig])
        .unwrap();
}

fn broadcast_on(db: &CloudDb, user: &str, title: &str, media_id: &str) -> String {
    let dest = db.create_destination(user, "채널", "rtmps://a/live2", "••••").unwrap();
    db.create_broadcast(user, title, media_id, &dest.id, true).unwrap().id
}

// -- 1. which files are worth measuring ---------------------------------

/// Only the copy paths. `-c copy` hands the file's own keyframe spacing
/// straight to YouTube; anything that went through an encode came out with a
/// regular spacing by construction.
#[test]
fn only_the_stream_copy_paths_are_measured() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user(&db, "a@x.com");
    let direct = ready_media(&db, &u, "direct.mp4", "direct");
    let hybrid = ready_media(&db, &u, "hybrid.mp4", "hybrid");
    ready_media(&db, &u, "canonical.mp4", "canonical");
    ready_media(&db, &u, "live.mp4", "live_normalize");

    let ids: Vec<String> = db.copy_path_media().unwrap().into_iter().map(|(id, ..)| id).collect();
    assert!(ids.contains(&direct), "direct 은 원본 키프레임이 그대로 나갑니다");
    assert!(ids.contains(&hybrid), "hybrid 도 그림은 복사입니다");
    assert_eq!(ids.len(), 2, "인코딩을 거친 파일은 다시 볼 필요가 없습니다: {ids:?}");
}

/// The sweep's log line has to name the customer and the file, because that is
/// what turns "20 rows" into something an operator can answer a ticket with.
#[test]
fn the_sweep_can_name_the_customer_and_the_file() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user(&db, "warm@x.com");
    ready_media(&db, &u, "재즈_라이브.mp4", "direct");

    let rows = db.copy_path_media().unwrap();
    assert_eq!(rows.len(), 1);
    let (_, email, filename, prepared) = &rows[0];
    assert_eq!(email, "warm@x.com");
    assert_eq!(filename, "재즈_라이브.mp4");
    assert_eq!(prepared, "prep/재즈_라이브.mp4", "측정할 대상은 방송이 실제로 읽는 파일입니다");
}

/// A row with no recorded mode predates the column, and every such file came
/// out of the one canonical encode. The same reading `playlist_shapes` takes.
#[test]
fn a_file_from_before_the_mode_column_is_left_alone() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user(&db, "a@x.com");
    let m = db.create_media(&u, "legacy.mp4", 1024, "src/legacy.mp4").unwrap();
    db.record_media_prepared(&m.id, "prep/legacy.mp4", 60.0, 1024, 2048).unwrap();

    assert!(
        db.copy_path_media().unwrap().is_empty(),
        "기록 없는 행은 canonical 로 읽습니다 — playlist_shapes 와 같은 해석",
    );
}

/// A file being prepared right now must not be queued a second time: the
/// in-flight guard is memory, and two threads on one file is two encodes.
#[test]
fn a_file_still_being_prepared_is_not_swept_again() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user(&db, "a@x.com");
    let id = ready_media(&db, &u, "busy.mp4", "direct");
    db.set_media_state(&id, MediaState::Preparing).unwrap();

    assert!(db.copy_path_media().unwrap().is_empty(), "ready 인 것만 봅니다");
}

// -- 2. getting the new file on air -------------------------------------

/// The video a broadcast was created with.
#[test]
fn the_broadcast_that_names_the_media_directly_is_flagged() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user(&db, "a@x.com");
    let m = ready_media(&db, &u, "one.mp4", "direct");
    let b = broadcast_on(&db, &u, "LIVE", &m);

    assert_eq!(db.flag_broadcasts_for_restart(&m).unwrap(), vec![b.clone()]);
    let taken = db.take_broadcasts_needing_restart().unwrap();
    assert_eq!(taken, vec![(b, u)]);
}

/// And the video that is merely the third item of a playlist. A broadcast can
/// play a file it was never created with, which is exactly the shape that was
/// broken: the customer's playlist was edited long after the broadcast was made.
#[test]
fn a_broadcast_is_flagged_for_a_video_that_is_only_a_playlist_item() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user(&db, "a@x.com");
    let first = ready_media(&db, &u, "first.mp4", "direct");
    let third = ready_media(&db, &u, "third.mp4", "direct");
    let b = broadcast_on(&db, &u, "LIVE", &first);
    db.replace_items(
        &u,
        &b,
        &[
            NewItem { media_id: first.clone(), enabled: true, repeat_count: 1 },
            NewItem { media_id: third.clone(), enabled: true, repeat_count: 1 },
        ],
    )
    .unwrap();

    assert_eq!(
        db.flag_broadcasts_for_restart(&third).unwrap(),
        vec![b],
        "플레이리스트 안의 영상만 바뀌어도 그 방송은 다시 시작해야 합니다",
    );
}

/// Someone else's broadcast is not restarted because our file moved.
#[test]
fn an_unrelated_broadcast_is_not_restarted() {
    let db = CloudDb::open_in_memory().unwrap();
    let mine = user(&db, "mine@x.com");
    let theirs = user(&db, "theirs@x.com");
    let my_media = ready_media(&db, &mine, "mine.mp4", "direct");
    let their_media = ready_media(&db, &theirs, "theirs.mp4", "direct");
    let my_b = broadcast_on(&db, &mine, "MINE", &my_media);
    broadcast_on(&db, &theirs, "THEIRS", &their_media);

    assert_eq!(db.flag_broadcasts_for_restart(&my_media).unwrap(), vec![my_b]);
}

/// The flag is a one-shot. It is cleared as it is taken, so a restart that
/// fails is reported once instead of retried every thirty seconds for the life
/// of the process.
#[test]
fn the_flag_is_taken_once_and_does_not_come_back() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user(&db, "a@x.com");
    let m = ready_media(&db, &u, "one.mp4", "direct");
    broadcast_on(&db, &u, "LIVE", &m);

    db.flag_broadcasts_for_restart(&m).unwrap();
    assert_eq!(db.take_broadcasts_needing_restart().unwrap().len(), 1);
    assert!(
        db.take_broadcasts_needing_restart().unwrap().is_empty(),
        "두 번째 호출에 또 나오면 30초마다 방송을 다시 시작하게 됩니다",
    );
}

/// Flagging twice before the scheduler gets there is still one restart.
#[test]
fn two_files_of_one_broadcast_moving_is_still_one_restart() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user(&db, "a@x.com");
    let a = ready_media(&db, &u, "a.mp4", "direct");
    let c = ready_media(&db, &u, "c.mp4", "direct");
    let b = broadcast_on(&db, &u, "LIVE", &a);
    db.replace_items(
        &u,
        &b,
        &[
            NewItem { media_id: a.clone(), enabled: true, repeat_count: 1 },
            NewItem { media_id: c.clone(), enabled: true, repeat_count: 1 },
        ],
    )
    .unwrap();

    db.flag_broadcasts_for_restart(&a).unwrap();
    db.flag_broadcasts_for_restart(&c).unwrap();
    assert_eq!(
        db.take_broadcasts_needing_restart().unwrap().len(),
        1,
        "한 방송에 두 파일이 바뀌어도 재시작은 한 번입니다",
    );
}

/// Nothing to do is not an error, and must not restart anything.
#[test]
fn nothing_flagged_means_nothing_restarted() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user(&db, "a@x.com");
    let m = ready_media(&db, &u, "one.mp4", "direct");
    broadcast_on(&db, &u, "LIVE", &m);

    assert!(db.take_broadcasts_needing_restart().unwrap().is_empty());
}

// -- 3. and never taking a broadcast down -------------------------------

/// The hazard the restart guard exists for.
///
/// `live_normalize` keeps the file's own size and frame rate, so a fixed item
/// does not come out looking like the unfixed ones beside it. `start` refuses a
/// playlist whose items cannot be joined — so restarting a mixed playlist after
/// fixing one item would turn a stream that was buffering into one that is off
/// the air.
#[test]
fn a_fixed_item_beside_an_unfixed_one_cannot_be_joined() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user(&db, "a@x.com");
    let fixed = ready_media(&db, &u, "fixed.mp4", "live_normalize");
    let untouched = ready_media(&db, &u, "untouched.mp4", "direct");
    signature(&db, &fixed, "h264/1920x1080/yuv420p/2/1/1/90000/CRC32:aaaa/aac/2ch@48000");
    signature(&db, &untouched, "h264/1280x720/yuv420p/2/1/1/90000/CRC32:bbbb/aac/2ch@48000");
    let b = broadcast_on(&db, &u, "LIVE", &fixed);
    db.replace_items(
        &u,
        &b,
        &[
            NewItem { media_id: fixed, enabled: true, repeat_count: 1 },
            NewItem { media_id: untouched, enabled: true, repeat_count: 1 },
        ],
    )
    .unwrap();

    assert!(
        db.check_playlist_joinable(&b).is_err(),
        "고친 항목과 안 고친 항목이 이어붙여진다면 재시작 가드는 필요 없습니다",
    );
}

/// And the case that does restart cleanly, which is most of them: one video.
#[test]
fn a_single_item_playlist_restarts_cleanly_after_a_fix() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = user(&db, "a@x.com");
    let only = ready_media(&db, &u, "only.mp4", "live_normalize");
    signature(&db, &only, "h264/1280x720/yuv420p/2/1/1/90000/CRC32:cccc/aac/2ch@48000");
    let b = broadcast_on(&db, &u, "LIVE", &only);

    db.check_playlist_joinable(&b).expect("영상 하나짜리는 언제나 이어붙일 수 있습니다");
}
