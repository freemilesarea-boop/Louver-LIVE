//! # Louver Live core
//!
//! All of the product's behaviour lives here, deliberately free of any Tauri or
//! UI dependency (§68). The desktop crate is a thin IPC layer over this API,
//! which is what lets the whole engine be unit- and integration-tested on any
//! platform, including CI machines with no GUI stack.
//!
//! ## The central design decision
//!
//! Heavy encoding happens once, at import time ([`media::normalize`]). The live
//! path ([`streaming`]) remuxes pre-normalized files through FFmpeg's concat
//! demuxer with `-c copy` and `-stream_loop -1`, so a 24-hour broadcast runs
//! with no video encoder in the process at all (§2, §41). This was validated
//! before the architecture was settled — see `tests/media_pipeline.rs`.

#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(clippy::all)]

pub mod clock;
pub mod config;
pub mod database;
pub mod error;
pub mod logging;
pub mod media;
pub mod runtime;
pub mod scheduler;
pub mod security;
pub mod session;
pub mod streaming;
pub mod system;
pub mod youtube;

pub use config::{OutputProfile, StreamMode, DEFAULT_LOCAL_TEST_URL, DEFAULT_RTMPS_URL};
pub use error::{ErrorCode, LouverError, Result};
pub use streaming::playlist::PlaybackMode;
pub use streaming::state::StreamState;

pub const APP_NAME: &str = "Louver Live";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Standard locations under the application data directory.
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub data_dir: std::path::PathBuf,
}

impl AppPaths {
    pub fn new(data_dir: impl Into<std::path::PathBuf>) -> Self {
        Self { data_dir: data_dir.into() }
    }

    pub fn default_for_os() -> Self {
        Self::new(security::default_app_data_dir())
    }

    pub fn database(&self) -> std::path::PathBuf {
        self.data_dir.join("louver.db")
    }
    pub fn cache_dir(&self) -> std::path::PathBuf {
        self.data_dir.join("cache")
    }
    pub fn logs_dir(&self) -> std::path::PathBuf {
        self.data_dir.join("logs")
    }
    pub fn session_file(&self) -> std::path::PathBuf {
        self.data_dir.join("session.json")
    }
    pub fn manifest_file(&self) -> std::path::PathBuf {
        self.data_dir.join("current-playlist.txt")
    }
    pub fn dry_run_dir(&self) -> std::path::PathBuf {
        self.data_dir.join("dry-run")
    }

    pub fn ensure(&self) -> Result<()> {
        for d in [self.data_dir.clone(), self.cache_dir(), self.logs_dir(), self.dry_run_dir()] {
            std::fs::create_dir_all(d)?;
        }
        self.discard_obsolete_license();
        Ok(())
    }

    /// Remove the licence file older versions kept here. Never fails.
    ///
    /// Louver Live had an in-app licence gate: a signed `license.json` had to
    /// be present or a broadcast was refused. Who may use the program is now
    /// decided before the installer changes hands, so the file means nothing
    /// and nothing reads it. It is deleted because leaving a file that looks
    /// like a credential in a user's data directory invites the question of
    /// what it is for.
    ///
    /// Deliberately infallible. A read-only directory, a file held open by a
    /// backup tool, a permission a user changed years ago — none of that is a
    /// reason to refuse to start, because the file is already irrelevant. It
    /// is tried once per launch and forgotten.
    fn discard_obsolete_license(&self) {
        let _ = std::fs::remove_file(self.data_dir.join("license.json"));
    }
}

/// Settings keys stored in the `settings` table.
pub mod settings_keys {
    pub const RTMPS_URL: &str = "rtmps_url";
    /// Where "로컬 방송 테스트" publishes when an endpoint is listening there.
    pub const LOCAL_TEST_URL: &str = "local_test_url";
    pub const OUTPUT_PROFILE: &str = "output_profile";
    pub const LAUNCH_AT_STARTUP: &str = "launch_at_startup";
    pub const START_MINIMIZED: &str = "start_minimized";
    pub const MINIMIZE_TO_TRAY: &str = "minimize_to_tray";
    pub const STREAM_MODE: &str = "stream_mode";
    pub const AUTO_RECONNECT: &str = "auto_reconnect";
    pub const CACHE_LOCATION: &str = "cache_location";
    pub const DEVELOPER_MODE: &str = "developer_mode";
    pub const HARDWARE_ENCODER: &str = "hardware_encoder";
    pub const ACTIVE_PLAYLIST: &str = "active_playlist";
    pub const FIRST_RUN_COMPLETE: &str = "first_run_complete";
    pub const WARNED_ABOUT_UPTIME: &str = "warned_about_uptime";
    /// Whether this computer is watching the clock for scheduled broadcasts.
    ///
    /// Separate from a schedule's own enabled flag: saving a rule is not the
    /// same as switching the machine on, and the difference decides whether
    /// anything happens tonight.
    pub const SCHEDULER_ARMED: &str = "scheduler_armed";
    /// Whether a relaunch puts the scheduler back the way the user left it.
    pub const SCHEDULER_RESTORE_ON_LAUNCH: &str = "scheduler_restore_on_launch";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_paths_are_all_under_the_data_dir() {
        let p = AppPaths::new("/data/LouverLive");
        for path in [p.database(), p.cache_dir(), p.logs_dir(), p.session_file(), p.manifest_file()] {
            assert!(path.starts_with("/data/LouverLive"), "{path:?} escaped the data dir");
        }
        assert!(p.database().ends_with("louver.db"));
        assert!(p.logs_dir().ends_with("logs"));
    }

    /// §9: a licence file left by an older install changes nothing.
    #[test]
    fn a_leftover_licence_file_is_removed_and_never_read() {
        let d = tempfile::tempdir().unwrap();
        let p = AppPaths::new(d.path().join("LouverLive"));
        std::fs::create_dir_all(&p.data_dir).unwrap();
        let stale = p.data_dir.join("license.json");
        std::fs::write(&stale, b"{\"payload\":{},\"signature\":\"not-a-signature\"}").unwrap();

        p.ensure().expect("a leftover licence must not stop the app starting");
        assert!(!stale.exists(), "the obsolete licence file is still there");
    }

    /// And a licence file that cannot be deleted changes nothing either.
    #[test]
    fn a_licence_file_that_will_not_delete_does_not_stop_startup() {
        let d = tempfile::tempdir().unwrap();
        let p = AppPaths::new(d.path().join("LouverLive"));
        std::fs::create_dir_all(&p.data_dir).unwrap();
        // A directory by that name cannot be removed with remove_file, which
        // stands in for every reason a delete fails on a real machine.
        std::fs::create_dir(p.data_dir.join("license.json")).unwrap();

        p.ensure().expect("a licence file that resists deletion must not stop startup");
    }

    #[test]
    fn ensure_creates_every_directory() {
        let d = tempfile::tempdir().unwrap();
        let p = AppPaths::new(d.path().join("LouverLive"));
        p.ensure().unwrap();
        assert!(p.cache_dir().is_dir());
        assert!(p.logs_dir().is_dir());
        assert!(p.dry_run_dir().is_dir());
    }

    #[test]
    fn version_is_exposed() {
        assert!(!VERSION.is_empty());
        assert_eq!(APP_NAME, "Louver Live");
    }
}
