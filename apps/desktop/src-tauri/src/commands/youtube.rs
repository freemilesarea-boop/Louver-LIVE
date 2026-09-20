//! YouTube account, broadcast metadata, presets and the chat bot (V2).
//!
//! None of these touch the broadcast. The worst a failing call here can do is
//! report an error; FFmpeg keeps publishing either way.

use super::CmdResult;
use crate::state::AppState;
use crate::youtube_service::YoutubeStatus;
use louver_core::youtube::chat::{ChatSettings, ChatStatus};
use louver_core::youtube::{keys, BroadcastMetadata, BroadcastPreset, LiveBroadcast, Privacy};
use tauri::State;

#[tauri::command]
pub fn youtube_status(state: State<'_, AppState>) -> CmdResult<YoutubeStatus> {
    Ok(state.youtube.status())
}

/// Store the OAuth client for this installation (§1).
///
/// The id is ordinary configuration; the secret goes to the keychain beside
/// the refresh token and never to SQLite.
#[tauri::command]
pub fn youtube_set_credentials(
    state: State<'_, AppState>,
    client_id: String,
    client_secret: String,
) -> CmdResult<YoutubeStatus> {
    state.youtube.set_credentials(&client_id, &client_secret)?;
    Ok(state.youtube.status())
}

/// Start consent and return the URL to open. The UI opens it and then polls
/// `youtube_status`, so the window never blocks on the browser.
#[tauri::command]
pub fn youtube_begin_connect(state: State<'_, AppState>) -> CmdResult<String> {
    state.youtube.begin_connect()
}

#[tauri::command]
pub fn youtube_disconnect(state: State<'_, AppState>) -> CmdResult<YoutubeStatus> {
    state.youtube.disconnect()?;
    Ok(state.youtube.status())
}

#[tauri::command]
pub fn youtube_get_metadata(state: State<'_, AppState>) -> CmdResult<BroadcastMetadata> {
    Ok(stored_metadata(&state))
}

/// Save what the user typed, without sending anything.
#[tauri::command]
pub fn youtube_save_metadata(
    state: State<'_, AppState>,
    metadata: BroadcastMetadata,
) -> CmdResult<BroadcastMetadata> {
    let m = metadata.cleaned();
    m.validate()?;
    let db = &state.db;
    db.set_setting(keys::METADATA_TITLE, &m.title)?;
    db.set_setting(keys::METADATA_DESCRIPTION, &m.description)?;
    db.set_setting(keys::METADATA_TAGS, &m.tags.join("\n"))?;
    db.set_setting(keys::METADATA_CATEGORY, &m.category_id)?;
    db.set_setting(keys::METADATA_PRIVACY, m.privacy.as_api())?;
    Ok(m)
}

/// Push the saved metadata to the live broadcast (§3).
#[tauri::command]
pub fn youtube_apply_metadata(state: State<'_, AppState>) -> CmdResult<LiveBroadcast> {
    let m = stored_metadata(&state);
    state.youtube.apply_metadata(&m)
}

/// Whether metadata is pushed automatically when a broadcast starts.
#[tauri::command]
pub fn youtube_set_apply_on_start(state: State<'_, AppState>, enabled: bool) -> CmdResult<()> {
    state.db.set_setting(keys::APPLY_ON_START, if enabled { "true" } else { "false" })
}

#[tauri::command]
pub fn youtube_current_broadcast(state: State<'_, AppState>) -> CmdResult<LiveBroadcast> {
    state.youtube.current_broadcast()
}

// --- presets (§4) ---------------------------------------------------------

#[tauri::command]
pub fn youtube_list_presets(state: State<'_, AppState>) -> CmdResult<Vec<BroadcastPreset>> {
    state.db.list_presets()
}

#[tauri::command]
pub fn youtube_save_preset(
    state: State<'_, AppState>,
    name: String,
    metadata: BroadcastMetadata,
) -> CmdResult<Vec<BroadcastPreset>> {
    let m = metadata.cleaned();
    m.validate()?;
    state.db.save_preset(&name, &m)?;
    state.db.list_presets()
}

#[tauri::command]
pub fn youtube_delete_preset(state: State<'_, AppState>, id: i64) -> CmdResult<Vec<BroadcastPreset>> {
    state.db.delete_preset(id)?;
    state.db.list_presets()
}

// --- chat (§5, §6, §7) ----------------------------------------------------

#[tauri::command]
pub fn chat_list_messages(state: State<'_, AppState>) -> CmdResult<Vec<louver_core::youtube::ChatMessage>> {
    state.db.list_chat_messages()
}

#[tauri::command]
pub fn chat_add_message(
    state: State<'_, AppState>,
    text: String,
) -> CmdResult<Vec<louver_core::youtube::ChatMessage>> {
    state.db.add_chat_message(&text)?;
    state.db.list_chat_messages()
}

#[tauri::command]
pub fn chat_update_message(
    state: State<'_, AppState>,
    id: i64,
    text: String,
    enabled: bool,
) -> CmdResult<Vec<louver_core::youtube::ChatMessage>> {
    state.db.update_chat_message(id, &text, enabled)?;
    state.db.list_chat_messages()
}

#[tauri::command]
pub fn chat_delete_message(
    state: State<'_, AppState>,
    id: i64,
) -> CmdResult<Vec<louver_core::youtube::ChatMessage>> {
    state.db.delete_chat_message(id)?;
    state.db.list_chat_messages()
}

#[tauri::command]
pub fn chat_reorder_messages(
    state: State<'_, AppState>,
    ids: Vec<i64>,
) -> CmdResult<Vec<louver_core::youtube::ChatMessage>> {
    state.db.reorder_chat_messages(&ids)?;
    state.db.list_chat_messages()
}

#[tauri::command]
pub fn chat_get_settings(state: State<'_, AppState>) -> CmdResult<ChatSettings> {
    Ok(state.youtube.chat_settings())
}

#[tauri::command]
pub fn chat_save_settings(state: State<'_, AppState>, settings: ChatSettings) -> CmdResult<ChatSettings> {
    state.youtube.save_chat_settings(&settings)?;
    // Turning the bot off takes effect at once rather than at the next
    // broadcast; turning it on waits for the broadcast to be live.
    if !settings.enabled {
        state.youtube.stop_bot();
    }
    Ok(state.youtube.chat_settings())
}

#[tauri::command]
pub fn chat_status(state: State<'_, AppState>) -> CmdResult<ChatStatus> {
    Ok(state.youtube.chat_status())
}

/// The floor the UI must not offer to go below (§6).
#[tauri::command]
pub fn chat_min_interval_secs() -> CmdResult<u64> {
    Ok(louver_core::youtube::MIN_INTERVAL_SECS)
}

fn stored_metadata(state: &AppState) -> BroadcastMetadata {
    let db = &state.db;
    BroadcastMetadata {
        title: db.get_setting_or(keys::METADATA_TITLE, ""),
        description: db.get_setting_or(keys::METADATA_DESCRIPTION, ""),
        tags: db
            .get_setting_or(keys::METADATA_TAGS, "")
            .lines()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect(),
        category_id: db.get_setting_or(keys::METADATA_CATEGORY, "10"),
        privacy: Privacy::from_api(&db.get_setting_or(keys::METADATA_PRIVACY, "unlisted")),
    }
}
