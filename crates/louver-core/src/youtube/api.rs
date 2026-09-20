//! The YouTube calls this product makes, and nothing else.
//!
//! Five operations: read the channel, find the live broadcast, update its
//! metadata, update the video's tags, post a chat message. Each one maps an
//! HTTP failure onto an error code the UI can explain, because "400 Bad
//! Request" is not something a user can act on.

use super::metadata::{BroadcastMetadata, Privacy};
use super::oauth::urlencode;
use super::quota::ApiMethod;
use crate::error::{ErrorCode, LouverError, Result};
use serde::{Deserialize, Serialize};

pub const API_BASE: &str = "https://www.googleapis.com/youtube/v3";

/// How many broadcasts one `liveBroadcasts.list` page asks for. Google's
/// maximum, so the common case — a channel with a handful — is one request.
pub const BROADCAST_PAGE_SIZE: u32 = 50;

/// How far the search for this window's broadcast will page.
///
/// A channel with years of past broadcasts would otherwise be read to the end
/// on every scheduled start, at a quota unit a page, to answer a question the
/// first page nearly always settles. Four pages is 200 broadcasts; beyond that
/// the app creates one rather than keep looking, which is the safe way to be
/// wrong — a duplicate broadcast is visible and fixable, a start that never
/// happens is neither.
pub const MAX_BROADCAST_PAGES: usize = 4;
/// YouTube truncates chat messages beyond this.
pub const MAX_CHAT_MESSAGE_CHARS: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelInfo {
    pub id: String,
    pub title: String,
}

/// The live broadcast currently on air, as far as this app cares.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveBroadcast {
    /// The broadcast id, which is also the watch-page video id.
    pub id: String,
    pub title: String,
    pub privacy: Privacy,
    /// Present only once the broadcast is actually live and chat is enabled.
    pub active_live_chat_id: Option<String>,
    /// `created` / `ready` / `testing` / `live` / `complete`.
    pub life_cycle_status: String,
    /// The ingestion stream this broadcast is bound to, once it is bound.
    /// Checked rather than assumed: binding the wrong stream produces a
    /// broadcast that never goes live and says nothing about why.
    pub bound_stream_id: Option<String>,
    pub scheduled_start_time: Option<String>,
    /// True when YouTube takes the broadcast live and ends it by itself once
    /// the ingestion stream starts and stops.
    pub enable_auto_start: bool,
    pub enable_auto_stop: bool,
}

/// One of the channel's ingestion endpoints.
///
/// The stream key lives in `ingestion_key`, which is why this type never
/// leaves the process: it is matched against the saved key in memory and then
/// dropped. Nothing here is logged, stored or shown.
#[derive(Clone, PartialEq, Eq)]
pub struct LiveStream {
    pub id: String,
    pub title: String,
    /// `active` / `inactive` / `ready` / `error`.
    pub stream_status: String,
    ingestion_key: String,
}

/// Hand-written so `{:?}` cannot print the stream key.
impl std::fmt::Debug for LiveStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveStream")
            .field("id", &self.id)
            .field("title", &self.title)
            .field("stream_status", &self.stream_status)
            .field("ingestion_key", &"<redacted>")
            .finish()
    }
}

impl LiveStream {
    /// Is this the endpoint the saved stream key publishes to?
    ///
    /// A comparison and nothing else: the key is never returned, logged or
    /// written down, here or anywhere the result of this travels.
    pub fn matches_key(&self, stream_key: &str) -> bool {
        !self.ingestion_key.is_empty() && self.ingestion_key == stream_key.trim()
    }

    pub fn is_active(&self) -> bool {
        self.stream_status == "active"
    }
}

impl LiveBroadcast {
    pub fn is_live(&self) -> bool {
        self.life_cycle_status == "live"
    }
}

/// One HTTP exchange. Abstracted so every call above it can be tested against
/// a fake, and so the transport can be swapped without touching the logic.
pub trait HttpClient: Send + Sync + std::fmt::Debug {
    /// `body` is None for GET. Returns (status, body).
    fn request(
        &self,
        method: &str,
        url: &str,
        bearer: &str,
        body: Option<serde_json::Value>,
    ) -> Result<(u16, String)>;
}

#[derive(Debug)]
pub struct YoutubeApi<'a> {
    pub http: &'a dyn HttpClient,
    pub base: String,
}

impl<'a> YoutubeApi<'a> {
    pub fn new(http: &'a dyn HttpClient) -> Self {
        Self { http, base: API_BASE.to_string() }
    }

    /// Point at a different host. Used by the test harness.
    pub fn with_base(http: &'a dyn HttpClient, base: impl Into<String>) -> Self {
        Self { http, base: base.into() }
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let url = format!("{}{}", self.base, path);
        // The same classification the quota meter uses, so the name in a log
        // line and the units charged for it can never disagree.
        let api_method = ApiMethod::classify(method, &url);
        let (status, text) = self.http.request(method, &url, token, body)?;
        if (200..300).contains(&status) {
            return serde_json::from_str(&text).map_err(|e| {
                LouverError::with_detail(
                    ErrorCode::YoutubeApiFailed,
                    format!("{} 응답을 해석하지 못했습니다: {e}", api_method.name()),
                )
            });
        }
        Err(classify_call(api_method, status, &text))
    }

    /// Which channel the connected account is.
    pub fn my_channel(&self, token: &str) -> Result<ChannelInfo> {
        let v = self.call("GET", "/channels?part=snippet&mine=true", token, None)?;
        let item = v["items"].get(0).ok_or_else(|| {
            LouverError::with_detail(
                ErrorCode::YoutubeNotConnected,
                "이 계정에 연결된 YouTube 채널이 없습니다",
            )
        })?;
        Ok(ChannelInfo {
            id: item["id"].as_str().unwrap_or_default().to_string(),
            title: item["snippet"]["title"].as_str().unwrap_or_default().to_string(),
        })
    }

    /// The broadcast that is on air, if there is one.
    ///
    /// Asks for active broadcasts first. A broadcast that has been created but
    /// not started yet is not "active", so `upcoming` is consulted as well —
    /// metadata can usefully be set before going live.
    pub fn active_broadcast(&self, token: &str) -> Result<LiveBroadcast> {
        for status in ["active", "upcoming"] {
            let path = format!(
                "/liveBroadcasts?part=id,snippet,status,contentDetails&broadcastStatus={status}&broadcastType=all&maxResults=5"
            );
            let v = self.call("GET", &path, token, None)?;
            if let Some(items) = v["items"].as_array() {
                if let Some(b) = items.iter().map(parse_broadcast).next() {
                    return Ok(b);
                }
            }
        }
        Err(LouverError::new(ErrorCode::YoutubeNoActiveBroadcast))
    }

    /// Re-read one broadcast, to pick up an `activeLiveChatId` that appears
    /// only once it goes live.
    pub fn broadcast_by_id(&self, token: &str, id: &str) -> Result<LiveBroadcast> {
        let path = format!("/liveBroadcasts?part=id,snippet,status,contentDetails&id={id}");
        let v = self.call("GET", &path, token, None)?;
        v["items"]
            .as_array()
            .and_then(|i| i.first())
            .map(parse_broadcast)
            .ok_or_else(|| LouverError::new(ErrorCode::YoutubeNoActiveBroadcast))
    }

    /// One page of this channel's broadcasts.
    ///
    /// **`mine=true` is the only filter.** `liveBroadcasts.list` accepts
    /// exactly one of `id`, `mine` and `broadcastStatus`, and real Google
    /// answers any pair of them with
    /// `HTTP 400 incompatibleParameters: Incompatible parameters specified in
    /// the request: broadcastStatus, mine` — which is what stopped every
    /// scheduled start on a real Mac before the first request had even been
    /// made against the channel. Narrowing to `upcoming` therefore happens
    /// here, in memory, where it costs nothing and cannot be refused.
    ///
    /// Returns the page and the token for the next one, if there is one.
    pub fn broadcasts_page(
        &self,
        token: &str,
        page_token: Option<&str>,
    ) -> Result<(Vec<LiveBroadcast>, Option<String>)> {
        let mut path = format!(
            "/liveBroadcasts?part=id,snippet,status,contentDetails&mine=true&broadcastType=all&maxResults={BROADCAST_PAGE_SIZE}"
        );
        if let Some(t) = page_token.filter(|t| !t.is_empty()) {
            path.push_str(&format!("&pageToken={}", urlencode(t)));
        }
        let v = self.call("GET", &path, token, None)?;
        let items =
            v["items"].as_array().map(|i| i.iter().map(parse_broadcast).collect()).unwrap_or_default();
        let next = v["nextPageToken"].as_str().filter(|t| !t.is_empty()).map(str::to_string);
        Ok((items, next))
    }

    /// This channel's broadcasts, up to [`MAX_BROADCAST_PAGES`] pages.
    ///
    /// The caller usually wants [`Self::broadcasts_page`] so it can stop as
    /// soon as it has found what it is looking for; this is the whole-list
    /// form, for callers that need one.
    pub fn my_broadcasts(&self, token: &str) -> Result<Vec<LiveBroadcast>> {
        let mut all = Vec::new();
        let mut page: Option<String> = None;
        for _ in 0..MAX_BROADCAST_PAGES {
            let (items, next) = self.broadcasts_page(token, page.as_deref())?;
            all.extend(items);
            match next {
                Some(t) => page = Some(t),
                None => break,
            }
        }
        Ok(all)
    }

    /// Create the broadcast for a scheduled window.
    ///
    /// `enableAutoStart` and `enableAutoStop` are asked for so that YouTube
    /// takes the broadcast live when the ingestion stream starts and ends it
    /// when the stream stops. The explicit transition is still implemented —
    /// see [`Self::transition_broadcast`] — because a channel or a broadcast
    /// type that will not auto-start has to work too.
    pub fn create_broadcast(
        &self,
        token: &str,
        meta: &BroadcastMetadata,
        scheduled_start: &str,
        scheduled_end: Option<&str>,
        auto_start_stop: bool,
    ) -> Result<LiveBroadcast> {
        let mut snippet = serde_json::json!({
            "title": meta.title,
            "description": meta.description,
            "scheduledStartTime": scheduled_start,
        });
        if let Some(end) = scheduled_end {
            snippet["scheduledEndTime"] = serde_json::Value::String(end.to_string());
        }
        let body = serde_json::json!({
            "snippet": snippet,
            "status": {
                "privacyStatus": meta.privacy.as_api(),
                // Required by YouTube on insert; a music playlist stream is
                // not made for kids, and saying so is what keeps live chat
                // available at all.
                "selfDeclaredMadeForKids": false,
            },
            "contentDetails": {
                "enableAutoStart": auto_start_stop,
                "enableAutoStop": auto_start_stop,
                "enableDvr": true,
                "recordFromStart": true,
            },
        });
        let v =
            self.call("POST", "/liveBroadcasts?part=id,snippet,status,contentDetails", token, Some(body))?;
        Ok(parse_broadcast(&v))
    }

    /// The channel's ingestion endpoints, with their keys.
    ///
    /// The keys are in the response, which is why the result is matched in
    /// memory and dropped. Nothing here reaches a log or the database.
    pub fn my_streams(&self, token: &str) -> Result<Vec<LiveStream>> {
        let path = "/liveStreams?part=id,snippet,cdn,status&mine=true&maxResults=50";
        let v = self.call("GET", path, token, None)?;
        Ok(v["items"].as_array().map(|i| i.iter().map(parse_stream).collect()).unwrap_or_default())
    }

    /// One stream, to check whether it has started receiving video.
    pub fn stream_by_id(&self, token: &str, stream_id: &str) -> Result<LiveStream> {
        let path = format!("/liveStreams?part=id,snippet,cdn,status&id={stream_id}");
        let v = self.call("GET", &path, token, None)?;
        v["items"]
            .as_array()
            .and_then(|i| i.first())
            .map(parse_stream)
            .ok_or_else(|| LouverError::with_detail(ErrorCode::YoutubeApiFailed, "스트림을 찾지 못했습니다"))
    }

    /// Attach a broadcast to the ingestion stream it should take video from.
    ///
    /// Returns the broadcast as Google reports it afterwards, so the caller
    /// can check `bound_stream_id` rather than trust the 200.
    pub fn bind_broadcast(&self, token: &str, broadcast_id: &str, stream_id: &str) -> Result<LiveBroadcast> {
        let path = format!(
            "/liveBroadcasts/bind?id={broadcast_id}&streamId={stream_id}&part=id,snippet,status,contentDetails"
        );
        let v = self.call("POST", &path, token, None)?;
        Ok(parse_broadcast(&v))
    }

    /// Move a broadcast to `testing`, `live` or `complete`.
    ///
    /// YouTube refuses `live` while the bound stream is inactive, so the
    /// caller checks the stream first; asking anyway produces an error that
    /// reads like a permissions problem and is not one.
    pub fn transition_broadcast(
        &self,
        token: &str,
        broadcast_id: &str,
        status: &str,
    ) -> Result<LiveBroadcast> {
        let path = format!(
            "/liveBroadcasts/transition?id={broadcast_id}&broadcastStatus={status}&part=id,snippet,status,contentDetails"
        );
        let v = self.call("POST", &path, token, None)?;
        Ok(parse_broadcast(&v))
    }

    /// Set title, description and privacy on the broadcast.
    ///
    /// `liveBroadcasts.update` replaces each part it is given, so `snippet`
    /// must carry `scheduledStartTime` even though it is not being changed:
    /// omitting it is how a broadcast loses its schedule.
    pub fn update_broadcast(
        &self,
        token: &str,
        broadcast_id: &str,
        meta: &BroadcastMetadata,
        scheduled_start_time: Option<&str>,
    ) -> Result<()> {
        let mut snippet = serde_json::json!({
            "title": meta.title,
            "description": meta.description,
        });
        if let Some(t) = scheduled_start_time {
            snippet["scheduledStartTime"] = serde_json::Value::String(t.to_string());
        }
        let body = serde_json::json!({
            "id": broadcast_id,
            "snippet": snippet,
            "status": { "privacyStatus": meta.privacy.as_api() },
        });
        self.call("PUT", "/liveBroadcasts?part=id,snippet,status", token, Some(body))?;
        Ok(())
    }

    /// Set the tags on the broadcast's video resource.
    ///
    /// `videos.update` replaces the whole `snippet`, and `title` and
    /// `categoryId` are required within it. Sending tags alone therefore wipes
    /// the title and fails on the missing category — so the current resource
    /// is read first and the new tags are merged into it.
    pub fn update_video_tags(&self, token: &str, video_id: &str, tags: &[String]) -> Result<()> {
        let existing = self.call("GET", &format!("/videos?part=snippet&id={video_id}"), token, None)?;
        let snippet =
            existing["items"].as_array().and_then(|i| i.first()).map(|i| i["snippet"].clone()).ok_or_else(
                || {
                    LouverError::with_detail(
                        ErrorCode::YoutubeNoActiveBroadcast,
                        format!("영상 {video_id}을 찾지 못했습니다"),
                    )
                },
            )?;

        let merged = merge_tags_into_snippet(&snippet, tags);
        let body = serde_json::json!({ "id": video_id, "snippet": merged });
        self.call("PUT", "/videos?part=snippet", token, Some(body))?;
        Ok(())
    }

    /// Apply tags and the category, which live on the video rather than the
    /// broadcast — and the title and description, which live on both.
    ///
    /// The title matters here. `liveBroadcasts.update` sets the broadcast's
    /// title, but the watch page and YouTube Studio read the *video*, and the
    /// two are separate resources that Google reconciles in its own time. This
    /// call reads the video snippet and writes it back; leaving the title out
    /// of the merge therefore wrote whatever the video still said — typically
    /// the channel's default stream title, "Playlist" — straight back over the
    /// title that had just been set. Everything the user did not choose
    /// (`defaultLanguage` and the rest) still survives, which is the reason
    /// the read comes first.
    pub fn update_video_snippet(&self, token: &str, video_id: &str, meta: &BroadcastMetadata) -> Result<()> {
        let snippet = self.video_snippet(token, video_id)?;
        let body =
            serde_json::json!({ "id": video_id, "snippet": merge_metadata_into_snippet(&snippet, meta) });
        self.call("PUT", "/videos?part=snippet", token, Some(body))?;
        Ok(())
    }

    /// The video's current snippet, as Google has it.
    pub fn video_snippet(&self, token: &str, video_id: &str) -> Result<serde_json::Value> {
        let existing =
            self.call("GET", &format!("/videos?part=snippet,status&id={video_id}"), token, None)?;
        existing["items"].as_array().and_then(|i| i.first()).map(|i| i["snippet"].clone()).ok_or_else(|| {
            LouverError::with_detail(
                ErrorCode::YoutubeNoActiveBroadcast,
                format!("영상 {video_id}을 찾지 못했습니다"),
            )
        })
    }

    /// Post one message to the live chat.
    pub fn send_chat_message(&self, token: &str, live_chat_id: &str, text: &str) -> Result<()> {
        if text.chars().count() > MAX_CHAT_MESSAGE_CHARS {
            return Err(LouverError::new(ErrorCode::ChatMessageTooLong));
        }
        let body = serde_json::json!({
            "snippet": {
                "liveChatId": live_chat_id,
                "type": "textMessageEvent",
                "textMessageDetails": { "messageText": text },
            }
        });
        self.call("POST", "/liveChat/messages?part=snippet", token, Some(body))?;
        Ok(())
    }
}

/// Keep everything already on the video, replacing only the tags.
///
/// This is the whole point of reading before writing: `title`, `description`,
/// `categoryId` and `defaultLanguage` survive a tag change.
pub fn merge_tags_into_snippet(existing: &serde_json::Value, tags: &[String]) -> serde_json::Value {
    let mut merged = existing.clone();
    if !merged.is_object() {
        merged = serde_json::json!({});
    }
    merged["tags"] =
        serde_json::Value::Array(tags.iter().map(|t| serde_json::Value::String(t.clone())).collect());
    merged
}

/// Every field the user chose, written over the video's current snippet.
///
/// The fields the user did not choose are carried through untouched, because
/// `videos.update` replaces the whole `snippet` part.
pub fn merge_metadata_into_snippet(
    existing: &serde_json::Value,
    meta: &BroadcastMetadata,
) -> serde_json::Value {
    let mut merged = merge_tags_into_snippet(existing, &meta.tags);
    merged["title"] = serde_json::Value::String(meta.title.clone());
    merged["description"] = serde_json::Value::String(meta.description.clone());
    merged["categoryId"] = serde_json::Value::String(meta.category_id.clone());
    merged
}

/// Which fields of a metadata apply actually took, read back from Google.
///
/// A 200 is not the answer to "did the title change": `liveBroadcasts.update`
/// answers 200 and the watch page can still read "Playlist". This is the
/// resource as Google returns it afterwards, compared field by field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MetadataVerification {
    pub title: FieldCheck,
    pub description: FieldCheck,
    pub tags: FieldCheck,
    pub category: FieldCheck,
    pub privacy: FieldCheck,
}

impl MetadataVerification {
    pub fn all_applied(&self) -> bool {
        [&self.title, &self.description, &self.tags, &self.category, &self.privacy].iter().all(|f| f.applied)
    }

    /// The fields that came back different, for the message the user reads.
    pub fn mismatches(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        for (name, f) in [
            ("제목", &self.title),
            ("설명", &self.description),
            ("태그", &self.tags),
            ("카테고리", &self.category),
            ("공개범위", &self.privacy),
        ] {
            if !f.applied {
                v.push(name);
            }
        }
        v
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FieldCheck {
    pub applied: bool,
    /// What Google reports now. Shown next to what was asked for.
    pub actual: String,
}

impl FieldCheck {
    fn compare(requested: &str, actual: &str) -> Self {
        Self { applied: requested == actual, actual: actual.to_string() }
    }
}

/// Compare what was asked for against the video snippet and broadcast status
/// Google returns after the update.
pub fn verify_metadata(
    meta: &BroadcastMetadata,
    snippet: &serde_json::Value,
    privacy_status: &str,
) -> MetadataVerification {
    let text = |k: &str| snippet[k].as_str().unwrap_or_default().to_string();
    let actual_tags: Vec<String> = snippet["tags"]
        .as_array()
        .map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    MetadataVerification {
        title: FieldCheck::compare(&meta.title, &text("title")),
        description: FieldCheck::compare(&meta.description, &text("description")),
        // Order is YouTube's to keep, so the comparison is by set, not by list.
        tags: FieldCheck {
            applied: {
                let mut a = actual_tags.clone();
                let mut b = meta.tags.clone();
                a.sort();
                b.sort();
                a == b
            },
            actual: actual_tags.join(", "),
        },
        category: FieldCheck::compare(&meta.category_id, &text("categoryId")),
        privacy: FieldCheck::compare(meta.privacy.as_api(), privacy_status),
    }
}

fn parse_broadcast(item: &serde_json::Value) -> LiveBroadcast {
    let text = |v: &serde_json::Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_string);
    LiveBroadcast {
        id: item["id"].as_str().unwrap_or_default().to_string(),
        title: item["snippet"]["title"].as_str().unwrap_or_default().to_string(),
        privacy: Privacy::from_api(item["status"]["privacyStatus"].as_str().unwrap_or("unlisted")),
        active_live_chat_id: item["snippet"]["liveChatId"]
            .as_str()
            .or_else(|| item["snippet"]["activeLiveChatId"].as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        life_cycle_status: item["status"]["lifeCycleStatus"].as_str().unwrap_or_default().to_string(),
        bound_stream_id: text(&item["contentDetails"]["boundStreamId"]),
        scheduled_start_time: text(&item["snippet"]["scheduledStartTime"]),
        enable_auto_start: item["contentDetails"]["enableAutoStart"].as_bool().unwrap_or(false),
        enable_auto_stop: item["contentDetails"]["enableAutoStop"].as_bool().unwrap_or(false),
    }
}

/// Build a [`LiveStream`] from a Google-shaped item. Tests use it so the
/// private ingestion key is populated the way a real response populates it.
#[cfg(test)]
pub fn parse_stream_for_tests(item: &serde_json::Value) -> LiveStream {
    parse_stream(item)
}

fn parse_stream(item: &serde_json::Value) -> LiveStream {
    LiveStream {
        id: item["id"].as_str().unwrap_or_default().to_string(),
        title: item["snippet"]["title"].as_str().unwrap_or_default().to_string(),
        stream_status: item["status"]["streamStatus"].as_str().unwrap_or_default().to_string(),
        ingestion_key: item["cdn"]["ingestionInfo"]["streamName"].as_str().unwrap_or_default().to_string(),
    }
}

/// What Google said about a failed request, field by field.
///
/// Kept whole rather than flattened into one sentence: "a YouTube call failed"
/// and "liveBroadcasts.insert was refused with insufficientPermissions" are
/// the same event, and only the second one can be acted on. Everything here
/// comes out of the `error` object — the method, the status, Google's numeric
/// code, its reason and its message. Nothing else of the response body is
/// read, so a credential Google happened to echo back cannot travel with it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ApiFailure {
    /// Google's name for the request, e.g. `liveBroadcasts.insert`.
    pub method: &'static str,
    pub status: u16,
    /// `error.code`, which is normally the HTTP status repeated.
    pub google_code: Option<i64>,
    /// `error.errors[0].reason`, e.g. `insufficientPermissions`.
    pub reason: String,
    /// `error.message`, verbatim.
    pub message: String,
}

impl ApiFailure {
    /// Read the three fields out of Google's error envelope.
    pub fn parse(method: ApiMethod, status: u16, body: &str) -> Self {
        let v = serde_json::from_str::<serde_json::Value>(body).ok();
        let err = v.as_ref().map(|v| &v["error"]);
        let reason = err
            .and_then(|e| e["errors"].as_array())
            .and_then(|e| e.first())
            .and_then(|e| e["reason"].as_str())
            .unwrap_or_default()
            .to_string();
        let message = err
            .and_then(|e| e["message"].as_str())
            .map(str::to_string)
            // No `error` object at all — an HTML error page, or a proxy. The
            // first 200 characters are the only clue there is.
            .unwrap_or_else(|| body.chars().take(200).collect());
        Self {
            method: method.name(),
            status,
            google_code: err.and_then(|e| e["code"].as_i64()),
            reason,
            message,
        }
    }

    /// One line naming the request, the status and Google's own words.
    ///
    /// The shape the logs and the 상세정보 disclosure both use:
    /// `liveBroadcasts.insert HTTP 403 reason=insufficientPermissions · …`.
    pub fn describe(&self) -> String {
        let mut s = format!("{} HTTP {}", self.method, self.status);
        if let Some(c) = self.google_code.filter(|c| *c != self.status as i64) {
            s.push_str(&format!(" code={c}"));
        }
        if !self.reason.is_empty() {
            s.push_str(&format!(" reason={}", self.reason));
        }
        if !self.message.is_empty() {
            s.push_str(&format!(" · {}", self.message));
        }
        s
    }
}

/// Turn an HTTP failure into something the UI can say out loud.
///
/// Google returns 403 for several unrelated conditions, so the reason string
/// decides rather than the status alone.
pub fn classify(status: u16, body: &str) -> LouverError {
    classify_call(ApiMethod::Unknown, status, body)
}

/// [`classify`], with the request's own name kept in the detail.
pub fn classify_call(method: ApiMethod, status: u16, body: &str) -> LouverError {
    let failure = ApiFailure::parse(method, status, body);
    let reason = failure.reason.clone();
    let detail = failure.describe();

    let code = match (status, reason.as_str()) {
        (401, _) | (_, "authError") => ErrorCode::YoutubeAuthExpired,
        (_, "quotaExceeded") | (_, "dailyLimitExceeded") => ErrorCode::YoutubeQuotaExceeded,
        (_, "rateLimitExceeded") | (_, "userRateLimitExceeded") => ErrorCode::ChatRateLimited,
        (429, _) => ErrorCode::ChatRateLimited,
        (_, "liveChatDisabled") | (_, "chatDisabled") => ErrorCode::ChatDisabled,
        (_, "liveChatEnded") | (_, "chatEnded") => ErrorCode::ChatEnded,
        (_, "liveChatNotFound") => ErrorCode::ChatEnded,
        (403, _) => ErrorCode::YoutubeApiFailed,
        (404, _) => ErrorCode::YoutubeNoActiveBroadcast,
        _ => ErrorCode::YoutubeApiFailed,
    };
    LouverError::with_detail(code, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_update_keeps_every_other_field_of_the_snippet() {
        // The failure this prevents: sending only tags blanks the title and
        // drops the category, and YouTube rejects or mangles the video.
        let existing = serde_json::json!({
            "title": "ROOM. 24/7",
            "description": "lofi all night",
            "categoryId": "10",
            "defaultLanguage": "ko",
            "tags": ["old"],
            "channelId": "UC123",
        });
        let merged = merge_tags_into_snippet(&existing, &["lofi".into(), "jazz".into()]);

        assert_eq!(merged["title"], "ROOM. 24/7");
        assert_eq!(merged["description"], "lofi all night");
        assert_eq!(merged["categoryId"], "10");
        assert_eq!(merged["defaultLanguage"], "ko");
        assert_eq!(merged["channelId"], "UC123");
        assert_eq!(merged["tags"], serde_json::json!(["lofi", "jazz"]));
    }

    #[test]
    fn clearing_the_tags_leaves_an_empty_list_rather_than_removing_the_field() {
        let merged = merge_tags_into_snippet(&serde_json::json!({"title": "t"}), &[]);
        assert_eq!(merged["tags"], serde_json::json!([]));
        assert_eq!(merged["title"], "t");
    }

    #[test]
    fn google_failures_map_onto_something_a_user_can_act_on() {
        let err = |status, reason: &str| {
            classify(
                status,
                &format!(
                    r#"{{"error":{{"code":{status},"message":"nope","errors":[{{"reason":"{reason}"}}]}}}}"#
                ),
            )
            .code_str
        };
        assert_eq!(err(401, "authError"), "LL-YOUTUBE-002");
        assert_eq!(err(403, "quotaExceeded"), "LL-YOUTUBE-005");
        assert_eq!(err(403, "rateLimitExceeded"), "LL-CHAT-001");
        assert_eq!(err(403, "liveChatDisabled"), "LL-CHAT-002");
        assert_eq!(err(403, "liveChatEnded"), "LL-CHAT-003");
        assert_eq!(err(404, "notFound"), "LL-YOUTUBE-003");
        assert_eq!(err(500, "backendError"), "LL-YOUTUBE-004");
        // A 429 with no reason at all is still a rate limit.
        assert_eq!(classify(429, "").code_str, "LL-CHAT-001");
    }

    #[test]
    fn a_broadcast_without_a_chat_id_is_parsed_as_having_none() {
        let b = parse_broadcast(&serde_json::json!({
            "id": "abc",
            "snippet": { "title": "t" },
            "status": { "privacyStatus": "unlisted", "lifeCycleStatus": "ready" },
        }));
        assert_eq!(b.active_live_chat_id, None);
        assert!(!b.is_live());

        let b = parse_broadcast(&serde_json::json!({
            "id": "abc",
            "snippet": { "title": "t", "liveChatId": "chat-1" },
            "status": { "privacyStatus": "public", "lifeCycleStatus": "live" },
        }));
        assert_eq!(b.active_live_chat_id.as_deref(), Some("chat-1"));
        assert!(b.is_live());
        assert_eq!(b.privacy, Privacy::Public);
    }

    #[test]
    fn an_over_long_chat_message_is_refused_before_it_is_sent() {
        #[derive(Debug)]
        struct NoCalls;
        impl HttpClient for NoCalls {
            fn request(
                &self,
                _: &str,
                _: &str,
                _: &str,
                _: Option<serde_json::Value>,
            ) -> Result<(u16, String)> {
                panic!("the request should never have been made")
            }
        }
        let api = YoutubeApi::new(&NoCalls);
        let err = api.send_chat_message("t", "chat", &"가".repeat(201)).unwrap_err();
        assert_eq!(err.code_str, "LL-CHAT-004");
    }
}
