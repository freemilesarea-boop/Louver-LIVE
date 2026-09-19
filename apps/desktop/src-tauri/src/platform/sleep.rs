//! Sleep prevention while broadcasting (§23).
//!
//! Deliberately does *not* try to defeat a closed laptop lid: §23 says the UI
//! must be honest about that instead of working around it.

use louver_core::error::Result;
use louver_core::system::SleepPreventer;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Default)]
pub struct OsSleepPreventer {
    active: AtomicBool,
}

impl OsSleepPreventer {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SleepPreventer for OsSleepPreventer {
    fn prevent_sleep(&self, _reason: &str) -> Result<()> {
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::System::Power::{
                SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
            };
            SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED);
        }
        #[cfg(target_os = "macos")]
        {
            // `caffeinate` is part of macOS and needs no entitlement. The child
            // is tied to our pid, so it exits with the app even on a crash.
            let _ = std::process::Command::new("/usr/bin/caffeinate")
                .args(["-dimsu", "-w", &std::process::id().to_string()])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
        self.active.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn allow_sleep(&self) -> Result<()> {
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS};
            SetThreadExecutionState(ES_CONTINUOUS);
        }
        #[cfg(target_os = "macos")]
        {
            // The caffeinate child is bound to our pid; killing it by name
            // would hit other apps' instances, so let it exit on its own when
            // we do, and simply drop the assertion flag here.
            let _ = std::process::Command::new("/usr/bin/pkill")
                .args(["-f", &format!("caffeinate -dimsu -w {}", std::process::id())])
                .status();
        }
        self.active.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn is_preventing(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggling_updates_the_flag_and_restores_the_power_policy() {
        let p = OsSleepPreventer::new();
        assert!(!p.is_preventing());
        p.prevent_sleep("방송 중").unwrap();
        assert!(p.is_preventing());
        p.allow_sleep().unwrap();
        assert!(!p.is_preventing(), "§23: power policy must be restored after a broadcast");
    }
}
