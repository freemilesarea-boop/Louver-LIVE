//! Upload, look, prepare. §8.
//!
//! The rule is the one the desktop already follows and measured: decide with
//! `ffprobe`, then do only what is needed. A source already matching the
//! broadcast profile is remuxed at about 119x realtime; one that is not is
//! encoded at about 3.45x. Either way it happens **here**, once, at upload —
//! never while a broadcast is on air, which is what keeps a server's CPU free
//! enough to run three streams at once.
//!
//! None of the deciding or the doing is written here. `plan_for` and
//! `normalize_one` are the desktop's, tested against real media, and this calls
//! them.

use crate::db::CloudDb;
use crate::storage::Storage;
use crate::{CloudError, Result};
use louver_core::config::OutputProfile;
use louver_core::media::cache::MediaCache;
use louver_core::media::normalize::{normalize_one, CancelToken};
use louver_core::media::probe::probe;
use louver_core::streaming::ffmpeg::{FfmpegCommandBuilder, FfmpegTools};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// The profile everything is prepared into.
pub const CLOUD_PROFILE: OutputProfile = OutputProfile::P1080p30;

/// Turns an uploaded file into a broadcastable one.
#[derive(Clone)]
pub struct Ingest {
    db: CloudDb,
    storage: Arc<dyn Storage>,
    tools: FfmpegTools,
    encoder: String,
    /// Media ids being prepared right now.
    ///
    /// Two preparations of one upload at the same time do not merely waste a
    /// core: the second files the first one's output away mid-run and both die
    /// with "no such file". A retry after one finishes is fine; an overlap is
    /// not, so the second caller is told the work is already under way.
    in_flight: Arc<Mutex<HashSet<String>>>,
}

impl std::fmt::Debug for Ingest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ingest")
    }
}

impl Ingest {
    pub fn new(db: CloudDb, storage: Arc<dyn Storage>, tools: FfmpegTools, encoder: String) -> Self {
        Self { db, storage, tools, encoder, in_flight: Arc::new(Mutex::new(HashSet::new())) }
    }

    /// Store a file that has finished uploading, then analyse and prepare it.
    ///
    /// Returns as soon as the row exists, the way the desktop's add does, and
    /// hands the rest to a thread. A 90-minute source takes half a second to
    /// remux per minute of video and nobody should hold an HTTP connection open
    /// for it.
    pub fn accept_upload(
        &self,
        user_id: &str,
        filename: &str,
        temp_file: &std::path::Path,
    ) -> Result<crate::CloudMedia> {
        let size = std::fs::metadata(temp_file)?.len() as i64;
        self.db.check_upload_allowed(user_id, size)?;

        let key = self.storage.put_file(user_id, filename, temp_file)?;
        let media = self.db.create_media(user_id, filename, size, &key)?;

        let this = self.clone();
        let id = media.id.clone();
        std::thread::spawn(move || {
            if let Err(e) = this.prepare(&id) {
                let _ = this.db.record_media_failed(&id, &e.to_string());
            }
        });
        Ok(media)
    }

    /// Analyse, then remux or encode into the broadcast profile.
    ///
    /// Synchronous and public, so a retry can be driven from anywhere. It
    /// returns immediately when this media is already being prepared, which is
    /// what makes a retry safe to call without knowing whether the upload's own
    /// thread is still working.
    pub fn prepare(&self, media_id: &str) -> Result<()> {
        // Claim this media, or leave it to whoever has it.
        {
            let mut busy = self.in_flight.lock().unwrap();
            if !busy.insert(media_id.to_string()) {
                return Ok(());
            }
        }
        let done = Claim { set: Arc::clone(&self.in_flight), id: media_id.to_string() };
        let outcome = self.prepare_now(media_id);
        drop(done);
        outcome
    }

    fn prepare_now(&self, media_id: &str) -> Result<()> {
        let builder =
            FfmpegCommandBuilder::new(self.tools.clone(), CLOUD_PROFILE).with_encoder(self.encoder.clone());

        let row = self
            .db
            .raw()
            .lock()
            .unwrap()
            .query_row("SELECT user_id, storage_path FROM media WHERE id=?1", [media_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|_| CloudError::NotFound("media"))?;
        let (user_id, key) = row;

        self.db.set_media_state(media_id, crate::MediaState::Analysing)?;
        let local = self.storage.localize(&key)?;
        let info = probe(&builder, &local)?;
        self.db.record_media_analysis(media_id, &info)?;

        // Prepared output goes beside the store, then gets filed like any other
        // object, so an S3 backend uploads it rather than leaving it on a disk.
        let scratch = self.storage.scratch_dir().join(media_id);
        std::fs::create_dir_all(&scratch)?;
        let cache = MediaCache::new(&scratch);

        let out = normalize_one(
            &builder,
            &cache,
            &local,
            media_id,
            &info,
            CLOUD_PROFILE,
            &CancelToken::new(),
            |_| {},
        )?;

        // `normalize_one` makes the plan itself — copy what is already right,
        // encode only what is not — and `out.plan` says which it chose, for
        // §22's costing once there is somewhere to put it.
        debug_assert!(out.plan.label().len() > 2);

        let prepared_key =
            self.storage.put_file(&user_id, &format!("prepared-{media_id}.mp4"), &out.output_path)?;
        let _ = std::fs::remove_dir_all(&scratch);

        let original = self.storage.size_bytes(&key).unwrap_or(0) as i64;
        let prepared = self.storage.size_bytes(&prepared_key).unwrap_or(0) as i64;
        self.db.record_media_prepared(media_id, &prepared_key, out.duration_secs, original + prepared)?;
        Ok(())
    }

    /// What was done, for the log and for §22's costing.
    pub fn describe_plan(&self, media_id: &str) -> Result<String> {
        let m = self
            .db
            .raw()
            .lock()
            .unwrap()
            .query_row("SELECT state FROM media WHERE id=?1", [media_id], |r| r.get::<_, String>(0))
            .map_err(|_| CloudError::NotFound("media"))?;
        Ok(m)
    }
}

/// Releases an in-flight claim however `prepare_now` ends, panic included.
struct Claim {
    set: Arc<Mutex<HashSet<String>>>,
    id: String,
}

impl Drop for Claim {
    fn drop(&mut self) {
        if let Ok(mut busy) = self.set.lock() {
            busy.remove(&self.id);
        }
    }
}
