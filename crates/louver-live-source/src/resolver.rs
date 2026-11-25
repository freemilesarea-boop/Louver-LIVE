//! A YouTube watch address becomes a stream FFmpeg can open.
//!
//! What is supported, and what is deliberately not:
//!
//!  * **Supported**: a public or unlisted YouTube Live broadcast the user owns,
//!    or has permission to restream. The product asks for that confirmation in
//!    the UI; nothing here can verify it, and nothing here tries to.
//!  * **Not supported, by construction**: anything that needs a sign-in, a
//!    purchase, an age token or a DRM licence. `yt-dlp` is run with no cookies,
//!    no credentials and no browser profile, so a video that needs any of them
//!    simply fails to resolve. There is no code path that could be given one.
//!
//! Why `yt-dlp`: YouTube publishes no API that hands back the HLS manifest of a
//! live broadcast for restreaming — not even the owner's own. `yt-dlp` reads the
//! watch page the way a player does. That is also why this feature is Beta: the
//! tool needs updating whenever YouTube changes, and a stale copy fails closed
//! (resolution errors) rather than silently sending the wrong thing.
//!
//! ## Handing a user string to a subprocess
//!
//! `yt-dlp` accepts hundreds of extractors, local file paths and a config file
//! of its own. Passing an unchecked string to it would be its own vulnerability
//! class, so:
//!
//!  * [`classify`] runs first and only lets through `youtube.com` / `youtu.be`
//!    hosts with a plausible video id. A path, another site, or an option-shaped
//!    string never reaches the binary.
//!  * `--ignore-config` so a file on disk cannot add options.
//!  * `--` before the URL, so an address can never be read as a flag.
//!  * the resolved manifest URL is then put through the production SSRF check
//!    ([`louver_cloud::cctv::validate`]) before FFmpeg sees it — `yt-dlp` could
//!    be made to return `http://127.0.0.1/`, and that must fail.

use crate::error::{LiveSourceError, Result};
use std::process::Command;
use std::time::Duration;

/// How long the resolver may take before the child is killed.
pub const RESOLVE_TIMEOUT: Duration = Duration::from_secs(20);

/// The longest watch URL this will consider. A YouTube watch URL is short; a
/// long one is someone probing.
pub const MAX_WATCH_URL_CHARS: usize = 300;

/// What kind of address the user typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// A YouTube watch page, which has to be resolved to a stream.
    YoutubeWatch,
    /// Already a stream FFmpeg can open (the existing CCTV case).
    DirectStream,
}

/// What the resolver found. No credential, no cookie, no signature — the
/// manifest URL is the one thing FFmpeg needs and the only thing kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSource {
    /// The address FFmpeg opens. Time-limited: YouTube's manifest URLs expire,
    /// which is why this is never written to a database.
    pub manifest_url: String,
    pub is_live: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// The broadcast's title, for the operator's log. Not sent anywhere.
    pub title: Option<String>,
}

/// Resolving a watch address to a stream.
///
/// A trait so every test in this crate runs without a network: the real
/// implementation shells out to `yt-dlp`, and the tests use a fake.
pub trait LiveSourceResolver: Send + Sync {
    fn resolve(&self, watch_url: &str) -> Result<ResolvedSource>;
}

/// Decide what the user typed, refusing anything this worker will not open.
///
/// Returns `DirectStream` for an http(s) address that is not YouTube, so the
/// existing CCTV behaviour is reachable through the same entry point.
pub fn classify(raw: &str) -> Result<SourceKind> {
    let url = raw.trim();
    if url.is_empty() {
        return Err(LiveSourceError::invalid("주소를 입력해 주세요."));
    }
    if url.chars().count() > MAX_WATCH_URL_CHARS {
        return Err(LiveSourceError::invalid(format!("주소가 너무 깁니다 ({MAX_WATCH_URL_CHARS}자 이내).")));
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(LiveSourceError::invalid("주소에 공백이나 제어문자가 들어 있습니다."));
    }
    // An address that starts with `-` would be read as an option by any
    // subprocess. Refused here rather than relied on `--` alone.
    if url.starts_with('-') {
        return Err(LiveSourceError::invalid("주소가 '-' 로 시작할 수 없습니다."));
    }
    let (scheme, rest) = url.split_once("://").ok_or_else(|| {
        LiveSourceError::invalid("http:// 또는 https:// 로 시작하는 주소만 사용할 수 있습니다.")
    })?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(LiveSourceError::invalid(format!("{scheme}:// 는 사용할 수 없습니다.")));
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    // `https://youtube.com@evil.example/` reads as host evil.example.
    if authority.contains('@') {
        return Err(LiveSourceError::invalid("주소에 아이디·비밀번호를 포함할 수 없습니다."));
    }
    let host = authority.rsplit_once(':').map(|(h, _)| h).unwrap_or(authority).to_ascii_lowercase();
    if is_youtube_host(&host) {
        // A watch address has to actually name a video, or there is nothing to
        // resolve and `yt-dlp` would be handed a channel or a search page.
        video_id(url).ok_or_else(|| {
            LiveSourceError::invalid(
                "YouTube 영상 주소가 아닙니다. https://www.youtube.com/watch?v=... 또는 https://youtu.be/... 형식이 필요합니다.",
            )
        })?;
        return Ok(SourceKind::YoutubeWatch);
    }
    Ok(SourceKind::DirectStream)
}

fn is_youtube_host(host: &str) -> bool {
    const HOSTS: &[&str] =
        &["youtube.com", "www.youtube.com", "m.youtube.com", "music.youtube.com", "youtu.be", "www.youtu.be"];
    HOSTS.contains(&host)
}

/// The video id in a watch or short address, if there is one.
///
/// Public because the worker logs it: a video id is not a secret (it is in
/// every share link), and without it an operator cannot tell two failing
/// workers apart.
pub fn video_id(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://")?.1;
    let (authority, path_and_query) = match after_scheme.find(['/', '?', '#']) {
        Some(i) => (&after_scheme[..i], &after_scheme[i..]),
        None => (after_scheme, ""),
    };
    let host = authority.rsplit_once(':').map(|(h, _)| h).unwrap_or(authority).to_ascii_lowercase();
    let candidate = if host.ends_with("youtu.be") {
        path_and_query.trim_start_matches('/').split(['?', '#', '/']).next().unwrap_or("").to_string()
    } else {
        let query = path_and_query.split_once('?').map(|(_, q)| q).unwrap_or("");
        let mut found = String::new();
        for pair in query.split('&') {
            if let Some(v) = pair.strip_prefix("v=") {
                found = v.split('#').next().unwrap_or("").to_string();
                break;
            }
        }
        if found.is_empty() {
            // `/live/<id>` and `/embed/<id>` are the other two shapes a user
            // can copy out of a browser.
            let path = path_and_query.split('?').next().unwrap_or("");
            for prefix in ["/live/", "/embed/", "/shorts/"] {
                if let Some(rest) = path.strip_prefix(prefix) {
                    found = rest.split('/').next().unwrap_or("").to_string();
                    break;
                }
            }
        }
        found
    };
    is_video_id(&candidate).then_some(candidate)
}

/// YouTube ids are 11 characters of the URL-safe base64 alphabet.
fn is_video_id(s: &str) -> bool {
    s.len() == 11 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The real resolver: `yt-dlp`, run once, read once, killed on timeout.
#[derive(Debug, Clone)]
pub struct YtDlpResolver {
    pub binary: std::path::PathBuf,
    pub timeout: Duration,
    /// Cap handed to `yt-dlp` as well as to FFmpeg. Asking for a 1080p format
    /// means less to re-encode and, where YouTube offers a 1080p rendition, no
    /// scaling at all.
    pub max_height: u32,
}

impl YtDlpResolver {
    pub fn new(binary: impl Into<std::path::PathBuf>) -> Self {
        Self { binary: binary.into(), timeout: RESOLVE_TIMEOUT, max_height: crate::args::MAX_HEIGHT }
    }

    /// The argv, as a value, so it can be asserted without a subprocess.
    ///
    /// `--dump-single-json` implies simulate: nothing is downloaded and nothing
    /// is written to disk.
    pub fn argv(&self, watch_url: &str) -> Vec<String> {
        vec![
            // No config file may add options to this command.
            "--ignore-config".into(),
            "--no-warnings".into(),
            "--no-progress".into(),
            // A live URL is one video. A playlist would be a surprise fan-out.
            "--no-playlist".into(),
            // Resolve only; never write a file.
            "--dump-single-json".into(),
            "--socket-timeout".into(),
            self.timeout.as_secs().to_string(),
            // Prefer an HLS rendition at or below the cap, because that is what
            // FFmpeg reads best. The trailing `/best` keeps a source that
            // offers nothing else usable.
            "-f".into(),
            format!("best[height<=?{h}][protocol^=m3u8]/best[height<=?{h}]/best", h = self.max_height),
            // Nothing after this is an option.
            "--".into(),
            watch_url.to_string(),
        ]
    }
}

impl LiveSourceResolver for YtDlpResolver {
    fn resolve(&self, watch_url: &str) -> Result<ResolvedSource> {
        if classify(watch_url)? != SourceKind::YoutubeWatch {
            return Err(LiveSourceError::invalid("YouTube 주소가 아닙니다."));
        }
        let mut cmd = Command::new(&self.binary);
        cmd.args(self.argv(watch_url));
        let run = crate::process::run_with_timeout(cmd, self.timeout)
            .map_err(|e| LiveSourceError::resolver(format!("yt-dlp를 실행할 수 없습니다: {e}")))?;
        if run.timed_out {
            return Err(LiveSourceError::resolver(format!(
                "{}초 안에 영상 정보를 가져오지 못했습니다.",
                self.timeout.as_secs()
            )));
        }
        let stderr = String::from_utf8_lossy(&run.stderr).to_string();
        if !run.ok {
            let (kind, message) = classify_resolver_stderr(&stderr);
            return Err(LiveSourceError::new(kind, message));
        }
        let parsed: serde_json::Value = serde_json::from_slice(&run.stdout)
            .map_err(|e| LiveSourceError::resolver(format!("영상 정보를 해석할 수 없습니다: {e}")))?;
        parse_metadata(&parsed)
    }
}

/// Turn `yt-dlp`'s JSON into a source, or say why it cannot be used.
///
/// Separated from the subprocess so every branch is tested without `yt-dlp`
/// installed and without a network.
pub fn parse_metadata(v: &serde_json::Value) -> Result<ResolvedSource> {
    // `is_live` is the field that decides this feature. A finished broadcast
    // still has a watch page and still resolves to a URL, and sending that
    // would publish a recording as if it were live.
    let is_live = v.get("is_live").and_then(|x| x.as_bool()).unwrap_or(false);
    let live_status = v.get("live_status").and_then(|x| x.as_str()).unwrap_or("");
    if !is_live {
        let why = match live_status {
            "is_upcoming" => "아직 시작되지 않은 예약 방송입니다. 방송이 시작된 뒤에 다시 시도해 주세요.",
            "post_live" | "was_live" => "이미 종료된 방송입니다. 진행 중인 라이브 주소가 필요합니다.",
            _ => "라이브 방송이 아닙니다. 진행 중인 YouTube Live 주소가 필요합니다.",
        };
        return Err(LiveSourceError::not_live(why));
    }
    let manifest_url = v
        .get("url")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("manifest_url").and_then(|x| x.as_str()))
        .unwrap_or_default()
        .to_string();
    if manifest_url.is_empty() {
        return Err(LiveSourceError::unavailable("이 방송에서 재생 가능한 스트림을 찾지 못했습니다."));
    }
    // The resolver is not trusted with the address it returns. Anything that is
    // not a public http(s) stream is refused here, by production's own check.
    louver_cloud::cctv::validate(&manifest_url)
        .map_err(|e| LiveSourceError::invalid(format!("스트림 주소를 사용할 수 없습니다: {e}")))?;
    Ok(ResolvedSource {
        manifest_url,
        is_live: true,
        width: v.get("width").and_then(|x| x.as_u64()).map(|n| n as u32),
        height: v.get("height").and_then(|x| x.as_u64()).map(|n| n as u32),
        title: v.get("title").and_then(|x| x.as_str()).map(|s| s.chars().take(80).collect()),
    })
}

/// `yt-dlp`'s complaint, as a kind and a sentence the user can act on.
pub fn classify_resolver_stderr(stderr: &str) -> (crate::error::ErrorKind, String) {
    use crate::error::ErrorKind as K;
    let e = stderr.to_lowercase();
    if e.contains("private video") {
        (K::Invalid, "비공개 영상입니다. 본인이 소유하거나 재송출 허가를 받은 공개·일부공개 라이브만 사용할 수 있습니다.".into())
    } else if e.contains("members-only") || e.contains("join this channel") {
        (K::Invalid, "멤버십 전용 영상입니다. 사용할 수 없습니다.".into())
    } else if e.contains("sign in to confirm your age") || e.contains("age-restricted") {
        (K::Invalid, "연령 제한 영상입니다. 사용할 수 없습니다.".into())
    } else if e.contains("sign in") || e.contains("cookies") || e.contains("not a bot") {
        (
            K::Unavailable,
            "이 영상을 보려면 로그인이 필요합니다. 로그인이 필요한 영상은 지원하지 않습니다.".into(),
        )
    } else if e.contains("video unavailable") || e.contains("does not exist") || e.contains("removed") {
        (K::Invalid, "해당 영상을 찾을 수 없습니다. 주소를 확인해 주세요.".into())
    } else if e.contains("this live event will begin")
        || e.contains("is_upcoming")
        || e.contains("premieres in")
    {
        (K::NotLive, "아직 시작되지 않은 예약 방송입니다.".into())
    } else if e.contains("timed out") || e.contains("timeout") {
        (K::Unavailable, "YouTube 응답 시간을 초과했습니다.".into())
    } else if e.contains("unable to download") || e.contains("failed to resolve") || e.contains("getaddrinfo")
    {
        (K::Unavailable, "YouTube에 연결할 수 없습니다. 서버의 네트워크를 확인해 주세요.".into())
    } else {
        // Keep the tool's own first line rather than inventing a cause. It
        // contains a URL and an error string, never a credential: this command
        // is run with no cookies and no credentials to leak.
        let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("알 수 없는 오류").trim();
        (K::ResolverFailed, format!("영상 정보를 가져오지 못했습니다: {first}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    fn refused(url: &str) -> String {
        match classify(url) {
            Ok(k) => panic!("{url} accepted as {k:?}"),
            Err(e) => e.message,
        }
    }

    #[test]
    fn the_three_shapes_a_user_can_copy_are_all_youtube() {
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ",
            "https://www.youtube.com/live/dQw4w9WgXcQ",
            "https://m.youtube.com/watch?v=dQw4w9WgXcQ&t=10",
            "https://www.youtube.com/embed/dQw4w9WgXcQ",
        ] {
            assert_eq!(classify(url).unwrap(), SourceKind::YoutubeWatch, "{url}");
            assert_eq!(video_id(url).as_deref(), Some("dQw4w9WgXcQ"), "{url}");
        }
    }

    #[test]
    fn a_non_youtube_stream_is_still_the_existing_direct_case() {
        assert_eq!(classify("https://93.184.216.34/hls/cam.m3u8").unwrap(), SourceKind::DirectStream);
        assert_eq!(video_id("https://93.184.216.34/hls/cam.m3u8"), None);
    }

    #[test]
    fn a_youtube_host_without_a_video_is_refused_rather_than_handed_to_yt_dlp() {
        // A channel, a search, a playlist: all would make yt-dlp do something
        // other than resolve one live stream.
        for url in [
            "https://www.youtube.com/",
            "https://www.youtube.com/@somechannel",
            "https://www.youtube.com/results?search_query=live",
            "https://www.youtube.com/playlist?list=PL123",
            "https://www.youtube.com/watch?v=tooshort",
        ] {
            assert!(refused(url).contains("YouTube 영상 주소가 아닙니다"), "{url}");
        }
    }

    #[test]
    fn nothing_option_shaped_or_local_reaches_the_subprocess() {
        for url in [
            "--exec=rm -rf /",
            "-o/tmp/x",
            "file:///etc/passwd",
            "/etc/passwd",
            "ytsearch:live",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ --exec echo",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ\n--exec",
            "https://youtube.com@evil.example/watch?v=dQw4w9WgXcQ",
            "",
        ] {
            assert!(!refused(url).is_empty(), "{url:?}");
        }
    }

    #[test]
    fn the_argv_cannot_be_extended_by_the_url() {
        let r = YtDlpResolver::new("/usr/bin/yt-dlp");
        let a = r.argv("https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        // `--` must be the last option, and the URL the last element.
        assert_eq!(a[a.len() - 2], "--");
        assert_eq!(a.last().unwrap(), "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert!(a.contains(&"--ignore-config".to_string()), "a config file must not add options");
        assert!(a.contains(&"--no-playlist".to_string()));
        assert!(a.contains(&"--dump-single-json".to_string()), "resolve only, never download");
        // Nothing that would authenticate, write a file or run a command.
        for forbidden in ["--cookies", "--cookies-from-browser", "--username", "--password", "--exec", "-o"] {
            assert!(!a.iter().any(|x| x == forbidden), "{forbidden} must not be passed");
        }
    }

    fn live_json(extra: serde_json::Value) -> serde_json::Value {
        let mut v = serde_json::json!({
            "is_live": true,
            "live_status": "is_live",
            "url": "https://93.184.216.34/videoplayback/hls/manifest.m3u8",
            "width": 1920, "height": 1080, "title": "우리 가게 라이브"
        });
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        v
    }

    #[test]
    fn a_live_broadcast_resolves_to_its_manifest() {
        let r = parse_metadata(&live_json(serde_json::json!({}))).unwrap();
        assert!(r.is_live);
        assert_eq!(r.manifest_url, "https://93.184.216.34/videoplayback/hls/manifest.m3u8");
        assert_eq!((r.width, r.height), (Some(1920), Some(1080)));
        assert_eq!(r.title.as_deref(), Some("우리 가게 라이브"));
    }

    #[test]
    fn a_broadcast_that_is_not_live_is_refused_with_the_reason() {
        for (status, expect) in [
            ("is_upcoming", "아직 시작되지 않은"),
            ("was_live", "이미 종료된"),
            ("post_live", "이미 종료된"),
            ("not_live", "라이브 방송이 아닙니다"),
        ] {
            let v = live_json(serde_json::json!({ "is_live": false, "live_status": status }));
            let e = parse_metadata(&v).unwrap_err();
            assert_eq!(e.kind, ErrorKind::NotLive, "{status}");
            assert!(e.message.contains(expect), "{status} → {}", e.message);
        }
    }

    #[test]
    fn a_resolver_that_returns_an_internal_address_is_not_trusted() {
        // The whole reason the resolved URL is re-validated: if yt-dlp (or a
        // page it read) can be made to return this, the server must not open it.
        for bad in [
            "http://127.0.0.1:8080/api/me",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.5/s.m3u8",
            "file:///etc/passwd",
        ] {
            let v = live_json(serde_json::json!({ "url": bad }));
            let e = parse_metadata(&v).unwrap_err();
            assert_eq!(e.kind, ErrorKind::Invalid, "{bad}");
            assert!(e.message.contains("스트림 주소를 사용할 수 없습니다"), "{bad} → {}", e.message);
        }
    }

    #[test]
    fn a_broadcast_with_no_playable_stream_says_so() {
        let v = live_json(serde_json::json!({ "url": serde_json::Value::Null }));
        let mut v = v;
        v.as_object_mut().unwrap().remove("url");
        let e = parse_metadata(&v).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Unavailable);
    }

    #[test]
    fn yt_dlps_complaints_become_causes_a_user_can_act_on() {
        for (stderr, kind) in [
            (
                "ERROR: [youtube] abc: Private video. Sign in if you've been granted access",
                ErrorKind::Invalid,
            ),
            (
                "ERROR: [youtube] abc: Join this channel to get access to members-only content",
                ErrorKind::Invalid,
            ),
            ("ERROR: [youtube] abc: Sign in to confirm your age", ErrorKind::Invalid),
            ("ERROR: [youtube] abc: Video unavailable", ErrorKind::Invalid),
            ("ERROR: [youtube] abc: This live event will begin in 3 hours", ErrorKind::NotLive),
            ("ERROR: unable to download video data: timed out", ErrorKind::Unavailable),
            ("ERROR: [youtube] abc: Unable to download API page: getaddrinfo failed", ErrorKind::Unavailable),
            ("ERROR: something new under the sun", ErrorKind::ResolverFailed),
        ] {
            assert_eq!(classify_resolver_stderr(stderr).0, kind, "{stderr}");
        }
    }

    #[test]
    fn a_title_cannot_be_used_as_storage() {
        let v = live_json(serde_json::json!({ "title": "x".repeat(5000) }));
        assert_eq!(parse_metadata(&v).unwrap().title.unwrap().chars().count(), 80);
    }
}
