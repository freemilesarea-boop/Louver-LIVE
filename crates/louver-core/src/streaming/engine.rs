//! The broadcast session: turns a playlist into a running FFmpeg (§13).

use crate::config::{OutputProfile, StreamMode};
use crate::database::models::{Media, PlaylistItem};
use crate::error::{ErrorCode, LouverError, Result};
use crate::streaming::manifest::write_manifest;
use crate::streaming::playlist::{resolve_play_order, PlaybackMode};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// An item resolved to the exact file that will be broadcast.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedItem {
    pub media_id: i64,
    pub display_name: String,
    /// The normalized cache file, or the source when it was already compatible.
    pub path: PathBuf,
    /// Broadcast duration: the whole-frame length of the file being sent.
    pub duration_secs: f64,
}

/// Everything a session needs to start.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPlan {
    pub playlist_id: i64,
    pub items: Vec<ResolvedItem>,
    pub manifest_path: PathBuf,
    pub profile: OutputProfile,
    pub mode: StreamMode,
    pub playback_mode: PlaybackMode,
    pub order_seed: i64,
    pub total_duration_secs: f64,
}

impl SessionPlan {
    /// Which item is playing `elapsed` seconds in, given the loop repeats (§24).
    pub fn item_at(&self, elapsed_secs: f64) -> Option<(usize, &ResolvedItem)> {
        if self.items.is_empty() || self.total_duration_secs <= 0.0 {
            return None;
        }
        let mut t = elapsed_secs.rem_euclid(self.total_duration_secs);
        for (i, item) in self.items.iter().enumerate() {
            if t < item.duration_secs {
                return Some((i, item));
            }
            t -= item.duration_secs;
        }
        self.items.last().map(|i| (self.items.len() - 1, i))
    }

    /// The item after `index`, wrapping around the loop.
    pub fn next_item(&self, index: usize) -> Option<&ResolvedItem> {
        if self.items.is_empty() {
            return None;
        }
        self.items.get((index + 1) % self.items.len())
    }
}

/// Resolve a playlist into a concrete, broadcastable plan.
///
/// Returns an error rather than silently skipping an unready file: §29 CHECK 3
/// should already have caught it, and quietly dropping a video from a 24-hour
/// broadcast is worse than refusing to start.
#[allow(clippy::too_many_arguments)]
pub fn build_plan(
    playlist_id: i64,
    items: &[PlaylistItem],
    media_by_id: &dyn Fn(i64) -> Option<Media>,
    mode: PlaybackMode,
    profile: OutputProfile,
    stream_mode: StreamMode,
    order_seed: i64,
    manifest_path: &Path,
) -> Result<SessionPlan> {
    let ordered = resolve_play_order(items, mode, order_seed as u64);
    if ordered.is_empty() {
        return Err(LouverError::new(ErrorCode::StreamEmptyPlaylist));
    }

    let mut resolved = Vec::with_capacity(ordered.len());
    for it in &ordered {
        let m = media_by_id(it.media_id).ok_or_else(|| {
            LouverError::with_detail(ErrorCode::MediaFileMissing, format!("media id {}", it.media_id))
        })?;
        if !m.status.is_broadcast_ready() {
            return Err(LouverError::with_detail(
                ErrorCode::StreamNotNormalized,
                m.display_name.clone(),
            ));
        }
        let path = PathBuf::from(m.normalized_path.clone().unwrap_or_else(|| m.source_path.clone()));
        if !path.is_file() {
            return Err(LouverError::with_detail(
                ErrorCode::MediaFileMissing,
                path.display().to_string(),
            ));
        }
        resolved.push(ResolvedItem {
            media_id: m.id,
            display_name: m.display_name.clone(),
            duration_secs: m.normalized_duration_secs.unwrap_or(m.duration_secs),
            path,
        });
    }

    write_manifest(manifest_path, &resolved.iter().map(|r| r.path.clone()).collect::<Vec<_>>())?;

    Ok(SessionPlan {
        playlist_id,
        total_duration_secs: resolved.iter().map(|r| r.duration_secs).sum(),
        items: resolved,
        manifest_path: manifest_path.to_path_buf(),
        profile,
        mode: stream_mode,
        playback_mode: mode,
        order_seed,
    })
}

/// A fresh seed for shuffle ordering.
pub fn new_order_seed() -> i64 {
    use rand::Rng;
    rand::thread_rng().gen_range(1..i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::models::MediaStatus;

    fn media(id: i64, name: &str, path: &Path, dur: f64, status: MediaStatus) -> Media {
        Media {
            id,
            source_path: path.to_string_lossy().into_owned(),
            display_name: name.into(),
            status,
            media_hash: format!("h{id}"),
            normalized_path: Some(path.to_string_lossy().into_owned()),
            normalized_profile: Some("1080p30".into()),
            duration_secs: dur,
            normalized_duration_secs: Some(dur),
            width: 1920,
            height: 1080,
            fps: 30.0,
            video_codec: "h264".into(),
            audio_codec: Some("aac".into()),
            pixel_format: Some("yuv420p".into()),
            is_hdr: false,
            file_size: 1,
            added_at: String::new(),
            last_error: None,
        }
    }

    fn items(n: i64) -> Vec<PlaylistItem> {
        (0..n)
            .map(|i| PlaylistItem { id: i + 1, playlist_id: 1, media_id: i + 1, position: i, enabled: true })
            .collect()
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        media: Vec<Media>,
        manifest: PathBuf,
    }

    fn fixture(durations: &[f64]) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let media = durations
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let p = dir.path().join(format!("n{i}.mp4"));
                std::fs::write(&p, b"video").unwrap();
                media(i as i64 + 1, &format!("night{:02}.mp4", i + 1), &p, *d, MediaStatus::Normalized)
            })
            .collect();
        let manifest = dir.path().join("manifest.txt");
        Fixture { _dir: dir, media, manifest }
    }

    fn lookup(ms: &[Media]) -> impl Fn(i64) -> Option<Media> + '_ {
        move |id| ms.iter().find(|m| m.id == id).cloned()
    }

    #[test]
    fn a_plan_lists_items_in_order_and_writes_the_manifest() {
        let f = fixture(&[3602.0, 3501.0, 3734.0]);
        let plan = build_plan(
            1, &items(3), &lookup(&f.media), PlaybackMode::Sequential,
            OutputProfile::P1080p30, StreamMode::StreamCopy, 1, &f.manifest,
        )
        .unwrap();

        assert_eq!(plan.items.len(), 3);
        assert_eq!(plan.items[0].display_name, "night01.mp4");
        assert_eq!(plan.total_duration_secs, 3602.0 + 3501.0 + 3734.0);

        let m = std::fs::read_to_string(&f.manifest).unwrap();
        assert!(m.starts_with("ffconcat version 1.0"));
        assert_eq!(m.lines().count(), 4);
        for it in &plan.items {
            assert!(m.contains(&format!("file '{}'", it.path.display())));
        }
    }

    #[test]
    fn disabled_items_are_left_out_of_the_manifest() {
        let f = fixture(&[10.0, 10.0, 10.0]);
        let mut its = items(3);
        its[1].enabled = false;
        let plan = build_plan(
            1, &its, &lookup(&f.media), PlaybackMode::Sequential,
            OutputProfile::P1080p30, StreamMode::StreamCopy, 1, &f.manifest,
        )
        .unwrap();
        assert_eq!(plan.items.len(), 2);
        assert_eq!(plan.total_duration_secs, 20.0);
        assert_eq!(std::fs::read_to_string(&f.manifest).unwrap().lines().count(), 3);
    }

    #[test]
    fn an_empty_playlist_is_rejected() {
        let f = fixture(&[]);
        let e = build_plan(
            1, &[], &lookup(&f.media), PlaybackMode::Sequential,
            OutputProfile::P1080p30, StreamMode::StreamCopy, 1, &f.manifest,
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::StreamEmptyPlaylist);
    }

    #[test]
    fn an_unnormalized_item_refuses_to_start_rather_than_being_skipped() {
        let mut f = fixture(&[10.0, 10.0]);
        f.media[1].status = MediaStatus::OptimizationRequired;
        let e = build_plan(
            1, &items(2), &lookup(&f.media), PlaybackMode::Sequential,
            OutputProfile::P1080p30, StreamMode::StreamCopy, 1, &f.manifest,
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::StreamNotNormalized);
        assert_eq!(e.detail.unwrap(), "night02.mp4");
    }

    #[test]
    fn a_vanished_file_refuses_to_start() {
        let mut f = fixture(&[10.0]);
        f.media[0].normalized_path = Some("/no/such/file.mp4".into());
        let e = build_plan(
            1, &items(1), &lookup(&f.media), PlaybackMode::Sequential,
            OutputProfile::P1080p30, StreamMode::StreamCopy, 1, &f.manifest,
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::MediaFileMissing);
    }

    #[test]
    fn a_compatible_file_is_broadcast_from_its_original_path() {
        let mut f = fixture(&[10.0]);
        f.media[0].status = MediaStatus::Compatible;
        f.media[0].normalized_path = None;
        let plan = build_plan(
            1, &items(1), &lookup(&f.media), PlaybackMode::Sequential,
            OutputProfile::P1080p30, StreamMode::StreamCopy, 1, &f.manifest,
        )
        .unwrap();
        assert_eq!(plan.items[0].path.to_string_lossy(), f.media[0].source_path);
    }

    // --- "now playing / up next" over an infinite loop (§24) ---------------

    #[test]
    fn current_and_next_item_track_the_loop() {
        let f = fixture(&[10.0, 12.0, 8.0]); // 30s cycle
        let plan = build_plan(
            1, &items(3), &lookup(&f.media), PlaybackMode::Sequential,
            OutputProfile::P1080p30, StreamMode::StreamCopy, 1, &f.manifest,
        )
        .unwrap();

        for (elapsed, want_idx) in [
            (0.0, 0), (9.9, 0), (10.0, 1), (21.9, 1), (22.0, 2), (29.9, 2),
            // second cycle
            (30.0, 0), (45.0, 1),
            // hours later, still correct
            (3600.0 + 5.0, 0),
        ] {
            let (i, item) = plan.item_at(elapsed).unwrap();
            assert_eq!(i, want_idx, "at {elapsed}s expected item {want_idx}, got {}", item.display_name);
        }

        assert_eq!(plan.next_item(0).unwrap().display_name, "night02.mp4");
        assert_eq!(plan.next_item(2).unwrap().display_name, "night01.mp4", "must wrap around");
    }

    #[test]
    fn item_lookup_is_safe_for_degenerate_plans() {
        let f = fixture(&[]);
        let plan = SessionPlan {
            playlist_id: 1,
            items: vec![],
            manifest_path: f.manifest.clone(),
            profile: OutputProfile::P1080p30,
            mode: StreamMode::StreamCopy,
            playback_mode: PlaybackMode::Sequential,
            order_seed: 1,
            total_duration_secs: 0.0,
        };
        assert!(plan.item_at(5.0).is_none());
        assert!(plan.next_item(0).is_none());
    }

    #[test]
    fn the_same_seed_reproduces_a_shuffled_order_so_a_session_can_resume() {
        let f = fixture(&[10.0, 10.0, 10.0, 10.0, 10.0]);
        let build = |seed| {
            build_plan(
                1, &items(5), &lookup(&f.media), PlaybackMode::ShuffleOnce,
                OutputProfile::P1080p30, StreamMode::StreamCopy, seed, &f.manifest,
            )
            .unwrap()
            .items
            .iter()
            .map(|i| i.media_id)
            .collect::<Vec<_>>()
        };
        assert_eq!(build(12345), build(12345), "recovery must replay the same order (§32)");
    }

    #[test]
    fn order_seeds_are_positive_and_vary() {
        let seeds: std::collections::HashSet<i64> = (0..20).map(|_| new_order_seed()).collect();
        assert!(seeds.len() > 15, "seeds should not collide constantly");
        assert!(seeds.iter().all(|s| *s > 0));
    }
}
