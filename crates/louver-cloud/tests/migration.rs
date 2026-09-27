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

/// §17: the YouTube schema is added to a production database, not built beside it.
///
/// No DROP, no recreate, no row lost. The check is deliberately structural — the
/// tables and columns are read out of `sqlite_master` and `PRAGMA table_info`
/// rather than inferred from a query succeeding — because a migration that
/// recreated `broadcasts` would pass every behavioural test and still have
/// destroyed the production table on the way.
#[test]
fn the_youtube_schema_is_added_without_disturbing_what_was_there() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cloud.db");
    let (broadcast_id, sealed) = yesterdays_database(&path);

    // What the old file's `broadcasts` table looked like, before.
    let before: Vec<String> = {
        let c = Connection::open(&path).unwrap();
        columns(&c, "broadcasts")
    };

    let db = CloudDb::open(&path).unwrap();
    let conn = db.raw();
    let guard = conn.lock().unwrap();

    // The new tables exist.
    for table in ["youtube_accounts", "oauth_states"] {
        let n: i64 = guard
            .query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1", [table], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 1, "{table} was not created");
    }

    // Every column the old table had is still there, in the same order, and the
    // new ones were appended.
    let after = columns(&guard, "broadcasts");
    assert_eq!(&after[..before.len()], &before[..], "an existing column was moved or dropped");
    for added in ["youtube_account_id", "youtube_broadcast_id", "youtube_stream_id", "youtube_status"] {
        assert!(after.contains(&added.to_string()), "{added} is missing");
    }
    for added in ["kind", "youtube_account_id", "youtube_stream_id"] {
        assert!(columns(&guard, "stream_destinations").contains(&added.to_string()), "{added} is missing");
    }

    // The old rows are where they were.
    let rows: i64 = guard.query_row("SELECT count(*) FROM broadcasts", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 1, "a migration deleted a broadcast");
    let still: Vec<u8> = guard
        .query_row("SELECT sealed FROM credentials WHERE account='destination:d-old'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(still, sealed, "a migration must never touch an encrypted value");
    drop(guard);

    // And the broadcast that was on air reads as a manual RTMPS one, which is
    // what it is: every YouTube field is absent rather than guessed at.
    let b = db.broadcast_owned("u-old", &broadcast_id).unwrap();
    assert!(b.youtube.account_id.is_none());
    assert!(b.youtube.broadcast_id.is_none());
    assert!(b.youtube.status.is_none());
    assert_eq!(
        db.destination_owned("u-old", "d-old").unwrap().kind,
        louver_cloud::DestinationKind::ManualRtmps,
        "a destination written before the column existed is a pasted key, because that is all it could be"
    );

    // Nothing to connect, and asking is not an error.
    assert!(db.youtube_accounts_for("u-old").unwrap().is_empty());
}

fn columns(c: &Connection, table: &str) -> Vec<String> {
    let mut st = c.prepare(&format!("PRAGMA table_info({table})")).unwrap();
    let rows = st.query_map([], |r| r.get::<_, String>(1)).unwrap();
    rows.map(|r| r.unwrap()).collect()
}

/// Signup's columns are added to a production database, and the account that is
/// already using it keeps working.
///
/// The thing this is really protecting: the operator's account on the live server
/// was made before signup asked for a name, so it has none and has never agreed
/// to anything. If reading it required either, they would be locked out of their
/// own broadcasts by a deployment.
#[test]
fn an_account_from_before_signup_asked_for_a_name_keeps_its_row_and_its_session() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cloud.db");
    let (broadcast_id, _) = yesterdays_database(&path);

    // A browser that signed in yesterday and has not signed out.
    let signed_in = louver_cloud::credentials::token_hash("a-token-from-yesterday");
    let before: Vec<String> = {
        let c = Connection::open(&path).unwrap();
        c.execute(
            "INSERT INTO auth_sessions (token_hash, user_id, expires_at)
             VALUES (?1, 'u-old', datetime('now', '+20 days'))",
            [&signed_in],
        )
        .unwrap();
        columns(&c, "users")
    };
    assert!(!before.contains(&"name".to_string()), "the fixture must be the old schema");

    let db = CloudDb::open(&path).unwrap();

    // The columns were appended, and nothing moved.
    let after = columns(&db.raw().lock().unwrap(), "users");
    assert_eq!(&after[..before.len()], &before[..], "an existing column was moved or dropped");
    for added in ["name", "terms_accepted_at", "privacy_accepted_at", "terms_version"] {
        assert!(after.contains(&added.to_string()), "{added} is missing");
    }

    // The account reads, with the new fields absent rather than invented.
    let user = db.user("u-old").unwrap();
    assert_eq!(user.email, "me@example.com");
    assert_eq!(user.plan_id, "business", "the plan it was on is the plan it is on");
    assert_eq!(user.name, None);
    assert_eq!(user.terms_accepted_at, None, "nobody can retroactively have agreed");
    assert_eq!(user.privacy_accepted_at, None);
    assert_eq!(db.user_by_email("me@example.com").unwrap().id, "u-old");

    // Signing in still works: the stored hash is the stored hash.
    let (id, _) = db.password_hash_for("me@example.com").unwrap();
    assert_eq!(id, "u-old");

    // The session it already had is still a session — a migration that logged
    // everybody out would be a migration nobody forgives.
    assert_eq!(db.user_for_token(&signed_in).unwrap(), "u-old");

    // The subscription row survived, and so did the broadcast that was on air.
    assert_eq!(db.subscription("u-old").unwrap().plan_id, "business");
    assert_eq!(db.broadcast_owned("u-old", &broadcast_id).unwrap().name, "COLORISTE 테스트");
    assert_eq!(db.broadcasts_wanting_to_run().unwrap().len(), 1);
}

/// A signup on a migrated database records consent; `create_user` still does not.
#[test]
fn signup_records_consent_and_the_bootstrap_path_still_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cloud.db");
    yesterdays_database(&path);
    let db = CloudDb::open(&path).unwrap();

    let signed_up = db
        .register_user(&louver_cloud::db::Signup {
            name: "홍길동",
            email: "new@example.com",
            password_hash: "salt:hash",
            terms_version: "2026-09-27",
        })
        .unwrap();
    assert_eq!(signed_up.name.as_deref(), Some("홍길동"));
    assert!(signed_up.terms_accepted_at.is_some());
    assert!(signed_up.privacy_accepted_at.is_some());
    // A subscription row came with it, in the same transaction — and it is an
    // unsubscribed one. Signing up no longer grants a paid entitlement.
    let sub = db.subscription(&signed_up.id).unwrap();
    assert_eq!(sub.plan_id, louver_cloud::db::UNSUBSCRIBED_PLAN);
    assert_eq!(sub.status, louver_cloud::db::SUBSCRIPTION_UNSUBSCRIBED);
    assert!(!sub.active);
    assert!(sub.plan.is_none(), "there is no plan to report");

    // The CLI's path is unchanged: no name, and no agreement it has no right to
    // give.
    let bootstrapped = db.create_user("ops@example.com", "salt:hash", "business").unwrap();
    assert_eq!(bootstrapped.name, None);
    assert_eq!(bootstrapped.terms_accepted_at, None);
    let ops = db.subscription(&bootstrapped.id).unwrap();
    assert_eq!(ops.plan_id, "business");
    assert!(ops.active, "the bootstrap path still grants what it is told to");
    assert_eq!(ops.plan.map(|p| p.max_concurrent_streams()), Some(3));
}

/// Pricing is added to a production database without touching what is on it.
///
/// The account this fixture holds is the shape of the operator's: on Business,
/// with a running broadcast, a sealed stream key and a subscription row written
/// before `status` meant anything. It has to come out of the migration on
/// Business, entitled to three streams, with all of it still there.
#[test]
fn a_production_database_gains_prices_and_keeps_every_entitlement_it_had() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cloud.db");
    let (broadcast_id, sealed) = yesterdays_database(&path);

    let before = {
        let c = Connection::open(&path).unwrap();
        // The fixture is the old schema: a label and a bag of limits, no price.
        let cols = columns(&c, "plans");
        assert!(!cols.contains(&"monthly_price_krw".to_string()), "the fixture must predate pricing");
        cols
    };

    let db = CloudDb::open(&path).unwrap();

    // Appended, not rearranged.
    let after = columns(&db.raw().lock().unwrap(), "plans");
    assert_eq!(&after[..before.len()], &before[..], "an existing column was moved or dropped");
    for added in ["monthly_price_krw", "description", "active", "sort_order"] {
        assert!(after.contains(&added.to_string()), "{added} is missing");
    }

    // The operator's account is untouched, and still entitled to three streams.
    let sub = db.subscription("u-old").unwrap();
    assert_eq!(sub.plan_id, "business");
    assert_eq!(sub.status, "active");
    assert!(sub.active, "a production account must not come out of this unsubscribed");
    assert_eq!(sub.plan.as_ref().map(|p| p.max_concurrent_streams()), Some(3));
    assert_eq!(
        db.limit("u-old", louver_cloud::entitlement::MAX_CONCURRENT_STREAMS).unwrap(),
        3,
        "the enforcement path reads the same three"
    );

    // Its plan row, which existed before prices did, now carries one — this is
    // why `seed_plans` updates the presentation columns on conflict rather than
    // doing nothing. Left alone, the pricing page would show ₩0 for ever.
    assert_eq!(db.plan("business").unwrap().monthly_price_krw, 59_900);
    assert_eq!(db.plan("basic").unwrap().monthly_price_krw, 19_900);
    assert_eq!(db.plan("pro").unwrap().monthly_price_krw, 39_900);

    // The limits on those rows were *not* overwritten: an operator may have
    // raised one for a customer, and this fixture's Business row says 3.
    assert_eq!(db.plan("business").unwrap().max_concurrent_streams(), 3);

    // And everything the account owns is where it was.
    assert_eq!(db.broadcast_owned("u-old", &broadcast_id).unwrap().name, "COLORISTE 테스트");
    assert_eq!(db.broadcasts_wanting_to_run().unwrap().len(), 1, "it was running and it still is");
    let still: Vec<u8> = db
        .raw()
        .lock()
        .unwrap()
        .query_row("SELECT sealed FROM credentials WHERE account='destination:d-old'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(still, sealed, "a migration must never touch an encrypted value");
    assert_eq!(db.items_owned("u-old", &broadcast_id).unwrap().len(), 1);
}
