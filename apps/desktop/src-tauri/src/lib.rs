//! Louver Live desktop shell.
//!
//! This crate owns windows, the tray and the IPC surface. Every decision about
//! broadcasting lives in `louver-core` (§68), so this file is mostly wiring.

pub mod commands;
pub mod platform;
pub mod state;
pub mod youtube_service;

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

/// Keep the chat bot attached to whatever is on air, and to nothing else.
///
/// Called once a second from the broadcast loop, and never blocks it: the
/// lookup that needs the network runs on its own thread, and at most one runs
/// at a time.
///
/// Metadata is *not* applied here. It used to be, gated on the stream already
/// being LIVE, which meant YouTube went live under the channel's default title
/// and the user's title arrived seconds later if at all. It now happens in the
/// runtime's pre-start hook, before FFmpeg connects. What is left here is the
/// chat bot, which genuinely cannot start earlier: `activeLiveChatId` does not
/// exist until the broadcast is live.
fn youtube_follow_broadcast(state: &AppState, live: bool, broadcasting: bool) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WORKING: AtomicBool = AtomicBool::new(false);
    /// One transition attempt at a time; the tick runs every second and the
    /// stream can take a while to register as active.
    static GOING_LIVE: AtomicBool = AtomicBool::new(false);

    if !live {
        if state.youtube.bot_broadcast_id().is_some() {
            state.youtube.stop_bot();
        }
        // Only once the broadcast is really over. A dropped connection is not
        // the end of one: clearing here on every non-LIVE tick would wipe the
        // record of what happened to the metadata the moment the stream went
        // into RECONNECTING, which is exactly when the user goes looking.
        if !broadcasting {
            // End the broadcast on the channel too, so nothing is left `live`
            // with nothing publishing to it — which is exactly the stale state
            // the next window would try to reuse. Off the tick, because it
            // calls Google.
            if state.youtube.provisioned().is_some() {
                let youtube = std::sync::Arc::clone(&state.youtube);
                std::thread::spawn(move || youtube.finish_broadcast());
            }
            state.youtube.reset_live_session();
        }
        return;
    }
    if !state.youtube.status().connected {
        return;
    }

    // Take the provisioned broadcast live, now that FFmpeg is publishing and
    // YouTube's ingestion stream has something to report. Asking before the
    // stream is active is refused, so `try_go_live` checks the stream first.
    if state.youtube.provisioned().is_some_and(|p| !p.went_live) {
        if GOING_LIVE.swap(true, Ordering::SeqCst) {
            return;
        }
        let youtube = std::sync::Arc::clone(&state.youtube);
        std::thread::spawn(move || {
            if let Err(e) = youtube.try_go_live() {
                youtube.note_go_live_failure(&e);
            }
            GOING_LIVE.store(false, Ordering::SeqCst);
        });
        return;
    }
    if !state.youtube.chat_settings().enabled || state.youtube.bot_is_running() {
        return;
    }
    // The chat bot is the heaviest spender there is — a message every interval,
    // all day. With the day's free allowance gone it does not start, and the
    // broadcast carries on without it.
    if state.youtube.quota_exhausted() {
        return;
    }
    if WORKING.swap(true, Ordering::SeqCst) {
        return; // one round at a time
    }

    let youtube = std::sync::Arc::clone(&state.youtube);
    let messages = state.db.list_chat_messages().unwrap_or_default();
    std::thread::spawn(move || {
        // Looked up fresh every time, so the chat id belongs to the broadcast
        // that is on air now (§7) — never the one before it.
        if let Ok(b) = youtube.current_broadcast() {
            youtube.start_bot_for(&b.id, messages);
        }
        WORKING.store(false, Ordering::SeqCst);
    });
}

/// Runs the YouTube side of a broadcast before FFmpeg is launched.
///
/// Installed on the runtime, so the manual Start button and the scheduler go
/// through it identically (§B-7).
#[derive(Debug)]
struct YoutubePreStart(std::sync::Arc<youtube_service::YoutubeService>);

impl louver_core::runtime::PreStartHook for YoutubePreStart {
    fn before_stream(&self, opts: &louver_core::runtime::StartOptions) -> louver_core::error::Result<()> {
        // A manual start can be asked what to do about a failure; a scheduled
        // one cannot, so it follows the policy set in advance.
        let scheduled = opts.reason != louver_core::runtime::StartReason::Manual;
        // The window a created broadcast is scheduled for. A manual start has
        // none and uses now, which is what it is.
        let window = opts.occurrence.as_ref().map(|o| {
            (louver_core::runtime::local_to_utc(o.start), Some(louver_core::runtime::local_to_utc(o.end)))
        });
        self.0.prepare_for_broadcast_reason(scheduled, window)
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .invoke_handler(tauri::generate_handler![
            // media
            commands::media::import_media,
            commands::media::add_media,
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
            commands::schedule::scheduler_status,
            commands::schedule::scheduler_arm,
            commands::schedule::scheduler_disarm,
            commands::schedule::scheduler_set_restore,
            // streaming
            commands::streaming::get_status,
            commands::streaming::run_preflight,
            commands::streaming::start_broadcast,
            commands::streaming::stop_broadcast,
            commands::streaming::start_dry_run,
            commands::streaming::stream_mode_label,
            commands::streaming::stream_diagnostics,
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
            // youtube (V2)
            commands::youtube::youtube_status,
            commands::youtube::youtube_set_credentials,
            commands::youtube::youtube_begin_connect,
            commands::youtube::youtube_switch_account,
            commands::youtube::youtube_disconnect,
            commands::youtube::youtube_get_metadata,
            commands::youtube::youtube_save_metadata,
            commands::youtube::youtube_apply_metadata,
            commands::youtube::youtube_set_apply_on_start,
            commands::youtube::youtube_apply_state,
            commands::youtube::youtube_quota,
            commands::youtube::youtube_set_schedule_holds,
            commands::youtube::youtube_schedule_holds,
            commands::youtube::youtube_apply_plan,
            commands::youtube::youtube_current_broadcast,
            commands::youtube::youtube_list_presets,
            commands::youtube::youtube_save_preset,
            commands::youtube::youtube_delete_preset,
            commands::youtube::chat_list_messages,
            commands::youtube::chat_add_message,
            commands::youtube::chat_update_message,
            commands::youtube::chat_delete_message,
            commands::youtube::chat_reorder_messages,
            commands::youtube::chat_get_settings,
            commands::youtube::chat_save_settings,
            commands::youtube::chat_status,
            commands::youtube::chat_min_interval_secs,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            let paths = AppPaths::default_for_os();
            let state = AppState::build(handle.clone(), paths)?;
            state.logger.info(LogTarget::App, &format!("Louver Live {} 시작", louver_core::VERSION));

            // Whether the OAuth client is configured, and nothing about what
            // it is. Google answering "client_secret is missing" while the
            // operator is certain they set the variable is a question these
            // two lines settle in one look — and neither can leak a value,
            // because neither has one to leak.
            {
                let (id, secret) = louver_core::youtube::oauth::credential_presence();
                let word = |b: bool| if b { "configured" } else { "missing" };
                state.logger.info(LogTarget::App, &format!("OAuth Client ID: {}", word(id)));
                state.logger.info(LogTarget::App, &format!("OAuth Client Secret: {}", word(secret)));
            }

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
                // Installed before recovery, so a broadcast resumed at launch
                // gets its metadata applied like any other (§B-7).
                rt.set_pre_start(std::sync::Arc::new(YoutubePreStart(std::sync::Arc::clone(&state.youtube))));
                // Put the scheduler back the way the user left it, before
                // recovery runs — a machine that was watching the clock when
                // it was shut down should be watching it again, and one the
                // user deliberately stopped must stay stopped.
                rt.restore_armed_state();
                state.logger.info(
                    LogTarget::App,
                    if rt.is_armed() {
                        "SCHEDULER_ARMED: 이전 상태를 복원했습니다"
                    } else {
                        "SCHEDULER_STOPPED: 예약 감시가 꺼져 있습니다"
                    },
                );
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
                    let (live, broadcasting) = if let Ok(mut rt) = s.runtime.lock() {
                        rt.tick();
                        let real = !rt.status().dry_run;
                        (
                            rt.state() == louver_core::streaming::state::StreamState::Live && real,
                            rt.is_active() && real,
                        )
                    } else {
                        (false, false)
                    };
                    // The chat bot follows the broadcast (§7). Starting it is
                    // handed to another thread because finding the broadcast
                    // means calling Google, and this loop must never wait on
                    // the network — it is the loop that keeps FFmpeg alive.
                    youtube_follow_broadcast(&s, live, broadcasting);
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
