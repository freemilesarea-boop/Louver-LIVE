//! A live video source, for the traffic-CCTV proof of concept.
//!
//! What this module is for: a broadcast normally takes both its picture and its
//! sound from the videos in its playlist. With a CCTV URL set, the picture comes
//! from that live stream instead and the sound still comes from the playlist —
//! see `FfmpegCommandBuilder::build_live_video_stream_args`.
//!
//! Everything here is about the URL, because the URL is the dangerous part. A
//! user types an address and the **server** opens it, which is a server-side
//! request forgery by construction: without the checks below, `http://127.0.0.1:8080/`
//! would make 247streams fetch its own API, and `http://169.254.169.254/` would
//! make it fetch the cloud provider's credential endpoint. So:
//!
//!  - only `http://` and `https://` are accepted, by the parser here *and* by
//!    FFmpeg's own `-protocol_whitelist` (an HLS playlist can name a different
//!    protocol for its segments, which is how a `file:` read gets in);
//!  - the host is resolved and **every** address it resolves to must be a
//!    public one;
//!  - credentials in the URL are refused — `http://ok.example@127.0.0.1/` is
//!    the oldest way past a host check there is;
//!  - the URL never reaches a shell. It is one element of an argv vector, as
//!    everything in this codebase is (§60), so there is nothing to escape.
//!
//! What this is *not*: a defence against a host that resolves to a public
//! address when it is checked and a private one when FFmpeg connects (DNS
//! rebinding). Closing that needs the connection itself pinned to the address
//! that was checked, which FFmpeg does not offer. It is written down rather
//! than papered over, because this is a proof of concept.

use crate::{CloudError, Result};
use louver_core::streaming::ffmpeg::FfmpegTools;
use rusqlite::OptionalExtension;
use serde::Serialize;
use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Long enough for a real playlist URL with query parameters, short enough that
/// the column is not storage.
pub const MAX_URL_CHARS: usize = 2048;

/// How long `Test Connection` may take before the child is killed.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(12);

/// How wide a catalogue thumbnail is, in pixels. Height follows the source.
///
/// Wide enough to tell a tunnel wall from a river at a glance in a grid, small
/// enough that a few thousand of them are megabytes rather than gigabytes.
pub const THUMB_WIDTH: i64 = 640;

/// How long a frame grab may take.
///
/// More than the probe, because this one has to decode: an HLS source hands
/// over a playlist, then a segment, and only then a keyframe.
pub const THUMB_TIMEOUT: Duration = Duration::from_secs(20);

/// The protocols FFmpeg may use for this input, and nothing else.
///
/// `crypto` is there because an AES-128 HLS playlist needs it; `file` is
/// deliberately absent, so a playlist that points at a local path fails instead
/// of reading the server's disk.
///
/// Re-exported from the core rather than written out again. It used to be a
/// second copy of the same string with a comment on each asking to be kept in
/// step, and nothing enforcing it. The two uses are the probe behind
/// `연결 테스트` and the broadcast itself, so a list that drifted would mean a
/// URL that tests fine and then fails on air — or worse, one the probe refuses
/// while the broadcast opens it.
pub use louver_core::streaming::ffmpeg::LIVE_SOURCE_PROTOCOLS as PROTOCOL_WHITELIST;

/// A URL that has passed [`validate`].
///
/// A separate type so that nothing can reach FFmpeg without having been
/// checked: the only way to make one is to validate a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveVideoUrl(String);

impl LiveVideoUrl {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_string(self) -> String {
        self.0
    }
}

/// Check a CCTV URL, or say why it cannot be used.
///
/// The messages are what the user reads, so they name the actual problem rather
/// than "invalid URL".
pub fn validate(raw: &str) -> Result<LiveVideoUrl> {
    let url = raw.trim();
    if url.is_empty() {
        return Err(CloudError::Invalid("CCTV 주소를 입력해 주세요.".into()));
    }
    if url.chars().count() > MAX_URL_CHARS {
        return Err(CloudError::Invalid(format!("주소가 너무 깁니다 ({MAX_URL_CHARS}자 이내).")));
    }
    // A control character or a space would break a log line, and in an argv
    // vector it is never anything the user meant to type.
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(CloudError::Invalid("주소에 공백이나 제어문자가 들어 있습니다.".into()));
    }

    let (scheme, rest) = url.split_once("://").ok_or_else(|| {
        CloudError::Invalid("http:// 또는 https:// 로 시작하는 주소만 사용할 수 있습니다.".into())
    })?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(CloudError::Invalid(format!(
            "{scheme}:// 는 사용할 수 없습니다. http:// 또는 https:// 주소를 입력해 주세요."
        )));
    }

    // The authority is everything before the first `/`, `?` or `#`.
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() {
        return Err(CloudError::Invalid("주소에 호스트가 없습니다.".into()));
    }
    if authority.contains('@') {
        // `http://trusted.example@127.0.0.1/` is read as host 127.0.0.1 by
        // every HTTP client and as host trusted.example by a careless check.
        return Err(CloudError::Invalid(
            "주소에 아이디·비밀번호를 포함할 수 없습니다. 인증이 필요한 CCTV는 이번 테스트에서 지원하지 않습니다.".into(),
        ));
    }

    let (host, port) = split_host_port(authority)?;
    if host.is_empty() {
        return Err(CloudError::Invalid("주소에 호스트가 없습니다.".into()));
    }
    if is_metadata_host(&host) {
        return Err(CloudError::Invalid("내부 메타데이터 주소는 사용할 수 없습니다.".into()));
    }
    if let Some(site) = watch_page_site(&host) {
        return Err(CloudError::Invalid(format!(
            "{site} 링크는 사용할 수 없습니다. {site} 주소는 영상이 아니라 웹페이지라서, \
             FFmpeg가 열면 영상 트랙을 찾지 못합니다. CCTV·카메라가 직접 제공하는 \
             HLS(.m3u8) 등의 스트림 주소를 입력해 주세요."
        )));
    }

    let port = port.unwrap_or(if scheme == "https" { 443 } else { 80 });
    for ip in resolve(&host, port)? {
        if let Some(why) = blocked_reason(ip) {
            return Err(CloudError::Invalid(format!(
                "{host} 는 {why} 주소({ip})로 연결됩니다. 외부에서 접속할 수 있는 CCTV 주소만 사용할 수 있습니다."
            )));
        }
    }

    Ok(LiveVideoUrl(url.to_string()))
}

/// `example.com:8080` → `("example.com", Some(8080))`, `[::1]:80` → `("::1", Some(80))`.
fn split_host_port(authority: &str) -> Result<(String, Option<u16>)> {
    // A bracketed IPv6 literal, whose colons are part of the address.
    if let Some(stripped) = authority.strip_prefix('[') {
        let (host, tail) = stripped
            .split_once(']')
            .ok_or_else(|| CloudError::Invalid("IPv6 주소의 괄호가 닫히지 않았습니다.".into()))?;
        let port = match tail.strip_prefix(':') {
            Some(p) => Some(parse_port(p)?),
            None => None,
        };
        return Ok((host.to_ascii_lowercase(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, p)) => Ok((host.to_ascii_lowercase(), Some(parse_port(p)?))),
        None => Ok((authority.to_ascii_lowercase(), None)),
    }
}

fn parse_port(raw: &str) -> Result<u16> {
    raw.parse::<u16>()
        .ok()
        .filter(|p| *p > 0)
        .ok_or_else(|| CloudError::Invalid(format!("포트 번호가 올바르지 않습니다: {raw}")))
}

/// Sites whose URLs are web pages rather than streams.
///
/// FFmpeg opens a URL and expects a container. A `youtube.com/watch?v=…`
/// address is HTML, so FFmpeg answers `Invalid data found when processing
/// input` — which is true and tells the customer nothing. Worse, that only
/// happens at *start* time: the address saves cleanly, the broadcast then
/// fails, and the supervisor retries it every sixty seconds for ever.
///
/// Extracting a playable stream from one of these pages needs a separate tool
/// (`yt-dlp` and the like), which this service does not ship and which these
/// sites' terms do not generally permit. So the answer is no, and the place to
/// say no is here, where the customer is still looking at the field.
///
/// Matched on the page hosts only. `googlevideo.com` and the other CDN hosts
/// are deliberately absent: a signed media URL from one of them is a real
/// stream that FFmpeg can read, and refusing it would be wrong.
fn watch_page_site(host: &str) -> Option<&'static str> {
    const SITES: &[(&str, &str)] = &[
        ("youtube.com", "YouTube"),
        ("youtu.be", "YouTube"),
        ("youtube-nocookie.com", "YouTube"),
        ("twitch.tv", "Twitch"),
        ("vimeo.com", "Vimeo"),
        ("tiktok.com", "TikTok"),
        ("instagram.com", "Instagram"),
        ("facebook.com", "Facebook"),
        ("fb.watch", "Facebook"),
        ("chzzk.naver.com", "치지직"),
        ("tv.naver.com", "네이버 TV"),
        ("tv.kakao.com", "카카오 TV"),
        ("afreecatv.com", "아프리카TV"),
        ("sooplive.co.kr", "SOOP"),
    ];
    SITES.iter().find_map(|(domain, name)| {
        // `youtube.com` and `www.youtube.com` and `m.youtube.com`, but not
        // `notyoutube.com` — the dot is what makes it a subdomain.
        (host == *domain || host.ends_with(&format!(".{domain}"))).then_some(*name)
    })
}

/// Host names that name an internal service whatever they resolve to.
fn is_metadata_host(host: &str) -> bool {
    const NAMES: &[&str] = &[
        "localhost",
        "metadata",
        "metadata.google.internal",
        "metadata.goog",
        "instance-data",
        "instance-data.ec2.internal",
    ];
    NAMES.contains(&host) || host.ends_with(".localhost") || host.ends_with(".internal")
}

fn resolve(host: &str, port: u16) -> Result<Vec<IpAddr>> {
    let addrs: Vec<IpAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| CloudError::Invalid(format!("{host} 주소를 찾을 수 없습니다: {e}")))?
        .map(|s| s.ip())
        .collect();
    if addrs.is_empty() {
        return Err(CloudError::Invalid(format!("{host} 주소를 찾을 수 없습니다.")));
    }
    Ok(addrs)
}

/// Why this address may not be fetched, or `None` when it may.
///
/// Spelled out range by range rather than relying on `is_global`, which is
/// still unstable: an address this misses is an address the server will open.
pub fn blocked_reason(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            if v4.is_loopback() {
                Some("루프백")
            } else if v4.is_private() {
                Some("사설망")
            } else if v4.is_link_local() {
                // 169.254.0.0/16, which is where every cloud metadata endpoint
                // lives.
                Some("링크 로컬")
            } else if v4.is_unspecified() || o[0] == 0 {
                Some("예약된")
            } else if v4.is_broadcast() || v4.is_multicast() {
                Some("브로드캐스트·멀티캐스트")
            } else if o[0] == 100 && (64..128).contains(&o[1]) {
                Some("통신사 내부(CGNAT)")
            } else if o[0] == 192 && o[1] == 0 && (o[2] == 0 || o[2] == 2) {
                Some("예약된")
            } else if o[0] == 198 && (o[1] == 18 || o[1] == 19) {
                Some("벤치마크 전용")
            } else if (o[0] == 198 && o[1] == 51 && o[2] == 100) || (o[0] == 203 && o[1] == 0 && o[2] == 113)
            {
                Some("문서용")
            } else if o[0] >= 240 {
                Some("예약된")
            } else {
                None
            }
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                // `::ffff:127.0.0.1` is loopback wearing a hat.
                return blocked_reason(IpAddr::V4(v4));
            }
            let s = v6.segments();
            if v6.is_loopback() {
                Some("루프백")
            } else if v6.is_unspecified() {
                Some("예약된")
            } else if v6.is_multicast() {
                Some("멀티캐스트")
            } else if (s[0] & 0xffc0) == 0xfe80 {
                Some("링크 로컬")
            } else if (s[0] & 0xfe00) == 0xfc00 {
                Some("사설망(ULA)")
            } else if s[0] == 0x0064 && s[1] == 0xff9b {
                // 64:ff9b::/96 translates to an IPv4 address.
                Some("IPv4 변환")
            } else if s[0] == 0x2001 && s[1] == 0x0db8 {
                Some("문서용")
            } else {
                None
            }
        }
    }
}

/* ------------------------------------------------------------- the probe */

/// What `Test Connection` found.
#[derive(Debug, Clone, Serialize, Default)]
pub struct CctvCheck {
    pub ok: bool,
    /// A short machine-readable cause when `ok` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What to show the user, in Korean. Set either way.
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<i64>,
    /// As a decimal, e.g. "29.97". From `avg_frame_rate`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<String>,
    /// FFmpeg's own name for the container, e.g. `hls`, `mpegts`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_type: Option<String>,
    /// Whether the source carries sound. Reported so the user can see that it
    /// is being dropped on purpose; it is never sent to YouTube.
    pub has_audio: bool,
}

impl CctvCheck {
    fn failed(reason: &str, message: impl Into<String>) -> Self {
        Self { ok: false, reason: Some(reason.to_string()), message: message.into(), ..Default::default() }
    }
}

/// ffprobe's argv for a live source. Input options precede `-i`, as always.
pub fn probe_args(url: &str, timeout: Duration) -> Vec<String> {
    vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-print_format".into(),
        "json".into(),
        "-show_streams".into(),
        "-show_format".into(),
        // Microseconds, and per read: a source that accepts the connection and
        // then says nothing is the common CCTV failure.
        "-rw_timeout".into(),
        (timeout.as_micros() as u64).to_string(),
        "-protocol_whitelist".into(),
        PROTOCOL_WHITELIST.into(),
        // Enough of the stream to see the video parameters, not enough to sit
        // here for a minute.
        "-analyzeduration".into(),
        "4000000".into(),
        "-probesize".into(),
        "4000000".into(),
        "-i".into(),
        url.to_string(),
    ]
}

/// Open the URL with ffprobe and report what is there.
///
/// Validates first, so this cannot be used to reach an address [`validate`]
/// refuses. The child is killed at `PROBE_TIMEOUT` whatever it is doing, so a
/// test that hangs leaves nothing behind.
pub fn test_connection(tools: &FfmpegTools, raw: &str) -> Result<CctvCheck> {
    let url = validate(raw)?;
    let mut cmd = Command::new(&tools.ffprobe);
    cmd.args(probe_args(url.as_str(), PROBE_TIMEOUT));
    let run = match run_with_timeout(cmd, PROBE_TIMEOUT) {
        Ok(r) => r,
        Err(e) => {
            return Ok(CctvCheck::failed("ffprobe_unavailable", format!("ffprobe를 실행할 수 없습니다: {e}")))
        }
    };

    if run.timed_out {
        return Ok(CctvCheck::failed(
            "timeout",
            format!(
                "{}초 안에 응답이 없습니다. 주소가 맞는지, 외부에서 접속 가능한지 확인해 주세요.",
                PROBE_TIMEOUT.as_secs()
            ),
        ));
    }
    let stderr = String::from_utf8_lossy(&run.stderr).to_string();
    if !run.ok {
        let (reason, message) = classify(&stderr);
        return Ok(CctvCheck::failed(reason, message));
    }

    let parsed: serde_json::Value = match serde_json::from_slice(&run.stdout) {
        Ok(v) => v,
        Err(e) => return Ok(CctvCheck::failed("unreadable", format!("스트림 정보를 읽을 수 없습니다: {e}"))),
    };
    Ok(summarise(&parsed))
}

/// ffmpeg's argv for one frame out of a live source, written to `dest`.
///
/// `-frames:v 1` and nothing else: no audio, no second output, no re-connect.
/// The scale filter keeps the aspect ratio — `-2` rounds the height to an even
/// number, which JPEG does not care about but every encoder after it does.
pub fn thumb_args(url: &str, dest: &std::path::Path, timeout: Duration) -> Vec<String> {
    vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-rw_timeout".into(),
        (timeout.as_micros() as u64).to_string(),
        "-protocol_whitelist".into(),
        PROTOCOL_WHITELIST.into(),
        "-i".into(),
        url.to_string(),
        "-frames:v".into(),
        "1".into(),
        "-vf".into(),
        format!("scale={THUMB_WIDTH}:-2"),
        // 2 is visually lossless and 31 is mush; 6 is a thumbnail.
        "-q:v".into(),
        "6".into(),
        "-f".into(),
        "image2".into(),
        "-y".into(),
        dest.to_string_lossy().to_string(),
    ]
}

/// Grab one frame and leave it at `dest`.
///
/// Validated first, so this cannot reach an address [`validate`] refuses — the
/// same rule the probe follows, for the same reason.
///
/// Written to a sibling and renamed, because the file this produces is served
/// to customers: a half-written JPEG under the real name would be served as a
/// broken image, and a rename is the only way to publish bytes all at once.
///
/// A failure here is not a failure of the camera. The caller has usually just
/// established that the stream opens, and a picture that could not be decoded
/// — a source that only hands over audio, a codec ffmpeg declines — is a
/// missing thumbnail, not a missing camera. So this reports the error and
/// lets the caller carry on.
pub fn capture_thumb(tools: &FfmpegTools, raw: &str, dest: &std::path::Path) -> Result<()> {
    let url = validate(raw)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // The pid keeps two captures of the same camera from writing one temp file.
    let staging = dest.with_extension(format!("part{}", std::process::id()));
    let mut cmd = Command::new(&tools.ffmpeg);
    cmd.args(thumb_args(url.as_str(), &staging, THUMB_TIMEOUT));
    let run = match run_with_timeout(cmd, THUMB_TIMEOUT) {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_file(&staging);
            return Err(CloudError::Invalid(format!("ffmpeg를 실행할 수 없습니다: {e}")));
        }
    };
    if run.timed_out || !run.ok {
        let _ = std::fs::remove_file(&staging);
        let why = if run.timed_out {
            format!("{}초 안에 한 장면도 받지 못했습니다.", THUMB_TIMEOUT.as_secs())
        } else {
            let stderr = String::from_utf8_lossy(&run.stderr);
            classify(&stderr).1
        };
        return Err(CloudError::Invalid(format!("미리보기를 만들지 못했습니다: {why}")));
    }
    // ffmpeg can exit 0 having written nothing when the stream ends before a
    // frame arrives. An empty file published under the real name is a broken
    // image on every card in the grid.
    match std::fs::metadata(&staging) {
        Ok(m) if m.len() > 0 => {}
        _ => {
            let _ = std::fs::remove_file(&staging);
            return Err(CloudError::Invalid("미리보기가 비어 있습니다.".into()));
        }
    }
    std::fs::rename(&staging, dest)?;
    Ok(())
}

/// Turn ffprobe's JSON into the answer the UI shows.
pub fn summarise(parsed: &serde_json::Value) -> CctvCheck {
    let streams = parsed.get("streams").and_then(|s| s.as_array()).cloned().unwrap_or_default();
    let video = streams.iter().find(|s| s.get("codec_type").and_then(|t| t.as_str()) == Some("video"));
    let has_audio = streams.iter().any(|s| s.get("codec_type").and_then(|t| t.as_str()) == Some("audio"));
    let stream_type = parsed
        .get("format")
        .and_then(|f| f.get("format_name"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string());

    let Some(v) = video else {
        return CctvCheck {
            has_audio,
            stream_type,
            ..CctvCheck::failed(
                "no_video",
                "이 주소에는 영상 트랙이 없습니다. 영상이 포함된 CCTV 스트림이 필요합니다.",
            )
        };
    };

    let codec = v.get("codec_name").and_then(|c| c.as_str()).map(|s| s.to_string());
    let width = v.get("width").and_then(|w| w.as_i64());
    let height = v.get("height").and_then(|h| h.as_i64());
    let fps = v
        .get("avg_frame_rate")
        .or_else(|| v.get("r_frame_rate"))
        .and_then(|f| f.as_str())
        .and_then(parse_rate);

    let size = match (width, height) {
        (Some(w), Some(h)) => format!("{w}×{h}"),
        _ => "해상도 미확인".to_string(),
    };
    let message = format!(
        "연결됨 · {} · {size}{}{}",
        codec.clone().unwrap_or_else(|| "코덱 미확인".into()),
        fps.clone().map(|f| format!(" · {f}fps")).unwrap_or_default(),
        if has_audio { " · 원본 오디오 있음(송출에는 쓰지 않습니다)" } else { "" },
    );
    CctvCheck {
        ok: true,
        reason: None,
        message,
        video_codec: codec,
        width,
        height,
        fps,
        stream_type,
        has_audio,
    }
}

/// `"30000/1001"` → `"29.97"`. `"0/0"` → `None`.
fn parse_rate(raw: &str) -> Option<String> {
    let (num, den) = raw.split_once('/')?;
    let (num, den): (f64, f64) = (num.parse().ok()?, den.parse().ok()?);
    if den == 0.0 || num == 0.0 {
        return None;
    }
    let fps = num / den;
    Some(if (fps - fps.round()).abs() < 0.01 {
        format!("{}", fps.round() as i64)
    } else {
        format!("{fps:.2}")
    })
}

/// ffprobe's complaint, as a cause and a sentence.
pub fn classify(stderr: &str) -> (&'static str, String) {
    let e = stderr.to_lowercase();
    if e.contains("401") || e.contains("unauthorized") {
        (
            "unauthorized",
            "인증이 필요한 주소입니다. 이번 테스트에서는 인증이 필요한 CCTV를 지원하지 않습니다.".into(),
        )
    } else if e.contains("403") || e.contains("forbidden") {
        ("forbidden", "서버가 접속을 거부했습니다(403). 외부 접근이 허용된 주소인지 확인해 주세요.".into())
    } else if e.contains("404") || e.contains("not found") {
        ("not_found", "해당 주소에 스트림이 없습니다(404).".into())
    } else if e.contains("connection refused") {
        ("connection_refused", "연결이 거부되었습니다. 주소와 포트를 확인해 주세요.".into())
    } else if e.contains("timed out") || e.contains("timeout") || e.contains("operation now in progress") {
        ("timeout", "응답 시간을 초과했습니다.".into())
    } else if e.contains("protocol not on whitelist") {
        (
            "protocol_blocked",
            "이 스트림이 허용되지 않은 프로토콜을 사용합니다. http/https 스트림만 사용할 수 있습니다.".into(),
        )
    } else if e.contains("invalid data found") || e.contains("invalid argument") {
        (
            "unsupported",
            "읽을 수 있는 스트림 형식이 아닙니다. HLS(.m3u8) 등 FFmpeg가 읽을 수 있는 주소가 필요합니다."
                .into(),
        )
    } else if e.contains("no such host")
        || e.contains("name or service not known")
        || e.contains("failed to resolve")
    {
        ("dns", "주소를 찾을 수 없습니다. 도메인이 맞는지 확인해 주세요.".into())
    } else {
        let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("알 수 없는 오류").trim();
        ("failed", format!("스트림을 열지 못했습니다: {first}"))
    }
}

/* ------------------------------------------------------- running a child */

struct Finished {
    ok: bool,
    timed_out: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Run a command, killing it at `limit`.
///
/// Both pipes are drained by their own thread: a probe that writes more than a
/// pipe buffer would otherwise block forever and never be seen to exit, which
/// is the deadlock this shape exists to avoid.
fn run_with_timeout(mut cmd: Command, limit: Duration) -> std::io::Result<Finished> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let mut out = child.stdout.take().expect("piped");
    let mut err = child.stderr.take().expect("piped");
    let out_t = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let err_t = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        b
    });

    let deadline = Instant::now() + limit;
    let (ok, timed_out) = loop {
        match child.try_wait()? {
            Some(status) => break (status.success(), false),
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break (false, true);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    Ok(Finished {
        ok,
        timed_out,
        stdout: out_t.join().unwrap_or_default(),
        stderr: err_t.join().unwrap_or_default(),
    })
}

/* --------------------------------------------------------- the catalogue */

/// How many rows one import may carry.
///
/// A published camera list is hundreds of rows and an operator pastes the whole
/// thing, so this is generous. It exists so that a mistake — a whole different
/// file, a runaway export — is refused rather than written.
pub const MAX_IMPORT_ROWS: usize = 2000;

/// A camera an operator is adding, before it is a row.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct NewCctvSource {
    pub region: String,
    pub provider: String,
    pub name: String,
    pub url: String,
}

/// What became of one row of an import.
///
/// Per row and not per file: a list of three hundred cameras with four bad
/// addresses in it should import two hundred and ninety six, and say which
/// four it did not and why.
#[derive(Debug, Clone, Serialize)]
pub struct ImportOutcome {
    pub url: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub message: String,
}

/// `kr` → `KR`. A two-letter ISO code and nothing else, so the UI can group on
/// it without matching prose.
fn region_code(raw: &str) -> Result<String> {
    let c = raw.trim().to_ascii_uppercase();
    if c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase()) {
        return Ok(c);
    }
    Err(CloudError::Invalid(format!("지역 코드는 두 글자 국가 코드여야 합니다 (KR, US, JP, GB 등): {raw}")))
}

fn one_line(raw: &str, what: &str, max: usize) -> Result<String> {
    let t = raw.trim();
    if t.is_empty() {
        return Err(CloudError::Invalid(format!("{what}을(를) 입력해 주세요.")));
    }
    if t.chars().count() > max {
        return Err(CloudError::Invalid(format!("{what}이(가) 너무 깁니다 ({max}자 이내).")));
    }
    if t.chars().any(char::is_control) {
        return Err(CloudError::Invalid(format!("{what}에 제어문자가 들어 있습니다.")));
    }
    Ok(t.to_string())
}

fn row_to_source(r: &rusqlite::Row<'_>) -> rusqlite::Result<crate::models::CctvSource> {
    Ok(crate::models::CctvSource {
        id: r.get("id")?,
        region: r.get("region")?,
        provider: r.get("provider")?,
        name: r.get("name")?,
        url: r.get("url")?,
        active: r.get::<_, i64>("active")? != 0,
        checked_at: r.get("checked_at")?,
        checked_ok: r.get::<_, i64>("checked_ok")? != 0,
        check_note: r.get("check_note")?,
    })
}

const SOURCE_COLUMNS: &str = "SELECT id, region, provider, name, url, active, checked_at, \
                              checked_ok, check_note FROM cctv_sources";

impl crate::CloudDb {
    /// The cameras a customer may choose from.
    ///
    /// Active **and** opened by this server. An imported row is somebody else's
    /// claim about a URL; until `record_cctv_check` has seen FFmpeg open it,
    /// it is not offered. That is what keeps four bad rows in a list of three
    /// hundred from becoming four broken broadcasts.
    pub fn cctv_sources_for_pick(&self) -> Result<Vec<crate::models::CctvSource>> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare(&format!(
            "{SOURCE_COLUMNS} WHERE active = 1 AND checked_ok = 1 ORDER BY region, provider, name"
        ))?;
        let rows = st.query_map([], row_to_source)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Every camera, including the ones that failed and the retired ones. For
    /// the operator console, which is the only place the failures are useful.
    pub fn cctv_sources_all(&self) -> Result<Vec<crate::models::CctvSource>> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare(&format!("{SOURCE_COLUMNS} ORDER BY region, provider, name"))?;
        let rows = st.query_map([], row_to_source)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn cctv_source(&self, id: &str) -> Result<crate::models::CctvSource> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        guard
            .query_row(&format!("{SOURCE_COLUMNS} WHERE id = ?1"), rusqlite::params![id], row_to_source)
            .optional()?
            .ok_or(CloudError::NotFound("cctv_source"))
    }

    /// Add or update cameras from an operator's list.
    ///
    /// The address goes through the same `validate` a customer's typed URL
    /// does, so an import cannot put an address into the database that the
    /// form would have refused — a list is not a way around the address rules.
    ///
    /// Importing the same list again updates the rows rather than duplicating
    /// them, and **resets the check**: a row whose name or address moved has
    /// not been opened in its new form, whatever the old row said.
    pub fn import_cctv_sources(&self, rows: &[NewCctvSource]) -> Result<Vec<ImportOutcome>> {
        if rows.len() > MAX_IMPORT_ROWS {
            return Err(CloudError::Invalid(format!(
                "한 번에 {MAX_IMPORT_ROWS}개까지 등록할 수 있습니다 ({}개를 받았습니다).",
                rows.len()
            )));
        }
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            match self.import_one(r) {
                Ok(id) => out.push(ImportOutcome {
                    url: r.url.trim().to_string(),
                    ok: true,
                    id: Some(id),
                    message: "등록했습니다. 연결 확인을 거쳐야 사용자에게 보입니다.".into(),
                }),
                Err(e) => out.push(ImportOutcome {
                    url: r.url.trim().to_string(),
                    ok: false,
                    id: None,
                    message: e.to_string(),
                }),
            }
        }
        Ok(out)
    }

    fn import_one(&self, r: &NewCctvSource) -> Result<String> {
        let region = region_code(&r.region)?;
        let provider = one_line(&r.provider, "제공처", 120)?;
        let name = one_line(&r.name, "카메라 이름", 200)?;
        // Resolves a host, so it is done before the connection is locked.
        let url = validate(&r.url)?.as_str().to_string();

        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let existing: Option<String> = guard
            .query_row("SELECT id FROM cctv_sources WHERE url = ?1", rusqlite::params![&url], |x| x.get(0))
            .optional()?;
        match existing {
            Some(id) => {
                guard.execute(
                    "UPDATE cctv_sources
                        SET region = ?2, provider = ?3, name = ?4, active = 1,
                            checked_at = NULL, checked_ok = 0, check_note = NULL
                      WHERE id = ?1",
                    rusqlite::params![&id, &region, &provider, &name],
                )?;
                Ok(id)
            }
            None => {
                let id = crate::new_id();
                guard.execute(
                    "INSERT INTO cctv_sources (id, region, provider, name, url) VALUES (?1,?2,?3,?4,?5)",
                    rusqlite::params![&id, &region, &provider, &name, &url],
                )?;
                Ok(id)
            }
        }
    }

    /// Record what FFmpeg found at a camera's address.
    ///
    /// The only thing that makes a row visible to a customer. The note is the
    /// operator's: it carries the probe's own message, including the failure
    /// reason, and is never shown in the form.
    pub fn record_cctv_check(&self, id: &str, check: &CctvCheck) -> Result<()> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let n = guard.execute(
            "UPDATE cctv_sources
                SET checked_at = datetime('now'), checked_ok = ?2, check_note = ?3
              WHERE id = ?1",
            rusqlite::params![id, check.ok as i64, &check.message],
        )?;
        if n == 0 {
            return Err(CloudError::NotFound("cctv_source"));
        }
        Ok(())
    }

    /// Retire a camera, or bring it back. Retiring leaves the row, so a
    /// broadcast that stopped working still has something to point at when
    /// somebody asks why.
    pub fn set_cctv_source_active(&self, id: &str, active: bool) -> Result<()> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let n = guard.execute(
            "UPDATE cctv_sources SET active = ?2 WHERE id = ?1",
            rusqlite::params![id, active as i64],
        )?;
        if n == 0 {
            return Err(CloudError::NotFound("cctv_source"));
        }
        Ok(())
    }

    pub fn delete_cctv_source(&self, id: &str) -> Result<()> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let n = guard.execute("DELETE FROM cctv_sources WHERE id = ?1", rusqlite::params![id])?;
        if n == 0 {
            return Err(CloudError::NotFound("cctv_source"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A public IP literal, so nothing here depends on a DNS server.
    const PUBLIC: &str = "https://93.184.216.34/live/stream.m3u8";

    fn refused(url: &str) -> String {
        match validate(url) {
            Ok(ok) => panic!("{url} was accepted as {}", ok.as_str()),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn a_public_https_stream_is_accepted() {
        assert_eq!(validate(PUBLIC).unwrap().as_str(), PUBLIC);
        assert!(validate("http://93.184.216.34:8080/hls/cam1.m3u8").is_ok());
    }

    #[test]
    fn a_youtube_link_is_refused_where_the_customer_can_still_see_the_field() {
        // The reported case. It used to save cleanly and then fail at start
        // time, every sixty seconds, for ever.
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.com/live/abc123",
            "https://m.youtube.com/watch?v=x",
            "https://youtu.be/dQw4w9WgXcQ",
            "https://www.youtube-nocookie.com/embed/x",
        ] {
            let e = refused(url);
            assert!(e.contains("YouTube"), "the site is named: {e}");
            assert!(e.contains("웹페이지"), "the reason is given: {e}");
            assert!(e.contains("m3u8"), "and what to use instead: {e}");
        }
    }

    #[test]
    fn the_other_watch_page_sites_are_refused_by_their_own_names() {
        for (url, name) in [
            ("https://www.twitch.tv/somechannel", "Twitch"),
            ("https://vimeo.com/123456", "Vimeo"),
            ("https://chzzk.naver.com/live/abc", "치지직"),
            ("https://play.sooplive.co.kr/abc", "SOOP"),
        ] {
            let e = refused(url);
            assert!(e.contains(name), "expected {name} in: {e}");
        }
    }

    #[test]
    fn a_hostname_that_merely_ends_in_a_site_name_is_not_that_site() {
        // `notyoutube.com` is somebody else's domain and a camera could live
        // on it; only the domain itself and its subdomains are refused.
        //
        // Asserted against `watch_page_site` and not through `validate`,
        // because `validate` resolves the host. Going through it made this a
        // test of whichever DNS the machine happens to have: in a sandbox with
        // no resolver the address is refused for being unresolvable and the
        // assertion passes for the wrong reason, while on a runner with real
        // DNS `notyoutube.com` resolves to a public address, `validate`
        // returns `Ok`, and the test fails. It did exactly that.
        for host in ["notyoutube.com", "myyoutu.be", "youtube.com.example.org", "fakeyoutu.be"] {
            assert!(watch_page_site(host).is_none(), "matched too greedily: {host}");
        }
        // And the ones that are the site, including subdomains.
        for host in ["youtube.com", "www.youtube.com", "m.youtube.com", "youtu.be"] {
            assert_eq!(watch_page_site(host), Some("YouTube"), "missed: {host}");
        }
    }

    #[test]
    fn a_signed_cdn_url_is_still_allowed() {
        // `googlevideo.com` serves real media that FFmpeg can read. Refusing
        // the page hosts must not sweep up the CDN.
        assert!(watch_page_site("rr3---sn-abc.googlevideo.com").is_none());
        assert!(watch_page_site("video-cdn.example.com").is_none());
    }

    #[test]
    fn the_probe_and_the_broadcast_are_given_the_same_protocol_list() {
        // Two copies of this string used to exist, each with a comment asking
        // to be kept in step with the other. A list that drifted would mean a
        // URL that passes 연결 테스트 and then fails on air.
        assert_eq!(PROTOCOL_WHITELIST, louver_core::streaming::ffmpeg::LIVE_SOURCE_PROTOCOLS);
        assert!(!PROTOCOL_WHITELIST.split(',').any(|p| p == "file"), "file must never be on it");
        let args = probe_args("https://cam.example/s.m3u8", PROBE_TIMEOUT);
        let i = args.iter().position(|a| a == "-protocol_whitelist").expect("the probe sets it");
        assert_eq!(args[i + 1], louver_core::streaming::ffmpeg::LIVE_SOURCE_PROTOCOLS);
    }

    #[test]
    fn the_server_will_not_fetch_itself() {
        // The whole reason this module exists: these are requests to 247streams
        // from 247streams, which is what SSRF means here.
        for url in [
            "http://127.0.0.1:8080/api/me",
            "http://127.1/api",
            "http://localhost:8080/health",
            "http://LOCALHOST/health",
            "http://[::1]:8080/api",
            "http://0.0.0.0/",
            "http://[::ffff:127.0.0.1]/",
        ] {
            let why = refused(url);
            assert!(!why.is_empty(), "{url}");
        }
    }

    #[test]
    fn a_cloud_metadata_endpoint_is_refused() {
        // 169.254.169.254 is where a provider hands out credentials.
        assert!(refused("http://169.254.169.254/latest/meta-data/").contains("링크 로컬"));
        assert!(refused("http://metadata.google.internal/computeMetadata/v1/").contains("내부"));
        assert!(refused("http://metadata/computeMetadata/").contains("내부"));
    }

    #[test]
    fn private_and_reserved_ranges_are_refused() {
        for (url, expect) in [
            ("http://10.0.0.5/s.m3u8", "사설망"),
            ("http://172.16.4.9/s.m3u8", "사설망"),
            ("http://192.168.1.10/s.m3u8", "사설망"),
            ("http://100.100.1.1/s.m3u8", "통신사"),
            ("http://[fd00::1]/s.m3u8", "사설망"),
            ("http://[fe80::1]/s.m3u8", "링크 로컬"),
            ("http://198.18.0.1/s.m3u8", "벤치마크"),
            ("http://203.0.113.9/s.m3u8", "문서용"),
            ("http://240.0.0.1/s.m3u8", "예약된"),
        ] {
            assert!(refused(url).contains(expect), "{url} → {}", refused(url));
        }
    }

    #[test]
    fn only_http_and_https_are_accepted() {
        for url in [
            "file:///etc/passwd",
            "ftp://93.184.216.34/x",
            // The two that read a local path under FFmpeg.
            "concat:/etc/passwd",
            "data:text/plain,hello",
            // No scheme at all.
            "93.184.216.34/live.m3u8",
            // A scheme FFmpeg speaks but this test does not take.
            "rtsp://93.184.216.34/cam",
            "rtmp://93.184.216.34/live",
        ] {
            assert!(!refused(url).is_empty(), "{url}");
        }
    }

    #[test]
    fn credentials_in_the_url_are_refused_because_they_hide_the_host() {
        // Read as host 127.0.0.1 by every HTTP client, and as host
        // "trusted.example" by a check that looks at the wrong half.
        let why = refused("http://trusted.example@127.0.0.1/api");
        assert!(why.contains("아이디"), "{why}");
        assert!(!refused("http://user:pw@93.184.216.34/s.m3u8").is_empty());
    }

    #[test]
    fn an_argument_cannot_be_smuggled_in_the_url() {
        // There is no shell, so this is about the argv: a space or a newline in
        // a URL is never what the user meant and would split a log line.
        for url in [
            "https://93.184.216.34/a.m3u8 -i /etc/passwd",
            "https://93.184.216.34/a.m3u8\nrtmp://evil/live",
            "https://93.184.216.34/a.m3u8\t-f",
            "  ",
            "",
        ] {
            assert!(!refused(url).is_empty(), "{url:?}");
        }
    }

    #[test]
    fn a_url_cannot_be_used_as_storage() {
        let long = format!("https://93.184.216.34/{}", "a".repeat(MAX_URL_CHARS));
        assert!(refused(&long).contains("너무 깁니다"));
    }

    #[test]
    fn a_port_has_to_be_a_port() {
        assert!(refused("http://93.184.216.34:0/x").contains("포트"));
        assert!(refused("http://93.184.216.34:99999/x").contains("포트"));
        assert!(refused("http://93.184.216.34:ssh/x").contains("포트"));
    }

    #[test]
    fn the_probe_reads_what_ffprobe_says() {
        let json = serde_json::json!({
            "streams": [
                { "codec_type": "video", "codec_name": "h264", "width": 1280, "height": 720,
                  "avg_frame_rate": "30000/1001" },
                { "codec_type": "audio", "codec_name": "aac" }
            ],
            "format": { "format_name": "hls" }
        });
        let c = summarise(&json);
        assert!(c.ok);
        assert_eq!(c.video_codec.as_deref(), Some("h264"));
        assert_eq!((c.width, c.height), (Some(1280), Some(720)));
        assert_eq!(c.fps.as_deref(), Some("29.97"));
        assert_eq!(c.stream_type.as_deref(), Some("hls"));
        // Reported, so the user can see it is being dropped on purpose.
        assert!(c.has_audio);
        assert!(c.message.contains("원본 오디오"), "{}", c.message);
    }

    #[test]
    fn a_thumbnail_asks_for_one_frame_and_nothing_else() {
        let args = thumb_args(
            "https://cam.example/live/a.m3u8",
            std::path::Path::new("/tmp/x.jpg"),
            Duration::from_secs(20),
        );
        let joined = args.join(" ");
        // One frame. Without this ffmpeg would sit on a live source forever.
        assert!(joined.contains("-frames:v 1"), "{joined}");
        // The same whitelist as the probe: a thumbnail must not be a way to
        // reach a protocol the probe refuses.
        let w = args.iter().position(|a| a == "-protocol_whitelist").expect("whitelist");
        assert_eq!(args[w + 1], PROTOCOL_WHITELIST);
        assert!(!args[w + 1].contains("file"), "file:// must stay out");
        // Input options before -i, or ffmpeg applies them to the output.
        let i = args.iter().position(|a| a == "-i").expect("-i");
        assert!(w < i, "whitelist must precede -i");
        assert!(joined.contains(&format!("scale={THUMB_WIDTH}:-2")), "{joined}");
        assert_eq!(args.last().map(String::as_str), Some("/tmp/x.jpg"));
    }

    #[test]
    fn a_thumbnail_is_never_written_for_an_address_the_probe_would_refuse() {
        let tools = FfmpegTools { ffmpeg: "ffmpeg".into(), ffprobe: "ffprobe".into() };
        let dir = std::env::temp_dir().join(format!("louver-thumb-{}", std::process::id()));
        let dest = dir.join("x.jpg");
        for bad in [
            "http://127.0.0.1/a.m3u8",
            "https://www.youtube.com/watch?v=abc",
            "file:///etc/passwd",
        ] {
            assert!(capture_thumb(&tools, bad, &dest).is_err(), "{bad}");
            assert!(!dest.exists(), "{bad} left a file behind");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_audio_only_url_is_not_a_video_source() {
        let json = serde_json::json!({
            "streams": [{ "codec_type": "audio", "codec_name": "aac" }],
            "format": { "format_name": "mp3" }
        });
        let c = summarise(&json);
        assert!(!c.ok);
        assert_eq!(c.reason.as_deref(), Some("no_video"));
    }

    #[test]
    fn a_whole_frame_rate_reads_as_a_whole_number() {
        let json = serde_json::json!({
            "streams": [{ "codec_type": "video", "codec_name": "hevc", "width": 640, "height": 480,
                          "avg_frame_rate": "25/1" }],
            "format": { "format_name": "mpegts" }
        });
        assert_eq!(summarise(&json).fps.as_deref(), Some("25"));
        // A camera that reports no rate at all says so rather than "0fps".
        let none = serde_json::json!({
            "streams": [{ "codec_type": "video", "codec_name": "hevc", "avg_frame_rate": "0/0" }],
            "format": { "format_name": "mpegts" }
        });
        assert!(summarise(&none).fps.is_none());
    }

    #[test]
    fn ffprobes_complaints_become_causes_a_user_can_act_on() {
        for (stderr, reason) in [
            ("Server returned 401 Unauthorized", "unauthorized"),
            ("Server returned 403 Forbidden", "forbidden"),
            ("Server returned 404 Not Found", "not_found"),
            ("tcp://x: Connection refused", "connection_refused"),
            ("Connection timed out", "timeout"),
            ("Protocol not on whitelist 'file,crypto'!", "protocol_blocked"),
            ("Invalid data found when processing input", "unsupported"),
            ("Failed to resolve hostname cam.example", "dns"),
        ] {
            assert_eq!(classify(stderr).0, reason, "{stderr}");
        }
        // Anything else keeps FFmpeg's own first line rather than inventing one.
        let (reason, message) = classify("something nobody has seen before\nsecond line");
        assert_eq!(reason, "failed");
        assert!(message.contains("something nobody has seen before"), "{message}");
    }

    #[test]
    fn the_probe_argv_puts_its_options_before_the_input() {
        let a = probe_args("https://cam.example/s.m3u8", Duration::from_secs(9));
        let i = a.iter().position(|x| x == "-i").unwrap();
        for flag in ["-rw_timeout", "-protocol_whitelist", "-print_format"] {
            assert!(a.iter().position(|x| x == flag).unwrap() < i, "{flag}");
        }
        assert_eq!(a.last().unwrap(), "https://cam.example/s.m3u8");
        assert_eq!(a[a.iter().position(|x| x == "-rw_timeout").unwrap() + 1], "9000000");
    }
}
