//! The real HTTP transport.
//!
//! Blocking, because everything around it is: the app has no async runtime and
//! introducing one for five API calls would be a poor trade.

use super::api::HttpClient;
use super::oauth::{ClientCredentials, TokenEndpoint, TokenResponse, TOKEN_ENDPOINT};
use crate::error::{ErrorCode, LouverError, Result};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Default)]
pub struct UreqClient {
    /// Overridden by tests to reach a local fake token endpoint.
    pub token_endpoint: Option<String>,
}

impl UreqClient {
    pub fn new() -> Self {
        Self::default()
    }

    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .user_agent(concat!("LouverLive/", env!("CARGO_PKG_VERSION")))
            // A 403 from Google is an answer, and its body says *why* — chat
            // disabled, chat ended, rate limited, quota gone. Left as the
            // default, ureq raises those as errors and drops the body, and
            // every one of them would reach the user as "연결하지 못했습니다".
            .http_status_as_error(false)
            .build()
            .into()
    }
}

fn transport_error(e: impl std::fmt::Display) -> LouverError {
    LouverError::with_detail(ErrorCode::YoutubeApiFailed, format!("네트워크 오류: {e}"))
}

impl HttpClient for UreqClient {
    fn request(
        &self,
        method: &str,
        url: &str,
        bearer: &str,
        body: Option<serde_json::Value>,
    ) -> Result<(u16, String)> {
        let agent = Self::agent();
        let auth = format!("Bearer {bearer}");

        let result = match (method, body) {
            ("GET", _) => agent.get(url).header("Authorization", &auth).call(),
            (m, Some(b)) => {
                let req = match m {
                    "POST" => agent.post(url),
                    "PUT" => agent.put(url),
                    _ => agent.post(url),
                };
                req.header("Authorization", &auth).header("Content-Type", "application/json").send_json(&b)
            }
            (m, None) => {
                let req = if m == "POST" { agent.post(url) } else { agent.put(url) };
                req.header("Authorization", &auth).send_empty()
            }
        };

        match result {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                let text = resp.body_mut().read_to_string().unwrap_or_default();
                Ok((status, text))
            }
            // Kept as a belt-and-braces path: with `http_status_as_error`
            // off this should not occur, and if it ever does the status is
            // still more useful than a transport error.
            Err(ureq::Error::StatusCode(code)) => Ok((code, String::new())),
            Err(e) => Err(transport_error(e)),
        }
    }
}

impl TokenEndpoint for UreqClient {
    fn exchange_code(
        &self,
        creds: &ClientCredentials,
        code: &str,
        redirect_uri: &str,
    ) -> Result<TokenResponse> {
        self.post_token(&[
            ("code", code),
            ("client_id", &creds.client_id),
            ("client_secret", &creds.client_secret),
            ("redirect_uri", redirect_uri),
            ("grant_type", "authorization_code"),
        ])
    }

    fn refresh(&self, creds: &ClientCredentials, refresh_token: &str) -> Result<TokenResponse> {
        self.post_token(&[
            ("refresh_token", refresh_token),
            ("client_id", &creds.client_id),
            ("client_secret", &creds.client_secret),
            ("grant_type", "refresh_token"),
        ])
    }
}

impl UreqClient {
    fn post_token(&self, form: &[(&str, &str)]) -> Result<TokenResponse> {
        let url = self.token_endpoint.as_deref().unwrap_or(TOKEN_ENDPOINT);
        let agent = Self::agent();
        let result = agent
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send(&form_encode(form));

        match result {
            Ok(mut resp) => {
                let text = resp.body_mut().read_to_string().unwrap_or_default();
                serde_json::from_str(&text).map_err(|_| {
                    // The body of a token failure names the reason, and it is
                    // almost always an expired or revoked grant.
                    LouverError::with_detail(ErrorCode::YoutubeAuthExpired, first_line_of_error(&text))
                })
            }
            Err(ureq::Error::StatusCode(code)) => Err(LouverError::with_detail(
                ErrorCode::YoutubeAuthExpired,
                format!("토큰 요청이 거부되었습니다 (HTTP {code})"),
            )),
            Err(e) => Err(transport_error(e)),
        }
    }
}

fn form_encode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", super::oauth::urlencode(k), super::oauth::urlencode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn first_line_of_error(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error_description"].as_str().or_else(|| v["error"].as_str()).map(str::to_string))
        .unwrap_or_else(|| body.chars().take(120).collect())
}
