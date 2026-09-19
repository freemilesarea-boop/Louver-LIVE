//! Rotating file logs with mandatory secret masking (§34).
//!
//! Every line goes through [`mask_secrets`] before it is written, so no code
//! path can put a stream key in a log file.

use crate::error::Result;
use crate::streaming::ffmpeg::mask_secrets;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
pub const MAX_LOG_FILES: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogTarget {
    App,
    Stream,
    Ffmpeg,
}

impl LogTarget {
    pub fn file_name(self) -> &'static str {
        match self {
            Self::App => "app.log",
            Self::Stream => "stream.log",
            Self::Ffmpeg => "ffmpeg.log",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

/// Size-rotating, masking file logger.
#[derive(Debug)]
pub struct Logger {
    dir: PathBuf,
    files: Mutex<std::collections::HashMap<&'static str, File>>,
    min_level: Level,
}

impl Logger {
    pub fn new(dir: impl Into<PathBuf>) -> Result<Arc<Self>> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Arc::new(Self {
            dir,
            files: Mutex::new(std::collections::HashMap::new()),
            min_level: if cfg!(debug_assertions) { Level::Debug } else { Level::Info },
        }))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn log(&self, target: LogTarget, level: Level, message: &str) {
        if level < self.min_level {
            return;
        }
        // The one place that matters: nothing is written unmasked (§34).
        let line = format!(
            "{} [{}] {}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            level.label(),
            mask_secrets(message)
        );
        let _ = self.write_line(target, &line);
    }

    fn write_line(&self, target: LogTarget, line: &str) -> Result<()> {
        let name = target.file_name();
        let path = self.dir.join(name);
        self.rotate_if_needed(&path)?;

        let mut files = self.files.lock().unwrap();
        // Re-open after a rotation, since the old handle points at the moved file.
        let needs_open = match files.get(name) {
            Some(f) => f.metadata().map(|m| m.len()).unwrap_or(0) > MAX_LOG_BYTES,
            None => true,
        };
        if needs_open {
            files.insert(name, OpenOptions::new().create(true).append(true).open(&path)?);
        }
        if let Some(f) = files.get_mut(name) {
            f.write_all(line.as_bytes())?;
            f.flush()?;
        }
        Ok(())
    }

    fn rotate_if_needed(&self, path: &Path) -> Result<()> {
        let Ok(meta) = std::fs::metadata(path) else { return Ok(()) };
        if meta.len() <= MAX_LOG_BYTES {
            return Ok(());
        }
        self.files.lock().unwrap().remove(path.file_name().unwrap().to_str().unwrap());
        rotate(path, MAX_LOG_FILES)
    }

    pub fn info(&self, t: LogTarget, m: &str) {
        self.log(t, Level::Info, m);
    }
    pub fn warn(&self, t: LogTarget, m: &str) {
        self.log(t, Level::Warn, m);
    }
    pub fn error(&self, t: LogTarget, m: &str) {
        self.log(t, Level::Error, m);
    }

    /// Tail of a log file, for the in-app Logs page (§61).
    pub fn tail(&self, target: LogTarget, lines: usize) -> Vec<String> {
        let Ok(content) = std::fs::read_to_string(self.dir.join(target.file_name())) else {
            return Vec::new();
        };
        let all: Vec<&str> = content.lines().collect();
        all[all.len().saturating_sub(lines)..].iter().map(|s| s.to_string()).collect()
    }
}

/// Shift `app.log` → `app.log.1` → … and drop anything past `keep` (§34).
pub fn rotate(path: &Path, keep: usize) -> Result<()> {
    let oldest = path.with_extension(format!(
        "{}.{}",
        path.extension().and_then(|e| e.to_str()).unwrap_or("log"),
        keep
    ));
    if oldest.exists() {
        std::fs::remove_file(&oldest)?;
    }
    for i in (1..keep).rev() {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("log");
        let from = path.with_extension(format!("{ext}.{i}"));
        let to = path.with_extension(format!("{ext}.{}", i + 1));
        if from.exists() {
            std::fs::rename(&from, &to)?;
        }
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("log");
    if path.exists() {
        std::fs::rename(path, path.with_extension(format!("{ext}.1")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_key_never_reaches_the_log_file() {
        let d = tempfile::tempdir().unwrap();
        let l = Logger::new(d.path()).unwrap();
        l.info(LogTarget::Ffmpeg, "publishing to rtmps://a.rtmps.youtube.com/live2/abcd-efgh-ijkl-mnop");
        l.error(LogTarget::Stream, "key abcd-efgh-ijkl-mnop was rejected");

        for f in ["ffmpeg.log", "stream.log"] {
            let c = std::fs::read_to_string(d.path().join(f)).unwrap();
            assert!(!c.contains("abcd-efgh"), "stream key leaked into {f}: {c}");
            assert!(c.contains("••••"), "masking marker missing in {f}");
        }
    }

    #[test]
    fn lines_carry_a_timestamp_and_level() {
        let d = tempfile::tempdir().unwrap();
        let l = Logger::new(d.path()).unwrap();
        l.warn(LogTarget::App, "저장 공간이 부족합니다");
        let c = std::fs::read_to_string(d.path().join("app.log")).unwrap();
        assert!(c.contains("[WARN]"));
        assert!(c.contains("저장 공간이 부족합니다"), "korean text must survive");
        assert!(c.starts_with("20"), "should start with a year: {c}");
    }

    #[test]
    fn targets_write_to_separate_files() {
        let d = tempfile::tempdir().unwrap();
        let l = Logger::new(d.path()).unwrap();
        l.info(LogTarget::App, "app line");
        l.info(LogTarget::Stream, "stream line");
        l.info(LogTarget::Ffmpeg, "ffmpeg line");
        for (f, want) in [("app.log", "app line"), ("stream.log", "stream line"), ("ffmpeg.log", "ffmpeg line")] {
            let c = std::fs::read_to_string(d.path().join(f)).unwrap();
            assert!(c.contains(want), "{f}");
            assert_eq!(c.lines().count(), 1, "{f} got someone else's line");
        }
    }

    #[test]
    fn tail_returns_the_last_lines() {
        let d = tempfile::tempdir().unwrap();
        let l = Logger::new(d.path()).unwrap();
        for i in 0..20 {
            l.info(LogTarget::App, &format!("line {i}"));
        }
        let t = l.tail(LogTarget::App, 5);
        assert_eq!(t.len(), 5);
        assert!(t[4].contains("line 19"));
        assert!(l.tail(LogTarget::Stream, 5).is_empty(), "missing file tails to empty");
    }

    #[test]
    fn rotation_shifts_files_and_drops_the_oldest() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("app.log");
        std::fs::write(&p, "newest").unwrap();
        for i in 1..=5 {
            std::fs::write(d.path().join(format!("app.log.{i}")), format!("gen{i}")).unwrap();
        }
        rotate(&p, 5).unwrap();

        assert!(!p.exists(), "the live file was moved aside");
        assert_eq!(std::fs::read_to_string(d.path().join("app.log.1")).unwrap(), "newest");
        assert_eq!(std::fs::read_to_string(d.path().join("app.log.2")).unwrap(), "gen1");
        assert_eq!(std::fs::read_to_string(d.path().join("app.log.5")).unwrap(), "gen4");
        assert!(!d.path().join("app.log.6").exists(), "must keep at most 5 (§34)");
    }

    #[test]
    fn rotation_limits_are_what_the_spec_asks_for() {
        assert_eq!(MAX_LOG_BYTES, 10 * 1024 * 1024);
        assert_eq!(MAX_LOG_FILES, 5);
    }

    #[test]
    fn a_large_log_is_rotated_automatically() {
        let d = tempfile::tempdir().unwrap();
        let l = Logger::new(d.path()).unwrap();
        std::fs::write(d.path().join("app.log"), vec![b'x'; (MAX_LOG_BYTES + 1) as usize]).unwrap();
        l.info(LogTarget::App, "after rotation");
        assert!(d.path().join("app.log.1").exists(), "oversized log was not rotated");
        let c = std::fs::read_to_string(d.path().join("app.log")).unwrap();
        assert!(c.contains("after rotation"));
        assert!(c.len() < 1000, "the new log should be small");
    }
}
