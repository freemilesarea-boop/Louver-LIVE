//! Concat-demuxer manifest generation (§13).
//!
//! FFmpeg's concat demuxer parses this file itself, so the single-quote escaping
//! rule here is FFmpeg's, not the shell's: inside `file '...'` a literal quote
//! is written as `'\''`.

use crate::error::Result;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Escape a path for the `file '<path>'` directive of the concat demuxer.
pub fn escape_concat_path(path: &Path) -> String {
    path.to_string_lossy().replace('\'', r"'\''")
}

/// Render a manifest listing each normalized file once, in play order.
pub fn render_manifest(files: &[PathBuf]) -> String {
    let mut s = String::from("ffconcat version 1.0\n");
    for f in files {
        let _ = writeln!(s, "file '{}'", escape_concat_path(f));
    }
    s
}

/// Write the manifest for a session. Paths must be absolute: FFmpeg resolves
/// relative entries against the manifest's own directory, which would silently
/// break as soon as the cache moves.
pub fn write_manifest(path: &Path, files: &[PathBuf]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, render_manifest(files))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_has_header_and_one_line_per_file() {
        let m = render_manifest(&[PathBuf::from("/c/a.mp4"), PathBuf::from("/c/b.mp4")]);
        let lines: Vec<&str> = m.lines().collect();
        assert_eq!(lines[0], "ffconcat version 1.0");
        assert_eq!(lines[1], "file '/c/a.mp4'");
        assert_eq!(lines[2], "file '/c/b.mp4'");
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn windows_and_korean_paths_are_written_verbatim() {
        let m = render_manifest(&[
            PathBuf::from(r"C:\Users\Test User\cache\재즈 영상 01.mp4"),
            PathBuf::from("/Users/test/Music/오늘 밤 재즈.mp4"),
        ]);
        assert!(m.contains(r"file 'C:\Users\Test User\cache\재즈 영상 01.mp4'"));
        assert!(m.contains("file '/Users/test/Music/오늘 밤 재즈.mp4'"));
    }

    #[test]
    fn single_quote_in_a_path_is_escaped_the_ffmpeg_way() {
        let p = PathBuf::from("/music/rock'n'roll.mp4");
        assert_eq!(escape_concat_path(&p), r"/music/rock'\''n'\''roll.mp4");
        assert!(render_manifest(&[p]).contains(r"file '/music/rock'\''n'\''roll.mp4'"));
    }

    #[test]
    fn empty_playlist_still_renders_a_valid_header() {
        assert_eq!(render_manifest(&[]), "ffconcat version 1.0\n");
    }

    #[test]
    fn manifest_round_trips_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("manifest.txt");
        write_manifest(&p, &[PathBuf::from("/c/a.mp4")]).unwrap();
        assert!(std::fs::read_to_string(&p).unwrap().contains("file '/c/a.mp4'"));
    }
}
