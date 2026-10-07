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

/// The protocols FFmpeg may use for this input, and nothing else.
///
/// `crypto` is here because an AES-128 HLS playlist needs it; `file` is
/// deliberately absent, so a playlist that points at a local path fails instead
/// of reading the server's disk.
pub const PROTOCOL_WHITELIST: &str = "http,https,tcp,tls,crypto";

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
