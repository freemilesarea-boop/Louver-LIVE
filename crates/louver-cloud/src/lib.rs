//! Louver Live Cloud: many users, many broadcasts, one engine.
//!
//! This crate is everything the desktop application does not need — accounts,
//! plans, uploads, and a manager that runs several broadcasts at once — and
//! nothing the desktop already does. The broadcasting itself is
//! [`louver_core`], unchanged: the same `BroadcastRuntime`, the same
//! `StreamSupervisor` and its tested restart policy, the same FFmpeg argv.
//!
//! The rule this crate holds to: if `louver-core` already does something, call
//! it. Replacing tested code with untested code is not a port.

pub mod credentials;
pub mod db;
pub mod entitlement;
pub mod manager;
pub mod models;
pub mod storage;

pub use db::CloudDb;
pub use models::*;
pub use storage::Storage;

/// Anything that can go wrong at the cloud layer.
///
/// Deliberately separate from [`louver_core::error::LouverError`]: that enum is
/// the vocabulary the desktop UI shows a broadcaster, and adding HTTP concerns
/// to it would put words in front of users that mean nothing to them.
#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error("{0}")]
    NotFound(&'static str),
    #[error("not permitted")]
    Forbidden,
    #[error("{0}")]
    Invalid(String),
    #[error("{limit} 한도를 초과했습니다 ({used}/{allowed})")]
    LimitReached { limit: &'static str, used: i64, allowed: i64 },
    #[error("이미 사용 중인 이메일입니다")]
    EmailTaken,
    #[error("이메일 또는 비밀번호가 올바르지 않습니다")]
    BadCredentials,
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("engine: {0}")]
    Engine(String),
    #[error("crypto")]
    Crypto,
}

impl From<louver_core::error::LouverError> for CloudError {
    fn from(e: louver_core::error::LouverError) -> Self {
        Self::Engine(e.message)
    }
}

pub type Result<T> = std::result::Result<T, CloudError>;

/// A short, unguessable public identifier.
///
/// Ownership is always checked server-side, so this is not a security control
/// — it simply keeps sequential integers out of URLs.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
