//! §12: a database written by an earlier release opens, keeps its rows, and works.
//!
//! The schema here is not a description of the old one — it *is* the old one,
//! copied from the release that is running on the production server: no
//! playlist table, no metadata, no settings, no schedule, no `ffmpeg_pid`, no
//! `kind` on a destination. A new binary has to open this file and carry on,
//! because the alternative is a stream key nobody can read any more.

use louver_cloud::{CloudDb, DesiredState, Privacy};
use rusqlite::Connection;

/// Exactly what the first cloud release created.
const ORIGINAL_SCHEMA: &str = r#"
CREATE TABLE plans (id TEXT PRIMARY KEY, label TEXT NOT NULL, limits TEXT NOT NULL);
CREATE TABLE users (
    id TEXT PRIMARY KEY, email TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL, plan_id TEXT NOT NULL REFERENCES plans(id),
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE subscriptions (
    user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    plan_id TEXT NOT NULL REFERENCES plans(id),
    status TEXT NOT NULL DEFAULT 'active',
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE auth_sessions (
    token_hash TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (datetime('now')), expires_at TEXT NOT NULL
);
CREATE TABLE credentials (account TEXT PRIMARY KEY, sealed BLOB NOT NULL);
CREATE TABLE media (
    id TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    filename TEXT NOT NULL, size_bytes INTEGER NOT NULL DEFAULT 0, state TEXT NOT NULL,
    duration_secs REAL NOT NULL DEFAULT 0, width INTEGER NOT NULL DEFAULT 0,
    height INTEGER NOT NULL DEFAULT 0, fps REAL NOT NULL DEFAULT 0,
    video_codec TEXT NOT NULL DEFAULT '', audio_codec TEXT,
    container TEXT NOT NULL DEFAULT '', bitrate_bps INTEGER NOT NULL DEFAULT 0,
    storage_path TEXT NOT NULL, prepared_path TEXT, prepared_duration_secs REAL,
    last_error TEXT, created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE stream_destinations (
    id TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    label TEXT NOT NULL, rtmps_url TEXT NOT NULL, key_masked TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE broadcasts (
    id TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL, media_id TEXT NOT NULL REFERENCES media(id),
    destination_id TEXT NOT NULL REFERENCES stream_destinations(id),
    loop_forever INTEGER NOT NULL DEFAULT 1,
    desired_state TEXT NOT NULL DEFAULT 'stopped',
    runtime_state TEXT NOT NULL DEFAULT 'CREATED',
    restart_count INTEGER NOT NULL DEFAULT 0, last_error TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    started_at TEXT, stopped_at TEXT, last_heartbeat TEXT,
    bytes_sent INTEGER NOT NULL DEFAULT 0, uptime_secs INTEGER NOT NULL DEFAULT 0,
    ffmpeg_exit_code INTEGER
);
CREATE TABLE broadcast_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    broadcast_id TEXT NOT NULL REFERENCES broadcasts(id) ON DELETE CASCADE,
    at TEXT NOT NULL DEFAULT (datetime('now')), level TEXT NOT NULL, message TEXT NOT NULL
);
"#;

/// A database as the production server has it: one account, one video, one
/// destination with a sealed key, one running broadcast.
fn yesterdays_database(path: &std::path::Path) -> (String, Vec<u8>) {
    let c = Connection::open(path).unwrap();
    c.execute_batch(ORIGINAL_SCHEMA).unwrap();
    c.execute(
        "INSERT INTO plans (id, label, limits) VALUES ('business','Business',
         '{\"max_concurrent_streams\":3,\"max_broadcasts\":30,\"max_storage_bytes\":400,\"max_upload_bytes\":32}')",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO users (id, email, password_hash, plan_id)
         VALUES ('u-old','me@example.com','salt:hash','business')",
        [],
    )
    .unwrap();
    c.execute("INSERT INTO subscriptions (user_id, plan_id) VALUES ('u-old','business')", []).unwrap();
    c.execute(
        "INSERT INTO media (id, user_id, filename, state, storage_path, prepared_path,
                            duration_secs, prepared_duration_secs, width, height, fps)
         VALUES ('m-old','u-old','coloriste.mp4','ready','u-old/orig.mp4','u-old/prepared.mp4',
                 5340.0, 5340.0, 1920, 1080, 30.0)",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO stream_destinations (id, user_id, label, rtmps_url, key_masked)
         VALUES ('d-old','u-old','내 채널','rtmps://a.rtmps.youtube.com/live2','••••••••••••')",
        [],
    )
    .unwrap();
    // A sealed blob, byte for byte what the credential store wrote.
    let sealed = vec![9u8, 8, 7, 6, 5, 4, 3, 2, 1, 0, 42, 42, 99, 99];
    c.execute("INSERT INTO credentials (account, sealed) VALUES ('destination:d-old', ?1)", [&sealed])
        .unwrap();
    c.execute(
        "INSERT INTO broadcasts (id, user_id, name, media_id, destination_id, desired_state,
                                 runtime_state, bytes_sent, uptime_secs)
         VALUES ('b-old','u-old','COLORISTE 테스트','m-old','d-old','running','RUNNING',
                 123456789, 4200)",
        [],
    )
    .unwrap();
    ("b-old".to_string(), sealed)
}

#[test]
fn yesterdays_database_opens_and_nothing_is_lost() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cloud.db");
    let (broadcast_id, sealed) = yesterdays_database(&path);

    // The new binary opens the old file. This is the whole test: if this panics,
    // a deployment loses a production database.
    let db = CloudDb::open(&path).unwrap();

    // The rows are still the rows.
    let b = db.broadcast_owned("u-old", &broadcast_id).unwrap();
    assert_eq!(b.name, "COLORISTE 테스트");
    assert_eq!(b.media_id, "m-old");
    assert_eq!(b.destination_id, "d-old");
    assert_eq!(b.desired_state, DesiredState::Running, "it was running and it still is");
    assert_eq!(b.bytes_sent, 123_456_789);
    assert_eq!(b.uptime_secs, 4200);

    // The new columns read as their defaults rather than as an error.
    assert_eq!(b.privacy, Privacy::Private);
    assert_eq!(b.settings, louver_cloud::StreamSettings::default());
    assert!(!b.schedule.enabled);
    assert_eq!(b.ffmpeg_pid, None);

    // The single video became a one-item playlist, so the editor has something
    // to show — and the old column still points at it.
    let items = db.items_owned("u-old", &broadcast_id).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].filename, "coloriste.mp4");
    assert_eq!(items[0].position, 0);
    assert!(items[0].enabled);
    assert_eq!(b.item_count, 1);

    // The title defaults to the name rather than to nothing.
    assert_eq!(b.title, "COLORISTE 테스트");

    // What the worker would play, out of a row written before playlists existed.
    let playlist = db.prepared_items_for(&broadcast_id).unwrap();
    assert_eq!(playlist.len(), 1);
    assert_eq!(playlist[0].filename, "coloriste.mp4");

    // The sealed key is untouched — the same bytes, under the same account name.
    let still: Vec<u8> = db
        .raw()
        .lock()
        .unwrap()
        .query_row("SELECT sealed FROM credentials WHERE account='destination:d-old'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(still, sealed, "a migration must never touch an encrypted value");

    // The destination is what it always was: a pasted stream key.
    let d = db.destination_owned("u-old", "d-old").unwrap();
    assert_eq!(d.rtmps_url, "rtmps://a.rtmps.youtube.com/live2");
    assert_eq!(d.key_masked, "••••••••••••");

    // Recovery still sees it as something that should be running.
    let wanted = db.broadcasts_wanting_to_run().unwrap();
    assert_eq!(wanted.len(), 1);
    assert_eq!(wanted[0].id, broadcast_id);
}

#[test]
fn opening_it_twice_changes_nothing_further() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cloud.db");
    let (broadcast_id, _) = yesterdays_database(&path);

    let first = CloudDb::open(&path).unwrap();
    let item_id = first.items_for(&broadcast_id).unwrap()[0].id.clone();
    drop(first);

    // A restart must not adopt the same broadcast again, or a playlist would
    // grow a copy of its video every time the server came up.
    let second = CloudDb::open(&path).unwrap();
    let items = second.items_for(&broadcast_id).unwrap();
    assert_eq!(items.len(), 1, "the playlist gained an item on reopen");
    assert_eq!(items[0].id, item_id, "the item was replaced rather than kept");
}

#[test]
fn an_edited_playlist_is_not_re_adopted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cloud.db");
    let (broadcast_id, _) = yesterdays_database(&path);
    let db = CloudDb::open(&path).unwrap();

    // Two more videos, and the original moved to the end.
    for (id, name) in [("m-2", "second.mp4"), ("m-3", "third.mp4")] {
        db.raw()
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO media (id, user_id, filename, state, storage_path, prepared_path,
                                    duration_secs, prepared_duration_secs)
                 VALUES (?1,'u-old',?2,'ready','u-old/o.mp4','u-old/p.mp4', 60.0, 60.0)",
                rusqlite::params![id, name],
            )
            .unwrap();
    }
    let wanted = ["m-2", "m-3", "m-old"].map(|m| louver_cloud::db::NewItem {
        media_id: m.into(),
        enabled: true,
        repeat_count: 1,
    });
    db.replace_items("u-old", &broadcast_id, &wanted).unwrap();
    drop(db);

    let reopened = CloudDb::open(&path).unwrap();
    let items = reopened.items_for(&broadcast_id).unwrap();
    assert_eq!(
        items.iter().map(|i| i.media_id.as_str()).collect::<Vec<_>>(),
        ["m-2", "m-3", "m-old"],
        "a restart rearranged someone's playlist"
    );
    // And `media_id` follows the first item, for anything still reading it.
    assert_eq!(reopened.broadcast(&broadcast_id).unwrap().media_id, "m-2");
}
