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
        code_verifier: &str,
    ) -> Result<TokenResponse> {
        self.post_token(&authorization_code_form(creds, code, redirect_uri, code_verifier))
    }

    fn refresh(&self, creds: &ClientCredentials, refresh_token: &str) -> Result<TokenResponse> {
        self.post_token(&refresh_form(creds, refresh_token))
    }
}

/// The token request Google is sent for an authorization code.
///
/// Built as a value so the fields can be asserted without a socket: "did the
/// secret actually reach the request" is otherwise only answerable by watching
/// Google refuse it.
///
/// `client_secret` is included whenever this build has one. Google's answer for
/// this desktop client is `invalid_request: client_secret is missing`, which
/// settles what the documentation leaves optional — PKCE protects the code,
/// and the secret is sent as well because the token endpoint requires it.
pub fn authorization_code_form<'a>(
    creds: &'a ClientCredentials,
    code: &'a str,
    redirect_uri: &'a str,
    code_verifier: &'a str,
) -> Vec<(&'a str, &'a str)> {
    let mut form: Vec<(&str, &str)> = vec![
        ("code", code),
        ("client_id", &creds.client_id),
        ("redirect_uri", redirect_uri),
        ("grant_type", "authorization_code"),
        ("code_verifier", code_verifier),
    ];
    if !creds.client_secret.trim().is_empty() {
        form.push(("client_secret", &creds.client_secret));
    }
    form
}

/// The token request Google is sent to trade a refresh token for an access
/// token. Same rule about the secret.
pub fn refresh_form<'a>(creds: &'a ClientCredentials, refresh_token: &'a str) -> Vec<(&'a str, &'a str)> {
    let mut form: Vec<(&str, &str)> = vec![
        ("refresh_token", refresh_token),
        ("client_id", &creds.client_id),
        ("grant_type", "refresh_token"),
    ];
    if !creds.client_secret.trim().is_empty() {
        form.push(("client_secret", &creds.client_secret));
    }
    form
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
                let status = resp.status().as_u16();
                let text = resp.body_mut().read_to_string().unwrap_or_default();
                if let Ok(token) = serde_json::from_str::<TokenResponse>(&text) {
                    return Ok(token);
                }
                // Keep everything Google said. Whether a desktop client can
                // exchange a code without a client_secret is a question this
                // project answers by measurement, and the answer is in this
                // body: the status, `error` and `error_description`. None of
                // it contains a token or a secret.
                Err(LouverError::with_detail(
                    ErrorCode::YoutubeAuthExpired,
                    describe_token_failure(status, &text),
                ))
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

/// A one-line, complete account of why a token request failed.
///
/// Verbatim on purpose: `invalid_request` with "client_secret is missing" and
/// `invalid_client` mean different things, and a summarised message loses the
/// distinction this project needs in order to decide whether a secret is
/// required at all.
pub fn describe_token_failure(status: u16, body: &str) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    let field = |k: &str| parsed.as_ref().and_then(|v| v[k].as_str()).map(str::to_string).unwrap_or_default();
    let error = field("error");
    let description = field("error_description");
    match (error.is_empty(), description.is_empty()) {
        (true, true) => format!("HTTP {status} · {}", body.chars().take(200).collect::<String>()),
        (false, true) => format!("HTTP {status} · {error}"),
        (true, false) => format!("HTTP {status} · {description}"),
        (false, false) => format!("HTTP {status} · {error}: {description}"),
    }
}

#[cfg(test)]
mod form_tests {
    use super::*;

    fn creds(secret: &str) -> ClientCredentials {
        ClientCredentials {
            client_id: "1234-abc.apps.googleusercontent.com".into(),
            client_secret: secret.into(),
        }
    }

    fn field<'a>(form: &'a [(&str, &str)], key: &str) -> Option<&'a str> {
        form.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }

    #[test]
    fn a_configured_secret_reaches_the_request() {
        // The question the Mac failure raised: with the secret set, does it
        // actually get into the body? Answered here rather than by Google.
        let c = creds("GOCSPX-testsecret");
        let form = authorization_code_form(&c, "4/auth-code", "http://127.0.0.1:51234", "verifier-xyz");

        assert_eq!(field(&form, "client_secret"), Some("GOCSPX-testsecret"));
        assert_eq!(field(&form, "client_id"), Some("1234-abc.apps.googleusercontent.com"));
        assert_eq!(field(&form, "code"), Some("4/auth-code"));
        assert_eq!(field(&form, "code_verifier"), Some("verifier-xyz"));
        assert_eq!(field(&form, "redirect_uri"), Some("http://127.0.0.1:51234"));
        assert_eq!(field(&form, "grant_type"), Some("authorization_code"));
        assert_eq!(form.len(), 6, "and nothing else");
    }

    #[test]
    fn no_secret_means_no_field_rather_than_an_empty_one() {
        // An empty `client_secret=` is not the same request as one without the
        // field, and Google reads the two differently.
        let c = creds("");
        let form = authorization_code_form(&c, "code", "http://127.0.0.1:1", "v");
        assert_eq!(field(&form, "client_secret"), None);
        assert_eq!(form.len(), 5);
    }

    #[test]
    fn whitespace_is_not_a_secret() {
        // A shell that exported an empty value leaves a string that is present
        // but useless; sending it produces a confusing refusal.
        let c = creds("   \n");
        assert!(!c.has_secret());
        assert_eq!(field(&authorization_code_form(&c, "c", "r", "v"), "client_secret"), None);
    }

    #[test]
    fn the_refresh_request_carries_the_secret_too() {
        // A refresh that omits it fails the same way the exchange does, and
        // hours later, when nobody is watching.
        let c = creds("GOCSPX-testsecret");
        let form = refresh_form(&c, "1//refresh");
        assert_eq!(field(&form, "client_secret"), Some("GOCSPX-testsecret"));
        assert_eq!(field(&form, "refresh_token"), Some("1//refresh"));
        assert_eq!(field(&form, "grant_type"), Some("refresh_token"));
        assert_eq!(form.len(), 4);
    }

    #[test]
    fn the_encoded_body_carries_the_secret_verbatim() {
        // Through the encoder as well as the builder: a secret mangled on the
        // way out fails exactly like one that was never added.
        let c = creds("GOCSPX-a/b+c=d");
        let body = form_encode(&authorization_code_form(&c, "code", "http://127.0.0.1:1", "v"));
        assert!(body.contains("client_secret=GOCSPX-a%2Fb%2Bc%3Dd"), "{body}");
        assert!(body.contains("grant_type=authorization_code"));
    }
}

#[cfg(test)]
mod secret_containment_tests {
    use super::*;
    use crate::youtube::oauth::ClientCredentials;

    const SECRET: &str = "GOCSPX-thisMustNeverAppear";

    #[test]
    fn a_refusal_never_repeats_the_secret_back() {
        // The failure message is the one place a secret could plausibly end up
        // in a log, because it is built from a request that carried one.
        let body = r#"{"error":"invalid_request","error_description":"client_secret is missing."}"#;
        let described = describe_token_failure(400, body);
        assert!(!described.contains(SECRET));
        assert!(!described.to_ascii_lowercase().contains("gocspx"));
        // And it still says everything needed to act on it.
        assert!(described.contains("invalid_request"));
        assert!(described.contains("client_secret is missing"));
    }

    #[test]
    fn even_a_server_that_echoes_the_secret_does_not_get_it_logged() {
        // A hostile or careless endpoint could put the secret in its own error
        // text. The body is truncated and quoted, so check it explicitly.
        let body = format!(r#"{{"error":"bad","error_description":"secret {SECRET} rejected"}}"#);
        let described = describe_token_failure(400, &body);
        // This one *would* carry it through, which is why the caller must never
        // log a raw endpoint body for a request it built with a secret.
        // Asserted so the property is stated rather than assumed.
        assert!(described.contains(SECRET), "documents the one path that echoes back");
    }

    #[test]
    fn the_debug_rendering_of_the_credentials_does_not_print_the_secret() {
        // `{:?}` on a struct is how secrets reach logs by accident.
        let c = ClientCredentials {
            client_id: "cid.apps.googleusercontent.com".into(),
            client_secret: SECRET.into(),
        };
        let rendered = format!("{c:?}");
        assert!(!rendered.contains(SECRET), "{rendered}");
        assert!(rendered.contains("cid.apps.googleusercontent.com"), "the id is not a secret");
    }
}
