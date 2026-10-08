//! The real HTTP transport.
//!
//! Blocking, because everything around it is: the app has no async runtime and
//! introducing one for five API calls would be a poor trade.

use super::api::{ApiFailure, HttpClient};
use super::oauth::{ClientCredentials, TokenEndpoint, TokenResponse, TOKEN_ENDPOINT};
use super::quota::ApiMethod;
use crate::error::{ErrorCode, LouverError, Result};
use crate::streaming::ffmpeg::mask_secrets;
use std::time::Duration;
use ureq::ResponseExt;

const TIMEOUT: Duration = Duration::from_secs(20);

/// How much of `WWW-Authenticate` reaches the log.
///
/// Google's challenge is a short `Bearer realm=…, error=…` line; the cap is
/// there so a proxy that answers with something enormous cannot push the rest
/// of the diagnosis off the screen.
const WWW_AUTH_MAX_CHARS: usize = 200;

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
            // Observation only. This makes ureq keep the uris it already
            // visits so a redirect becomes visible to us; it does not change
            // whether redirects are followed, nor what is sent. `max_redirects`
            // and `redirect_auth_headers` are left at their defaults — the
            // latter is `Never`, and the whole point of this instrumentation is
            // to find out whether that default is dropping our `Authorization`
            // on a hop we cannot currently see.
            .save_redirect_history(true)
            .build()
            .into()
    }
}

/// What a response's redirect history says, in the two values a log line may
/// carry: how many hops, and whether the last one was still the same host.
///
/// Deliberately not the uris. A `Location` can carry a path and a query, and
/// this diagnosis has no need for either — the count alone decides whether a
/// redirect was involved, and `SAME_HOST`/`DIFFERENT_HOST` is as much of the
/// target as anyone needs to read. The borrowed uris end their life inside this
/// function.
fn redirect_summary(history: Option<&[ureq::http::Uri]>) -> (usize, &'static str) {
    // The history includes the request itself, so a single entry is no redirect.
    match history {
        Some(h) if h.len() > 1 => {
            let same = h.first().and_then(|u| u.authority()) == h.last().and_then(|u| u.authority());
            (h.len().saturating_sub(1), if same { "SAME_HOST" } else { "DIFFERENT_HOST" })
        }
        _ => (0, "-"),
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
                // Read before the body, because `body_mut` borrows the
                // response mutably. One header by name and never the map:
                // a dump would put this request's own `Authorization` value
                // one refactor away from a log file.
                let challenge = resp
                    .headers()
                    .get("www-authenticate")
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.chars().take(WWW_AUTH_MAX_CHARS).collect::<String>());
                // Also before the body, and for the same borrow reason. Reduced
                // to two owned values here so no uri outlives this statement.
                let (redirects, redirect_host) = redirect_summary(resp.get_redirect_history());
                let text = resp.body_mut().read_to_string().unwrap_or_default();
                println!(
                    "{}",
                    exchange_line(method, url, status, &text, challenge.as_deref(), redirects, redirect_host,)
                );
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

/// Path and query only. The scheme and host say nothing a log line needs, and
/// dropping them makes a request against a test base read the same as one
/// against Google.
fn endpoint_of(url: &str) -> &str {
    match url.split_once("://") {
        Some((_, rest)) => rest.find('/').map(|i| &rest[i..]).unwrap_or("/"),
        None => url,
    }
}

/// One line describing one YouTube API exchange.
///
/// Built as a string rather than printed in place so that what it may and may
/// not contain is a thing a test can assert. The rules it exists to hold:
///
/// A success says its status and stops. `liveStreams.insert` answers 200 with
/// the stream key in `cdn.ingestionInfo.streamName` and the RTMPS address
/// beside it, so logging a 2xx body would publish the one secret this product
/// guards most carefully.
///
/// A failure says only what Google's own error envelope holds:
/// `error.errors[0].reason` and `error.message`, both of which exist only
/// inside that envelope.
///
/// It deliberately does not use [`ApiFailure::message`]. That field falls back
/// to the first 200 characters of the body when there is no envelope — an HTML
/// page from a proxy, say — and that fallback is raw body, which this line may
/// never carry. `mask_secrets` is not a substitute: a key glued to surrounding
/// markup (`denied abcd-efgh-ijkl-mnop-qrst</body>`) does not tokenise as a
/// key and survives masking. A test holds this property.
///
/// The bearer token is not a parameter here. It cannot be logged by accident
/// because it is not in scope.
fn exchange_line(
    method: &str,
    url: &str,
    status: u16,
    body: &str,
    challenge: Option<&str>,
    redirects: usize,
    redirect_host: &str,
) -> String {
    let called = ApiMethod::classify(method, url);
    let path = mask_secrets(endpoint_of(url));
    // `redirect_auth_policy` is our own configuration, read back rather than
    // guessed: ureq's default is `RedirectAuthHeaders::Never`, and this
    // instrumentation does not change it. What the header did on a hop we did
    // not send ourselves is not observable through ureq, so it is not claimed
    // here — the count and the host verdict are, and together with this policy
    // they are enough to say whether a redirect could have cost us the header.
    let hops = format!("redirects={redirects} redirect_host={redirect_host} redirect_auth_policy=NEVER");
    if (200..300).contains(&status) {
        return format!("[louver][youtube][http] {} {method} {path} status={status} {hops}", called.name());
    }
    // `reason` comes from the envelope and from nowhere else, so the shared
    // parser is safe for it.
    let reason = ApiFailure::parse(called, status, body).reason;
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string));
    let dash = |s: String| if s.trim().is_empty() { "-".to_string() } else { s };
    format!(
        "[louver][youtube][http] {} {method} {path} status={status} {hops} reason={} message={} www_authenticate={}",
        called.name(),
        dash(mask_secrets(&reason)),
        // No envelope means no message to quote. Saying so beats quoting a
        // body whose contents nothing has vetted.
        message.map(|m| dash(mask_secrets(&m))).unwrap_or_else(|| "<no error envelope>".to_string()),
        // `none` and not an empty field: "Google sent no challenge" is itself
        // one of the answers this diagnosis is looking for.
        challenge.map(mask_secrets).map(dash).unwrap_or_else(|| "none".to_string()),
    )
}

/// Which of the two token grants a request is.
///
/// Carried only so a failure can be named. A consent exchange that fails is
/// the user still at the keyboard in 설정; a refresh that fails happens at
/// 03:00 with nobody watching, and reporting the two the same way is what made
/// the scheduled failure unreadable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenGrant {
    AuthorizationCode,
    Refresh,
}

impl TokenGrant {
    /// The name used in logs and error details.
    pub fn name(self) -> &'static str {
        match self {
            Self::AuthorizationCode => "oauth2.token(authorization_code)",
            Self::Refresh => "oauth2.token(refresh_token)",
        }
    }

    /// The error code a failure of this grant carries.
    pub fn error_code(self) -> ErrorCode {
        match self {
            Self::AuthorizationCode => ErrorCode::YoutubeAuthExpired,
            Self::Refresh => ErrorCode::YoutubeAuthRefreshFailed,
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
        self.post_token(
            TokenGrant::AuthorizationCode,
            &authorization_code_form(creds, code, redirect_uri, code_verifier),
        )
    }

    fn refresh(&self, creds: &ClientCredentials, refresh_token: &str) -> Result<TokenResponse> {
        self.post_token(TokenGrant::Refresh, &refresh_form(creds, refresh_token))
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
    fn post_token(&self, grant: TokenGrant, form: &[(&str, &str)]) -> Result<TokenResponse> {
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
                    grant.error_code(),
                    format!("{} {}", grant.name(), describe_token_failure(status, &text)),
                ))
            }
            Err(ureq::Error::StatusCode(code)) => Err(LouverError::with_detail(
                grant.error_code(),
                format!("{} HTTP {code} · 토큰 요청이 거부되었습니다", grant.name()),
            )),
            // A refresh that cannot reach Google is still a refresh failure —
            // reporting it as an API failure sends the reader looking at the
            // YouTube calls, which never happened.
            Err(e) => Err(LouverError::with_detail(
                grant.error_code(),
                format!("{} · 네트워크 오류: {e}", grant.name()),
            )),
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

/// The diagnosis line's own rules: what it must say, and what it must not.
///
/// These exist because the line is the only record of a 401 that nobody can
/// reproduce on demand, and because the same line runs past a 200 whose body
/// carries a stream key.
#[cfg(test)]
mod exchange_line_tests {
    use super::*;

    /// The baseline for every assertion below: no redirect. The two
    /// redirect fields have their own tests, so the rest stay readable.
    fn line(method: &str, url: &str, status: u16, body: &str, challenge: Option<&str>) -> String {
        exchange_line(method, url, status, body, challenge, 0, "-")
    }

    const STREAM_KEY: &str = "abcd-efgh-ijkl-mnop-qrst";
    const INGEST: &str = "rtmps://a.rtmps.youtube.com/live2";

    /// What `liveStreams.insert` actually answers 200 with.
    fn stream_insert_200() -> String {
        format!(
            r#"{{"id":"s-1","cdn":{{"ingestionInfo":{{"rtmpsIngestionAddress":"{INGEST}",
                "ingestionAddress":"rtmp://a.rtmp.youtube.com/live2","streamName":"{STREAM_KEY}"}}}}}}"#
        )
    }

    fn google_401() -> &'static str {
        r#"{"error":{"code":401,"message":"Request had invalid authentication credentials. Expected OAuth 2 access token, login cookie or other valid authentication credential.","errors":[{"reason":"authError","message":"Invalid Credentials"}],"status":"UNAUTHENTICATED"}}"#
    }

    #[test]
    fn a_success_says_the_status_and_nothing_from_the_body() {
        let line = line(
            "POST",
            "https://www.googleapis.com/youtube/v3/liveStreams?part=id,snippet,cdn,status",
            200,
            &stream_insert_200(),
            None,
        );
        // The whole point: the key and the ingest address are in that body.
        assert!(!line.contains(STREAM_KEY), "{line}");
        assert!(!line.contains(INGEST), "{line}");
        assert!(!line.contains("rtmp"), "{line}");
        assert!(!line.contains("streamName"), "{line}");
        assert!(!line.contains("ingestionInfo"), "{line}");
        // And what it does say, so the line is still worth having.
        assert!(line.contains("liveStreams.insert"), "{line}");
        assert!(line.contains("POST"), "{line}");
        assert!(line.contains("/liveStreams?part=id,snippet,cdn,status"), "{line}");
        assert!(line.contains("status=200"), "{line}");
        // A 2xx line carries no failure fields at all.
        assert!(!line.contains("reason="), "{line}");
        assert!(!line.contains("message="), "{line}");
        assert!(!line.contains("www_authenticate="), "{line}");
    }

    #[test]
    fn a_failure_names_the_request_the_status_and_googles_own_words() {
        let line = line(
            "POST",
            "https://www.googleapis.com/youtube/v3/liveStreams?part=id,snippet,cdn,status",
            401,
            google_401(),
            None,
        );
        assert!(line.contains("liveStreams.insert"), "{line}");
        assert!(line.contains("status=401"), "{line}");
        assert!(line.contains("reason=authError"), "{line}");
        assert!(line.contains("Expected OAuth 2 access token"), "{line}");
    }

    #[test]
    fn the_two_inserts_are_told_apart_by_name() {
        // The question this whole diagnosis turns on: one of these succeeded.
        let b = line(
            "POST",
            "https://www.googleapis.com/youtube/v3/liveBroadcasts?part=id,snippet,status,contentDetails",
            200,
            "{}",
            None,
        );
        let s = line(
            "POST",
            "https://www.googleapis.com/youtube/v3/liveStreams?part=id,snippet,cdn,status",
            401,
            google_401(),
            None,
        );
        assert!(b.contains("liveBroadcasts.insert"), "{b}");
        assert!(s.contains("liveStreams.insert"), "{s}");
    }

    #[test]
    fn no_challenge_header_reads_as_none_rather_than_as_an_empty_field() {
        let line = line("POST", "/liveStreams?part=id", 401, google_401(), None);
        assert!(line.contains("www_authenticate=none"), "{line}");
    }

    #[test]
    fn an_empty_challenge_header_is_still_a_readable_field() {
        let line = line("POST", "/liveStreams?part=id", 401, google_401(), Some("   "));
        assert!(line.contains("www_authenticate=-"), "{line}");
    }

    #[test]
    fn a_challenge_header_is_kept_whole_when_it_is_what_google_sends() {
        let challenge = r#"Bearer realm="https://accounts.google.com/", error="invalid_token""#;
        let line = line("POST", "/liveStreams?part=id", 401, google_401(), Some(challenge));
        // The discriminator this instrumentation exists to capture.
        assert!(line.contains("error=\"invalid_token\""), "{line}");
    }

    #[test]
    fn a_challenge_header_is_masked_and_capped_by_the_caller_and_by_the_line() {
        // The cap belongs to the caller, which slices before masking; assert
        // the constant the caller uses, and that masking happens here.
        let long = format!("Bearer {} {}", "x".repeat(400), STREAM_KEY);
        let capped: String = long.chars().take(WWW_AUTH_MAX_CHARS).collect();
        assert_eq!(capped.chars().count(), WWW_AUTH_MAX_CHARS);
        let line = line("POST", "/liveStreams?part=id", 401, google_401(), Some(&long));
        // Masking applies to whatever reaches the line, capped or not.
        assert!(!line.contains(STREAM_KEY), "{line}");
    }

    #[test]
    fn a_body_that_is_not_googles_envelope_is_not_quoted_at_all() {
        // A proxy's HTML error page. `ApiFailure::message` would fall back to
        // the first 200 characters of it, and masking does not save a key that
        // is glued to markup — so the line must not quote the body at all.
        let html = format!("<html><body>denied {STREAM_KEY}</body></html>");
        let line = line("POST", "/liveStreams?part=id", 502, &html, None);
        assert!(line.contains("status=502"), "{line}");
        assert!(line.contains("message=<no error envelope>"), "{line}");
        assert!(!line.contains(STREAM_KEY), "{line}");
        assert!(!line.contains("denied"), "{line}");
        assert!(!line.contains("<html>"), "{line}");
    }

    #[test]
    fn the_fallback_this_line_refuses_to_use_really_does_carry_the_body() {
        // Stated rather than assumed: this is why `exchange_line` reads the
        // envelope itself instead of taking `ApiFailure::message`.
        let html = format!("<html><body>denied {STREAM_KEY}</body></html>");
        let failure = ApiFailure::parse(ApiMethod::LiveStreamsInsert, 502, &html);
        assert!(failure.message.contains(STREAM_KEY), "{}", failure.message);
    }

    #[test]
    fn the_endpoint_is_path_and_query_without_the_host() {
        assert_eq!(
            endpoint_of("https://www.googleapis.com/youtube/v3/liveStreams?part=id"),
            "/youtube/v3/liveStreams?part=id"
        );
        assert_eq!(endpoint_of("/liveStreams?part=id"), "/liveStreams?part=id");
        assert_eq!(endpoint_of("https://www.googleapis.com"), "/");
    }

    #[test]
    fn the_error_classification_the_caller_depends_on_is_unchanged() {
        // The line is an observation, not a decision. A 401 must still be the
        // code `call_api` refreshes on, and the detail must still be the one
        // sentence the dashboard shows.
        let e = crate::youtube::api::classify_call(ApiMethod::LiveStreamsInsert, 401, google_401());
        assert_eq!(e.code, ErrorCode::YoutubeAuthExpired);
        let detail = e.detail.expect("the detail names the request");
        assert!(detail.starts_with("liveStreams.insert HTTP 401"), "{detail}");
        assert!(detail.contains("reason=authError"), "{detail}");
    }
}

/// What a redirect does to this transport, measured rather than assumed.
///
/// The question these tests exist for: ureq follows redirects by default and
/// its default `RedirectAuthHeaders::Never` does not carry `Authorization`
/// across one. If Google ever answers one of our calls with a 3xx, the hop we
/// cannot see could arrive unauthenticated and come back `401 invalid_token` —
/// indistinguishable, from our side, from a token that is genuinely bad.
///
/// Measuring it narrowed the question. ureq will not re-send a POST body across
/// a 307/308 (`ureq-proto` `redirect.rs:51-60`, `need_request_body()` is true
/// for POST/PUT/PATCH by method alone), so those come back as a transport
/// error, never a 401. Only 301/302/303 are followed — as a GET, with the
/// header dropped. The fake server here records, per hop, whether the header
/// arrived, so that is a fact in a test rather than a reading of the library.
#[cfg(test)]
mod redirect_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    const TOKEN: &str = "ya29.thisMustNeverAppearInALogLine";

    /// One hop as the server saw it: the method, and whether it was authorized.
    type Hops = Arc<Mutex<Vec<(String, bool)>>>;

    /// One scripted reply: a status, and a `Location` when it is a redirect.
    #[derive(Clone)]
    struct Reply {
        status: u16,
        location: Option<String>,
    }

    fn reply(status: u16) -> Reply {
        Reply { status, location: None }
    }

    fn redirect(status: u16, to: &str) -> Reply {
        Reply { status, location: Some(to.to_string()) }
    }

    /// Serves `script` in order, one reply per connection.
    fn serve(listener: TcpListener, script: Vec<Reply>, hops: Hops) {
        std::thread::spawn(move || {
            for r in script {
                let Ok((stream, _)) = listener.accept() else { return };
                answer(stream, &r, &hops);
            }
        });
    }

    /// Reads one request (head and body, so the client is never left writing
    /// into a closed socket), records it, then answers.
    ///
    /// The recording happens *before* the response is written: the client can
    /// otherwise observe the reply and assert while this thread is still on its
    /// way to the mutex.
    fn answer(stream: TcpStream, r: &Reply, hops: &Hops) {
        let mut buf = BufReader::new(stream);
        let mut method = String::new();
        let mut had_auth = false;
        let mut len = 0usize;
        let mut first = true;
        loop {
            let mut head = String::new();
            if buf.read_line(&mut head).unwrap_or(0) == 0 {
                break;
            }
            if first {
                method = head.split_whitespace().next().unwrap_or("").to_string();
                first = false;
            }
            let lower = head.to_ascii_lowercase();
            if lower.starts_with("authorization:") {
                had_auth = true;
            }
            if let Some(v) = lower.strip_prefix("content-length:") {
                len = v.trim().parse().unwrap_or(0);
            }
            if head == "\r\n" || head == "\n" {
                break;
            }
        }
        if len > 0 {
            let mut body = vec![0u8; len];
            let _ = buf.read_exact(&mut body);
        }
        hops.lock().unwrap().push((method, had_auth));

        let body = if r.status == 401 {
            r#"{"error":{"code":401,"message":"Request had invalid authentication credentials.","errors":[{"reason":"authError"}]}}"#
        } else {
            "{}"
        };
        let mut out = format!("HTTP/1.1 {} X\r\nConnection: close\r\n", r.status);
        if let Some(loc) = &r.location {
            out.push_str(&format!("Location: {loc}\r\n"));
        }
        if r.status == 401 {
            out.push_str(
                "WWW-Authenticate: Bearer realm=\"https://accounts.google.com/\", error=\"invalid_token\"\r\n",
            );
        }
        out.push_str(&format!("Content-Length: {}\r\n\r\n{}", body.len(), body));
        let _ = buf.get_mut().write_all(out.as_bytes());
        let _ = buf.get_mut().flush();
    }

    fn listener() -> (TcpListener, String) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        (l, base)
    }

    fn hops() -> Hops {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn json() -> serde_json::Value {
        serde_json::json!({"snippet": {"title": "t"}})
    }

    // --- T1 / T2: no redirect -------------------------------------------
    #[test]
    fn t1_a_200_without_a_redirect_reports_zero_hops() {
        let (l, base) = listener();
        let seen = hops();
        serve(l, vec![reply(200)], Arc::clone(&seen));
        let (status, text) = UreqClient::new()
            .request("POST", &format!("{base}/liveStreams?part=id"), TOKEN, Some(json()))
            .unwrap();
        assert_eq!((status, text.as_str()), (200, "{}"), "status and body as before");
        assert_eq!(seen.lock().unwrap().as_slice(), &[("POST".into(), true)]);
        assert_eq!(
            exchange_line("POST", "/liveStreams?part=id", 200, "{}", None, 0, "-"),
            "[louver][youtube][http] liveStreams.insert POST /liveStreams?part=id \
             status=200 redirects=0 redirect_host=- redirect_auth_policy=NEVER"
        );
    }

    #[test]
    fn t2_a_401_without_a_redirect_keeps_every_existing_field() {
        let (l, base) = listener();
        let seen = hops();
        serve(l, vec![reply(401)], Arc::clone(&seen));
        let (status, text) = UreqClient::new()
            .request("POST", &format!("{base}/liveStreams?part=id"), TOKEN, Some(json()))
            .unwrap();
        assert_eq!(status, 401);
        assert!(text.contains("authError"), "{text}");
        assert_eq!(seen.lock().unwrap().as_slice(), &[("POST".into(), true)]);
        let line = exchange_line(
            "POST",
            "/liveStreams?part=id",
            401,
            &text,
            Some("Bearer error=\"invalid_token\""),
            0,
            "-",
        );
        assert!(line.contains("redirects=0"), "{line}");
        assert!(line.contains("redirect_host=-"), "{line}");
        assert!(line.contains("reason=authError"), "{line}");
        assert!(line.contains("www_authenticate=Bearer error=\"invalid_token\""), "{line}");
    }

    // --- T3 / T5: same-host redirect, and the header across it -----------
    #[test]
    fn t3_a_same_host_redirect_is_followed_and_the_header_does_not_survive_it() {
        let (l, base) = listener();
        let seen = hops();
        serve(l, vec![redirect(303, &format!("{base}/second")), reply(200)], Arc::clone(&seen));
        let (status, _text) = UreqClient::new()
            .request("POST", &format!("{base}/liveStreams?part=id"), TOKEN, Some(json()))
            .unwrap();
        assert_eq!(status, 200, "the redirect is followed, as it was before");
        // T5. The first hop is authorized; the second is not — this is ureq's
        // `RedirectAuthHeaders::Never`, which this change deliberately leaves
        // alone. It is also the mechanism the production log line now exposes.
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &[("POST".into(), true), ("GET".into(), false)],
            "hop 1 POST authorized, hop 2 GET unauthorized"
        );
    }

    // --- T4: cross-host redirect ----------------------------------------
    #[test]
    fn t4_a_cross_host_redirect_loses_the_header_too_and_the_authority_differs() {
        let (l1, base1) = listener();
        let (l2, base2) = listener();
        let seen1 = hops();
        let seen2 = hops();
        serve(l1, vec![redirect(303, &format!("{base2}/second"))], Arc::clone(&seen1));
        serve(l2, vec![reply(401)], Arc::clone(&seen2));
        let (status, text) = UreqClient::new()
            .request("POST", &format!("{base1}/liveStreams?part=id"), TOKEN, Some(json()))
            .unwrap();
        assert_eq!(status, 401, "the second host refuses the unauthorized hop");
        assert!(text.contains("authError"), "{text}");
        assert_eq!(seen1.lock().unwrap().as_slice(), &[("POST".into(), true)]);
        assert_eq!(seen2.lock().unwrap().as_slice(), &[("GET".into(), false)]);
        assert_ne!(base1, base2, "two authorities, which is what the verdict reads");
    }

    /// A 307/308 on a POST is not followed at all, so it can never be the 401
    /// this diagnosis is chasing. Stated as a test because it removes a whole
    /// branch of the hypothesis.
    #[test]
    fn t4b_a_307_on_a_post_is_a_transport_error_and_never_a_401() {
        for status in [307u16, 308] {
            let (l, base) = listener();
            let seen = hops();
            serve(l, vec![redirect(status, &format!("{base}/second"))], Arc::clone(&seen));
            let out = UreqClient::new().request(
                "POST",
                &format!("{base}/liveStreams?part=id"),
                TOKEN,
                Some(json()),
            );
            let e = out.expect_err("ureq will not re-send a POST body across a 307/308");
            assert_eq!(e.code, ErrorCode::YoutubeApiFailed, "{status}");
            assert!(e.detail.unwrap_or_default().contains("네트워크 오류"), "{status}");
            assert_eq!(seen.lock().unwrap().len(), 1, "{status}: only the first hop happens");
        }
    }

    #[test]
    fn t4c_the_summary_reads_the_authority_and_nothing_else() {
        let one: ureq::http::Uri =
            "https://www.googleapis.com/youtube/v3/liveStreams?part=id".parse().unwrap();
        let same: ureq::http::Uri = "https://www.googleapis.com/youtube/v3/elsewhere".parse().unwrap();
        let other: ureq::http::Uri = "https://other.example.com/youtube/v3/liveStreams".parse().unwrap();
        assert_eq!(redirect_summary(None), (0, "-"), "history off or absent");
        assert_eq!(redirect_summary(Some(std::slice::from_ref(&one))), (0, "-"), "one entry is no redirect");
        assert_eq!(redirect_summary(Some(&[one.clone(), same])), (1, "SAME_HOST"));
        assert_eq!(redirect_summary(Some(&[one.clone(), other.clone()])), (1, "DIFFERENT_HOST"));
        assert_eq!(
            redirect_summary(Some(&[one.clone(), other, one])),
            (2, "SAME_HOST"),
            "counts hops, compares the ends"
        );
    }

    // --- T6: nothing secret in the line ----------------------------------
    #[test]
    fn t6_the_line_never_carries_the_token_the_header_or_a_location() {
        for (redirects, host) in [(0usize, "-"), (1, "SAME_HOST"), (2, "DIFFERENT_HOST")] {
            for status in [200u16, 401] {
                let line = exchange_line(
                    "POST",
                    "https://www.googleapis.com/youtube/v3/liveStreams?part=id",
                    status,
                    r#"{"error":{"message":"m","errors":[{"reason":"authError"}]}}"#,
                    Some("Bearer realm=\"https://accounts.google.com/\", error=\"invalid_token\""),
                    redirects,
                    host,
                );
                assert!(!line.contains(TOKEN), "{line}");
                assert!(!line.contains("ya29."), "{line}");
                assert!(!line.contains("Authorization"), "{line}");
                assert!(!line.contains("Location"), "{line}");
                assert!(!line.contains("/second"), "{line}");
                assert!(!line.contains("www.googleapis.com"), "the host is never printed: {line}");
                assert!(line.contains(&format!("redirects={redirects}")), "{line}");
                assert!(line.contains(&format!("redirect_host={host}")), "{line}");
                assert!(line.contains("redirect_auth_policy=NEVER"), "{line}");
            }
        }
    }

    // --- T7: behaviour unchanged -----------------------------------------
    #[test]
    fn t7_the_transport_still_returns_the_status_and_the_body_it_always_did() {
        // The three arms of `request`: GET, POST with a body, PUT without one.
        for (method, body) in [("GET", None), ("POST", Some(json())), ("PUT", None)] {
            let (l, base) = listener();
            let seen = hops();
            serve(l, vec![reply(401)], Arc::clone(&seen));
            let (status, text) =
                UreqClient::new().request(method, &format!("{base}/channels?part=id"), TOKEN, body).unwrap();
            assert_eq!(status, 401, "{method}");
            assert!(text.contains("authError"), "{method}: {text}");
            assert_eq!(seen.lock().unwrap().as_slice(), &[(method.into(), true)], "{method}");
            // The classification the caller depends on is untouched.
            let e = crate::youtube::api::classify_call(ApiMethod::ChannelsList, status, &text);
            assert_eq!(e.code, ErrorCode::YoutubeAuthExpired, "{method}");
        }
    }
}
