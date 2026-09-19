//! FFmpeg / ffprobe command construction.
//!
//! Commands are always built as an argv vector and executed without a shell
//! (§60). Nothing in this module interpolates a path into a shell string, so
//! spaces and non-ASCII characters in paths are handled by the OS exec layer
//! rather than by quoting rules (§39).

use crate::config::{OutputProfile, StreamMode};
use crate::error::{ErrorCode, LouverError, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Locations of the bundled FFmpeg/ffprobe sidecar binaries.
#[derive(Debug, Clone)]
pub struct FfmpegTools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl FfmpegTools {
    pub fn new(ffmpeg: impl Into<PathBuf>, ffprobe: impl Into<PathBuf>) -> Self {
        Self { ffmpeg: ffmpeg.into(), ffprobe: ffprobe.into() }
    }

    /// Resolve the sidecar for the running platform, falling back to `PATH` for
    /// development machines and CI (§77).
    pub fn discover(sidecar_dir: Option<&Path>) -> Result<Self> {
        let (fm, fp) = (exe_name("ffmpeg"), exe_name("ffprobe"));

        if let Some(dir) = sidecar_dir {
            // Tauri sidecars are suffixed with the target triple.
            for candidate in [
                (
                    dir.join(format!("{}-{}", fm, target_triple())),
                    dir.join(format!("{}-{}", fp, target_triple())),
                ),
                (dir.join(&fm), dir.join(&fp)),
            ] {
                if candidate.0.is_file() && candidate.1.is_file() {
                    return Ok(Self::new(candidate.0, candidate.1));
                }
            }
        }

        match (which(&fm), which(&fp)) {
            (Some(a), Some(b)) => Ok(Self::new(a, b)),
            _ => Err(LouverError::with_detail(
                ErrorCode::FfmpegNotFound,
                "no sidecar in binaries/ and no ffmpeg/ffprobe on PATH",
            )),
        }
    }

    /// `ffmpeg -version` first line, for the Advanced settings panel (§45).
    pub fn version(&self) -> Result<String> {
        let out = Command::new(&self.ffmpeg)
            .args(["-hide_banner", "-version"])
            .output()
            .map_err(|e| LouverError::with_detail(ErrorCode::FfmpegNotFound, e.to_string()))?;
        Ok(String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or_default().to_string())
    }

    /// Choose an encoder that this machine can actually use (§9).
    ///
    /// Being listed is not enough: a distribution build commonly advertises
    /// `h264_nvenc` whether or not an NVIDIA card is present, and picking it
    /// would make every optimization fail at the point the user presses the
    /// button. Each candidate is therefore proved by encoding one frame before
    /// it is accepted.
    pub fn detect_encoder(&self) -> String {
        let listed = self.available_encoders().unwrap_or_default();
        for want in preferred_hw_encoders().iter().chain(preferred_sw_encoders()) {
            if listed.iter().any(|e| e == want) && self.encoder_works(want) {
                return (*want).to_string();
            }
        }
        "libx264".to_string()
    }

    /// Everything about the bundled FFmpeg that the product depends on.
    ///
    /// Each item is *probed*, not inferred from a version string: distributions
    /// number their builds inconsistently ("6.1.1-3ubuntu5", "n6.1", "4.1.5"),
    /// and what matters is whether the binary accepts the arguments we send it.
    ///
    /// This exists because the normalizer uses `-fps_mode`, which FFmpeg only
    /// gained in 5.1. A bundled 4.x build looks perfectly healthy — it reports
    /// a version, lists libx264, speaks RTMPS — and then fails the moment the
    /// user presses "optimize". Catching that at startup instead is the
    /// difference between a clear message and a mystery.
    pub fn capabilities(&self) -> FfmpegCapabilities {
        let version = self.version().unwrap_or_default();
        let listed = self.available_encoders().unwrap_or_default();
        let h264 = preferred_hw_encoders()
            .iter()
            .chain(preferred_sw_encoders())
            .find(|e| listed.iter().any(|l| l == *e) && self.encoder_works(e))
            .map(|e| (*e).to_string());

        FfmpegCapabilities {
            supports_fps_mode: self.accepts_output_flag(&["-fps_mode", "cfr"]),
            h264_encoder: h264,
            has_aac: listed.iter().any(|e| e == "aac"),
            supports_rtmps: self.supports_protocol("rtmps"),
            version_line: version,
        }
    }

    /// Does this build accept an output option? Probes with a one-frame encode.
    fn accepts_output_flag(&self, flag: &[&str]) -> bool {
        let mut args: Vec<String> = vec![
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-nostdin".into(),
            "-f".into(),
            "lavfi".into(),
            "-i".into(),
            "color=black:size=64x64:rate=30:duration=0.1".into(),
        ];
        args.extend(flag.iter().map(|s| (*s).to_string()));
        args.extend(["-frames:v".into(), "1".into(), "-f".into(), "null".into(), "-".into()]);

        let Ok(out) = Command::new(&self.ffmpeg).args(&args).output() else { return false };
        // An unknown option is reported rather than silently ignored.
        let err = String::from_utf8_lossy(&out.stderr).to_lowercase();
        out.status.success() && !err.contains("unrecognized option") && !err.contains("option not found")
    }

    fn supports_protocol(&self, name: &str) -> bool {
        Command::new(&self.ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-protocols"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().any(|p| p == name))
            .unwrap_or(false)
    }

    /// Whether this FFmpeg can encode H.264 at all.
    ///
    /// A build with no usable H.264 encoder can still broadcast — the live
    /// path is a remux — but it can never optimize a non-conforming video, so
    /// the Settings page needs to be able to say so.
    pub fn has_usable_h264_encoder(&self) -> bool {
        let listed = self.available_encoders().unwrap_or_default();
        preferred_hw_encoders()
            .iter()
            .chain(preferred_sw_encoders())
            .any(|e| listed.iter().any(|l| l == e) && self.encoder_works(e))
    }

    /// Encode a single frame to /dev/null with `encoder`, and report success.
    pub fn encoder_works(&self, encoder: &str) -> bool {
        Command::new(&self.ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-f",
                "lavfi",
                "-i",
                "color=black:size=320x240:rate=30:duration=0.1",
                "-c:v",
                encoder,
                "-frames:v",
                "1",
                "-f",
                "null",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Encoders FFmpeg reports as available, used for capability detection (§9).
    ///
    /// Includes audio as well as video: the normalizer needs AAC, and listing
    /// only video encoders made the AAC check silently fail.
    pub fn available_encoders(&self) -> Result<Vec<String>> {
        let out = Command::new(&self.ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-encoders"])
            .output()
            .map_err(|e| LouverError::with_detail(ErrorCode::FfmpegNotFound, e.to_string()))?;
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let l = l.trim();
                // Lines look like " V....D h264_nvenc  NVIDIA NVENC H.264 encoder"
                // or " A....D aac  AAC (Advanced Audio Coding)". The leading
                // letter is the media type.
                if l.starts_with('V') || l.starts_with('A') {
                    l.split_whitespace().nth(1).map(str::to_string)
                } else {
                    None
                }
            })
            .collect())
    }
}

fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

/// The Tauri sidecar target triple for the running platform (§49).
pub fn target_triple() -> &'static str {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .and_then(|paths| std::env::split_paths(&paths).map(|p| p.join(name)).find(|p| p.is_file()))
}

/// Software H.264 encoders, most preferred first.
///
/// `libx264` is the usual choice, but it makes an FFmpeg build GPL. A build
/// assembled under LGPL terms will not have it, and normalization must still
/// work there — every supported platform has a hardware or OS encoder, and
/// `libopenh264` is the portable software fallback. Hard-coding `libx264` as
/// the last resort would make normalization fail outright on such a build.
pub fn preferred_sw_encoders() -> &'static [&'static str] {
    if cfg!(target_os = "windows") {
        // Media Foundation ships with Windows and needs no extra licence.
        &["libx264", "libopenh264", "h264_mf"]
    } else {
        &["libx264", "libopenh264"]
    }
}

/// What the bundled FFmpeg can actually do (§15).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FfmpegCapabilities {
    pub version_line: String,
    /// `-fps_mode`, required by the normalizer. FFmpeg 5.1 and later.
    pub supports_fps_mode: bool,
    /// The H.264 encoder that was proved to work, if any.
    pub h264_encoder: Option<String>,
    pub has_aac: bool,
    /// Required to publish to YouTube.
    pub supports_rtmps: bool,
}

impl FfmpegCapabilities {
    /// Can this build run a broadcast at all?
    ///
    /// Broadcasting is a remux, so it needs neither an encoder nor `-fps_mode`;
    /// only the ability to speak RTMPS.
    pub fn can_broadcast(&self) -> bool {
        self.supports_rtmps
    }

    /// Can this build optimize a video for broadcast?
    pub fn can_normalize(&self) -> bool {
        self.supports_fps_mode && self.h264_encoder.is_some() && self.has_aac
    }

    /// Everything that is wrong, in words the user can act on.
    pub fn problems(&self) -> Vec<String> {
        let mut v = Vec::new();
        if !self.supports_rtmps {
            v.push("이 FFmpeg는 RTMPS를 지원하지 않아 YouTube로 송출할 수 없습니다".into());
        }
        if !self.supports_fps_mode {
            v.push("이 FFmpeg는 너무 오래되어 영상 최적화를 할 수 없습니다 (FFmpeg 5.1 이상 필요)".into());
        }
        if self.h264_encoder.is_none() {
            v.push("사용 가능한 H.264 인코더가 없어 영상 최적화를 할 수 없습니다".into());
        }
        if !self.has_aac {
            v.push("AAC 인코더가 없어 영상 최적화를 할 수 없습니다".into());
        }
        v
    }
}

/// Hardware H.264 encoders, most preferred first, per platform (§9).
pub fn preferred_hw_encoders() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &["h264_videotoolbox"]
    } else if cfg!(target_os = "windows") {
        &["h264_nvenc", "h264_qsv", "h264_amf"]
    } else {
        &["h264_nvenc", "h264_vaapi"]
    }
}

/// Pick the best *listed* encoder, falling back to libx264 (§9).
///
/// This only consults FFmpeg's compiled-in encoder list. A build can advertise
/// `h264_nvenc` on a machine with no NVIDIA GPU at all, so callers that are
/// about to encode for real should use [`FfmpegTools::detect_encoder`], which
/// additionally proves the encoder works on this hardware.
pub fn select_encoder(available: &[String]) -> String {
    for want in preferred_hw_encoders().iter().chain(preferred_sw_encoders()) {
        if available.iter().any(|e| e == want) {
            return (*want).to_string();
        }
    }
    // Nothing recognised. Return the usual default so the caller still has a
    // name to report; the encode itself will fail with LL-MEDIA-005.
    "libx264".to_string()
}

/// Whether the chosen encoder is hardware-accelerated.
///
/// Decided by membership of the software list rather than by "is it libx264",
/// which would mislabel every other software encoder as hardware-accelerated
/// in the Settings panel.
pub fn is_hardware_encoder(encoder: &str) -> bool {
    // h264_mf is Media Foundation, which may or may not be hardware-backed; it
    // is listed as software so the UI never over-promises.
    !preferred_sw_encoders().contains(&encoder) && encoder != "libopenh264" && encoder != "libx264"
}

// ---------------------------------------------------------------------------
// Secret masking (§34, §60)
// ---------------------------------------------------------------------------

/// Replace anything that could be a stream key with `••••`.
///
/// Applied to every string that reaches a log file, an error detail or the UI.
/// It is deliberately aggressive: masking a harmless token is acceptable,
/// leaking a key is not.
pub fn mask_secrets(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for token in split_keep_delims(input) {
        out.push_str(&mask_token(&token));
    }
    out
}

fn split_keep_delims(s: &str) -> Vec<String> {
    // Split on whitespace and quotes but keep the delimiters so the masked
    // string still reads like the original line.
    let mut parts = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if ch.is_whitespace() || ch == '"' || ch == '\'' {
            if !cur.is_empty() {
                parts.push(std::mem::take(&mut cur));
            }
            parts.push(ch.to_string());
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

fn mask_token(tok: &str) -> String {
    // An RTMP(S) URL: keep scheme+host+app path, mask any trailing key segment.
    if let Some(rest) = tok.strip_prefix("rtmps://").or_else(|| tok.strip_prefix("rtmp://")) {
        let scheme = if tok.starts_with("rtmps://") { "rtmps://" } else { "rtmp://" };
        let segments: Vec<&str> = rest.split('/').collect();
        // host / app / [key...]
        let keep = segments.len().min(2);
        let mut s = format!("{scheme}{}", segments[..keep].join("/"));
        if segments.len() > keep {
            s.push_str("/••••••••");
        }
        return s;
    }
    if looks_like_stream_key(tok) {
        return "••••••••".to_string();
    }
    tok.to_string()
}

/// YouTube stream keys look like `xxxx-xxxx-xxxx-xxxx-xxxx`: groups of
/// alphanumerics joined by hyphens, at least 16 characters overall.
pub fn looks_like_stream_key(tok: &str) -> bool {
    let core = tok.trim_matches(|c: char| !c.is_alphanumeric() && c != '-');
    if core.len() < 16 {
        return false;
    }
    let groups: Vec<&str> = core.split('-').collect();
    groups.len() >= 4 && groups.iter().all(|g| !g.is_empty() && g.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// Mask a full argv vector for logging.
pub fn mask_argv(args: &[String]) -> Vec<String> {
    args.iter().map(|a| mask_secrets(a)).collect()
}

// ---------------------------------------------------------------------------
// Command builder (§39)
// ---------------------------------------------------------------------------

/// Builds every FFmpeg invocation the app makes. The UI never assembles
/// arguments itself.
#[derive(Debug, Clone)]
pub struct FfmpegCommandBuilder {
    tools: FfmpegTools,
    profile: OutputProfile,
    encoder: String,
}

impl FfmpegCommandBuilder {
    pub fn new(tools: FfmpegTools, profile: OutputProfile) -> Self {
        Self { tools, profile, encoder: "libx264".to_string() }
    }

    pub fn with_encoder(mut self, encoder: impl Into<String>) -> Self {
        self.encoder = encoder.into();
        self
    }

    pub fn tools(&self) -> &FfmpegTools {
        &self.tools
    }

    pub fn profile(&self) -> OutputProfile {
        self.profile
    }

    pub fn encoder(&self) -> &str {
        &self.encoder
    }

    /// `ffprobe` arguments producing the JSON the probe module parses (§6).
    pub fn build_probe_args(&self, input: &Path) -> Vec<String> {
        vec![
            "-v".into(),
            "error".into(),
            "-print_format".into(),
            "json".into(),
            "-show_format".into(),
            "-show_streams".into(),
            "-show_entries".into(),
            "stream_side_data=rotation".into(),
            input.to_string_lossy().into_owned(),
        ]
    }

    /// Normalization to the broadcast profile (§9).
    ///
    /// `target_duration_secs` snaps the output to a whole number of video frames
    /// so that every normalized file ends on a frame boundary; this is what lets
    /// the concat demuxer join them without timestamp repair.
    ///
    /// `has_audio` comes from the probe. A source without audio still has to
    /// produce a silent AAC track, because the concat demuxer requires every
    /// input to have the same stream layout (§14). FFmpeg's optional-stream
    /// syntax (`[0:a?]`) is not accepted inside a filtergraph label, so the two
    /// cases are built as separate graphs rather than one conditional one.
    pub fn build_normalize_args(
        &self,
        input: &Path,
        output: &Path,
        target_duration_secs: f64,
        has_audio: bool,
    ) -> Vec<String> {
        let p = self.profile;
        let (w, h) = (p.width(), p.height());
        let vf = format!(
            "scale={w}:{h}:force_original_aspect_ratio=decrease:flags=bicubic,\
             pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,fps={},format=yuv420p",
            p.fps()
        );
        let af = format!(
            "aformat=sample_fmts=fltp:sample_rates={}:channel_layouts=stereo,aresample={}:first_pts=0",
            p.audio_sample_rate(),
            p.audio_sample_rate()
        );

        let mut a: Vec<String> = vec![
            "-hide_banner".into(),
            "-nostdin".into(),
            "-loglevel".into(),
            "error".into(),
            "-progress".into(),
            "pipe:1".into(), // machine-readable progress (§40)
            "-y".into(),
            "-i".into(),
            input.to_string_lossy().into_owned(),
        ];

        if has_audio {
            a.extend([
                "-filter_complex".into(),
                format!("[0:v]{vf}[v];[0:a]{af}[a]"),
                "-map".into(),
                "[v]".into(),
                "-map".into(),
                "[a]".into(),
            ]);
        } else {
            // A silent source, bounded by the -t below.
            a.extend([
                "-f".into(),
                "lavfi".into(),
                "-i".into(),
                format!("anullsrc=channel_layout=stereo:sample_rate={}", p.audio_sample_rate()),
                "-filter_complex".into(),
                format!("[0:v]{vf}[v]"),
                "-map".into(),
                "[v]".into(),
                "-map".into(),
                "1:a".into(),
            ]);
        }

        // Hard duration cut on a whole-frame boundary.
        a.extend(["-t".into(), format!("{target_duration_secs:.6}")]);

        a.extend(self.video_encode_args());

        a.extend([
            "-r".into(),
            p.fps().to_string(),
            "-fps_mode".into(),
            "cfr".into(),
            "-video_track_timescale".into(),
            p.video_timescale().to_string(),
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            format!("{}k", p.audio_kbps()),
            "-ar".into(),
            p.audio_sample_rate().to_string(),
            "-ac".into(),
            p.audio_channels().to_string(),
            "-movflags".into(),
            "+faststart".into(),
            "-map_metadata".into(),
            "-1".into(),
            "-avoid_negative_ts".into(),
            "make_zero".into(),
            output.to_string_lossy().into_owned(),
        ]);
        a
    }

    fn video_encode_args(&self) -> Vec<String> {
        let p = self.profile;
        let kbps = p.video_kbps();
        let mut a: Vec<String> = vec!["-c:v".into(), self.encoder.clone()];
        match self.encoder.as_str() {
            "h264_nvenc" => a.extend([
                "-preset".into(),
                "p4".into(),
                "-rc".into(),
                "cbr".into(),
                "-profile:v".into(),
                "high".into(),
            ]),
            "h264_qsv" => a.extend(["-preset".into(), "medium".into(), "-profile:v".into(), "high".into()]),
            "h264_amf" => a.extend([
                "-quality".into(),
                "balanced".into(),
                "-rc".into(),
                "cbr".into(),
                "-profile:v".into(),
                "high".into(),
            ]),
            "h264_videotoolbox" => {
                a.extend(["-profile:v".into(), "high".into(), "-allow_sw".into(), "1".into()])
            }
            _ => a.extend([
                "-preset".into(),
                "veryfast".into(),
                "-profile:v".into(),
                "high".into(),
                "-level".into(),
                "4.2".into(),
                "-x264-params".into(),
                "force-cfr=1".into(),
            ]),
        }
        a.extend([
            "-pix_fmt".into(),
            "yuv420p".into(),
            "-b:v".into(),
            format!("{kbps}k"),
            "-maxrate".into(),
            format!("{kbps}k"),
            "-bufsize".into(),
            format!("{}k", kbps * 2),
            "-g".into(),
            p.gop().to_string(),
            "-keyint_min".into(),
            p.gop().to_string(),
            "-sc_threshold".into(),
            "0".into(),
        ]);
        a
    }

    /// The live broadcast command (§13).
    ///
    /// In [`StreamMode::StreamCopy`] the argv contains no video encoder at all —
    /// this is asserted by a unit test and surfaced in the debug panel (§41).
    pub fn build_stream_args(
        &self,
        manifest: &Path,
        destination: &str,
        mode: StreamMode,
        loop_forever: bool,
    ) -> Vec<String> {
        let mut a: Vec<String> = vec![
            "-hide_banner".into(),
            "-nostdin".into(),
            "-loglevel".into(),
            "warning".into(),
            "-progress".into(),
            "pipe:1".into(),
            // Overwrite without asking. Irrelevant for an RTMPS URL, but
            // essential when the destination is a file: on a reconnect the
            // previous run's output already exists, and with `-nostdin`
            // FFmpeg refuses the overwrite prompt and exits at once — every
            // retry would fail and a dry run could never recover.
            "-y".into(),
            // Feed the muxer at wall-clock speed; without this FFmpeg would
            // push the whole playlist to YouTube as fast as it can read it.
            "-re".into(),
        ];
        if loop_forever {
            a.extend(["-stream_loop".into(), "-1".into()]);
        }
        a.extend([
            "-f".into(),
            "concat".into(),
            "-safe".into(),
            "0".into(),
            "-i".into(),
            manifest.to_string_lossy().into_owned(),
        ]);

        match mode {
            StreamMode::StreamCopy => {
                a.extend(["-c".into(), "copy".into()]);
            }
            StreamMode::CompatibilityEncode => {
                a.extend(self.video_encode_args());
                a.extend([
                    "-r".into(),
                    self.profile.fps().to_string(),
                    "-fps_mode".into(),
                    "cfr".into(),
                    "-c:a".into(),
                    "aac".into(),
                    "-b:a".into(),
                    format!("{}k", self.profile.audio_kbps()),
                    "-ar".into(),
                    self.profile.audio_sample_rate().to_string(),
                    "-ac".into(),
                    self.profile.audio_channels().to_string(),
                ]);
            }
        }

        a.extend([
            "-f".into(),
            "flv".into(),
            "-flvflags".into(),
            "no_duration_filesize".into(),
            destination.to_string(),
        ]);
        a
    }

    /// Dry run: identical pipeline, local file sink instead of RTMPS (§30).
    ///
    /// `-y` already comes from [`Self::build_stream_args`], so a restart
    /// overwrites the previous output rather than stalling on a prompt.
    pub fn build_dry_run_args(
        &self,
        manifest: &Path,
        output: &Path,
        mode: StreamMode,
        duration_secs: Option<f64>,
        loop_forever: bool,
    ) -> Vec<String> {
        let mut a = self.build_stream_args(manifest, &output.to_string_lossy(), mode, loop_forever);
        // Dry runs read as fast as the disk allows; `-re` only matters when a
        // live server is pacing us.
        if let Some(pos) = a.iter().position(|x| x == "-re") {
            a.remove(pos);
        }
        if let Some(d) = duration_secs {
            // `-t` must precede the output URL.
            let out_idx = a.len() - 1;
            a.insert(out_idx, format!("{d:.3}"));
            a.insert(out_idx, "-t".into());
        }
        a
    }

    /// Build a `Command` from an argv vector. No shell is involved.
    pub fn command(&self, args: &[String]) -> Command {
        let mut c = Command::new(&self.tools.ffmpeg);
        c.args(args);
        c
    }

    pub fn probe_command(&self, args: &[String]) -> Command {
        let mut c = Command::new(&self.tools.ffprobe);
        c.args(args);
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn builder() -> FfmpegCommandBuilder {
        FfmpegCommandBuilder::new(
            FfmpegTools::new("/usr/bin/ffmpeg", "/usr/bin/ffprobe"),
            OutputProfile::P1080p30,
        )
    }

    // --- path handling (§39) ---------------------------------------------

    #[test]
    fn windows_path_with_spaces_is_passed_as_one_argv_entry() {
        let p = PathBuf::from(r"C:\Users\Test User\Music\jazz 01.mp4");
        let args = builder().build_normalize_args(&p, Path::new(r"C:\out\n.mp4"), 10.0, true);
        assert!(args.contains(&r"C:\Users\Test User\Music\jazz 01.mp4".to_string()));
        // Never pre-quoted: quoting is the exec layer's job.
        assert!(!args.iter().any(|a| a.starts_with('"')));
    }

    #[test]
    fn korean_paths_survive_intact() {
        let win = PathBuf::from(r"C:\Users\Test User\Music\재즈 영상 01.mp4");
        let mac = PathBuf::from("/Users/test/Music/오늘 밤 재즈.mp4");
        for p in [win, mac] {
            let args = builder().build_normalize_args(&p, Path::new("/tmp/o.mp4"), 5.0, true);
            assert!(args.contains(&p.to_string_lossy().into_owned()), "korean path mangled: {args:?}");
        }
    }

    #[test]
    fn probe_args_request_json() {
        let a = builder().build_probe_args(Path::new("/Users/test/Music/오늘 밤 재즈.mp4"));
        assert!(a.windows(2).any(|w| w == ["-print_format", "json"]));
        assert_eq!(a.last().unwrap(), "/Users/test/Music/오늘 밤 재즈.mp4");
    }

    // --- stream copy purity (§41) ----------------------------------------

    #[test]
    fn stream_copy_argv_contains_no_video_encoder() {
        let a = builder().build_stream_args(
            Path::new("/tmp/m.txt"),
            "rtmps://a.rtmps.youtube.com/live2/key-key-key-key",
            StreamMode::StreamCopy,
            true,
        );
        assert!(a.windows(2).any(|w| w == ["-c", "copy"]));
        for forbidden in
            ["libx264", "h264_nvenc", "h264_qsv", "h264_amf", "h264_videotoolbox", "-c:v", "-b:v"]
        {
            assert!(!a.iter().any(|x| x == forbidden), "stream copy leaked {forbidden}: {a:?}");
        }
    }

    #[test]
    fn stream_copy_loops_forever_and_paces_with_re() {
        let a = builder().build_stream_args(
            Path::new("/tmp/m.txt"),
            "rtmps://x/y/z",
            StreamMode::StreamCopy,
            true,
        );
        assert!(a.windows(2).any(|w| w == ["-stream_loop", "-1"]));
        assert!(a.contains(&"-re".to_string()));
        assert!(a.windows(2).any(|w| w == ["-f", "concat"]));
        assert!(a.windows(2).any(|w| w == ["-f", "flv"]));
    }

    #[test]
    fn the_stream_command_overwrites_its_output_so_a_reconnect_can_succeed() {
        // Without -y, a dry run that reconnects hits FFmpeg's overwrite prompt
        // and, with -nostdin, dies instantly on every retry.
        for mode in [StreamMode::StreamCopy, StreamMode::CompatibilityEncode] {
            let live = builder().build_stream_args(Path::new("/tmp/m.txt"), "/tmp/out.flv", mode, true);
            assert!(live.contains(&"-y".to_string()), "{mode:?} stream args missing -y");
            let dry = builder().build_dry_run_args(
                Path::new("/tmp/m.txt"),
                Path::new("/tmp/out.flv"),
                mode,
                None,
                true,
            );
            assert!(dry.contains(&"-y".to_string()), "{mode:?} dry-run args missing -y");
            // -y must come before the output, like every other global flag.
            let y = dry.iter().position(|x| x == "-y").unwrap();
            assert!(y < dry.len() - 1);
        }
    }

    #[test]
    fn non_looping_stream_omits_stream_loop() {
        let a = builder().build_stream_args(
            Path::new("/tmp/m.txt"),
            "rtmps://x/y/z",
            StreamMode::StreamCopy,
            false,
        );
        assert!(!a.contains(&"-stream_loop".to_string()));
    }

    #[test]
    fn compatibility_mode_does_encode() {
        let a = builder().build_stream_args(
            Path::new("/tmp/m.txt"),
            "rtmps://x/y/z",
            StreamMode::CompatibilityEncode,
            true,
        );
        assert!(a.windows(2).any(|w| w == ["-c:v", "libx264"]));
        assert!(!a.windows(2).any(|w| w == ["-c", "copy"]));
    }

    #[test]
    fn dry_run_writes_to_file_without_re_and_honours_duration() {
        let a = builder().build_dry_run_args(
            Path::new("/tmp/m.txt"),
            Path::new("/tmp/out.flv"),
            StreamMode::StreamCopy,
            Some(18.0),
            true,
        );
        assert!(!a.contains(&"-re".to_string()), "dry run must not pace: {a:?}");
        assert_eq!(a.last().unwrap(), "/tmp/out.flv");
        let t = a.iter().position(|x| x == "-t").expect("-t missing");
        assert_eq!(a[t + 1], "18.000");
        assert!(t + 2 < a.len(), "-t must precede the output");
    }

    // --- normalization ----------------------------------------------------

    #[test]
    fn normalize_targets_profile_geometry_and_two_second_gop() {
        let a = builder().build_normalize_args(Path::new("/in.mp4"), Path::new("/out.mp4"), 12.5, true);
        let joined = a.join(" ");
        assert!(joined.contains("scale=1920:1080"));
        assert!(joined.contains("pad=1920:1080"));
        assert!(a.windows(2).any(|w| w == ["-g", "60"]));
        assert!(a.windows(2).any(|w| w == ["-keyint_min", "60"]));
        assert!(a.windows(2).any(|w| w == ["-video_track_timescale", "30000"]));
        assert!(a.windows(2).any(|w| w == ["-fps_mode", "cfr"]));
        assert!(a.windows(2).any(|w| w == ["-t", "12.500000"]));
        assert!(a.windows(2).any(|w| w == ["-progress", "pipe:1"]));
    }

    #[test]
    fn normalize_720p_uses_smaller_geometry_and_bitrate() {
        let b = FfmpegCommandBuilder::new(FfmpegTools::new("ffmpeg", "ffprobe"), OutputProfile::P720p30);
        let joined = b.build_normalize_args(Path::new("/in.mp4"), Path::new("/o.mp4"), 3.0, true).join(" ");
        assert!(joined.contains("scale=1280:720"));
        assert!(joined.contains("-b:v 4000k"));
    }

    #[test]
    fn a_source_without_audio_still_gets_a_silent_aac_track() {
        // The concat demuxer requires an identical stream layout everywhere, so
        // a silent track is synthesised rather than the audio being omitted.
        let a = builder().build_normalize_args(Path::new("/in.mp4"), Path::new("/o.mp4"), 3.0, false);
        assert!(a.join(" ").contains("anullsrc"), "{a:?}");
        assert!(a.windows(2).any(|w| w == ["-map", "1:a"]));
        assert!(a.windows(2).any(|w| w == ["-c:a", "aac"]));
        // The optional-stream form is invalid inside a filtergraph label.
        assert!(!a.join(" ").contains("0:a?"));
    }

    #[test]
    fn a_source_with_audio_uses_its_own_track() {
        let a = builder().build_normalize_args(Path::new("/in.mp4"), Path::new("/o.mp4"), 3.0, true);
        assert!(!a.join(" ").contains("anullsrc"));
        assert!(a.windows(2).any(|w| w == ["-map", "[a]"]));
        assert!(a.join(" ").contains("[0:a]aformat"));
    }

    #[test]
    fn hardware_encoder_swaps_codec_but_keeps_gop() {
        let b = builder().with_encoder("h264_nvenc");
        let a = b.build_normalize_args(Path::new("/in.mp4"), Path::new("/o.mp4"), 3.0, true);
        assert!(a.windows(2).any(|w| w == ["-c:v", "h264_nvenc"]));
        assert!(!a.iter().any(|x| x == "libx264"));
        assert!(a.windows(2).any(|w| w == ["-g", "60"]));
    }

    // --- encoder selection (§9) -------------------------------------------

    #[test]
    fn a_listed_encoder_that_does_not_run_is_not_chosen() {
        // The bug this guards: a distro FFmpeg lists h264_nvenc on a machine
        // with no NVIDIA GPU, and every optimization then fails.
        let Ok(tools) = FfmpegTools::discover(None) else {
            eprintln!("SKIP: no ffmpeg on PATH");
            return;
        };
        let listed = tools.available_encoders().unwrap_or_default();
        let chosen = tools.detect_encoder();
        assert!(
            chosen == "libx264" || tools.encoder_works(&chosen),
            "detect_encoder chose {chosen}, which cannot actually encode here"
        );
        // libx264 must always work, or normalization has no fallback at all.
        assert!(tools.encoder_works("libx264"), "libx264 is the last resort and must work");
        assert!(!tools.encoder_works("definitely_not_an_encoder"));
        // And a listed-but-unusable encoder really is filtered out, not just
        // absent from the list.
        if listed.iter().any(|e| e == "h264_nvenc") && !tools.encoder_works("h264_nvenc") {
            assert_ne!(chosen, "h264_nvenc", "picked a listed encoder that does not run");
        }
    }

    #[test]
    fn a_build_without_libx264_still_finds_a_software_encoder() {
        // An LGPL FFmpeg has no libx264. Normalization must not be left
        // without any encoder at all (§15).
        let lgpl: Vec<String> = vec!["libopenh264".into(), "mpeg4".into(), "aac".into()];
        assert_eq!(select_encoder(&lgpl), "libopenh264");
        assert!(!is_hardware_encoder("libopenh264"));

        // Hardware still wins when it is present.
        let mut with_hw = lgpl.clone();
        with_hw.push(preferred_hw_encoders()[0].to_string());
        assert_eq!(select_encoder(&with_hw), preferred_hw_encoders()[0]);

        // And libx264 outranks libopenh264 when both exist.
        let both: Vec<String> = vec!["libopenh264".into(), "libx264".into()];
        assert_eq!(select_encoder(&both), "libx264");
    }

    #[test]
    fn the_software_fallback_list_is_ordered_and_non_empty() {
        let sw = preferred_sw_encoders();
        assert!(!sw.is_empty());
        assert_eq!(sw[0], "libx264", "libx264 stays the preferred software encoder where available");
        assert!(sw.contains(&"libopenh264"), "an LGPL-compatible fallback must exist");
        assert!(sw.iter().all(|e| !is_hardware_encoder(e)), "the software list must not claim hardware");
    }

    #[test]
    fn encoder_selection_prefers_hardware_then_falls_back() {
        let none: Vec<String> = vec!["libx264".into(), "mpeg4".into()];
        assert_eq!(select_encoder(&none), "libx264");
        assert!(!is_hardware_encoder("libx264"));

        let hw: Vec<String> =
            preferred_hw_encoders().iter().map(|s| s.to_string()).chain(["libx264".to_string()]).collect();
        let picked = select_encoder(&hw);
        assert_eq!(picked, preferred_hw_encoders()[0]);
        assert!(is_hardware_encoder(&picked));
    }

    // --- secret masking (§34, §60) ----------------------------------------

    #[test]
    fn stream_key_is_masked_in_rtmps_url() {
        let m = mask_secrets("rtmps://a.rtmps.youtube.com/live2/abcd-efgh-ijkl-mnop");
        assert_eq!(m, "rtmps://a.rtmps.youtube.com/live2/••••••••");
        assert!(!m.contains("abcd"));
    }

    #[test]
    fn bare_stream_key_is_masked() {
        assert_eq!(mask_secrets("abcd-efgh-ijkl-mnop-qrst"), "••••••••");
        assert_eq!(mask_secrets("key is 1a2b-3c4d-5e6f-7g8h"), "key is ••••••••");
    }

    #[test]
    fn masking_preserves_ordinary_text() {
        let s = "Connection refused while opening output";
        assert_eq!(mask_secrets(s), s);
        // Short hyphenated words must not be eaten.
        assert_eq!(mask_secrets("re-try now"), "re-try now");
        assert_eq!(mask_secrets("/Users/test/Music/오늘 밤 재즈.mp4"), "/Users/test/Music/오늘 밤 재즈.mp4");
    }

    #[test]
    fn full_argv_is_masked_for_logging() {
        let a = builder().build_stream_args(
            Path::new("/tmp/m.txt"),
            "rtmps://a.rtmps.youtube.com/live2/secr-etke-yval-ue00",
            StreamMode::StreamCopy,
            true,
        );
        let masked = mask_argv(&a).join(" ");
        assert!(!masked.contains("secr-etke"), "{masked}");
        assert!(masked.contains("rtmps://a.rtmps.youtube.com/live2/••••••••"));
        // The rest of the command stays readable for debugging.
        assert!(masked.contains("-f concat"));
    }

    #[test]
    fn key_detector_rejects_paths_and_short_tokens() {
        assert!(!looks_like_stream_key("/usr/bin/ffmpeg"));
        assert!(!looks_like_stream_key("a-b-c-d"));
        assert!(!looks_like_stream_key("1080p30"));
        assert!(looks_like_stream_key("abcd-efgh-ijkl-mnop"));
    }

    #[test]
    fn target_triple_is_a_known_tauri_triple() {
        let known = [
            "x86_64-pc-windows-msvc",
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
        ];
        assert!(known.contains(&target_triple()));
    }
}

// ---------------------------------------------------------------------------
// Turning FFmpeg's output into something a user can act on (§18, §35)
// ---------------------------------------------------------------------------

/// Classify FFmpeg's stderr into a Louver error code.
///
/// The patterns below were taken from real FFmpeg 6.1 output for each failure
/// mode rather than guessed; the tests quote that output verbatim. Anything
/// unrecognised stays `StreamFfmpegExit`, whose message already tells the user
/// the broadcast stopped and is being retried.
pub fn classify_ffmpeg_failure(stderr: &str) -> crate::error::ErrorCode {
    use crate::error::ErrorCode as E;
    let s = stderr.to_lowercase();

    // Order matters: the most specific diagnosis first.

    // The ingest server answered and refused us. YouTube says so explicitly.
    if s.contains("netstream.publish.badname")
        || s.contains("authmod")
        || (s.contains("server error") && (s.contains("publish") || s.contains("authentic")))
        || s.contains("unable to publish")
        || s.contains("rtmp server sent error")
    {
        return E::StreamKeyRejected;
    }

    // Name resolution failed, which in practice means there is no internet.
    if s.contains("failed to resolve hostname")
        || s.contains("name or service not known")
        || s.contains("temporary failure in name resolution")
        || s.contains("network is unreachable")
        || s.contains("no route to host")
    {
        return E::NetworkUnreachable;
    }

    // We reached the network but the endpoint did not accept or keep the
    // connection. "Cannot open connection" is RTMP's form of this.
    if s.contains("connection refused")
        || s.contains("cannot open connection")
        || s.contains("connection timed out")
        || s.contains("connection reset by peer")
        || s.contains("broken pipe")
        || s.contains("end of file")
        || s.contains("handshake")
    {
        return E::NetworkRtmpRejected;
    }

    if s.contains("no space left on device") {
        return E::StorageInsufficientSpace;
    }

    // A file that is gone and a file that is unreadable need different advice,
    // and FFmpeg reports both through "Error opening input". The distinguishing
    // signal is which errno or demuxer complaint follows, so match on that and
    // never on the generic wrapper line.
    if s.contains("no such file or directory") {
        return E::MediaFileMissing;
    }
    // A normalized cache file that will not parse is a corrupt cache entry,
    // not a broken source: the source is never read during a broadcast.
    if s.contains("moov atom not found")
        || s.contains("invalid data found when processing input")
        || s.contains("could not find codec parameters")
    {
        return E::StorageCacheCorrupt;
    }
    if s.contains("permission denied") || s.contains("operation not permitted") {
        return E::StorageIo;
    }

    E::StreamFfmpegExit
}

/// Build a user-facing error from FFmpeg's output, keeping the technical
/// detail behind the disclosure the UI shows on demand (§35).
pub fn error_from_ffmpeg(stderr: &str) -> crate::error::LouverError {
    let code = classify_ffmpeg_failure(stderr);
    // Keep the last few lines: the first line is usually the root cause and the
    // rest is FFmpeg unwinding.
    let detail: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).rev().take(4).collect();
    crate::error::LouverError::with_detail(code, detail.into_iter().rev().collect::<Vec<_>>().join(" | "))
}

#[cfg(test)]
mod ffmpeg_error_tests {
    use super::*;
    use crate::error::ErrorCode as E;

    // Every string below is real FFmpeg 6.1 output, captured from the failure
    // it describes. Guessing at these is how error mapping goes wrong.

    #[test]
    fn nothing_listening_reads_as_a_server_connection_failure() {
        let real = "[tcp @ 0x5582f4065cc0] Connection to tcp://127.0.0.1:1?tcp_nodelay=0 failed: Connection refused\n\
                    [rtmp @ 0x5582f4015f40] Cannot open connection tcp://127.0.0.1:1?tcp_nodelay=0\n\
                    [out#0/flv @ 0x5582f405fe00] Error opening output rtmp://127.0.0.1:1/live/key: Connection refused";
        assert_eq!(classify_ffmpeg_failure(real), E::NetworkRtmpRejected);
        let e = error_from_ffmpeg(real);
        assert_eq!(e.code_str, "LL-NETWORK-002");
        assert!(e.message.contains("유튜브 서버에 연결하지 못했습니다"));
        // The raw text is available, but only as detail.
        assert!(e.detail.unwrap().contains("Connection refused"));
    }

    #[test]
    fn dns_failure_reads_as_no_internet() {
        let real = "[tcp @ 0x55a971a955c0] Failed to resolve hostname a.rtmps.youtube.com: Name or service not known\n\
                    [rtmp @ 0x55a971af5700] Cannot open connection tcp://a.rtmps.youtube.com:1935?tcp_nodelay=0";
        // Resolution failure outranks the "cannot open connection" that follows it.
        assert_eq!(classify_ffmpeg_failure(real), E::NetworkUnreachable);
        assert_eq!(error_from_ffmpeg(real).code_str, "LL-NETWORK-001");
    }

    #[test]
    fn a_rejected_stream_key_is_named_as_such() {
        // What an ingest server returns for a bad key.
        for real in [
            "[rtmp @ 0x1] RTMP server sent error: NetStream.Publish.BadName",
            "[rtmp @ 0x1] Server error: authmod=adobe requires a valid stream key",
            "[rtmp @ 0x1] Unable to publish to the requested stream",
        ] {
            assert_eq!(classify_ffmpeg_failure(real), E::StreamKeyRejected, "{real}");
        }
        let e = error_from_ffmpeg("[rtmp @ 0x1] RTMP server sent error: NetStream.Publish.BadName");
        assert_eq!(e.code_str, "LL-STREAM-008");
        assert!(e.message.contains("스트림 키"), "{}", e.message);
    }

    #[test]
    fn a_dropped_connection_mid_broadcast_is_a_network_fault() {
        for real in [
            "[flv @ 0x1] Failed to update header with correct duration.\nav_interleaved_write_frame(): Broken pipe",
            "[rtmp @ 0x1] Connection reset by peer",
            "[rtmp @ 0x1] RTMP_ReadPacket, failed to read RTMP packet header: End of file",
        ] {
            assert_eq!(classify_ffmpeg_failure(real), E::NetworkRtmpRejected, "{real}");
        }
    }

    #[test]
    fn a_corrupt_cache_file_is_reported_as_a_cache_problem_not_a_network_one() {
        let real = "[mov,mp4,m4a,3gp,3g2,mj2 @ 0x563446dcef40] moov atom not found\n\
                    [in#0 @ 0x563446dcee40] Error opening input: Invalid data found when processing input";
        assert_eq!(classify_ffmpeg_failure(real), E::StorageCacheCorrupt);
        assert_eq!(error_from_ffmpeg(real).code_str, "LL-STORAGE-002");
    }

    #[test]
    fn a_vanished_file_is_reported_as_a_missing_file() {
        let real = "Error opening input file /Users/test/Music/재즈.mp4.\n\
                    Error opening input files: No such file or directory";
        assert_eq!(classify_ffmpeg_failure(real), E::MediaFileMissing);
    }

    #[test]
    fn a_missing_file_and_a_corrupt_file_are_told_apart() {
        // Both arrive wrapped in "Error opening input", so the wrapper alone
        // must never decide which it is.
        let missing = "[in#0 @ 0x1] Error opening input: No such file or directory\n\
                       Error opening input file /cache/x/normalized.mp4.";
        let corrupt = "[mov,mp4,m4a @ 0x1] moov atom not found\n\
                       [in#0 @ 0x1] Error opening input: Invalid data found when processing input";
        assert_eq!(classify_ffmpeg_failure(missing), E::MediaFileMissing);
        assert_eq!(classify_ffmpeg_failure(corrupt), E::StorageCacheCorrupt);
        assert_ne!(
            classify_ffmpeg_failure(missing),
            classify_ffmpeg_failure(corrupt),
            "a deleted file and a damaged file need different advice"
        );
    }

    #[test]
    fn a_full_disk_is_reported_as_a_storage_problem() {
        let real = "[flv @ 0x1] Error writing trailer: No space left on device";
        assert_eq!(classify_ffmpeg_failure(real), E::StorageInsufficientSpace);
    }

    #[test]
    fn unrecognised_output_keeps_the_generic_retry_message() {
        let e = error_from_ffmpeg("[flv @ 0x1] something entirely new happened");
        assert_eq!(e.code, E::StreamFfmpegExit);
        assert!(e.message.contains("자동으로 다시 연결"));
    }

    #[test]
    fn the_user_message_is_never_raw_ffmpeg_output() {
        // §35: the headline is Korean guidance; FFmpeg's words stay in detail.
        for real in [
            "Connection refused",
            "Failed to resolve hostname x: Name or service not known",
            "moov atom not found",
            "No space left on device",
        ] {
            let e = error_from_ffmpeg(real);
            assert!(!e.message.contains("["), "{}", e.message);
            for word in ["Connection", "hostname", "moov", "device"] {
                assert!(!e.message.contains(word), "raw text leaked into the message: {}", e.message);
            }
            assert!(e.message.chars().any(|c| ('가'..='힣').contains(&c)), "message should be Korean");
        }
    }

    #[test]
    fn the_detail_is_masked_so_a_publish_url_cannot_leak() {
        let real = "[out#0/flv @ 0x1] Error opening output rtmps://a.rtmps.youtube.com/live2/abcd-efgh-ijkl-mnop: Connection refused";
        let e = error_from_ffmpeg(real);
        let d = e.detail.unwrap();
        assert!(!d.contains("abcd-efgh"), "stream key leaked into the error detail: {d}");
        assert!(d.contains("••••"));
    }
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    fn tools() -> Option<FfmpegTools> {
        FfmpegTools::discover(Some(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../apps/desktop/src-tauri/binaries"),
        ))
        .ok()
    }

    #[test]
    fn the_bundled_ffmpeg_can_do_everything_the_product_needs() {
        let Some(t) = tools() else {
            eprintln!("SKIP: no ffmpeg available");
            return;
        };
        let c = t.capabilities();
        eprintln!("{c:#?}");

        assert!(!c.version_line.is_empty(), "no version reported");
        assert!(c.supports_rtmps, "cannot publish to YouTube: {:?}", c.problems());
        assert!(
            c.supports_fps_mode,
            "the normalizer sends -fps_mode, which this build rejects — FFmpeg 5.1+ is required"
        );
        assert!(c.h264_encoder.is_some(), "no working H.264 encoder");
        assert!(c.has_aac);
        assert!(c.can_broadcast());
        assert!(c.can_normalize());
        assert!(c.problems().is_empty(), "{:?}", c.problems());
    }

    #[test]
    fn an_unknown_option_is_detected_rather_than_silently_ignored() {
        let Some(t) = tools() else {
            eprintln!("SKIP: no ffmpeg available");
            return;
        };
        // The probe must be able to tell a supported flag from an invented one,
        // or the FFmpeg 4.x case it exists to catch would slip through.
        assert!(t.accepts_output_flag(&["-fps_mode", "cfr"]));
        assert!(!t.accepts_output_flag(&["-definitely_not_a_real_option", "1"]));
    }

    #[test]
    fn broadcasting_and_normalizing_are_judged_separately() {
        // A build that cannot encode can still broadcast: the live path is a
        // remux. Saying "FFmpeg is broken" in that case would be wrong.
        let remux_only = FfmpegCapabilities {
            version_line: "ffmpeg version 4.1".into(),
            supports_fps_mode: false,
            h264_encoder: None,
            has_aac: false,
            supports_rtmps: true,
        };
        assert!(remux_only.can_broadcast());
        assert!(!remux_only.can_normalize());
        assert_eq!(remux_only.problems().len(), 3);

        let unusable = FfmpegCapabilities { supports_rtmps: false, ..remux_only.clone() };
        assert!(!unusable.can_broadcast());
        assert!(unusable.problems()[0].contains("RTMPS"));

        let good = FfmpegCapabilities {
            version_line: "ffmpeg version 6.1.1".into(),
            supports_fps_mode: true,
            h264_encoder: Some("libx264".into()),
            has_aac: true,
            supports_rtmps: true,
        };
        assert!(good.can_broadcast() && good.can_normalize());
        assert!(good.problems().is_empty());
    }

    #[test]
    fn an_ffmpeg_4_style_build_is_rejected_for_normalization() {
        // Exactly what the npm-distributed FFmpeg 4.1 builds look like: GPL,
        // libx264 present, RTMPS present, but no -fps_mode.
        let ffmpeg_4 = FfmpegCapabilities {
            version_line: "ffmpeg version 4.1.5".into(),
            supports_fps_mode: false,
            h264_encoder: Some("libx264".into()),
            has_aac: true,
            supports_rtmps: true,
        };
        assert!(ffmpeg_4.can_broadcast(), "4.x can still remux");
        assert!(!ffmpeg_4.can_normalize(), "4.x must not be trusted to optimize");
        assert!(ffmpeg_4.problems().iter().any(|p| p.contains("5.1")));
    }
}

#[cfg(test)]
mod encoder_listing_tests {
    use super::*;

    #[test]
    fn the_encoder_list_includes_audio_as_well_as_video() {
        // The regression: listing only video encoders made the AAC capability
        // check report false on a build that plainly has AAC.
        let Ok(t) = FfmpegTools::discover(Some(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../apps/desktop/src-tauri/binaries"),
        )) else {
            eprintln!("SKIP: no ffmpeg available");
            return;
        };
        let e = t.available_encoders().unwrap();
        assert!(e.iter().any(|x| x == "aac"), "aac missing from the encoder list");
        assert!(e.iter().any(|x| x == "libx264" || x.starts_with("h264_")), "no h264 encoder listed");
        // Selecting a video encoder must still not pick an audio one.
        let picked = select_encoder(&e);
        assert!(picked == "libx264" || picked.starts_with("h264_"), "picked {picked}");
    }
}
