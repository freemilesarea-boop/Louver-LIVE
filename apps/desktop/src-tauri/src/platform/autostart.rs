//! Launch at login (§21), delegating to the Tauri autostart plugin.

use louver_core::error::{ErrorCode, LouverError, Result};
use louver_core::system::AutostartManager;
use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;

#[derive(Debug)]
pub struct TauriAutostart {
    app: AppHandle,
}

impl TauriAutostart {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

fn wrap(e: impl std::fmt::Display) -> LouverError {
    LouverError::with_detail(ErrorCode::ConfigInvalid, e.to_string())
}

impl AutostartManager for TauriAutostart {
    fn enable(&self) -> Result<()> {
        self.app.autolaunch().enable().map_err(wrap)
    }
    fn disable(&self) -> Result<()> {
        self.app.autolaunch().disable().map_err(wrap)
    }
    fn is_enabled(&self) -> bool {
        self.app.autolaunch().is_enabled().unwrap_or(false)
    }
}
