//! Google OAuth for an installed application.
//!
//! The loopback flow: the app opens the consent page in the user's browser and
//! listens on a local port for the redirect Google makes back to it. No
//! password ever reaches this application, and nothing is typed into a
//! terminal.
//!
//! The refresh token goes to the OS keychain, never to SQLite and never to a
//! log. The access token lives in memory only and is re-minted from the
//! refresh token when it expires.

use crate::error::{ErrorCode, LouverError, Result};
use crate::security::SecretStore;
use base64::Engine as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Keychain account holding the refresh token.
const REFRESH_TOKEN_ACCOUNT: &str = "youtube_refresh_token";
/// And the OAuth client secret, when the user supplies their own credentials.
const CLIENT_SECRET_ACCOUNT: &str = "youtube_client_secret";

pub const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

/// The one scope this product needs.
///
/// `youtube.force-ssl` covers reading the channel, updating a broadcast's
/// metadata, updating a video's tags and posting a live chat message. Asking
/// for `youtube` as well would add nothing and widen the consent screen; the
/// narrower `youtube.readonly` cannot post chat.
pub const SCOPE: &str = "https://www.googleapis.com/auth/youtube.force-ssl";

/// OAuth client credentials for this installation.
///
/// Baked into the build. Users never see or enter these: a person who wants to
/// broadcast music should not have to open the Google Cloud console.
///
/// Google issues a secret even for "Desktop app" clients and **requires it in
/// the token exchange**, so PKCE does not remove the need for one here — its
/// job is to stop an intercepted authorization code from being redeemed by
/// anyone else. Google documents this secret as not confidential for installed
/// apps, which is why it can be distributed inside the binary; it is still
/// kept out of this repository and out of the database.
#[derive(Debug, Clone)]
pub struct ClientCredentials {
    pub client_id: String,
    pub client_secret: String,
}

/// Supplied at build time. `LOUVER_GOOGLE_CLIENT_ID` is the documented name;
/// the older `LOUVER_YOUTUBE_*` pair is still read so existing build scripts
/// keep working.
pub const BUILT_IN_CLIENT_ID: Option<&str> = match option_env!("LOUVER_GOOGLE_CLIENT_ID") {
    Some(v) => Some(v),
    None => option_env!("LOUVER_YOUTUBE_CLIENT_ID"),
};
pub const BUILT_IN_CLIENT_SECRET: Option<&str> = match option_env!("LOUVER_GOOGLE_CLIENT_SECRET") {
    Some(v) => Some(v),
    None => option_env!("LOUVER_YOUTUBE_CLIENT_SECRET"),
};

impl ClientCredentials {
    /// The client this build ships with, if it was given one.
    pub fn built_in() -> Option<Self> {
        let id = BUILT_IN_CLIENT_ID?.trim();
        if id.is_empty() {
            return None;
        }
        Some(Self {
            client_id: id.to_string(),
            client_secret: BUILT_IN_CLIENT_SECRET.unwrap_or_default().trim().to_string(),
        })
    }
}

/// Proof Key for Code Exchange (RFC 7636), S256.
///
/// A loopback redirect can be observed by anything else running on the
/// machine. PKCE makes an intercepted authorization code useless without the
/// verifier, which never leaves this process until the exchange.
#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn new() -> Self {
        let verifier = random_token(64);
        let digest = <Sha256 as Digest>::digest(verifier.as_bytes());
        Self { verifier, challenge: base64_url_nopad(&digest) }
    }

    /// The challenge for a given verifier. Separate so the RFC's own test
    /// vector can be checked against it.
    pub fn challenge_for(verifier: &str) -> String {
        base64_url_nopad(&<Sha256 as Digest>::digest(verifier.as_bytes()))
    }
}

impl Default for Pkce {
    fn default() -> Self {
        Self::new()
    }
}

fn base64_url_nopad(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// An unguessable token from the OS random source.
///
/// Used for both the PKCE verifier and the `state` parameter, so neither can
/// be predicted by something else on the machine. The alphabet is RFC 7636's
/// unreserved set.
fn random_token(len: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut bytes = vec![0u8; len];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    bytes.iter().map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect()
}

/// Tokens as Google returns them.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

/// An access token and when it stops being usable.
#[derive(Debug, Clone)]
struct AccessToken {
    value: String,
    expires_at: Instant,
}

/// Holds the credentials and hands out a valid access token on demand.
#[derive(Debug)]
pub struct TokenStore {
    secrets: Arc<dyn SecretStore>,
    access: Mutex<Option<AccessToken>>,
}

impl TokenStore {
    pub fn new(secrets: Arc<dyn SecretStore>) -> Self {
        Self { secrets, access: Mutex::new(None) }
    }

    /// Where the refresh token is kept, so Settings can show it is the OS
    /// keychain and not the database.
    pub fn backend_name(&self) -> &'static str {
        self.secrets.backend_name()
    }

    pub fn is_secure(&self) -> bool {
        self.secrets.is_secure()
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.secrets.get(REFRESH_TOKEN_ACCOUNT), Ok(Some(t)) if !t.is_empty())
    }

    pub fn save_refresh_token(&self, token: &str) -> Result<()> {
        self.secrets.set(REFRESH_TOKEN_ACCOUNT, token)
    }

    pub fn refresh_token(&self) -> Result<String> {
        self.secrets
            .get(REFRESH_TOKEN_ACCOUNT)?
            .filter(|t| !t.is_empty())
            .ok_or_else(|| LouverError::new(ErrorCode::YoutubeNotConnected))
    }

    pub fn save_client_secret(&self, secret: &str) -> Result<()> {
        self.secrets.set(CLIENT_SECRET_ACCOUNT, secret)
    }

    pub fn stored_client_secret(&self) -> Option<String> {
        self.secrets.get(CLIENT_SECRET_ACCOUNT).ok().flatten().filter(|s| !s.is_empty())
    }

    /// Forget everything. The user is disconnected until they consent again.
    pub fn disconnect(&self) -> Result<()> {
        *self.access.lock().unwrap() = None;
        let _ = self.secrets.delete(CLIENT_SECRET_ACCOUNT);
        self.secrets.delete(REFRESH_TOKEN_ACCOUNT)
    }

    /// A token good for at least another minute, minting a new one if needed.
    pub fn access_token(&self, creds: &ClientCredentials, http: &dyn TokenEndpoint) -> Result<String> {
        {
            let cached = self.access.lock().unwrap();
            if let Some(t) = cached.as_ref() {
                // A minute of headroom: a token that expires mid-request is
                // an avoidable failure.
                if t.expires_at > Instant::now() + Duration::from_secs(60) {
                    return Ok(t.value.clone());
                }
            }
        }
        let refresh = self.refresh_token()?;
        let fresh = http.refresh(creds, &refresh)?;
        let ttl = Duration::from_secs(fresh.expires_in.unwrap_or(3600));
        // Google only returns a refresh token on the first consent; a rotated
        // one is stored when it appears and ignored when it does not.
        if let Some(new_refresh) = fresh.refresh_token.as_deref() {
            if !new_refresh.is_empty() && new_refresh != refresh {
                self.save_refresh_token(new_refresh)?;
            }
        }
        *self.access.lock().unwrap() =
            Some(AccessToken { value: fresh.access_token.clone(), expires_at: Instant::now() + ttl });
        Ok(fresh.access_token)
    }

    /// Drop the cached access token, so the next call mints a fresh one.
    pub fn invalidate_access_token(&self) {
        *self.access.lock().unwrap() = None;
    }
}

/// The token endpoint, abstracted so the flow can be tested without Google.
pub trait TokenEndpoint: Send + Sync {
    fn exchange_code(
        &self,
        creds: &ClientCredentials,
        code: &str,
        redirect_uri: &str,
        code_verifier: &str,
    ) -> Result<TokenResponse>;
    fn refresh(&self, creds: &ClientCredentials, refresh_token: &str) -> Result<TokenResponse>;
}

/// Percent-encode for a query string. Hand-rolled to avoid a dependency for
/// the handful of characters that actually occur here.
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// What the consent screen should ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentPrompt {
    /// Connecting for the first time, or reconnecting the same account.
    Consent,
    /// "계정 변경": show the account chooser rather than assuming the last one.
    SelectAccount,
}

impl ConsentPrompt {
    fn as_param(self) -> &'static str {
        match self {
            Self::Consent => "consent",
            Self::SelectAccount => "select_account%20consent",
        }
    }
}

/// The consent URL to open in the browser.
///
/// `access_type=offline` is what makes Google issue a refresh token at all,
/// and `prompt=consent` makes it do so again for an account that has already
/// consented — without it, reconnecting yields no refresh token and the app
/// would appear to connect and then fail an hour later.
pub fn consent_url(
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    pkce: &Pkce,
    prompt: ConsentPrompt,
) -> String {
    format!(
        "{AUTH_ENDPOINT}?client_id={}&redirect_uri={}&response_type=code&scope={}\
&access_type=offline&prompt={}&state={}&code_challenge={}&code_challenge_method=S256",
        urlencode(client_id),
        urlencode(redirect_uri),
        urlencode(SCOPE),
        prompt.as_param(),
        urlencode(state),
        urlencode(&pkce.challenge),
    )
}

/// The result of waiting on the loopback redirect.
pub struct AuthorizationCode {
    pub code: String,
    pub redirect_uri: String,
}

/// A local one-shot HTTP server for the OAuth redirect.
///
/// Bound before the browser is opened so the port is known and cannot be taken
/// in between.
pub struct LoopbackServer {
    listener: TcpListener,
    port: u16,
    state: String,
}

impl LoopbackServer {
    pub fn bind() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| {
            LouverError::with_detail(ErrorCode::YoutubeApiFailed, format!("로컬 포트를 열지 못했습니다: {e}"))
        })?;
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        Ok(Self { listener, port, state: random_state() })
    }

    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn state(&self) -> &str {
        &self.state
    }

    /// Block until Google redirects back, or the wait runs out.
    ///
    /// The `state` parameter is checked: a redirect that does not carry the
    /// value this server generated did not come from the request it made.
    pub fn wait_for_code(self, timeout: Duration) -> Result<AuthorizationCode> {
        self.listener.set_nonblocking(true).ok();
        let deadline = Instant::now() + timeout;
        let redirect_uri = self.redirect_uri();

        while Instant::now() < deadline {
            match self.listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                    let mut line = String::new();
                    if BufReader::new(&stream).read_line(&mut line).is_err() {
                        continue;
                    }
                    let query = line.split_whitespace().nth(1).unwrap_or("");
                    let params = parse_query(query);

                    let reply = |stream: &mut std::net::TcpStream, body: &str| {
                        let page = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(page.as_bytes());
                        let _ = stream.flush();
                    };

                    if let Some(err) = params.get("error") {
                        reply(&mut stream, &page("연결이 취소되었습니다", "이 창을 닫아도 됩니다."));
                        return Err(LouverError::with_detail(
                            ErrorCode::YoutubeNotConnected,
                            format!("사용자가 권한을 허용하지 않았습니다 ({err})"),
                        ));
                    }
                    let (Some(code), Some(state)) = (params.get("code"), params.get("state")) else {
                        continue; // a favicon request or similar; keep waiting
                    };
                    if state != &self.state {
                        reply(&mut stream, &page("요청이 일치하지 않습니다", "다시 시도해주세요."));
                        return Err(LouverError::with_detail(
                            ErrorCode::YoutubeNotConnected,
                            "state 값이 일치하지 않습니다",
                        ));
                    }
                    reply(&mut stream, &page("YouTube 계정이 연결되었습니다", "Louver Live로 돌아가세요."));
                    return Ok(AuthorizationCode { code: code.clone(), redirect_uri });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    return Err(LouverError::with_detail(
                        ErrorCode::YoutubeApiFailed,
                        format!("로그인 응답을 받지 못했습니다: {e}"),
                    ))
                }
            }
        }
        Err(LouverError::with_detail(
            ErrorCode::YoutubeNotConnected,
            "제한 시간 안에 로그인이 끝나지 않았습니다",
        ))
    }
}

fn page(heading: &str, body: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><title>Louver Live</title>\
         <body style=\"font-family:-apple-system,system-ui,sans-serif;background:#0f1115;color:#e6e8ec;\
         display:flex;align-items:center;justify-content:center;height:100vh;margin:0\">\
         <div style=\"text-align:center\"><h1 style=\"font-weight:600;font-size:18px\">{heading}</h1>\
         <p style=\"color:#8b90a0;font-size:14px\">{body}</p></div>"
    )
}

fn parse_query(target: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let Some(q) = target.split_once('?').map(|(_, q)| q) else { return map };
    for pair in q.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            map.insert(k.to_string(), urldecode(v));
        }
    }
    map
}

fn urldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn random_state() -> String {
    random_token(32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::MemorySecretStore;

    #[test]
    fn the_pkce_challenge_matches_the_rfc_test_vector() {
        // RFC 7636 appendix B. If this ever drifts, Google rejects every
        // exchange with invalid_grant and the cause is not obvious.
        assert_eq!(
            Pkce::challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_verifier_is_long_unguessable_and_uses_only_unreserved_characters() {
        let a = Pkce::new();
        let b = Pkce::new();
        assert_ne!(a.verifier, b.verifier, "two flows must not share a verifier");
        assert!((43..=128).contains(&a.verifier.len()), "RFC 7636 length range");
        assert!(a.verifier.chars().all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c)));
        assert_eq!(a.challenge, Pkce::challenge_for(&a.verifier));
        // Base64url, no padding.
        assert!(!a.challenge.contains('='));
        assert!(!a.challenge.contains('+') && !a.challenge.contains('/'));
    }

    #[test]
    fn each_flow_gets_its_own_state() {
        let states: std::collections::HashSet<String> = (0..50).map(|_| random_state()).collect();
        assert_eq!(states.len(), 50, "state must not repeat");
        assert!(random_state().len() >= 32);
    }

    #[test]
    fn the_consent_url_carries_the_challenge_and_asks_for_the_account_chooser() {
        let pkce = Pkce::new();
        let url = consent_url("id", "http://127.0.0.1:5000", "abc", &pkce, ConsentPrompt::SelectAccount);
        assert!(url.contains(&format!("code_challenge={}", urlencode(&pkce.challenge))));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("prompt=select_account%20consent"));
        // The verifier itself must never appear in a URL that opens a browser.
        assert!(!url.contains(&pkce.verifier));
    }

    #[test]
    fn a_build_without_a_client_reports_that_rather_than_guessing() {
        // This build supplies none, so the product must say so instead of
        // sending an empty client_id to Google.
        if BUILT_IN_CLIENT_ID.is_none() {
            assert!(ClientCredentials::built_in().is_none());
        }
    }

    #[test]
    fn the_consent_url_asks_for_offline_access_and_one_scope() {
        let url = consent_url(
            "id.apps.googleusercontent.com",
            "http://127.0.0.1:5000",
            "abc",
            &Pkce::new(),
            ConsentPrompt::Consent,
        );
        assert!(url.contains("access_type=offline"), "no refresh token without it");
        assert!(url.contains("prompt=consent"), "reconnecting must re-issue a refresh token");
        assert!(url.contains(&urlencode(SCOPE)));
        assert!(!url.contains("auth/youtube&"), "only the one scope should be requested");
        assert!(url.contains("state=abc"));
    }

    #[test]
    fn query_parsing_handles_encoding_and_missing_values() {
        let p = parse_query("/?code=4%2F0Ab_x-y&state=s%20t&scope=a+b");
        assert_eq!(p.get("code").unwrap(), "4/0Ab_x-y");
        assert_eq!(p.get("state").unwrap(), "s t");
        assert_eq!(p.get("scope").unwrap(), "a b");
        assert!(parse_query("/favicon.ico").is_empty());
    }

    #[test]
    fn a_disconnect_removes_the_refresh_token_from_the_store() {
        let secrets = Arc::new(MemorySecretStore::new());
        let store = TokenStore::new(secrets.clone());
        store.save_refresh_token("1//refresh").unwrap();
        assert!(store.is_connected());
        store.disconnect().unwrap();
        assert!(!store.is_connected());
        assert!(store.refresh_token().is_err());
    }

    #[test]
    fn a_missing_refresh_token_reads_as_not_connected_rather_than_a_failure() {
        let store = TokenStore::new(Arc::new(MemorySecretStore::new()));
        assert_eq!(store.refresh_token().unwrap_err().code_str, "LL-YOUTUBE-001");
    }

    /// A token endpoint that counts calls, so caching can be observed.
    #[derive(Default)]
    struct FakeEndpoint {
        calls: Mutex<usize>,
        rotate: bool,
    }
    impl TokenEndpoint for FakeEndpoint {
        fn exchange_code(&self, _: &ClientCredentials, _: &str, _: &str, _: &str) -> Result<TokenResponse> {
            Ok(TokenResponse {
                access_token: "at".into(),
                refresh_token: Some("1//first".into()),
                expires_in: Some(3600),
            })
        }
        fn refresh(&self, _: &ClientCredentials, _: &str) -> Result<TokenResponse> {
            let mut n = self.calls.lock().unwrap();
            *n += 1;
            Ok(TokenResponse {
                access_token: format!("at{n}"),
                refresh_token: self.rotate.then(|| "1//rotated".to_string()),
                expires_in: Some(3600),
            })
        }
    }

    fn creds() -> ClientCredentials {
        ClientCredentials { client_id: "id".into(), client_secret: "secret".into() }
    }

    #[test]
    fn an_access_token_is_minted_once_and_then_reused() {
        let store = TokenStore::new(Arc::new(MemorySecretStore::new()));
        store.save_refresh_token("1//refresh").unwrap();
        let ep = FakeEndpoint::default();

        assert_eq!(store.access_token(&creds(), &ep).unwrap(), "at1");
        assert_eq!(store.access_token(&creds(), &ep).unwrap(), "at1");
        assert_eq!(*ep.calls.lock().unwrap(), 1, "the cached token should have been reused");

        store.invalidate_access_token();
        assert_eq!(store.access_token(&creds(), &ep).unwrap(), "at2");
    }

    #[test]
    fn a_rotated_refresh_token_replaces_the_stored_one() {
        let store = TokenStore::new(Arc::new(MemorySecretStore::new()));
        store.save_refresh_token("1//first").unwrap();
        let ep = FakeEndpoint { rotate: true, ..Default::default() };
        store.access_token(&creds(), &ep).unwrap();
        assert_eq!(store.refresh_token().unwrap(), "1//rotated");
    }
}
