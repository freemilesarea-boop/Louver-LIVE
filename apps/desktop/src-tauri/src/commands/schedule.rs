//! Schedule commands (§19, §27).

use super::CmdResult;
use crate::state::AppState;
use louver_core::database::models::{DaysOfWeek, Schedule};
use louver_core::error::{ErrorCode, LouverError};
use louver_core::scheduler::{self, active_occurrence, format_days, next_occurrence};
use serde::Serialize;
use tauri::State;

#[derive(Serialize)]
pub struct ScheduleView {
    #[serde(flatten)]
    pub schedule: Schedule,
    pub playlist_name: Option<String>,
    pub days_label: String,
    pub crosses_midnight: bool,
    pub next_start: Option<String>,
    pub next_end: Option<String>,
    pub window_duration_label: String,
    /// The window is open right now. Without this the row showed only
    /// `next_start`, which for a 17:14→17:40 schedule read at 17:17 is
    /// *tomorrow* — so an active window looked like a skipped one.
    pub active_now: bool,
    /// When the window that is open now ends.
    pub active_until: Option<String>,
    /// The schedule points at a playlist that no longer exists.
    pub playlist_missing: bool,
    /// The playlist exists but holds nothing broadcastable.
    pub playlist_ready_count: usize,
}

#[tauri::command]
pub fn list_schedules(state: State<'_, AppState>) -> CmdResult<Vec<ScheduleView>> {
    let now = state.clock.now_local();
    state
        .db
        .list_schedules()?
        .into_iter()
        .map(|s| {
            let start = scheduler::parse_time(&s.start_time)?;
            let end = scheduler::parse_time(&s.end_time)?;
            let next = next_occurrence(&s, now);
            let active = active_occurrence(&s, now);
            let playlist = state.db.get_playlist(s.playlist_id).ok().flatten();
            let ready = state
                .db
                .list_playlist_items(s.playlist_id)
                .unwrap_or_default()
                .iter()
                .filter(|i| i.enabled)
                .filter_map(|i| state.db.get_media(i.media_id).ok().flatten())
                .filter(|m| m.status.is_broadcast_ready())
                .count();
            Ok(ScheduleView {
                active_now: active.is_some(),
                active_until: active.as_ref().map(|o| o.end.format("%Y-%m-%d %H:%M").to_string()),
                playlist_missing: playlist.is_none(),
                playlist_ready_count: ready,
                playlist_name: playlist.as_ref().map(|p| p.name.clone()),
                days_label: format_days(s.days_of_week),
                crosses_midnight: scheduler::crosses_midnight(start, end),
                next_start: next.as_ref().map(|o| o.start.format("%Y-%m-%d %H:%M").to_string()),
                next_end: next.as_ref().map(|o| o.end.format("%Y-%m-%d %H:%M").to_string()),
                window_duration_label: louver_core::system::format_duration_ko(
                    scheduler::window_duration_secs(start, end),
                ),
                schedule: s,
            })
        })
        .collect()
}

#[tauri::command]
pub fn create_schedule(
    state: State<'_, AppState>,
    playlist_id: i64,
    days_of_week: u8,
    start_time: String,
    end_time: String,
) -> CmdResult<i64> {
    let s = Schedule {
        id: 0,
        playlist_id,
        days_of_week: DaysOfWeek(days_of_week),
        start_time,
        end_time,
        enabled: true,
    };
    scheduler::validate(&s)?;
    state.db.create_schedule(&s)
}

#[tauri::command]
pub fn update_schedule(
    state: State<'_, AppState>,
    id: i64,
    playlist_id: i64,
    days_of_week: u8,
    start_time: String,
    end_time: String,
    enabled: bool,
) -> CmdResult<()> {
    let s =
        Schedule { id, playlist_id, days_of_week: DaysOfWeek(days_of_week), start_time, end_time, enabled };
    scheduler::validate(&s)?;
    state.db.update_schedule(&s)
}

#[tauri::command]
pub fn delete_schedule(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    state.db.delete_schedule(id)
}

// --- the global scheduler switch --------------------------------------------

/// What this computer is doing about scheduled broadcasts.
///
/// Reported as a whole rather than as a flag, because the question the screen
/// has to answer is not "is a setting on" but "is anything actually going to
/// happen tonight, and when".
#[derive(Serialize)]
pub struct SchedulerStatusView {
    /// `STOPPED` / `ARMING` / `WAITING` / `STARTING` / `LIVE` / `STOPPING` / `ERROR`.
    pub state: String,
    pub armed: bool,
    /// Schedules whose own switch is on. Zero means arming would watch nothing.
    pub enabled_count: usize,
    pub next_start: Option<String>,
    pub next_end: Option<String>,
    pub next_playlist: Option<String>,
    /// Counts down to the next automatic start, for the screen that has to
    /// prove it is really waiting.
    pub seconds_until_start: Option<i64>,
    /// The window that is open right now, if one is.
    pub active_start: Option<String>,
    pub active_end: Option<String>,
    pub last_error: Option<louver_core::error::LouverError>,
    /// Whether a relaunch puts the scheduler back the way it was left.
    pub restore_on_launch: bool,
}

#[tauri::command]
pub fn scheduler_status(state: State<'_, AppState>) -> CmdResult<SchedulerStatusView> {
    let rt = state.runtime.lock().unwrap();
    let status = rt.status();
    let now = state.clock.now_local();
    let schedules = state.db.list_schedules()?;
    let enabled_count = schedules.iter().filter(|s| s.enabled).count();

    let next = schedules.iter().filter_map(|s| next_occurrence(s, now)).min_by_key(|o| o.start);
    Ok(SchedulerStatusView {
        state: status.scheduler_state.as_str().to_string(),
        armed: rt.is_armed(),
        enabled_count,
        next_start: next.as_ref().map(|o| o.start.format("%Y-%m-%d %H:%M").to_string()),
        next_end: next.as_ref().map(|o| o.end.format("%Y-%m-%d %H:%M").to_string()),
        next_playlist: next
            .as_ref()
            .and_then(|o| state.db.get_playlist(o.playlist_id).ok().flatten())
            .map(|p| p.name),
        seconds_until_start: next.as_ref().map(|o| (o.start - now).num_seconds().max(0)),
        active_start: status.active_occurrence.as_ref().map(|o| o.start.clone()),
        active_end: status.active_occurrence.as_ref().map(|o| o.end.clone()),
        last_error: status.last_start_error,
        restore_on_launch: state
            .db
            .get_setting_or(louver_core::settings_keys::SCHEDULER_RESTORE_ON_LAUNCH, "true")
            == "true",
    })
}

/// Start watching the clock.
///
/// Everything that would stop a scheduled broadcast is checked here rather
/// than at 3am, because at 3am there is nobody to tell. An unarmed scheduler
/// that says why is far better than an armed one that fails silently.
#[tauri::command]
pub fn scheduler_arm(state: State<'_, AppState>) -> CmdResult<SchedulerStatusView> {
    let schedules = state.db.list_schedules()?;
    let enabled: Vec<_> = schedules.iter().filter(|s| s.enabled).collect();
    if enabled.is_empty() {
        return Err(LouverError::with_detail(
            ErrorCode::ScheduleNoDays,
            "사용 중인 예약이 없습니다. 예약을 먼저 추가하거나 켜주세요.",
        ));
    }

    // Each schedule's times must make sense, and its playlist must be able to
    // broadcast. Checked per schedule so the message names the one at fault.
    for s in &enabled {
        scheduler::validate(s)?;
        let report = crate::commands::streaming::run_preflight(state.clone(), s.playlist_id, false)?;
        if !report.can_broadcast {
            let f = report.first_failure().expect("a blocked report names a failure");
            return Err(LouverError::with_detail(
                f.code
                    .as_deref()
                    .and_then(crate::commands::streaming::code_from_str)
                    .unwrap_or(ErrorCode::StreamInvalidTransition),
                format!("{} → {} 예약: {}", s.start_time, s.end_time, f.detail),
            ));
        }
    }

    // If the user asked for metadata to be applied automatically, an account
    // has to be connected — otherwise every scheduled window would stop and
    // ask a question nobody is there to answer.
    if state.youtube.apply_on_start_wanted()
        && !state.youtube.status().connected
        && state.youtube.schedule_holds_on_failure()
    {
        return Err(LouverError::with_detail(
            ErrorCode::YoutubeNotConnected,
            "방송 설정 자동 적용이 켜져 있어 YouTube 계정 연결이 필요합니다. \
             연결하거나, 방송 설정에서 자동 적용을 끄고 다시 시도해주세요.",
        ));
    }

    state.runtime.lock().unwrap().set_armed(true);
    state.logger.info(
        louver_core::logging::LogTarget::App,
        &format!("SCHEDULER_ARMED: 예약 {}개를 감시합니다", enabled.len()),
    );
    scheduler_status(state)
}

/// Stop watching the clock. Saved schedules stay saved.
#[tauri::command]
pub fn scheduler_disarm(state: State<'_, AppState>) -> CmdResult<SchedulerStatusView> {
    state.runtime.lock().unwrap().set_armed(false);
    state.logger.info(louver_core::logging::LogTarget::App, "SCHEDULER_DISARMED: 예약 감시를 중지했습니다");
    scheduler_status(state)
}

/// Whether a relaunch puts the scheduler back the way it was left.
#[tauri::command]
pub fn scheduler_set_restore(state: State<'_, AppState>, enabled: bool) -> CmdResult<()> {
    state.db.set_setting(
        louver_core::settings_keys::SCHEDULER_RESTORE_ON_LAUNCH,
        if enabled { "true" } else { "false" },
    )
}
