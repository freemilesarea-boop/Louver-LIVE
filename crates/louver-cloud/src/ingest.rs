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
use louver_core::media::normalize::{
    direct_source_is_broadcastable, estimated_prepared_bytes, normalize_one_with, plan_preparation,
    CancelToken, PrepareMode,
};
use louver_core::media::probe::{probe, probe_max_keyframe_gap, probe_signature, StreamSignature};
use louver_core::streaming::ffmpeg::{FfmpegCommandBuilder, FfmpegTools};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// The profile everything is prepared into.
pub const CLOUD_PROFILE: OutputProfile = OutputProfile::P1080p30;

/// Free space this server will not let an upload eat into.
///
/// A full disk does not degrade this service, it stops it: SQLite cannot write,
/// a prepared file cannot be produced, the container's own log cannot be
/// appended to, and every broadcast that needs any of those dies with it. An
/// upload is the one thing a user can do that consumes a disk quickly, so it is
/// where the floor belongs. Five gigabytes is enough room for the database, the
/// logs and one in-flight preparation to finish and for an operator to react.
pub const DISK_FLOOR_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// How many uploads may be prepared at once.
///
/// Preparation is an FFmpeg remux or, for a source that does not match the
/// profile, a full encode. On the two-core server this runs on, an unbounded
/// thread per upload means a user who selects ten files starts ten encoders and
/// takes the CPU away from the broadcasts that are on air — the one thing that
/// must not happen, because those are what the service is. One at a time keeps
/// a core free; the queue is the price, and it is paid at upload rather than on
/// air.
pub const MAX_PREPARING_AT_ONCE: usize = 1;

/// How much of a live-normalized file to read back when checking its keyframes.
///
/// The encoder was told to force one every two seconds, so a minute is thirty
/// chances to catch it not having done so — and it is an index read rather than
/// a scan of hours of video.
const KEYFRAME_CHECK_SECS: u32 = 60;

/// How often to look at the disk while a conversion is writing to it.
///
/// Often enough that the floor is still there when it trips, rarely enough that
/// a four-hour encode does not stat the volume tens of thousands of times. At
/// the profile's capped 6.2 Mbps an output grows about 4.6MB in this window,
/// which is nothing against a 5GiB floor.
const FLOOR_CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

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
    /// How many preparations may run at once. See [`MAX_PREPARING_AT_ONCE`].
    ///
    /// A condvar rather than a thread pool because the callers are already
    /// threads of their own and all this has to do is make them wait.
    slots: Arc<(Mutex<usize>, std::sync::Condvar)>,
}

impl std::fmt::Debug for Ingest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ingest")
    }
}

impl Ingest {
    pub fn new(db: CloudDb, storage: Arc<dyn Storage>, tools: FfmpegTools, encoder: String) -> Self {
        Self {
            db,
            storage,
            tools,
            encoder,
            in_flight: Arc::new(Mutex::new(HashSet::new())),
            slots: Arc::new((Mutex::new(MAX_PREPARING_AT_ONCE), std::sync::Condvar::new())),
        }
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
        self.check_disk_has_room()?;

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

    /// Free bytes on the volume the store is on, or `None` when that cannot be
    /// read.
    ///
    /// `None` is not zero. An unrecognised mount, or a platform sysinfo has
    /// nothing to say about, means the question was not answered — and refusing
    /// every upload on that basis would be worse than the risk it guards
    /// against. Every caller treats it as "allow".
    fn free_bytes(&self) -> Option<u64> {
        let probe = self.storage.scratch_dir();
        let _ = std::fs::create_dir_all(&probe);
        match louver_core::system::available_disk_bytes(&probe) {
            0 => None,
            n => Some(n),
        }
    }

    /// Is `free` enough to write `additional` bytes and still leave the floor?
    ///
    /// The whole of the policy, as arithmetic, so it can be checked against
    /// numbers a test can choose rather than against whatever disk the test
    /// happens to run on. Every caller below is this function plus a `statvfs`.
    ///
    /// `None` means the volume could not be read, and answers yes — see
    /// [`Ingest::free_bytes`].
    pub fn fits(free: Option<u64>, additional: u64) -> bool {
        match free {
            None => true,
            Some(free) => free >= additional.saturating_add(DISK_FLOOR_BYTES),
        }
    }

    /// Is the volume still above the floor right now?
    ///
    /// For the upload handler, which has to be able to ask this while a body is
    /// still arriving. An unreadable volume answers `true`, for the same reason
    /// [`Self::free_bytes`] returns `None`: a question that could not be asked
    /// is not a refusal.
    pub fn disk_above_floor(&self) -> bool {
        Self::fits(self.free_bytes(), 0)
    }

    /// Is there room to take this upload into the store?
    ///
    /// **The incoming bytes are already on the disk when this runs.** The
    /// handler streams the body to a temp file, and `free` is read after that,
    /// so the N bytes have already been spent and counting them again reserves
    /// them twice. What happens next is a `rename` within one filesystem —
    /// `uploads/` and `media/` are both under the data directory — which moves
    /// a directory entry and no bytes at all.
    ///
    /// So the only thing this stage has to protect is the floor itself.
    ///
    /// What the preparation will cost is a separate question, asked separately
    /// once the file has been probed and there is a real answer to give. The
    /// old formula asked it here, before anything was known, and answered three
    /// times the source — which refused a 20GB upload unless 65GB were free,
    /// on a server with an 80GB disk.
    pub fn check_disk_has_room(&self) -> Result<()> {
        let free = self.free_bytes();
        if !Self::fits(free, 0) {
            let free = free.unwrap_or(0);
            eprintln!(
                "[louver] 업로드 거부: 디스크 여유 {}MB, 최소 확보 {}MB",
                free / 1_048_576,
                DISK_FLOOR_BYTES / 1_048_576,
            );
            return Err(CloudError::OutOfSpace);
        }
        Ok(())
    }

    /// Is there room for what this preparation is about to write?
    ///
    /// Asked after the probe and the plan, which is the first moment there is
    /// anything true to say. `estimated_prepared_bytes` is an upper bound drawn
    /// from the encoder's own rate caps, so a pass here means the output fits
    /// with the floor still standing underneath it.
    ///
    /// Anything already on the disk — the source, and on a re-preparation the
    /// prepared file that is about to be replaced — is inside `free` already
    /// and is not counted again. The old prepared file in particular must not
    /// be: it is not going anywhere, because a broadcast may be reading it.
    fn check_room_to_prepare(&self, media_id: &str, need: u64) -> Result<()> {
        let free = self.free_bytes();
        if !Self::fits(free, need) {
            let free = free.unwrap_or(0);
            let needed = need.saturating_add(DISK_FLOOR_BYTES);
            eprintln!(
                "[louver] media {}: 변환 보류 — 디스크 여유 {}MB, 필요 {}MB (예상 결과물 {}MB + 최소 확보 {}MB)",
                &media_id[..8.min(media_id.len())],
                free / 1_048_576,
                needed / 1_048_576,
                need / 1_048_576,
                DISK_FLOOR_BYTES / 1_048_576,
            );
            return Err(CloudError::OutOfSpace);
        }
        Ok(())
    }

    /// Stop this conversion if the disk falls to the floor while it runs.
    ///
    /// An estimate is an estimate. The one above is an upper bound on what
    /// FFmpeg writes, but it cannot know what else lands on the volume in the
    /// meantime — another upload, a log, a second preparation — so the floor is
    /// watched again while the output grows.
    ///
    /// Cancelling is the whole mechanism: `normalize_one_with` checks the token
    /// between progress lines, kills **its own child** and removes the partial
    /// file. No signal is sent to anything else, so a broadcast's FFmpeg cannot
    /// be touched by this — which is the one thing that must be true of any
    /// code that reacts to a full disk.
    fn floor_watch(&self, media_id: &str) -> (CancelToken, impl Fn(f64) + use<>) {
        let token = CancelToken::new();
        let watcher = token.clone();
        let storage = Arc::clone(&self.storage);
        let id = media_id.to_string();
        let last = Mutex::new(std::time::Instant::now());
        let on_progress = move |_percent: f64| {
            // FFmpeg reports progress several times a second; stat the volume
            // once every few seconds instead.
            {
                let mut last = last.lock().unwrap();
                if last.elapsed() < FLOOR_CHECK_EVERY {
                    return;
                }
                *last = std::time::Instant::now();
            }
            let free = louver_core::system::available_disk_bytes(&storage.scratch_dir());
            if free != 0 && free < DISK_FLOOR_BYTES && !watcher.is_cancelled() {
                eprintln!(
                    "[louver] media {}: 디스크 여유가 {}MB까지 떨어져 변환을 중단합니다",
                    &id[..8.min(id.len())],
                    free / 1_048_576,
                );
                watcher.cancel();
            }
        };
        (token, on_progress)
    }

    /// Report a conversion the floor watch stopped as what it was.
    ///
    /// `normalize_one_with` answers a cancellation with `MediaNormalizeCancelled`,
    /// which is right — it does not know why it was cancelled. Here it is known,
    /// and the user is owed the real reason: the server ran out of room, not
    /// their file. `OutOfSpace` is the contract the rest of the product already
    /// has for that, including the message the browser shows.
    fn out_of_space_if_cancelled(&self, cancel: &CancelToken, e: louver_core::LouverError) -> CloudError {
        if cancel.is_cancelled() {
            CloudError::OutOfSpace
        } else {
            e.into()
        }
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
        // Wait for a turn. Held across the whole of `prepare_now`, so at most
        // `MAX_PREPARING_AT_ONCE` encoders exist at any moment however many
        // files were selected.
        let _slot = Slot::take(Arc::clone(&self.slots));
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

        // The whole policy is in `plan_preparation`. `canonical` is the pinned
        // answer for media that has to match other items in a playlist — see
        // `ensure_playlist_compatible`.
        let pinned = self.db.prepare_target(media_id).unwrap_or_else(|_| "auto".into()) == "canonical";
        let prep = plan_preparation(&builder, &local, &info, CLOUD_PROFILE, pinned);
        // §8: why, in a line an operator can read when a user says it was slow.
        // Nothing here is user content and nothing is a secret.
        println!(
            "[louver] media {}: {} in={}x{}@{:.2}fps/{} audio={}/{}Hz gop={}",
            &media_id[..8.min(media_id.len())],
            prep.summary(),
            info.width,
            info.height,
            info.fps,
            if info.video_codec.is_empty() { "?" } else { &info.video_codec },
            info.audio_codec.clone().unwrap_or_else(|| "none".into()),
            info.audio_sample_rate.unwrap_or(0),
            prep.measured_gop_secs.map(|g| format!("{g:.1}s")).unwrap_or_else(|| "?".into()),
        );

        // Nothing to do to this file, so nothing is done to it.
        //
        // The preparation step exists to produce something a broadcast can
        // read. When the upload already *is* that thing, producing a second
        // copy of it is the whole cost of the step and none of its value: it
        // doubles what the account stores, doubles what the disk holds, and
        // hands the broadcast a file byte-identical to the one beside it.
        //
        // What makes this safe is not that the copy was unnecessary — it is
        // that the signature is still probed, from the file a broadcast will
        // actually open. The playlist compatibility check compares those
        // signatures and knows nothing about where the file came from, so a
        // playlist that mixes this with anything else converges exactly as it
        // did before. See `direct_source_is_broadcastable` for what "already
        // is that thing" means and why the container still has to be ours.
        if direct_source_is_broadcastable(&info, &prep) {
            let bytes = self.storage.size_bytes(&key).unwrap_or(0) as i64;
            // Written before the row is marked ready, for the same reason the
            // conversion path writes it first: the moment a media is ready a
            // broadcast may be built on it, and a ready row with no signature
            // reads as canonical when it is not.
            match self.signature_of(&key) {
                Ok(sig) => {
                    println!(
                        "[louver] media {}: prepared mode=direct {} (원본 그대로 사용, 사본 없음, {:.1}MB)",
                        &media_id[..8.min(media_id.len())],
                        sig.as_key(),
                        bytes as f64 / 1_048_576.0,
                    );
                    self.db.record_prepared_signature(media_id, PrepareMode::Direct.id(), &sig.as_key())?;
                }
                // A file whose shape cannot be read cannot be judged against
                // anything, and the copy would not have helped: the same probe
                // would have failed on the copy. Fall through to the normal
                // path, which is what has always handled a file this odd.
                Err(e) => {
                    eprintln!("[louver] media {media_id}: 원본을 읽지 못했습니다: {e}");
                    return self.prepare_by_conversion(
                        media_id,
                        &user_id,
                        &key,
                        &local,
                        &info,
                        prep,
                        self.storage.size_bytes(&key).unwrap_or(0),
                    );
                }
            }
            // The pointer is the source's own key. `prepared_media_for` needs
            // no special case, the manifest names the file that is there, and
            // nothing was written to the disk by getting here.
            self.retire_replaced_prepared(media_id, &user_id, &key)?;
            self.db.record_media_prepared(media_id, &key, info.duration_secs, bytes, 0)?;
            return Ok(());
        }

        // Everything below writes a file, so the disk is asked first — with a
        // number drawn from the encoder's own rate caps rather than a multiple
        // of the source. The source is already on the disk and is not counted;
        // on a re-preparation neither is the prepared file being replaced,
        // which stays where it is until something is reading it no longer.
        let source_bytes = self.storage.size_bytes(&key).unwrap_or(0);
        let need = estimated_prepared_bytes(&info, &prep, CLOUD_PROFILE, source_bytes);
        self.check_room_to_prepare(media_id, need)?;

        self.prepare_by_conversion(media_id, &user_id, &key, &local, &info, prep, source_bytes)
    }

    /// Produce a broadcastable file from a source that is not one yet.
    ///
    /// Everything from here down is what this step has always done; it moved
    /// into a function of its own so the direct case above could return before
    /// reaching it.
    #[allow(clippy::too_many_arguments)]
    fn prepare_by_conversion(
        &self,
        media_id: &str,
        user_id: &str,
        key: &crate::storage::ObjectKey,
        local: &std::path::Path,
        info: &louver_core::media::probe::MediaInfo,
        mut prep: louver_core::media::normalize::Preparation,
        source_bytes: u64,
    ) -> Result<()> {
        let builder =
            FfmpegCommandBuilder::new(self.tools.clone(), CLOUD_PROFILE).with_encoder(self.encoder.clone());

        // Prepared output goes beside the store, then gets filed like any other
        // object, so an S3 backend uploads it rather than leaving it on a disk.
        //
        // Emptied first: a previous run that failed after writing its cache
        // entry would otherwise be *reused*, and a re-preparation asked for
        // because the playlist needs a different format would quietly return
        // the old one.
        let scratch = self.storage.scratch_dir().join(media_id);
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch)?;
        let cache = MediaCache::new(&scratch);

        let (cancel, on_progress) = self.floor_watch(media_id);
        let mut out = normalize_one_with(
            &builder,
            &cache,
            local,
            media_id,
            info,
            CLOUD_PROFILE,
            Some(&prep),
            &cancel,
            &on_progress,
        )
        .map_err(|e| self.out_of_space_if_cancelled(&cancel, e))?;

        // A live-normalized file exists for one reason: to have keyframes close
        // enough together for a live ingest. Asking the encoder for them is not
        // the same as having them, so the result is measured, and a file that
        // still falls short is redone the canonical way rather than sent.
        if prep.mode == PrepareMode::LiveNormalize {
            let limit = CLOUD_PROFILE.max_copy_gop_secs();
            let got = probe_max_keyframe_gap(&builder, &out.output_path, KEYFRAME_CHECK_SECS);
            if got.is_some_and(|g| g > limit) {
                eprintln!(
                    "[louver] media {}: 키프레임 간격이 여전히 {:.1}초입니다. 표준 변환으로 다시 만듭니다",
                    &media_id[..8.min(media_id.len())],
                    got.unwrap_or_default(),
                );
                let _ = std::fs::remove_dir_all(&scratch);
                std::fs::create_dir_all(&scratch)?;
                let cache = MediaCache::new(&scratch);
                prep = plan_preparation(&builder, local, info, CLOUD_PROFILE, true);
                // A second output, at the canonical rate this time, so the
                // disk is asked again before it is written.
                let need = estimated_prepared_bytes(info, &prep, CLOUD_PROFILE, source_bytes);
                self.check_room_to_prepare(media_id, need)?;
                out = normalize_one_with(
                    &builder,
                    &cache,
                    local,
                    media_id,
                    info,
                    CLOUD_PROFILE,
                    Some(&prep),
                    &cancel,
                    &on_progress,
                )
                .map_err(|e| self.out_of_space_if_cancelled(&cancel, e))?;
            }
        }
        let mode = prep.mode.id();

        // `normalize_one` makes the plan itself — copy what is already right,
        // encode only what is not — and `out.plan` says which it chose, for
        // §22's costing once there is somewhere to put it.
        debug_assert!(out.plan.label().len() > 2);

        let prepared_key =
            match self.storage.put_file(user_id, &format!("prepared-{media_id}.mp4"), &out.output_path) {
                Ok(k) => k,
                Err(e) => {
                    // The scratch copy is the largest thing on the disk at this
                    // moment; leaving it behind on a failure is how a disk fills
                    // up one failed upload at a time.
                    let _ = std::fs::remove_dir_all(&scratch);
                    return Err(e);
                }
            };
        let _ = std::fs::remove_dir_all(&scratch);

        let original = self.storage.size_bytes(key).unwrap_or(0) as i64;
        let prepared = self.storage.size_bytes(&prepared_key).unwrap_or(0) as i64;

        // What the file turned out to be, read from the file rather than from
        // what was asked for. This is what a playlist compares — and it is
        // written *before* the media is marked ready, because the moment it is
        // ready a broadcast may be built on it, and a row that is ready with no
        // signature reads as canonical when it is not.
        match self.signature_of(&prepared_key) {
            Ok(sig) => {
                println!(
                    "[louver] media {}: prepared mode={} {} ({:.1}s, {:.0}x realtime)",
                    &media_id[..8.min(media_id.len())],
                    mode,
                    sig.as_key(),
                    out.duration_secs,
                    out.speed_x,
                );
                self.db.record_prepared_signature(media_id, mode, &sig.as_key())?;
            }
            // A file we cannot read the shape of cannot be judged compatible
            // with anything, so it is recorded as canonical-unknown and the
            // playlist check will send it down the canonical path if it is ever
            // mixed with something else.
            Err(e) => eprintln!("[louver] media {media_id}: 준비된 파일을 읽지 못했습니다: {e}"),
        }
        // Order matters, and this is the order: the new file exists and has
        // been probed, *then* the file it replaces is written off, *then* the
        // pointer moves. Nothing is unlinked at any point. A crash between any
        // two of these steps leaves the row pointing at a file that exists.
        self.retire_replaced_prepared(media_id, user_id, &prepared_key)?;
        self.db.record_media_prepared(media_id, &prepared_key, out.duration_secs, original, prepared)?;
        Ok(())
    }

    /// Write off the file this media used to be broadcast from.
    ///
    /// Called with the object that is about to become the pointer. Whatever the
    /// pointer named before — if it named anything, if it named something else,
    /// and if that something was not the source itself — is recorded in
    /// `storage_trash`.
    ///
    /// **It is not deleted.** This is the exact moment at which deleting is
    /// most dangerous: a re-preparation is started by a playlist that mixes
    /// formats, and a broadcast of that playlist may be on air right now with
    /// the old file open. Unlinking it would keep that broadcast alive on
    /// Linux and kill it at the next restart, when recovery re-opens a path
    /// that no longer exists. Leaving it costs disk; taking it costs the
    /// broadcast. This is where the orphans in production came from — they
    /// were the safe half of this trade, made by accident.
    fn retire_replaced_prepared(
        &self,
        media_id: &str,
        user_id: &str,
        new_key: &crate::storage::ObjectKey,
    ) -> Result<()> {
        let (old, source): (Option<String>, String) = match self.db.raw().lock().unwrap().query_row(
            "SELECT prepared_path, storage_path FROM media WHERE id=?1",
            [media_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ) {
            Ok(v) => v,
            Err(_) => return Ok(()),
        };
        let Some(old) = old else { return Ok(()) };
        // Unchanged pointer: nothing was replaced. And never the source — that
        // is the upload, it is still pointed at by `storage_path`, and a media
        // broadcast directly from it names it here on purpose.
        if old == *new_key || old == source {
            return Ok(());
        }
        let bytes = self.storage.size_bytes(&old).unwrap_or(0) as i64;
        println!(
            "[louver] media {}: 이전 변환본을 정리 대상으로 기록합니다 ({:.1}MB)",
            &media_id[..8.min(media_id.len())],
            bytes as f64 / 1_048_576.0,
        );
        self.db.trash_object(&old, Some(media_id), Some(user_id), "replaced_by_reprepare", bytes)
    }

    /// The exact shape of a prepared file, for the playlist compatibility check.
    fn signature_of(&self, prepared_key: &crate::storage::ObjectKey) -> Result<StreamSignature> {
        let builder =
            FfmpegCommandBuilder::new(self.tools.clone(), CLOUD_PROFILE).with_encoder(self.encoder.clone());
        let local = self.storage.localize(prepared_key)?;
        Ok(probe_signature(&builder, &local)?)
    }

    /// Make every item of a playlist safe to concatenate, re-preparing only what
    /// has to change.
    ///
    /// A playlist is one FFmpeg reading a concat manifest and copying packets
    /// into one RTMP stream. The demuxer joins them; it does not reconcile them.
    /// So the items have to agree on codec, geometry, frame rate, time base and
    /// SPS — and two files that merely look alike are not enough, which is why
    /// the comparison is a probe of the produced files rather than of intent.
    ///
    /// Safe without doing anything when **either**:
    ///
    /// * every item is canonical (including legacy files, which all came out of
    ///   the one canonical encode), or
    /// * every item is native and their signatures are identical — the common
    ///   case of one file, or several exports from the same source.
    ///
    /// Otherwise the native items are pinned to the canonical profile and
    /// re-prepared. That is the expensive path, and it now happens only when a
    /// user actually mixes formats in one playlist rather than for every upload.
    ///
    /// Returns how many media were queued for re-preparation.
    pub fn ensure_playlist_compatible(&self, broadcast_id: &str) -> Result<usize> {
        let shapes = self.db.playlist_shapes(broadcast_id)?;
        if shapes.len() < 2 {
            return Ok(0);
        }
        let all_canonical = shapes.iter().all(|(_, mode, _)| mode == "canonical");
        let first = shapes[0].2.clone();
        // Only the canonical conversion produces files that are interchangeable
        // with each other; `direct`, `hybrid` and `live_normalize` each keep
        // something of the source, so two of those agree only when their
        // signatures do.
        let all_same_native =
            shapes.iter().all(|(_, mode, sig)| mode != "canonical" && *sig == first) && first.is_some();
        if all_canonical || all_same_native {
            return Ok(0);
        }

        let mut queued = 0;
        for (media_id, mode, _) in shapes.iter() {
            if mode == "canonical" {
                continue;
            }
            println!(
                "[louver] playlist {}: media {} 를 표준 형식으로 다시 준비합니다 (형식이 섞여 있습니다)",
                &broadcast_id[..8.min(broadcast_id.len())],
                &media_id[..8.min(media_id.len())],
            );
            self.db.pin_to_canonical(media_id)?;
            // Back to preparing, so nothing starts a broadcast on a file that is
            // about to be replaced. `prepared_media_for` refuses anything that
            // is not ready, which is the gate that already exists.
            self.db.set_media_state(media_id, crate::MediaState::Preparing)?;
            let this = self.clone();
            let id = media_id.clone();
            std::thread::spawn(move || {
                if let Err(e) = this.prepare(&id) {
                    let _ = this.db.record_media_failed(&id, &e.to_string());
                }
            });
            queued += 1;
        }
        Ok(queued)
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

/// One of the [`MAX_PREPARING_AT_ONCE`] preparation slots, given back however
/// the work ends — a panic in FFmpeg handling included, which is the whole
/// reason this is a guard and not a pair of calls.
struct Slot(Arc<(Mutex<usize>, std::sync::Condvar)>);

impl Slot {
    fn take(slots: Arc<(Mutex<usize>, std::sync::Condvar)>) -> Self {
        {
            let (lock, ready) = &*slots;
            let mut free = lock.lock().unwrap();
            while *free == 0 {
                free = ready.wait(free).unwrap();
            }
            *free -= 1;
        }
        Self(slots)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let (lock, ready) = &*self.0;
        if let Ok(mut free) = lock.lock() {
            *free += 1;
            ready.notify_one();
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// At most one preparation at a time, however many threads ask.
    #[test]
    fn the_preparation_slots_are_never_oversubscribed() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let slots = Arc::new((Mutex::new(MAX_PREPARING_AT_ONCE), std::sync::Condvar::new()));
        let now = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let threads: Vec<_> = (0..8)
            .map(|_| {
                let slots = Arc::clone(&slots);
                let now = Arc::clone(&now);
                let peak = Arc::clone(&peak);
                std::thread::spawn(move || {
                    let _slot = Slot::take(slots);
                    let held = now.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(held, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    now.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), MAX_PREPARING_AT_ONCE);
        assert_eq!(*slots.0.lock().unwrap(), MAX_PREPARING_AT_ONCE, "a slot was not given back");
    }

    #[test]
    fn a_dropped_slot_is_returned_even_when_the_work_panics() {
        let slots = Arc::new((Mutex::new(1usize), std::sync::Condvar::new()));
        let taken = Arc::clone(&slots);
        let _ = std::thread::spawn(move || {
            let _slot = Slot::take(taken);
            panic!("FFmpeg handling blew up");
        })
        .join();
        assert_eq!(*slots.0.lock().unwrap(), 1, "a panicking preparation kept its slot for ever");
    }
}
