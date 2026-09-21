// Do not open a console window alongside the GUI on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // `--credential-check` answers one question without starting the app:
    // did this build get an OAuth client compiled into it?
    //
    // It exists because that cannot be told from the outside. `option_env!`
    // is resolved when louver-core compiles, so a release built without the
    // secrets present produces an installer that is correct in every other
    // way and whose connect button says it carries no client. The release
    // workflow runs this against the binary it just built, and a support
    // conversation can ask a customer to run it too.
    //
    // It prints whether, never what: a client id and a secret are what this
    // must not put on a terminal or in a CI log.
    if std::env::args().any(|a| a == "--credential-check") {
        let (id, secret) = louver_core::youtube::oauth::credential_presence();
        let word = |b: bool| if b { "configured" } else { "missing" };
        println!("OAuth Client ID: {}", word(id));
        println!("OAuth Client Secret: {}", word(secret));
        // Non-zero when the build cannot connect an account at all, so the
        // workflow needs no output parsing to fail on it.
        std::process::exit(if id { 0 } else { 1 });
    }
    louver_desktop::run()
}
