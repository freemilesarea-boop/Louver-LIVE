//! The camera catalogue: what gets in, and what gets offered.
//!
//! Two separate questions, and the difference is the point of the table. An
//! import is somebody else's list — a published feed, a spreadsheet, an export
//! from a traffic authority. Getting into the table means the address passed
//! the same rules a customer's typed one does. Being *offered* means more than
//! that: this server opened it and saw video.

use louver_cloud::cctv::{CctvCheck, ImportOutcome, NewCctvSource, MAX_IMPORT_ROWS};
use louver_cloud::{CloudDb, CloudError};

fn row(region: &str, name: &str, url: &str) -> NewCctvSource {
    NewCctvSource {
        region: region.into(),
        provider: "한국도로공사".into(),
        name: name.into(),
        url: url.into(),
    }
}

/// IP literals, so nothing here resolves a host or touches the network.
const CAM_A: &str = "https://93.184.216.34/live/a.m3u8";
const CAM_B: &str = "https://93.184.216.34/live/b.m3u8";

fn ok_check() -> CctvCheck {
    CctvCheck { ok: true, message: "h264 1280x720".into(), ..Default::default() }
}

fn failed_check() -> CctvCheck {
    CctvCheck { ok: false, message: "열 수 없습니다".into(), ..Default::default() }
}

fn only(outcomes: &[ImportOutcome]) -> &ImportOutcome {
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    &outcomes[0]
}

#[test]
fn an_imported_camera_is_not_offered_until_this_server_has_opened_it() {
    let db = CloudDb::open_in_memory().unwrap();
    let out = db.import_cctv_sources(&[row("kr", "경부선 서울요금소", CAM_A)]).unwrap();
    let id = only(&out).id.clone().expect("the row was written");
    assert!(only(&out).ok);

    // In the table, and not on offer: nobody has opened it.
    assert_eq!(db.cctv_sources_all().unwrap().len(), 1);
    assert!(db.cctv_sources_for_pick().unwrap().is_empty(), "an unchecked camera is a claim, not a camera");

    db.record_cctv_check(&id, &ok_check()).unwrap();
    let pick = db.cctv_sources_for_pick().unwrap();
    assert_eq!(pick.len(), 1);
    assert_eq!(pick[0].name, "경부선 서울요금소");
    assert_eq!(pick[0].region, "KR", "the code is stored upper case whatever was typed");
    assert!(pick[0].checked_ok);
}

#[test]
fn a_camera_this_server_could_not_open_is_never_offered() {
    let db = CloudDb::open_in_memory().unwrap();
    let out = db.import_cctv_sources(&[row("KR", "안 열리는 카메라", CAM_A)]).unwrap();
    let id = only(&out).id.clone().unwrap();

    db.record_cctv_check(&id, &failed_check()).unwrap();
    assert!(db.cctv_sources_for_pick().unwrap().is_empty());
    // The operator can still see it, and why.
    let all = db.cctv_sources_all().unwrap();
    assert_eq!(all[0].check_note.as_deref(), Some("열 수 없습니다"));
    assert!(all[0].checked_at.is_some());
}

/// The list is not a way around the address rules.
#[test]
fn an_import_cannot_store_an_address_the_form_would_refuse() {
    let db = CloudDb::open_in_memory().unwrap();
    let out = db
        .import_cctv_sources(&[
            row("KR", "유튜브 링크", "https://www.youtube.com/watch?v=abc"),
            row("KR", "내부망", "http://127.0.0.1:8080/api/me"),
            row("KR", "rtsp", "rtsp://93.184.216.34/live"),
            row("KR", "멀쩡한 카메라", CAM_A),
        ])
        .unwrap();

    assert_eq!(out.len(), 4, "every row is reported, good or bad");
    assert!(!out[0].ok && out[0].message.contains("YouTube"));
    assert!(!out[1].ok, "{}", out[1].message);
    assert!(!out[2].ok && out[2].message.contains("rtsp"));
    assert!(out[3].ok, "a bad row must not take the good ones down with it");
    assert_eq!(db.cctv_sources_all().unwrap().len(), 1);
}

#[test]
fn a_region_that_is_not_a_country_code_is_refused() {
    let db = CloudDb::open_in_memory().unwrap();
    let out = db.import_cctv_sources(&[row("서울", "이름", CAM_A)]).unwrap();
    assert!(!only(&out).ok);
    assert!(only(&out).message.contains("국가 코드"), "{}", only(&out).message);
}

#[test]
fn importing_the_same_list_again_updates_the_row_and_forgets_the_old_check() {
    let db = CloudDb::open_in_memory().unwrap();
    let id = only(&db.import_cctv_sources(&[row("KR", "옛 이름", CAM_A)]).unwrap()).id.clone().unwrap();
    db.record_cctv_check(&id, &ok_check()).unwrap();
    assert_eq!(db.cctv_sources_for_pick().unwrap().len(), 1);

    // Same address, new name: one row, and no longer offered until it is
    // opened again — the row that was checked is not quite this row.
    let again = db.import_cctv_sources(&[row("KR", "새 이름", CAM_A)]).unwrap();
    assert_eq!(only(&again).id.as_deref(), Some(id.as_str()), "no duplicate row");
    assert_eq!(db.cctv_sources_all().unwrap().len(), 1);
    assert_eq!(db.cctv_sources_all().unwrap()[0].name, "새 이름");
    assert!(db.cctv_sources_for_pick().unwrap().is_empty(), "the check belonged to the old row");
}

#[test]
fn retiring_a_camera_hides_it_without_losing_why_a_broadcast_broke() {
    let db = CloudDb::open_in_memory().unwrap();
    let id = only(&db.import_cctv_sources(&[row("GB", "런던 A40", CAM_B)]).unwrap()).id.clone().unwrap();
    db.record_cctv_check(&id, &ok_check()).unwrap();
    assert_eq!(db.cctv_sources_for_pick().unwrap().len(), 1);

    db.set_cctv_source_active(&id, false).unwrap();
    assert!(db.cctv_sources_for_pick().unwrap().is_empty());
    assert_eq!(db.cctv_sources_all().unwrap().len(), 1, "the row stays, so the question stays answerable");

    db.set_cctv_source_active(&id, true).unwrap();
    assert_eq!(db.cctv_sources_for_pick().unwrap().len(), 1);
}

#[test]
fn a_runaway_import_is_refused_rather_than_written() {
    let db = CloudDb::open_in_memory().unwrap();
    let many: Vec<NewCctvSource> = (0..MAX_IMPORT_ROWS + 1)
        .map(|i| row("KR", "c", &format!("https://93.184.216.34/live/{i}.m3u8")))
        .collect();
    assert!(matches!(db.import_cctv_sources(&many), Err(CloudError::Invalid(_))));
    assert!(db.cctv_sources_all().unwrap().is_empty(), "nothing may be written");
}

#[test]
fn checking_or_retiring_a_camera_that_is_not_there_says_so() {
    let db = CloudDb::open_in_memory().unwrap();
    assert!(matches!(db.record_cctv_check("nope", &ok_check()), Err(CloudError::NotFound(_))));
    assert!(matches!(db.set_cctv_source_active("nope", false), Err(CloudError::NotFound(_))));
    assert!(matches!(db.delete_cctv_source("nope"), Err(CloudError::NotFound(_))));
}

/// A camera is its name, not its address.
///
/// Korea's ITS hands out a tokenised URL that expires in about a day, so the
/// same camera arrives with a different address on every import. Matching on
/// the address made each of those a new row: the catalogue grew by thousands
/// of rows a day and the picker offered every camera twice.
#[test]
fn the_same_camera_at_a_new_address_updates_the_row_it_already_has() {
    let db = CloudDb::open_in_memory().unwrap();
    let first = only(&db.import_cctv_sources(&[row("KR", "한강대교", CAM_A)]).unwrap())
        .id
        .clone()
        .expect("imported");
    db.record_cctv_check(&first, &ok_check()).unwrap();
    assert_eq!(db.cctv_sources_for_pick().unwrap().len(), 1);

    // Tomorrow's token for the same camera.
    let again = only(&db.import_cctv_sources(&[row("KR", "한강대교", CAM_B)]).unwrap())
        .id
        .clone()
        .expect("imported");
    assert_eq!(again, first, "the same camera must keep its row");

    let all = db.cctv_sources_all().unwrap();
    assert_eq!(all.len(), 1, "one camera, one row: {all:?}");
    assert_eq!(all[0].url, CAM_B, "the row must now hold today's address");
    // And the move resets the check, so yesterday's success cannot offer an
    // address this server has not opened.
    assert!(db.cctv_sources_for_pick().unwrap().is_empty());
}

/// A different camera with a different name is still a different camera.
#[test]
fn two_cameras_under_one_provider_do_not_collapse_into_one() {
    let db = CloudDb::open_in_memory().unwrap();
    db.import_cctv_sources(&[row("KR", "한강대교", CAM_A), row("KR", "성남요금소", CAM_B)])
        .unwrap();
    assert_eq!(db.cctv_sources_all().unwrap().len(), 2);
}

/// Cleaning up the rows the old rule already created.
#[test]
fn deduping_keeps_the_newest_copy_of_each_camera() {
    let db = CloudDb::open_in_memory().unwrap();
    // Built by hand, because `import_cctv_sources` can no longer produce the
    // mess this exists to clear: two rows, one camera, two addresses.
    let keep = {
        let conn = db.raw();
        let guard = conn.lock().unwrap();
        for (id, url, added) in [
            ("old", CAM_A, "2026-10-29 00:00:00"),
            ("new", CAM_B, "2026-10-30 00:00:00"),
        ] {
            guard
                .execute(
                    "INSERT INTO cctv_sources (id, region, provider, name, url, added_at)
                     VALUES (?1, 'KR', '한국도로공사', '한강대교', ?2, ?3)",
                    rusqlite::params![id, url, added],
                )
                .unwrap();
        }
        "new"
    };
    assert_eq!(db.cctv_sources_all().unwrap().len(), 2);

    assert_eq!(db.dedupe_cctv_sources().unwrap(), 1, "one copy removed");
    let left = db.cctv_sources_all().unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, keep, "the newest copy is the one holding today's URL");

    // Idempotent: nothing left to remove.
    assert_eq!(db.dedupe_cctv_sources().unwrap(), 0);
}
