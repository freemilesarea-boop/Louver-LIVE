//! Media import, inspection, normalization and caching.

pub mod cache;
pub mod normalize;
pub mod probe;

/// Container extensions accepted on import (§6).
pub const SUPPORTED_EXTENSIONS: &[&str] =
    &["mp4", "mov", "mkv", "m4v", "webm", "avi", "ts", "mpg", "mpeg", "flv", "wmv"];

pub fn is_supported_extension(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| SUPPORTED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn extension_filter_accepts_the_documented_formats() {
        for p in ["/a/b.mp4", "/a/b.MOV", "/a/b.mkv", "/a/오늘 밤.MP4"] {
            assert!(is_supported_extension(Path::new(p)), "{p}");
        }
    }

    #[test]
    fn extension_filter_rejects_non_video() {
        for p in ["/a/b.txt", "/a/b.jpg", "/a/b", "/a/b.mp3"] {
            assert!(!is_supported_extension(Path::new(p)), "{p}");
        }
    }
}
