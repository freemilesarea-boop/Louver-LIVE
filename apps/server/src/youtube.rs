//! Connecting a YouTube account over the wire. §2.
//!
//! Four routes and no logic: everything below is the cloud crate's provider,
//! which is where the token handling and the API calls live and where the tests
//! drive them against a fake Google.
//!
//! The one thing worth reading carefully is who the callback believes. The
//! session cookie is `SameSite=Strict`, so a browser arriving from
//! `accounts.google.com` does **not** send it, and the callback therefore cannot
//! take a `Caller`. What it takes instead is the `state`: a value this server
//! generated, stored against the user who started the flow, single-use and
//! expiring (§15). That is the CSRF defence the flow is supposed to have, and it
//! is also the identity — the account is attached to the user the state names,
//! never to whoever happens to open the URL.

use crate::auth::Caller;
use crate::error::ApiError;
use crate::state::App;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use louver_cloud::youtube::YoutubeAccount;
use louver_cloud::CloudError;
use serde::{Deserialize, Serialize};

type Out<T> = std::result::Result<Json<T>, ApiError>;

/// The same answer for every route when the server has no Google credentials:
/// a sentence an operator can act on, rather than a 500.
fn provider(app: &App) -> std::result::Result<louver_cloud::youtube::Youtube, ApiError> {
    app.youtube.clone().ok_or_else(|| {
        ApiError(CloudError::Invalid(
            "이 서버에는 YouTube 연결이 설정되지 않았습니다. 관리자에게 YOUTUBE_CLIENT_ID 와 YOUTUBE_CLIENT_SECRET 설정을 요청해 주세요."
                .into(),
        ))
    })
}

#[derive(Serialize)]
pub struct ConsentStart {
    /// Where the browser has to go. Carries no secret of ours: a client id, a
    /// redirect URI, a scope and the one-time state.
    pub url: String,
}

#[derive(Deserialize)]
pub struct StartQuery {
    /// `1` to be redirected straight to Google, for a plain link. Omitted by
    /// the web app, which wants the URL so it can show an error in the page.
    #[serde(default)]
    pub redirect: Option<String>,
}

/// Begin the flow. Requires a signed-in caller, because the state row it writes
/// is what the callback will trust.
pub async fn start(
    State(app): State<App>,
    Caller(uid): Caller,
    Query(q): Query<StartQuery>,
) -> std::result::Result<Response, ApiError> {
    let yt = provider(&app)?;
    let url = crate::blocking(move || yt.consent_url(&uid)).await?;
    if q.redirect.as_deref() == Some("1") {
        return Ok(see_other(&url));
    }
    Ok(Json(ConsentStart { url }).into_response())
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    /// Google's own refusal, `access_denied` when the user pressed cancel.
    #[serde(default)]
    pub error: Option<String>,
}

/// Where Google sends the browser back to.
///
/// Always a redirect into the app, never a JSON body: what arrives here is a
/// person looking at a browser window, and a raw `{"error":…}` would be the end
/// of the road for them. The failure is carried as a query parameter the app
/// renders.
pub async fn callback(State(app): State<App>, Query(q): Query<CallbackQuery>) -> Response {
    if let Some(err) = q.error.as_deref() {
        return see_other(&landing("error", refusal(err)));
    }
    let (Some(code), Some(state)) = (q.code.clone(), q.state.clone()) else {
        return see_other(&landing("error", "Google 응답에 필요한 값이 없습니다. 다시 시도해 주세요."));
    };
    let Some(yt) = app.youtube.clone() else {
        return see_other(&landing("error", "이 서버에는 YouTube 연결이 설정되지 않았습니다."));
    };

    // The code is a credential. It is used here and never written anywhere —
    // not to a log, not to the redirect it came in on.
    match tokio::task::spawn_blocking(move || yt.complete_consent(&state, &code)).await {
        Ok(Ok(account)) => see_other(&landing("connected", &account.channel_title)),
        Ok(Err(e)) => {
            eprintln!("[louver] youtube: 계정 연결 실패: {e}");
            see_other(&landing("error", &e.to_string()))
        }
        Err(_) => see_other(&landing("error", "연결 처리가 중단되었습니다. 다시 시도해 주세요.")),
    }
}

/// Google's `error` parameter, in words a user can do something about.
fn refusal(code: &str) -> &'static str {
    match code {
        "access_denied" => "YouTube 계정 연결을 취소했습니다.",
        "invalid_scope" => "요청한 권한이 거부되었습니다. 관리자에게 문의해 주세요.",
        _ => "Google이 연결을 거부했습니다. 잠시 후 다시 시도해 주세요.",
    }
}

/// The page the browser lands on, with what happened attached.
///
/// Relative on purpose: the server does not know its own public address, and
/// guessing one would send the user to the wrong host behind a proxy.
fn landing(outcome: &str, detail: &str) -> String {
    format!(
        "/?youtube={}&detail={}",
        louver_core::youtube::oauth::urlencode(outcome),
        louver_core::youtube::oauth::urlencode(detail)
    )
}

fn see_other(location: &str) -> Response {
    let mut headers = HeaderMap::new();
    if let Ok(v) = header::HeaderValue::from_str(location) {
        headers.insert(header::LOCATION, v);
    }
    (StatusCode::SEE_OTHER, headers).into_response()
}

/// The connected channels. No token and no stream key: `YoutubeAccount` has no
/// field that could carry one. §3.
pub async fn list_accounts(State(app): State<App>, Caller(uid): Caller) -> Out<Vec<YoutubeAccount>> {
    Ok(Json(crate::blocking(move || app.db.youtube_accounts_for(&uid)).await?))
}

/// Disconnect: the sealed tokens are deleted, then the row. §2.
///
/// Works whether or not this server still has Google credentials. A deployment
/// that removed them would otherwise strand every account connected with them.
pub async fn delete_account(
    State(app): State<App>,
    Caller(uid): Caller,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Out<Gone> {
    crate::blocking(move || louver_cloud::youtube::forget_account(&app.db, &app.keys, &uid, &id)).await?;
    Ok(Json(Gone { deleted: true }))
}

#[derive(Serialize)]
pub struct Gone {
    pub deleted: bool,
}

/// Whether this deployment can offer YouTube connecting at all, so the UI can
/// say "관리자가 설정해야 합니다" instead of showing a button that always fails.
#[derive(Serialize)]
pub struct Availability {
    pub configured: bool,
    /// The exact URI that has to be registered in the Google console. Not a
    /// secret, and the single most common thing to get wrong.
    pub redirect_uri: String,
}

pub async fn availability(State(app): State<App>, Caller(_uid): Caller) -> Json<Availability> {
    Json(Availability {
        configured: app.youtube.is_some(),
        redirect_uri: louver_cloud::youtube::redirect_uri(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_from_google_becomes_something_a_person_can_read() {
        assert_eq!(refusal("access_denied"), "YouTube 계정 연결을 취소했습니다.");
        assert!(refusal("something_new").contains("거부"));
    }

    #[test]
    fn the_landing_url_escapes_what_it_carries() {
        let url = landing("error", "실패: a&b=c");
        assert!(url.starts_with("/?youtube=error&detail="), "{url}");
        assert!(!url.contains("a&b=c"), "a message must not become extra parameters: {url}");
    }
}
