//! Playlist and playlist-item commands (§11, §25).

use super::CmdResult;
use crate::state::AppState;
use louver_core::config::OutputProfile;
use louver_core::database::models::{Media, Playlist, PlaylistItem};
use louver_core::PlaybackMode;
use serde::Serialize;
use tauri::State;

/// A playlist plus everything the Playlist page renders.
#[derive(Serialize)]
pub struct PlaylistView {
    pub playlist: Playlist,
    pub items: Vec<PlaylistItemView>,
    pub total_duration_secs: f64,
    pub total_duration_label: String,
    /// Items that still need optimizing before this playlist can broadcast.
    pub unready_count: usize,
}

#[derive(Serialize)]
pub struct PlaylistItemView {
    #[serde(flatten)]
    pub item: PlaylistItem,
    pub media: Media,
}

#[tauri::command]
pub fn list_playlists(state: State<'_, AppState>) -> CmdResult<Vec<Playlist>> {
    state.db.list_playlists()
}

#[tauri::command]
pub fn get_playlist(state: State<'_, AppState>, id: i64) -> CmdResult<Option<PlaylistView>> {
    let Some(playlist) = state.db.get_playlist(id)? else { return Ok(None) };
    let items = state.db.list_playlist_items(id)?;

    let mut views = Vec::new();
    let mut total = 0.0;
    let mut unready = 0;
    for item in items {
        let Some(media) = state.db.get_media(item.media_id)? else { continue };
        if item.enabled {
            total += media.normalized_duration_secs.unwrap_or(media.duration_secs);
            if !media.status.is_broadcast_ready() {
                unready += 1;
            }
        }
        views.push(PlaylistItemView { item, media });
    }

    Ok(Some(PlaylistView {
        playlist,
        items: views,
        total_duration_secs: total,
        total_duration_label: louver_core::system::format_duration_ko(total as i64),
        unready_count: unready,
    }))
}

#[tauri::command]
pub fn create_playlist(state: State<'_, AppState>, name: String) -> CmdResult<i64> {
    state.db.create_playlist(&name, PlaybackMode::Sequential, state.profile())
}

#[tauri::command]
pub fn update_playlist(
    state: State<'_, AppState>,
    id: i64,
    name: String,
    playback_mode: String,
    output_profile: String,
) -> CmdResult<()> {
    state.db.update_playlist(
        id,
        &name,
        PlaybackMode::from_id(&playback_mode).unwrap_or_default(),
        OutputProfile::from_id(&output_profile).unwrap_or_default(),
    )
}

#[tauri::command]
pub fn delete_playlist(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    state.db.delete_playlist(id)
}

#[tauri::command]
pub fn add_to_playlist(state: State<'_, AppState>, playlist_id: i64, media_ids: Vec<i64>) -> CmdResult<()> {
    for m in media_ids {
        state.db.add_playlist_item(playlist_id, m)?;
    }
    Ok(())
}

/// Persist a drag-and-drop reorder (§11).
#[tauri::command]
pub fn reorder_playlist(state: State<'_, AppState>, playlist_id: i64, item_ids: Vec<i64>) -> CmdResult<()> {
    state.db.reorder_playlist_items(playlist_id, &item_ids)
}

#[tauri::command]
pub fn set_item_enabled(state: State<'_, AppState>, item_id: i64, enabled: bool) -> CmdResult<()> {
    state.db.set_item_enabled(item_id, enabled)
}

#[tauri::command]
pub fn remove_playlist_item(state: State<'_, AppState>, item_id: i64) -> CmdResult<()> {
    state.db.remove_playlist_item(item_id)
}
