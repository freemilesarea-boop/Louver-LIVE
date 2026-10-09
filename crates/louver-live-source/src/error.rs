//! One error type, and nothing in it that could be a secret.
//!
//! Every message here is built from a URL that has already passed
//! [`crate::resolver::classify`] (so it is a YouTube watch address and nothing
//! else), from FFmpeg's or yt-dlp's own words, or from a number. No token,
//! stream key or ingest URL is ever a parameter to any of these constructors —
//! the destination is held by the worker and never reaches an error.

use std::fmt;

/// What went wrong, in the categories a caller acts on differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The address itself cannot be used. The user has to change it.
    Invalid,
    /// A real YouTube video, but not live (or no longer live).
    NotLive,
    /// Live, but this server cannot reach or read it.
    Unavailable,
    /// The resolver could not be run at all — missing binary, timeout.
    ResolverFailed,
    /// Refused on purpose: too many workers, or a cap was hit.
    Limit,
    /// No usable credential. The caller has to authenticate.
    Unauthorized,
    /// Authenticated, but this is not theirs. Reported the same way as
    /// `NotFound` over the wire, so one user cannot probe for another's jobs.
    Forbidden,
    /// No such job for this caller.
    NotFound,
    /// The request contradicts the current state.
    Conflict,
    /// FFmpeg could not be started, or died in a way a restart will not fix.
    Ffmpeg,
}

impl ErrorKind {
    /// A short, stable token for logs and tests.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::NotLive => "not_live",
            Self::Unavailable => "unavailable",
            Self::ResolverFailed => "resolver_failed",
            Self::Limit => "limit",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::Ffmpeg => "ffmpeg",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LiveSourceError {
    pub kind: ErrorKind,
    /// What the user reads. Korean, and it names the actual problem.
    pub message: String,
}

impl LiveSourceError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Invalid, message)
    }
    pub fn not_live(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotLive, message)
    }
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unavailable, message)
    }
    pub fn resolver(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::ResolverFailed, message)
    }
    pub fn limit(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Limit, message)
    }
    pub fn ffmpeg(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Ffmpeg, message)
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unauthorized, message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Conflict, message)
    }
}

impl fmt::Display for LiveSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for LiveSourceError {}

pub type Result<T> = std::result::Result<T, LiveSourceError>;
