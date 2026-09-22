//! Keeps the documentation honest (§35, §69).
//!
//! The error-code table in README.md is part of the product's contract: it is
//! what a user reads when something fails. A code added in Rust and not
//! documented there is a code the user cannot look up, so this test fails the
//! build rather than letting the table quietly rot.

use louver_core::error::ALL_ERROR_CODES;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The document's text, or None when it is not in this checkout.
fn try_read(name: &str) -> Option<String> {
    std::fs::read_to_string(repo_root().join(name)).ok()
}

fn read(name: &str) -> String {
    std::fs::read_to_string(repo_root().join(name)).unwrap_or_else(|e| panic!("cannot read {name}: {e}"))
}

#[test]
fn every_error_code_is_documented_in_the_readme() {
    let readme = read("README.md");
    let missing: Vec<&str> =
        ALL_ERROR_CODES.iter().map(|c| c.as_str()).filter(|c| !readme.contains(*c)).collect();
    assert!(missing.is_empty(), "these error codes are not in README.md's table: {missing:?}");
}

#[test]
fn the_readme_documents_no_codes_that_no_longer_exist() {
    let readme = read("README.md");
    let known: Vec<&str> = ALL_ERROR_CODES.iter().map(|c| c.as_str()).collect();

    // Pull every LL-XXX-NNN token out of the table.
    let mut stale = Vec::new();
    for token in readme.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
        if token.starts_with("LL-")
            && token.len() > 6
            && token.rsplit('-').next().is_some_and(|n| n.len() == 3 && n.chars().all(|c| c.is_ascii_digit()))
            && !known.contains(&token)
        {
            stale.push(token.to_string());
        }
    }
    stale.sort();
    stale.dedup();
    assert!(stale.is_empty(), "README.md documents codes that no longer exist: {stale:?}");
}

#[test]
fn the_required_documents_exist_and_are_not_placeholders() {
    // §69 names these five explicitly.
    for name in ["README.md", "ARCHITECTURE.md", "TESTING.md", "BENCHMARK.md", "LICENSES.md"] {
        let body = read(name);
        assert!(body.len() > 500, "{name} is too short to be real documentation");
        assert!(!body.to_uppercase().contains("TODO:"), "{name} still has a TODO");
    }
}

#[test]
fn the_readme_covers_what_the_spec_asks_it_to() {
    // §69's list: install, dev, test, build, stream key, first broadcast,
    // troubleshooting, log location.
    let readme = read("README.md");
    for section in [
        "## 설치",
        "## 개발 실행",
        "## 테스트",
        "## 빌드",
        "## 스트림 키 입력",
        "## 첫 방송",
        "## 문제 해결",
        "## 로그 위치",
    ] {
        assert!(readme.contains(section), "README.md is missing the section {section}");
    }
}

#[test]
fn the_benchmark_does_not_claim_numbers_it_did_not_measure() {
    // §65 forbids asserting a CPU target that was never measured, so the
    // document must carry both real figures and an explicit gap list.
    let bench = read("BENCHMARK.md");
    assert!(bench.contains("## Not measured"), "BENCHMARK.md must state what it did not measure");
    assert!(bench.contains("## Reproducing"), "BENCHMARK.md must say how to reproduce its figures");
}

/// Every document that tells someone where to look for a file.
const DOCS_WITH_PATHS: &[&str] = &[
    "README.md",
    "YOUTUBE_SETUP.md",
    "MACOS_RELEASE_TEST.md",
    "MACOS_QA.md",
    "LOCAL_TEST.md",
    "rc-results/youtube-test.md",
];

#[test]
fn no_document_sends_a_mac_user_to_a_folder_the_app_never_writes() {
    // A real diagnostic session was spent on this: the logs were documented
    // at `~/Library/Logs/com.louver.live/`, which the app has never written
    // to. `AppPaths` puts them under the application support directory, named
    // for the app rather than for the bundle identifier.
    let wrong = ["Library/Logs/com.louver.live", "Application Support/com.louver.live"];
    let mut checked = 0;
    for doc in DOCS_WITH_PATHS {
        // Skip one that is not in this checkout rather than failing: some of
        // these live under `rc-results/`, where everything but the checklist
        // itself is local test output. A missing document is not a wrong path.
        let Some(text) = try_read(doc) else { continue };
        checked += 1;
        for w in wrong {
            assert!(
                !text.contains(w),
                "{doc} points at {w}, which the app does not use — see AppPaths::logs_dir"
            );
        }
    }
    // And the list itself has not rotted away to nothing.
    assert!(checked >= 4, "only {checked} of the path documents were found");
}

#[test]
fn the_readme_names_the_path_the_code_actually_builds() {
    // Derived from the same function the app calls, so the table cannot drift
    // from the binary.
    let paths = louver_core::AppPaths::new("/Users/me/Library/Application Support/LouverLive");
    // `join` uses the host separator, so on Windows this comes back as
    // `…/LouverLive\logs` and would never match a document written for a Mac.
    // The documented path is the macOS one whatever machine runs the test.
    let logs = paths.logs_dir().to_string_lossy().replace('\\', "/").replace("/Users/me", "~");
    let readme = read("README.md");
    assert!(readme.contains(&logs), "README.md does not document {logs}");
    // The identifier is still right for the keychain item, so it is only the
    // *path* spelling that is forbidden above.
    assert!(read("MACOS_QA.md").contains("find-generic-password -s com.louver.live"));
}

#[test]
fn every_provisioning_event_the_code_writes_is_in_the_readme() {
    // The names are what a support conversation greps for. One added in Rust
    // and not documented is one nobody knows to look for.
    use louver_core::youtube::steps::ProvisionStep::*;
    let readme = read("README.md");
    for step in [
        TokenRefresh,
        BroadcastList,
        BroadcastInsert,
        StreamList,
        BroadcastBind,
        MetadataApply,
        StreamActive,
        BroadcastTransition,
    ] {
        assert!(readme.contains(step.event()), "{} is not documented in README.md", step.event());
    }
}

#[test]
fn every_file_that_carries_the_version_agrees_with_the_crate() {
    // Four places say the version, and the installer filenames come from the
    // Tauri one while the About box comes from the crate. A release whose
    // `.dmg` says 1.0.0 and whose window says 1.0.1 is a support call nobody
    // can answer, so they are checked against each other rather than trusted.
    let crate_version = env!("CARGO_PKG_VERSION");

    let field = |file: &str, key: &str| -> String {
        let text = read(file);
        let at = text.find(key).unwrap_or_else(|| panic!("{file} has no {key}"));
        let rest = &text[at + key.len()..];
        let start = rest.find('"').expect("no opening quote") + 1;
        let end = rest[start..].find('"').expect("no closing quote") + start;
        rest[start..end].to_string()
    };

    assert_eq!(field("package.json", "\"version\":"), crate_version, "package.json");
    assert_eq!(
        field("apps/desktop/src-tauri/tauri.conf.json", "\"version\":"),
        crate_version,
        "tauri.conf.json — this one names the installers"
    );

    // The workspace version the desktop crate inherits.
    let root = read("Cargo.toml");
    let line = root
        .lines()
        .skip_while(|l| !l.starts_with("[workspace.package]"))
        .find(|l| l.starts_with("version"))
        .expect("Cargo.toml has no [workspace.package] version");
    assert!(line.contains(crate_version), "Cargo.toml says {line}, crate says {crate_version}");
}

#[test]
fn the_credential_check_reports_presence_and_never_a_value() {
    // The one thing `--credential-check` exists to print, and the one thing
    // it must never print. Asserted on the source because running it needs a
    // built binary, and the format is a contract: the release workflow fails
    // the build on its exit code, and a support conversation reads its output.
    let main = read("apps/desktop/src-tauri/src/main.rs");
    assert!(main.contains("--credential-check"), "the flag is gone");
    assert!(main.contains("OAuth Client ID: {}"), "the ID line changed shape");
    assert!(main.contains("OAuth Client Secret: {}"), "the secret line changed shape");
    assert!(main.contains("credential_presence()"), "it must read the compiled-in presence");

    // `credential_presence` returns two booleans and nothing else, so there
    // is no value for the caller to print even by accident.
    let oauth = read("crates/louver-core/src/youtube/oauth.rs");
    assert!(
        oauth.contains("pub fn credential_presence() -> (bool, bool)"),
        "credential_presence must keep returning booleans, not the credentials"
    );

    // And the release workflow asks the binary rather than echoing secrets.
    let wf = read(".github/workflows/release.yml");
    assert!(wf.contains("--credential-check"), "the release no longer verifies the embedded client");
    assert!(
        !wf.contains("echo \"${{ secrets.LOUVER_GOOGLE_CLIENT_SECRET }}\""),
        "the workflow echoes the client secret"
    );
}

/// This release ships Windows and macOS only.
///
/// Not a style rule: a Linux entry in the matrix produces a `.deb` and an
/// `.AppImage` that get attached to the draft release and handed to
/// customers, and the decision to stop shipping those was a deliberate one.
/// If it is reversed, it should be reversed on purpose and this test is where
/// that is written down.
#[test]
fn the_release_builds_windows_and_macos_and_nothing_else() {
    let wf = read(".github/workflows/release.yml");

    // The matrix block, up to the steps that follow it — and only the values
    // in it. The comments there explain which runners were tried and rejected
    // and name them, which is exactly what this test must not read.
    let block = wf
        .split_once("matrix:")
        .and_then(|(_, rest)| rest.split_once("\n    steps:"))
        .map(|(m, _)| m.to_string())
        .expect("release.yml has no matrix");
    let matrix: String = block
        .lines()
        .map(|l| l.split_once('#').map_or(l, |(code, _)| code))
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    for banned in ["ubuntu", "linux-gnu"] {
        assert!(
            !matrix.contains(banned),
            "release matrix still targets {banned}; this release is Windows and macOS only:\n{matrix}"
        );
    }
    for wanted in ["x86_64-pc-windows-msvc", "aarch64-apple-darwin", "x86_64-apple-darwin"] {
        assert!(matrix.contains(wanted), "release matrix lost {wanted}:\n{matrix}");
    }

    // macos-13 was never given a runner; macos-15-intel was.
    assert!(!matrix.contains("macos-13"), "macos-13 never gets a runner; use macos-15-intel");
    assert!(matrix.contains("macos-15-intel"), "the Intel build needs macos-15-intel");

    // And nothing collects a Linux installer even if one appeared.
    let collect = wf.split_once("Collect the installers").expect("no collect step").1;
    let collect = collect.split_once("upload-artifact").map(|(c, _)| c).unwrap_or(collect);
    for banned in [".deb", ".AppImage", ".rpm"] {
        assert!(!collect.contains(banned), "the collect step still gathers {banned}");
    }
}

/// The checks that make an installer worth selling stay in the release.
///
/// Every one of these has been the difference between a release that works
/// and one that looks like it does. A green run that skipped them is worth
/// nothing, so removing one has to fail here first.
#[test]
fn the_release_never_gets_green_by_checking_less() {
    let wf = read(".github/workflows/release.yml");
    let action = read(".github/actions/sidecars/action.yml");

    assert!(
        !wf.contains("continue-on-error") && !action.contains("continue-on-error"),
        "a release step is allowed to fail without failing the release"
    );
    assert!(action.contains("--require-download"), "the release may ship the development FFmpeg again");
    assert!(action.contains("ffmpeg-manifest.mjs --check"), "the sidecar manifest check is gone");
    assert!(action.contains("check-sidecars.mjs"), "the sidecar architecture check is gone");
    assert!(
        wf.contains("--credential-check"),
        "the release no longer asks the binary about its OAuth client"
    );
    assert!(wf.contains("npm run verify"), "the release no longer runs the test suite");
}
