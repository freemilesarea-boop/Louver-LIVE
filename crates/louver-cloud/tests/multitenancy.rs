//! §15 and §19: one user must not be able to touch another's anything.
//!
//! The threat is not a clever attack — it is an ordinary IDOR, where knowing an
//! id is enough. Every read and write in the API layer goes through a `*_owned`
//! function that filters on `user_id` **in SQL**, rather than fetching the row
//! and comparing afterwards, because the second style is one forgotten `if`
//! away from a leak. These tests hold that line.

use louver_cloud::{CloudDb, CloudError, Storage};

struct Tenant {
    id: String,
    media: String,
    dest: String,
    broadcast: String,
}

fn tenant(db: &CloudDb, email: &str) -> Tenant {
    let u = db.create_user(email, "hash", "business").unwrap();
    let m = db.create_media(&u.id, "clip.mp4", 1024, &format!("{}/clip.mp4", u.id)).unwrap();
    db.record_media_prepared(&m.id, &format!("{}/prepared.mp4", u.id), 60.0, 2048).unwrap();
    let d = db.create_destination(&u.id, "채널", "rtmps://a/live2", "••••").unwrap();
    let b = db.create_broadcast(&u.id, "LIVE", &m.id, &d.id, true).unwrap();
    Tenant { id: u.id, media: m.id, dest: d.id, broadcast: b.id }
}

fn forbidden_or_missing<T: std::fmt::Debug>(r: louver_cloud::Result<T>, what: &str) {
    match r {
        Err(CloudError::NotFound(_)) | Err(CloudError::Forbidden) => {}
        other => panic!("{what} was reachable across tenants: {other:?}"),
    }
}

#[test]
fn a_user_cannot_read_another_users_rows_even_knowing_every_id() {
    let db = CloudDb::open_in_memory().unwrap();
    let a = tenant(&db, "a@x.com");
    let b = tenant(&db, "b@x.com");

    forbidden_or_missing(db.media_owned(&b.id, &a.media), "media");
    forbidden_or_missing(db.destination_owned(&b.id, &a.dest), "destination");
    forbidden_or_missing(db.broadcast_owned(&b.id, &a.broadcast), "broadcast");
    forbidden_or_missing(db.events_owned(&b.id, &a.broadcast, 50), "logs");

    // And each still sees their own.
    assert_eq!(db.media_owned(&a.id, &a.media).unwrap().id, a.media);
    assert_eq!(db.broadcast_owned(&b.id, &b.broadcast).unwrap().id, b.broadcast);
}

#[test]
fn a_listing_never_includes_another_tenant() {
    let db = CloudDb::open_in_memory().unwrap();
    let a = tenant(&db, "a@x.com");
    let b = tenant(&db, "b@x.com");

    let media: Vec<String> = db.media_for(&a.id).unwrap().into_iter().map(|m| m.id).collect();
    assert_eq!(media, vec![a.media.clone()]);
    assert!(!media.contains(&b.media));

    let casts: Vec<String> = db.broadcasts_for(&b.id).unwrap().into_iter().map(|x| x.id).collect();
    assert_eq!(casts, vec![b.broadcast.clone()]);

    let dests: Vec<String> = db.destinations_for(&a.id).unwrap().into_iter().map(|d| d.id).collect();
    assert_eq!(dests, vec![a.dest.clone()]);
}

#[test]
fn a_user_cannot_start_stop_or_delete_another_users_broadcast() {
    let db = CloudDb::open_in_memory().unwrap();
    let a = tenant(&db, "a@x.com");
    let b = tenant(&db, "b@x.com");

    forbidden_or_missing(db.claim_stream_slot(&b.id, &a.broadcast), "start");
    forbidden_or_missing(db.release_stream_slot(&b.id, &a.broadcast), "stop");
    forbidden_or_missing(db.delete_broadcast_owned(&b.id, &a.broadcast), "delete");
    forbidden_or_missing(db.delete_media_owned(&b.id, &a.media), "media delete");
    forbidden_or_missing(db.delete_destination_owned(&b.id, &a.dest), "destination delete");

    // A's broadcast is untouched, and still stopped.
    let still = db.broadcast_owned(&a.id, &a.broadcast).unwrap();
    assert_eq!(still.desired_state, louver_cloud::DesiredState::Stopped);
}

/// Borrowing someone else's media for your own broadcast is the subtler IDOR.
#[test]
fn a_broadcast_cannot_be_built_from_another_users_media_or_destination() {
    let db = CloudDb::open_in_memory().unwrap();
    let a = tenant(&db, "a@x.com");
    let b = tenant(&db, "b@x.com");

    forbidden_or_missing(
        db.create_broadcast(&b.id, "stolen", &a.media, &b.dest, true),
        "another tenant's media",
    );
    forbidden_or_missing(
        db.create_broadcast(&b.id, "stolen", &b.media, &a.dest, true),
        "another tenant's destination",
    );
}

/// §9: what leaves the server is a mask, and the struct has no field to forget.
#[test]
fn a_stream_key_is_not_in_anything_the_api_can_return() {
    let db = CloudDb::open_in_memory().unwrap();
    let u = db.create_user("keys@x.com", "hash", "pro").unwrap();
    const KEY: &str = "abcd-efgh-ijkl-mnop";

    let d = db.create_destination(&u.id, "채널", "rtmps://a/live2", "••••••••").unwrap();
    // The key goes to the sealed store, under this destination's own account.
    let store = louver_cloud::credentials::CredentialStore::new(db.raw(), [3u8; 32]);
    louver_core::security::SecretStore::set(
        &store,
        &louver_cloud::credentials::destination_account(&d.id),
        KEY,
    )
    .unwrap();

    // Anything serialised to a client.
    let json = serde_json::to_string(&db.destination_owned(&u.id, &d.id).unwrap()).unwrap();
    assert!(!json.contains(KEY), "the key is in the destination payload: {json}");
    assert!(json.contains("••••"), "the mask is missing");

    let sub = serde_json::to_string(&db.subscription(&u.id).unwrap()).unwrap();
    assert!(!sub.contains(KEY));

    // And the whole database file, byte for byte, holds no plaintext key.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cloud.db");
    let disk = CloudDb::open(&path).unwrap();
    let u2 = disk.create_user("disk@x.com", "hash", "pro").unwrap();
    let d2 = disk.create_destination(&u2.id, "채널", "rtmps://a/live2", "••••").unwrap();
    let s2 = louver_cloud::credentials::CredentialStore::new(disk.raw(), [4u8; 32]);
    louver_core::security::SecretStore::set(
        &s2,
        &louver_cloud::credentials::destination_account(&d2.id),
        KEY,
    )
    .unwrap();
    drop(s2);
    drop(disk);

    let mut found = false;
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let p = entry.unwrap().path();
        if let Ok(bytes) = std::fs::read(&p) {
            if bytes.windows(KEY.len()).any(|w| w == KEY.as_bytes()) {
                found = true;
            }
        }
    }
    assert!(!found, "the stream key is on disk in the clear");
}

/// Uploads are filed per user, so one tenant's object key cannot name another's.
#[test]
fn storage_keys_are_scoped_to_their_owner() {
    let dir = tempfile::tempdir().unwrap();
    let s = louver_cloud::storage::LocalStorage::new(dir.path().join("media"));
    let src = dir.path().join("in.mp4");
    std::fs::write(&src, b"x").unwrap();
    let key = s.put_file("tenant-a", "clip.mp4", &src).unwrap();
    assert!(key.starts_with("tenant-a/"));
    assert!(!key.contains("tenant-b"));
}
