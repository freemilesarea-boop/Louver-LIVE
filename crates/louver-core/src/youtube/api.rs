//! The YouTube calls this product makes, and nothing else.
//!
//! Five operations: read the channel, find the live broadcast, update its
//! metadata, update the video's tags, post a chat message. Each one maps an
//! HTTP failure onto an error code the UI can explain, because "400 Bad
//! Request" is not something a user can act on.

use super::metadata::{BroadcastMetadata, Privacy};
use crate::error::{ErrorCode, LouverError, Result};
use serde::{Deserialize, Serialize};

pub const API_BASE: &str = "https://www.googleapis.com/youtube/v3";
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
        let (status, text) = self.http.request(method, &url, token, body)?;
        if (200..300).contains(&status) {
            return serde_json::from_str(&text).map_err(|e| {
                LouverError::with_detail(
                    ErrorCode::YoutubeApiFailed,
                    format!("응답을 해석하지 못했습니다: {e}"),
                )
            });
        }
        Err(classify(status, &text))
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
    }
}

/// Turn an HTTP failure into something the UI can say out loud.
///
/// Google returns 403 for several unrelated conditions, so the reason string
/// decides rather than the status alone.
pub fn classify(status: u16, body: &str) -> LouverError {
    let reason = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v["error"]["errors"]
                .as_array()
                .and_then(|e| e.first())
                .and_then(|e| e["reason"].as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();

    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.chars().take(200).collect());

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
