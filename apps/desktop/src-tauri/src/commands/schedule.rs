//! Schedule commands (§19, §27).

use super::CmdResult;
use crate::state::AppState;
use louver_core::database::models::{DaysOfWeek, Schedule};
use louver_core::scheduler::{self, format_days, next_occurrence};
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
            Ok(ScheduleView {
                playlist_name: state.db.get_playlist(s.playlist_id).ok().flatten().map(|p| p.name),
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
