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
    for doc in DOCS_WITH_PATHS {
        let text = read(doc);
        for w in wrong {
            assert!(
                !text.contains(w),
                "{doc} points at {w}, which the app does not use — see AppPaths::logs_dir"
            );
        }
    }
}

#[test]
fn the_readme_names_the_path_the_code_actually_builds() {
    // Derived from the same function the app calls, so the table cannot drift
    // from the binary.
    let paths = louver_core::AppPaths::new("/Users/me/Library/Application Support/LouverLive");
    let logs = paths.logs_dir().to_string_lossy().replace("/Users/me", "~");
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
