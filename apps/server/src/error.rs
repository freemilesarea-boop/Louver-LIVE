//! Turning a `CloudError` into a status code and a body, once.
//!
//! Handlers return `Result<_, ApiError>` and never build a response by hand, so
//! a new error variant cannot accidentally become a 200 or leak a detail.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use louver_cloud::CloudError;

pub struct ApiError(pub CloudError);

impl From<CloudError> for ApiError {
    fn from(e: CloudError) -> Self {
        Self(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (code, body) = match &self.0 {
            CloudError::NotFound(_) => (StatusCode::NOT_FOUND, "찾을 수 없습니다".to_string()),
            // Deliberately the same answer as NotFound in spirit: telling a
            // caller "this exists but is not yours" is itself a fact about
            // someone else.
            CloudError::Forbidden => (StatusCode::NOT_FOUND, "찾을 수 없습니다".to_string()),
            CloudError::BadCredentials => (StatusCode::UNAUTHORIZED, self.0.to_string()),
            CloudError::EmailTaken => (StatusCode::CONFLICT, self.0.to_string()),
            CloudError::Invalid(_) => (StatusCode::BAD_REQUEST, self.0.to_string()),
            // 402 for all three: the caller has asked for something their plan
            // does not cover, and paying is what changes the answer.
            CloudError::LimitReached { .. }
            | CloudError::ConcurrencyReached { .. }
            | CloudError::NoSubscription => (StatusCode::PAYMENT_REQUIRED, self.0.to_string()),
            // 403, not 401: the credentials were right. What is refused is the
            // account, and saying so is what stops a support thread about a
            // password that "stopped working".
            CloudError::NotAdmin | CloudError::Disabled => (StatusCode::FORBIDDEN, self.0.to_string()),
            CloudError::TooManyAttempts => (StatusCode::TOO_MANY_REQUESTS, self.0.to_string()),
            // Nothing the caller did wrong and nothing paying would fix: the
            // server has no room. 507 rather than 500 so it is distinguishable
            // in a log without reading the message.
            CloudError::OutOfSpace => (StatusCode::INSUFFICIENT_STORAGE, self.0.to_string()),
            // Anything internal keeps its detail in the log, not in the body.
            CloudError::Db(_) | CloudError::Io(_) | CloudError::Crypto => {
                eprintln!("[louver] internal: {}", self.0);
                (StatusCode::INTERNAL_SERVER_ERROR, "서버 오류".to_string())
            }
            CloudError::Engine(m) => (StatusCode::BAD_REQUEST, m.clone()),
        };
        (code, Json(serde_json::json!({ "error": body }))).into_response()
    }
}

/// io and engine failures reach handlers directly often enough to be worth a
/// conversion of their own; both land on the same `CloudError` variants a
/// database call would have produced.
impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        Self(CloudError::Io(e))
    }
}

impl From<louver_core::error::LouverError> for ApiError {
    fn from(e: louver_core::error::LouverError) -> Self {
        Self(CloudError::from(e))
    }
}
