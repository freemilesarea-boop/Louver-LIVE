//! Playback order resolution.
//!
//! The engine is a trait so that V2 can add Dynamic Shuffle without touching
//! the streaming session (§12).

use crate::database::models::PlaylistItem;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackMode {
    /// 1 → 2 → 3 → 1 → 2 → 3 …
    #[default]
    Sequential,
    /// Shuffled once when the broadcast starts, then that order repeats.
    ShuffleOnce,
}

impl PlaybackMode {
    pub fn id(self) -> &'static str {
        match self {
            Self::Sequential => "sequential",
            Self::ShuffleOnce => "shuffle_once",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "sequential" => Some(Self::Sequential),
            "shuffle_once" => Some(Self::ShuffleOnce),
            _ => None,
        }
    }
}

/// Resolves the concrete play order for one broadcast session.
pub trait PlaylistOrderEngine: Send + Sync {
    /// Items are given in stored order; the returned vector is the play order.
    /// Disabled items are already filtered out by the caller.
    fn resolve(&self, items: &[PlaylistItem], seed: u64) -> Vec<PlaylistItem>;
}

pub struct SequentialEngine;

impl PlaylistOrderEngine for SequentialEngine {
    fn resolve(&self, items: &[PlaylistItem], _seed: u64) -> Vec<PlaylistItem> {
        let mut v = items.to_vec();
        v.sort_by_key(|i| i.position);
        v
    }
}

/// Shuffles once, avoiding a run of the same media id across the loop seam.
pub struct ShuffleOnceEngine;

impl PlaylistOrderEngine for ShuffleOnceEngine {
    fn resolve(&self, items: &[PlaylistItem], seed: u64) -> Vec<PlaylistItem> {
        let mut v = items.to_vec();
        v.sort_by_key(|i| i.position);
        if v.len() < 2 {
            return v;
        }
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
        v.shuffle(&mut rng);

        // The same media file may appear twice in a playlist. Break adjacent
        // duplicates, including across the wrap-around seam, so a loop never
        // plays one video twice in a row (§12).
        let n = v.len();
        let distinct = {
            let mut ids: Vec<i64> = v.iter().map(|i| i.media_id).collect();
            ids.sort_unstable();
            ids.dedup();
            ids.len()
        };
        if distinct < 2 {
            return v; // impossible to avoid; every item is the same media
        }
        for _attempt in 0..64 {
            let mut fixed = false;
            for i in 0..n {
                let j = (i + 1) % n;
                if v[i].media_id == v[j].media_id {
                    // swap j with a random position that breaks the run
                    let mut k = rng.gen_range(0..n);
                    for _ in 0..n {
                        let prev = v[(k + n - 1) % n].media_id;
                        let next = v[(k + 1) % n].media_id;
                        if k != i
                            && k != j
                            && v[k].media_id != v[i].media_id
                            && prev != v[j].media_id
                            && next != v[j].media_id
                        {
                            break;
                        }
                        k = (k + 1) % n;
                    }
                    v.swap(j, k);
                    fixed = true;
                }
            }
            if !fixed {
                break;
            }
        }
        v
    }
}

pub fn engine_for(mode: PlaybackMode) -> Box<dyn PlaylistOrderEngine> {
    match mode {
        PlaybackMode::Sequential => Box::new(SequentialEngine),
        PlaybackMode::ShuffleOnce => Box::new(ShuffleOnceEngine),
    }
}

/// Filter to the items that will actually be broadcast, then order them.
pub fn resolve_play_order(
    items: &[PlaylistItem],
    mode: PlaybackMode,
    seed: u64,
) -> Vec<PlaylistItem> {
    let enabled: Vec<PlaylistItem> = items.iter().filter(|i| i.enabled).cloned().collect();
    engine_for(mode).resolve(&enabled, seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(spec: &[(i64, i64, bool)]) -> Vec<PlaylistItem> {
        spec.iter()
            .enumerate()
            .map(|(idx, (id, media, enabled))| PlaylistItem {
                id: *id,
                playlist_id: 1,
                media_id: *media,
                position: idx as i64,
                enabled: *enabled,
            })
            .collect()
    }

    #[test]
    fn sequential_preserves_stored_order() {
        let v = items(&[(1, 10, true), (2, 20, true), (3, 30, true)]);
        let out = resolve_play_order(&v, PlaybackMode::Sequential, 0);
        assert_eq!(out.iter().map(|i| i.media_id).collect::<Vec<_>>(), vec![10, 20, 30]);
    }

    #[test]
    fn sequential_sorts_by_position_not_insertion() {
        let mut v = items(&[(1, 10, true), (2, 20, true), (3, 30, true)]);
        v[0].position = 5; // user dragged it to the end
        let out = resolve_play_order(&v, PlaybackMode::Sequential, 0);
        assert_eq!(out.iter().map(|i| i.media_id).collect::<Vec<_>>(), vec![20, 30, 10]);
    }

    #[test]
    fn disabled_items_are_excluded() {
        let v = items(&[(1, 10, true), (2, 20, false), (3, 30, true)]);
        for mode in [PlaybackMode::Sequential, PlaybackMode::ShuffleOnce] {
            let out = resolve_play_order(&v, mode, 7);
            assert_eq!(out.len(), 2, "{mode:?}");
            assert!(!out.iter().any(|i| i.media_id == 20));
        }
    }

    #[test]
    fn empty_playlist_resolves_to_empty() {
        for mode in [PlaybackMode::Sequential, PlaybackMode::ShuffleOnce] {
            assert!(resolve_play_order(&[], mode, 1).is_empty());
            let all_off = items(&[(1, 10, false)]);
            assert!(resolve_play_order(&all_off, mode, 1).is_empty());
        }
    }

    #[test]
    fn shuffle_keeps_every_item_exactly_once() {
        let v = items(&[(1, 10, true), (2, 20, true), (3, 30, true), (4, 40, true), (5, 50, true)]);
        let out = resolve_play_order(&v, PlaybackMode::ShuffleOnce, 42);
        let mut ids: Vec<i64> = out.iter().map(|i| i.media_id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![10, 20, 30, 40, 50]);
    }

    #[test]
    fn shuffle_is_deterministic_for_a_seed() {
        let v = items(&[(1, 10, true), (2, 20, true), (3, 30, true), (4, 40, true)]);
        let a = resolve_play_order(&v, PlaybackMode::ShuffleOnce, 99);
        let b = resolve_play_order(&v, PlaybackMode::ShuffleOnce, 99);
        assert_eq!(
            a.iter().map(|i| i.media_id).collect::<Vec<_>>(),
            b.iter().map(|i| i.media_id).collect::<Vec<_>>(),
            "same seed must give the same order so a session can be recovered"
        );
    }

    #[test]
    fn shuffle_actually_reorders_for_some_seed() {
        let v = items(&[(1, 10, true), (2, 20, true), (3, 30, true), (4, 40, true), (5, 50, true), (6, 60, true)]);
        let seq: Vec<i64> = resolve_play_order(&v, PlaybackMode::Sequential, 0).iter().map(|i| i.media_id).collect();
        let differs = (0..50u64).any(|s| {
            let sh: Vec<i64> = resolve_play_order(&v, PlaybackMode::ShuffleOnce, s).iter().map(|i| i.media_id).collect();
            sh != seq
        });
        assert!(differs, "shuffle never changed the order across 50 seeds");
    }

    #[test]
    fn shuffle_never_repeats_a_video_back_to_back_including_the_loop_seam() {
        // Duplicated media ids are the hard case: 10 appears three times.
        let v = items(&[
            (1, 10, true), (2, 10, true), (3, 10, true),
            (4, 20, true), (5, 30, true), (6, 40, true),
            (7, 50, true), (8, 60, true),
        ]);
        for seed in 0..200u64 {
            let out = resolve_play_order(&v, PlaybackMode::ShuffleOnce, seed);
            let n = out.len();
            for i in 0..n {
                let j = (i + 1) % n; // wraps: the loop seam matters
                assert_ne!(
                    out[i].media_id, out[j].media_id,
                    "seed {seed}: media {} repeats at {i}->{j} in {:?}",
                    out[i].media_id,
                    out.iter().map(|x| x.media_id).collect::<Vec<_>>()
                );
            }
        }
    }

    #[test]
    fn single_item_playlist_is_stable() {
        let v = items(&[(1, 10, true)]);
        for mode in [PlaybackMode::Sequential, PlaybackMode::ShuffleOnce] {
            let out = resolve_play_order(&v, mode, 3);
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].media_id, 10);
        }
    }

    #[test]
    fn playback_mode_round_trips() {
        for m in [PlaybackMode::Sequential, PlaybackMode::ShuffleOnce] {
            assert_eq!(PlaybackMode::from_id(m.id()), Some(m));
        }
        assert_eq!(PlaybackMode::from_id("dynamic_shuffle"), None);
    }
}
