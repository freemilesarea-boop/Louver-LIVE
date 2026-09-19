//! Louver Live desktop shell.
//!
//! This crate owns windows, the tray and the IPC surface. Every decision about
//! broadcasting lives in `louver-core` (§68), so this file is mostly wiring.

pub mod commands;
pub mod platform;
pub mod state;

use louver_core::logging::LogTarget;
use louver_core::system::AutostartManager;
use louver_core::{settings_keys, AppPaths};
use state::AppState;
use std::time::Duration;
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Manager, WindowEvent,
};

/// How often the broadcast loop advances. One second is fine: the supervisor's
/// own stall detection works on a 30s horizon and the UI only shows whole
/// seconds.
const TICK: Duration = Duration::from_secs(1);

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .invoke_handler(tauri::generate_handler![
            // media
            commands::media::import_media,
            commands::media::list_media,
            commands::media::delete_media,
            commands::media::compatibility_reasons,
            commands::media::estimate_optimization,
            commands::media::optimize_media,
            commands::media::cancel_optimization,
            // playlist
            commands::playlist::list_playlists,
            commands::playlist::get_playlist,
            commands::playlist::create_playlist,
            commands::playlist::update_playlist,
            commands::playlist::delete_playlist,
            commands::playlist::add_to_playlist,
            commands::playlist::reorder_playlist,
            commands::playlist::set_item_enabled,
            commands::playlist::remove_playlist_item,
            // schedule
            commands::schedule::list_schedules,
            commands::schedule::create_schedule,
            commands::schedule::update_schedule,
            commands::schedule::delete_schedule,
            // streaming
            commands::streaming::get_status,
            commands::streaming::run_preflight,
            commands::streaming::start_broadcast,
            commands::streaming::stop_broadcast,
            commands::streaming::start_dry_run,
            commands::streaming::stream_mode_label,
            commands::streaming::simulate_ffmpeg_crash,
            // settings
            commands::settings::get_settings,
            commands::settings::set_setting,
            commands::settings::set_stream_key,
            commands::settings::reveal_stream_key,
            commands::settings::clear_stream_key,
            commands::settings::get_license,
            commands::settings::install_license,
            // system
            commands::system::get_metrics,
            commands::system::recent_events,
            commands::system::read_log,
            commands::system::logs_directory,
            commands::system::clear_cache,
            commands::system::cache_in_use_count,
            commands::system::take_startup_notice,
            commands::system::uptime_warnings,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            let paths = AppPaths::default_for_os();
            let state = AppState::build(handle.clone(), paths)?;
            state.logger.info(LogTarget::App, &format!("Louver Live {} 시작", louver_core::VERSION));

            // Keep the "launch at startup" setting and the OS in agreement (§21).
            let autostart = platform::autostart::TauriAutostart::new(handle.clone());
            let want = state.db.get_setting_or(settings_keys::LAUNCH_AT_STARTUP, "false") == "true";
            if want != autostart.is_enabled() {
                let _ = if want { autostart.enable() } else { autostart.disable() };
            }

            // Clean up an FFmpeg orphaned by a crash, then decide whether to
            // resume the previous broadcast (§32, §33).
            {
                let mut rt = state.runtime.lock().unwrap();
                if let Some(pid) = rt.clean_orphan_process() {
                    state.logger.warn(LogTarget::App, &format!("이전 실행의 FFmpeg({pid})를 정리했습니다"));
                }
                if let Some(notice) = rt.recover_on_startup() {
                    *state.startup_notice.lock().unwrap() = Some(notice);
                }
            }

            let start_minimized = state.db.get_setting_or(settings_keys::START_MINIMIZED, "false") == "true";
            app.manage(state);

            build_tray(app.handle())?;

            if start_minimized {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }

            // The broadcast loop. It runs regardless of whether a window is
            // visible, which is what lets a minimized app keep broadcasting
            // (§21).
            let tick_handle = handle.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(TICK);
                if let Some(s) = tick_handle.try_state::<AppState>() {
                    if let Ok(mut rt) = s.runtime.lock() {
                        rt.tick();
                    }
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            // §22: closing the window while broadcasting hides it instead of
            // quitting. The webview shows the three-way dialog.
            if let WindowEvent::CloseRequested { api, .. } = event {
                let broadcasting = window
                    .app_handle()
                    .try_state::<AppState>()
                    .map(|s| s.runtime.lock().map(|r| r.is_active()).unwrap_or(false))
                    .unwrap_or(false);
                let minimize_to_tray = window
                    .app_handle()
                    .try_state::<AppState>()
                    .map(|s| s.db.get_setting_or(settings_keys::MINIMIZE_TO_TRAY, "true") == "true")
                    .unwrap_or(true);

                if broadcasting || minimize_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                    if broadcasting {
                        use tauri::Emitter;
                        let _ = window.emit("louver://close-while-live", ());
                    }
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to build the Louver Live application")
        .run(|app, event| {
            // Never leave FFmpeg behind when the process really does exit (§33).
            if let tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit = event {
                if let Some(s) = app.try_state::<AppState>() {
                    if let Ok(mut rt) = s.runtime.lock() {
                        let _ = rt.stop(true);
                    }
                    let _ = s.sleep.allow_sleep();
                }
            }
        });
}

/// System tray (§22).
fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Louver Live 열기", true, None::<&str>)?;
    let status = MenuItem::with_id(app, "status", "방송 상태", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "방송 종료", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "프로그램 종료", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &status, &stop, &quit])?;

    TrayIconBuilder::with_id("louver-tray")
        .icon(app.default_window_icon().cloned().expect("bundled window icon"))
        .tooltip("Louver Live")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" | "status" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                    if event.id().as_ref() == "status" {
                        use tauri::Emitter;
                        let _ = w.emit("louver://navigate", "dashboard");
                    }
                }
            }
            "stop" => {
                if let Some(s) = app.try_state::<AppState>() {
                    if let Ok(mut rt) = s.runtime.lock() {
                        let _ = rt.stop(true);
                    }
                }
            }
            "quit" => {
                if let Some(s) = app.try_state::<AppState>() {
                    if let Ok(mut rt) = s.runtime.lock() {
                        let _ = rt.stop(true);
                    }
                }
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}
