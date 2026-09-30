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

pub mod admin;
pub mod billing;
pub mod credentials;
pub mod db;
pub mod entitlement;
pub mod ingest;
pub mod manager;
pub mod media_audit;
pub mod models;
pub mod schedule;
pub mod storage;
pub mod youtube;

pub use db::CloudDb;
pub use models::*;
pub use storage::Storage;

/// A limit, in words a user can act on.
///
/// The default used to be `max_storage_bytes 한도를 초과했습니다
/// (5368709120/5368709120)` — a database key and two byte counts. Storage is
/// the limit a beta student is most likely to meet, so it says what to do about
/// it instead — and which of the two storage limits it was, because "one file
/// too big" and "account full" are fixed differently.
fn limit_message(limit: &str, plan_label: &str, used: i64, allowed: i64) -> String {
    match limit {
        // The unsubscribed plan allows nothing; "0GB를 초과" would read as a bug.
        entitlement::MAX_STORAGE_BYTES | entitlement::MAX_UPLOAD_BYTES if allowed <= 0 => {
            "영상을 올리려면 요금제가 필요합니다.".to_string()
        }
        entitlement::MAX_STORAGE_BYTES => format!(
            "{plan_label} 플랜 저장공간 {}를 초과합니다 (사용 중 {}). 사용하지 않는 영상을 삭제한 뒤 다시 올려주세요.",
            gb(allowed),
            gb(used)
        ),
        entitlement::MAX_UPLOAD_BYTES => format!(
            "파일 크기가 {plan_label} 플랜의 파일당 최대 용량 {}를 초과했습니다 (파일 {}).",
            gb(allowed),
            gb(used)
        ),
        entitlement::MAX_BROADCASTS => {
            format!("이 요금제에서는 방송을 {allowed}개까지 만들 수 있습니다.")
        }
        other => format!("{other} 한도를 초과했습니다 ({used}/{allowed})"),
    }
}

/// Bytes as the product writes them: GiB, called "GB", whole when it is whole.
fn gb(b: i64) -> String {
    let g = b as f64 / 1_073_741_824.0;
    if b % 1_073_741_824 == 0 {
        format!("{g:.0}GB")
    } else {
        format!("{g:.1}GB")
    }
}

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
    /// `plan_label` is only for the message: which plan's ceiling it was.
    #[error("{}", limit_message(limit, plan_label, *used, *allowed))]
    LimitReached { limit: &'static str, plan_label: String, used: i64, allowed: i64 },
    /// The concurrency ceiling, which is the entitlement that distinguishes the
    /// paid plans and therefore the one a user meets most often. Separate from
    /// `LimitReached` so the message can name their plan and their number
    /// instead of a database key.
    #[error("{plan_label} 요금제에서는 동시에 {allowed}개의 방송을 송출할 수 있습니다")]
    ConcurrencyReached { plan_label: String, used: i64, allowed: i64 },
    /// No subscription, or one that is no longer active.
    #[error("방송을 시작하려면 활성화된 요금제가 필요합니다")]
    NoSubscription,
    /// The server's own disk, not the user's plan. Separate from
    /// `LimitReached` because paying more would not help and the operator is
    /// the one who has to act.
    #[error("현재 서버 저장공간이 부족하여 업로드할 수 없습니다. 잠시 후 다시 시도해 주세요.")]
    OutOfSpace,
    /// Too many attempts from one caller in a short time. Not about this
    /// account — about the machine, which has two cores and a password hash
    /// that costs a third of one.
    #[error("시도가 너무 많습니다. 잠시 후 다시 시도해 주세요.")]
    TooManyAttempts,
    /// Signed in, and not an operator.
    ///
    /// Separate from [`CloudError::Forbidden`], which answers 404 on purpose so
    /// that one user cannot probe another's ids. The admin surface is not a
    /// resource somebody might own: a plain 403 is the honest answer, and it is
    /// what an operator wants to see in a log.
    #[error("관리자 권한이 필요합니다")]
    NotAdmin,
    /// The account has been switched off by an operator.
    #[error("이 계정은 사용이 중지되었습니다. 관리자에게 문의해 주세요.")]
    Disabled,
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
        // The detail is where FFmpeg's own stderr lives. Dropping it leaves a
        // server operator with "방송 준비에 실패했습니다" and nothing to act on,
        // which is exactly the position §17 asks us not to put them in. It is
        // already masked by the core on its way here.
        match e.detail.as_deref().filter(|d| !d.trim().is_empty()) {
            Some(detail) => Self::Engine(format!("{} ({detail})", e.message)),
            None => Self::Engine(e.message),
        }
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
