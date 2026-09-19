//! FFmpeg / ffprobe command construction.
//!
//! Commands are always built as an argv vector and executed without a shell
//! (§60). Nothing in this module interpolates a path into a shell string, so
//! spaces and non-ASCII characters in paths are handled by the OS exec layer
//! rather than by quoting rules (§39).

use crate::config::{OutputProfile, StreamMode};
use crate::error::{ErrorCode, LouverError, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

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

    /// Video encoders FFmpeg reports as available, used for hardware detection (§9).
    pub fn available_encoders(&self) -> Result<Vec<String>> {
        let out = Command::new(&self.ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-encoders"])
            .output()
            .map_err(|e| LouverError::with_detail(ErrorCode::FfmpegNotFound, e.to_string()))?;
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let l = l.trim();
                // Lines look like: " V....D h264_nvenc  NVIDIA NVENC H.264 encoder"
                if l.starts_with('V') {
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

/// Pick the best available encoder for normalization, falling back to libx264 (§9).
pub fn select_encoder(available: &[String]) -> String {
    for want in preferred_hw_encoders() {
        if available.iter().any(|e| e == want) {
            return (*want).to_string();
        }
    }
    "libx264".to_string()
}

/// Whether the chosen encoder is hardware-accelerated.
pub fn is_hardware_encoder(encoder: &str) -> bool {
    encoder != "libx264"
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
