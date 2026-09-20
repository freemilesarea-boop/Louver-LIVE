//! Tauri IPC commands. These are a thin translation layer: all behaviour lives
//! in `louver-core` (§68).

pub mod media;
pub mod playlist;
pub mod schedule;
pub mod settings;
pub mod streaming;
pub mod system;
pub mod youtube;

/// Errors cross the IPC boundary as the structured [`LouverError`], so the UI
/// can show a Korean message and keep the technical detail behind a disclosure
/// (§35).
pub type CmdResult<T> = std::result::Result<T, louver_core::LouverError>;
