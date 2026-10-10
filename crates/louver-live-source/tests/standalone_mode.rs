//! That the standalone test mode cannot authenticate against production.
//!
//! The unit tests in `src/standalone.rs` check the rules. These check the
//! binary: that the refusals happen before it serves anything, that the banner
//! says where authentication is actually going, and — the one that protects
//! customers rather than the test — that none of it changes what happens when
//! the flag is absent.

use std::process::Command;

const SIGNING: &str = "a-signing-secret-for-standalone-tests";
const GATE: &str = "a-gate-secret-for-the-standalone-tests";
const ADMIN: &str = "an-admin-secret-for-standalone-tests!";
const TEST_HOST: &str = "https://beta-test.example.com";

struct Run {
    out: String,
    ok: bool,
}

/// An address that is not on this machine, so `bind` fails with
/// `EADDRNOTAVAIL` whatever user the test runs as. A privileged port would not
/// do: as root `127.0.0.1:1` binds successfully and the process then serves
/// forever, which hangs the test rather than failing it.
const UNBINDABLE: &str = "203.0.113.1:9080";

/// Run the api binary against an address it cannot bind, so it starts, prints
/// its banner and exits. That is the window in which a misconfiguration is
/// either caught or served.
fn run(extra: &[&str]) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();

    let mut args: Vec<String> = vec![
        "--listen".into(),
        UNBINDABLE.into(),
        "--media-dir".into(),
        media.to_str().unwrap().into(),
        "--state-dir".into(),
        dir.path().join("state").to_str().unwrap().into(),
    ];
    args.extend(extra.iter().map(|s| (*s).to_string()));

    let out = Command::new(env!("CARGO_BIN_EXE_live-source-api"))
        .args(&args)
        .env("LOUVER_LIVE_SOURCE_SECRET", SIGNING)
        .env("LOUVER_LIVE_SOURCE_GATE_SECRET", GATE)
        .env("LOUVER_LIVE_SOURCE_ADMIN_SECRET", ADMIN)
        .env(
            "LOUVER_LIVE_SOURCE_DESTINATIONS",
            r#"{"beta-tester":{"beta-test-local":"rtmp://127.0.0.1:1935/live/test"}}"#,
        )
        .output()
        .expect("api binary");

    let both = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    for secret in [SIGNING, GATE, ADMIN] {
        assert!(!both.contains(secret), "a secret reached the output:\n{both}");
    }
    Run { ok: out.status.success(), out: both }
}

/// The banner line that proves the worker got as far as serving.
fn started(r: &Run) -> bool {
    r.out.contains("복원된 작업")
}

#[test]
fn standalone_mode_starts_with_a_test_origin_and_says_where_auth_goes() {
    let r = run(&["--standalone-test", "--origin", TEST_HOST, "--allow-origin", TEST_HOST]);
    assert!(started(&r), "should have reached the banner:\n{}", r.out);
    assert!(r.out.contains("mode=standalone-test"), "{}", r.out);
    assert!(r.out.contains(&format!("auth-origin={TEST_HOST}")), "{}", r.out);
    assert!(r.out.contains(&format!("allow-origin={TEST_HOST}")), "{}", r.out);
    // The whole point: production is not named as the authentication target.
    assert!(!r.out.contains("auth-origin=https://247streams.kr"), "{}", r.out);
}

#[test]
fn standalone_mode_refuses_to_start_against_the_production_origin() {
    let r = run(&["--standalone-test", "--origin", "https://247streams.kr"]);
    assert!(!r.ok, "should have exited non-zero:\n{}", r.out);
    assert!(!started(&r), "must refuse before serving anything:\n{}", r.out);
    assert!(r.out.contains("운영 호스트"), "{}", r.out);
}

#[test]
fn standalone_mode_refuses_a_missing_origin_rather_than_defaulting_to_production() {
    // The accident this guard exists for: the browser origin says test, and
    // `--origin` is simply absent, so without the guard authentication would
    // have gone to the live service.
    let r = run(&["--standalone-test", "--allow-origin", TEST_HOST]);
    assert!(!r.ok, "should have exited non-zero:\n{}", r.out);
    assert!(!started(&r), "must refuse before serving anything:\n{}", r.out);
    assert!(r.out.contains("--origin"), "{}", r.out);
}

#[test]
fn standalone_mode_refuses_an_allow_origin_that_is_not_the_test_host() {
    let r =
        run(&["--standalone-test", "--origin", TEST_HOST, "--allow-origin", "https://elsewhere.example.com"]);
    assert!(!r.ok, "should have exited non-zero:\n{}", r.out);
    assert!(!started(&r), "must refuse before serving anything:\n{}", r.out);
    assert!(r.out.contains("--allow-origin"), "{}", r.out);
}

#[test]
fn without_the_flag_nothing_changes_for_production() {
    // Backward compatibility, as behaviour rather than as a claim: no flag, no
    // `--origin`, and the production default is still what it has always been.
    let r = run(&[]);
    assert!(started(&r), "the production configuration must still start:\n{}", r.out);
    assert!(r.out.contains("mode=production"), "{}", r.out);
    assert!(r.out.contains("auth-origin=https://247streams.kr"), "{}", r.out);
    // `--allow-origin` still falls back to the origin.
    assert!(r.out.contains("allow-origin=https://247streams.kr"), "{}", r.out);
}

#[test]
fn without_the_flag_the_production_origin_is_still_accepted_explicitly() {
    let r = run(&["--origin", "https://247streams.kr", "--allow-origin", "https://247streams.kr"]);
    assert!(started(&r), "the production configuration must still start:\n{}", r.out);
    assert!(!r.out.contains("운영 호스트"), "the guard must not fire without its flag:\n{}", r.out);
}
