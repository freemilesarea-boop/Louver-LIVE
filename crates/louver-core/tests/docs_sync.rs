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
