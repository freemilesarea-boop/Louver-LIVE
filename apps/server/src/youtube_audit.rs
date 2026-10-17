//! `louver-server --youtube-audit`: what YouTube says about every broadcast,
//! read without writing anything.
//!
//! The operator-facing half of the P0 follow-up. It exists because the
//! question "is this customer actually live?" could not be answered from this
//! end: the dashboard knew what FFmpeg was doing and nothing else, and the one
//! tool that joined the two — `--diagnose` — opens the database with
//! [`CloudDb::open`], which runs the schema and every `ensure_column` and so
//! cannot be pointed at a live production database at all.
//!
//! So this one:
//!
//! * opens the database **read-only**, which SQLite enforces rather than this
//!   code promising it;
//! * asks YouTube with two `list` calls per broadcast and no writes, ever;
//! * does not refresh an expired token unless told to, because a refresh can
//!   rotate a refresh token that a read-only process cannot store;
//! * prints a verdict from the operator runbook's own seven words, separating
//!   what was observed from what was inferred;
//! * prints no stream key, no token, no title, and no customer identifier
//!   unless `--show-ids` is passed.
//!
//! It is safe to run against production while customers are broadcasting. It
//! starts nothing, stops nothing, and signals nothing.

use louver_cloud::audit::{Reach, Verdict};
use louver_cloud::credentials::{master_key_from_env, CredentialStore};
use louver_cloud::youtube::Youtube;
use louver_cloud::CloudDb;
use louver_core::security::SecretStore;
use louver_core::youtube::oauth::TokenEndpoint;
use louver_core::youtube::HttpClient;
use std::path::PathBuf;
use std::sync::Arc;

/// How the run was asked to behave. Parsed from argv so the flags are visible
/// in one place rather than read out of the environment.
struct Options {
    /// Print the real YouTube broadcast and stream ids.
    ///
    /// Off by default: they are customer identifiers and this output gets
    /// pasted into chats. On, when the operator needs to open the broadcast in
    /// YouTube Studio.
    show_ids: bool,
    /// Allow one token refresh for an account whose cached token has expired.
    ///
    /// Off by default. See [`Youtube::read_only_may_refresh`] — a refresh can
    /// rotate a refresh token this process has nowhere to put.
    allow_token_refresh: bool,
}

impl Options {
    fn from_args(args: &[String]) -> Self {
        Self {
            show_ids: args.iter().any(|a| a == "--show-ids"),
            allow_token_refresh: args.iter().any(|a| a == "--allow-token-refresh"),
        }
    }
}

/// An identifier, shown as little as the operator can work with.
///
/// Not a hash: these ids are in public watch URLs, so there is nothing to
/// protect cryptographically. The point is to keep customer identifiers out of
/// a transcript while leaving enough to match two of them up by eye — and the
/// comparison that actually matters is printed as a word, not left to the eye.
fn masked(id: &str) -> String {
    let n = id.chars().count();
    if n <= 6 {
        return "•".repeat(n);
    }
    let head: String = id.chars().take(2).collect();
    let tail: String = id.chars().skip(n - 2).collect();
    format!("{head}…{tail}")
}

fn show(id: Option<&str>, opts: &Options) -> String {
    match id {
        None => "—".into(),
        Some(s) if opts.show_ids => s.to_string(),
        Some(s) => masked(s),
    }
}

/// Is this pid a process that exists right now?
///
/// `/proc` rather than a signal: sending one to a customer's FFmpeg is exactly
/// what this tool promises not to do, and `kill(pid, 0)` is still a syscall
/// aimed at their process. Reading a directory entry cannot be mistaken for it.
///
/// A pid is also reused, so this is "something is running under that pid", not
/// "that FFmpeg is running". The output says which it means.
fn pid_alive(pid: i64) -> bool {
    pid > 0 && std::path::Path::new(&format!("/proc/{pid}")).exists()
}

pub fn run(args: &[String]) -> std::process::ExitCode {
    let opts = Options::from_args(args);
    let data = PathBuf::from(std::env::var("LOUVER_DATA_DIR").unwrap_or_else(|_| "/var/lib/louver".into()));
    let path = data.join("cloud.db");

    println!("=== 247streams YouTube 상태 점검 (읽기 전용) ===");
    println!("database : {} (read-only)", path.display());
    println!(
        "모드     : ids={} token-refresh={}",
        if opts.show_ids { "표시" } else { "가림" },
        if opts.allow_token_refresh { "허용" } else { "금지" }
    );

    // Read-only, enforced by SQLite. A missing file is an error rather than a
    // new empty database, which is what `open` would have made.
    let db = match CloudDb::open_read_only(&path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("[louver] 데이터베이스를 읽기 전용으로 열 수 없습니다: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let broadcasts = match db.all_broadcasts() {
        Ok(b) => b,
        Err(e) => {
            // The likeliest cause is worth naming: a read-only open does not
            // migrate, so a database older than this binary is missing columns
            // this binary selects. Reporting half a row would be worse.
            eprintln!("[louver] 방송 목록을 읽을 수 없습니다: {e}");
            eprintln!(
                "[louver] 이 데이터베이스가 현재 바이너리보다 오래된 스키마일 수 있습니다. \
                 읽기 전용 점검은 스키마를 변경하지 않으므로 같은 버전의 바이너리로 실행해야 합니다."
            );
            return std::process::ExitCode::FAILURE;
        }
    };

    let master = match master_key_from_env() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[louver] master key 를 읽을 수 없습니다: {e}");
            eprintln!("[louver] 토큰을 열 수 없으므로 YouTube 상태를 물어볼 수 없습니다.");
            return std::process::ExitCode::FAILURE;
        }
    };
    let keys: Arc<dyn SecretStore> = Arc::new(CredentialStore::new(db.raw(), master));

    let config = match louver_cloud::youtube::Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[louver] YouTube 설정을 읽을 수 없습니다: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let transport = Arc::new(louver_core::youtube::http::UreqClient {
        token_endpoint: std::env::var("YOUTUBE_TOKEN_ENDPOINT").ok().filter(|s| !s.trim().is_empty()),
        // The transport line prints the request path with its query, and the
        // YouTube broadcast id is in that query. This tool masks identifiers
        // unless asked, so it does its own reporting instead.
        quiet: true,
    });
    let yt = Youtube::new(
        db.clone(),
        Arc::clone(&keys),
        Arc::clone(&transport) as Arc<dyn HttpClient>,
        transport as Arc<dyn TokenEndpoint>,
        config,
    )
    .read_only()
    .read_only_may_refresh(opts.allow_token_refresh);

    let connected: Vec<_> = broadcasts.iter().filter(|b| b.youtube.account_id.is_some()).collect();
    let manual = broadcasts.len() - connected.len();
    println!(
        "방송     : 전체 {}개 / YouTube 연결 {}개 / 붙여넣은 키 {manual}개",
        broadcasts.len(),
        connected.len()
    );
    println!();

    let mut tally: std::collections::BTreeMap<&'static str, usize> = Default::default();
    let mut attention = Vec::new();

    for b in connected {
        let encoder_running = b.ffmpeg_pid.map(pid_alive).unwrap_or(false);
        let seen = yt.observe(&b.id);
        let f = seen.finding(b.youtube.stream_id.as_deref(), encoder_running);
        *tally.entry(f.verdict.label()).or_default() += 1;
        if f.verdict.needs_attention() {
            attention.push((b.id.clone(), b.name.clone(), f));
        }

        println!("--- {} [{}]", b.name, b.id);
        println!("    판정     : {}  — {}", f.verdict.label(), f.reason);

        // What this end believes. Facts about 247streams, not about YouTube.
        println!(
            "    내부     : desired={} runtime={} uptime={}s restarts={}",
            b.desired_state.id(),
            b.runtime_state.id(),
            b.uptime_secs,
            b.restart_count
        );
        println!(
            "    인코더   : pid={} → {}",
            b.ffmpeg_pid.map(|p| p.to_string()).unwrap_or_else(|| "—".into()),
            match b.ffmpeg_pid {
                None => "기록된 PID 없음".to_string(),
                Some(p) if pid_alive(p) => format!("PID {p} 로 실행 중인 프로세스가 있음"),
                Some(p) => format!("PID {p} 에 해당하는 프로세스가 없음"),
            }
        );
        println!(
            "    저장값   : youtube_status={} 확인시각={}",
            b.youtube.status.as_deref().unwrap_or("—"),
            b.youtube.checked_at.as_deref().unwrap_or("기록 없음")
        );

        // What YouTube said. Kept visually apart from the block above, because
        // conflating the two is the fault this whole exercise came from.
        match seen.reach {
            Reach::Answered => {
                println!(
                    "    YouTube  : lifeCycleStatus={} streamStatus={} health={}",
                    if seen.life_cycle_status.is_empty() { "—" } else { &seen.life_cycle_status },
                    if seen.stream_status.is_empty() { "—" } else { &seen.stream_status },
                    if seen.health_status.is_empty() { "—" } else { &seen.health_status }
                );
                println!(
                    "    설정     : enableAutoStart={} enableMonitorStream={}",
                    seen.enable_auto_start, seen.enable_monitor_stream
                );
                if !seen.configuration_issues.is_empty() {
                    println!("    수신문제 : {}", seen.configuration_issues.join(", "));
                }
                // §3's "저장된 방송 ID 와 실제 시청 대상이 같은가": printed as
                // the answer, not as two ids for the operator to compare.
                let stored = b.youtube.stream_id.as_deref();
                let bound = seen.bound_stream_id.as_deref();
                println!(
                    "    연결     : 저장 stream={} / YouTube bound={} → {}",
                    show(stored, &opts),
                    show(bound, &opts),
                    match (stored, bound) {
                        (Some(s), Some(t)) if s == t => "일치",
                        (_, None) => "YouTube 방송에 연결된 스트림이 없음",
                        (None, Some(_)) => "저장된 스트림이 없음",
                        _ => "불일치",
                    }
                );
                println!("    방송 ID  : {}", show(b.youtube.broadcast_id.as_deref(), &opts));
            }
            Reach::Refused => println!(
                "    YouTube  : 물어볼 수 없었습니다 (인증 거부) — {}",
                seen.error.as_deref().unwrap_or("이유 없음")
            ),
            Reach::Unreachable => println!(
                "    YouTube  : 물어볼 수 없었습니다 — {}",
                seen.error.as_deref().unwrap_or("이유 없음")
            ),
        }
        if seen.reach == Reach::Answered {
            if let Some(e) = &seen.error {
                println!("    부분실패 : {e}");
            }
        }

        // Evidence and inference, told apart on purpose.
        println!("    근거     : {}", evidence(&seen, &f.verdict));
        println!("    추정     : {}", assumption(&seen, encoder_running));
        if let Some(stuck) = f.stuck {
            println!("    조치     : {}", stuck.explain());
        }
        println!();
    }

    println!("=== 집계 ===");
    for (label, n) in &tally {
        println!("{label:<20} {n}");
    }

    if attention.is_empty() {
        println!("\n조치가 필요한 방송이 없습니다.");
    } else {
        println!("\n=== 조치가 필요한 방송 {}개 ===", attention.len());
        for (id, name, f) in &attention {
            println!("- [{}] {} → {} ({})", id, name, f.verdict.label(), f.reason);
        }
        println!(
            "\nSTUCK TESTING / STUCK READY 는 YouTube Studio 에서 '실시간 시작' 을 눌러야 합니다.\n\
             이 도구는 그 버튼을 누르지 않습니다 — 고객 채널에 쓰는 동작이기 때문입니다."
        );
    }

    // A verdict needing attention is not a failure of the tool, so the exit
    // code stays 0: an operator script should not treat "found problems" as
    // "could not look". Only being unable to look is a failure, and that
    // returned early above.
    std::process::ExitCode::SUCCESS
}

/// What was actually observed for this verdict.
fn evidence(seen: &louver_cloud::audit::Seen, verdict: &Verdict) -> String {
    match seen.reach {
        Reach::Refused => "YouTube 가 인증을 거부했습니다. 방송 상태에 대한 관측은 없습니다".into(),
        Reach::Unreachable => "YouTube 응답이 없습니다. 방송 상태에 대한 관측은 없습니다".into(),
        Reach::Answered if seen.not_found => "YouTube 가 이 방송 ID 를 모른다고 답했습니다 (404)".into(),
        Reach::Answered => format!(
            "liveBroadcasts.list 와 liveStreams.list 의 응답 — lifeCycleStatus={}, streamStatus={} → {}",
            if seen.life_cycle_status.is_empty() { "없음" } else { &seen.life_cycle_status },
            if seen.stream_status.is_empty() { "없음" } else { &seen.stream_status },
            verdict.label()
        ),
    }
}

/// What is inferred rather than seen — named so nobody mistakes it for a fact.
fn assumption(seen: &louver_cloud::audit::Seen, encoder_running: bool) -> String {
    let mut parts = Vec::new();
    if encoder_running {
        parts.push(
            "기록된 PID 로 프로세스가 존재하므로 인코더가 송출 중이라고 보았습니다 \
             (PID 재사용 가능성은 배제하지 못함)"
                .to_string(),
        );
    } else {
        parts.push("기록된 PID 에 프로세스가 없으므로 송출이 멈췄다고 보았습니다".to_string());
    }
    if seen.reach == Reach::Answered && seen.stream_status.is_empty() {
        parts.push("스트림 상태를 받지 못해 수신 여부는 판정하지 않았습니다".to_string());
    }
    parts.push("이 점검은 한 시점의 관측이며, 이후 YouTube 쪽 상태가 바뀌었을 수 있습니다".to_string());
    parts.join(" / ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_identifier_is_masked_but_still_matchable() {
        assert_eq!(masked("AbCdEfGhIj"), "Ab…Ij");
        assert_eq!(masked("short"), "•••••", "too short to show any of");
        assert_eq!(masked(""), "");
    }

    #[test]
    fn show_ids_is_opt_in() {
        let hidden = Options { show_ids: false, allow_token_refresh: false };
        let shown = Options { show_ids: true, allow_token_refresh: false };
        assert_eq!(show(Some("AbCdEfGhIj"), &hidden), "Ab…Ij");
        assert_eq!(show(Some("AbCdEfGhIj"), &shown), "AbCdEfGhIj");
        assert_eq!(show(None, &shown), "—");
    }

    #[test]
    fn neither_flag_is_on_by_default() {
        let o = Options::from_args(&["--youtube-audit".to_string()]);
        assert!(!o.show_ids, "customer identifiers stay hidden unless asked for");
        assert!(!o.allow_token_refresh, "a refresh can rotate a token we cannot store");
    }

    #[test]
    fn both_flags_are_recognised() {
        let o = Options::from_args(&[
            "--youtube-audit".to_string(),
            "--show-ids".to_string(),
            "--allow-token-refresh".to_string(),
        ]);
        assert!(o.show_ids);
        assert!(o.allow_token_refresh);
    }

    #[test]
    fn pid_zero_and_negative_are_never_alive() {
        assert!(!pid_alive(0));
        assert!(!pid_alive(-1));
    }

    #[test]
    fn an_unanswered_question_is_never_offered_as_evidence_about_the_broadcast() {
        let seen =
            louver_cloud::audit::Seen { reach: Reach::Unreachable, ..louver_cloud::audit::Seen::default() };
        let text = evidence(&seen, &Verdict::Unknown);
        assert!(text.contains("관측은 없습니다"), "got: {text}");
    }

    #[test]
    fn the_assumption_line_always_admits_the_observation_is_a_snapshot() {
        let seen = louver_cloud::audit::Seen::default();
        assert!(assumption(&seen, true).contains("한 시점의 관측"));
        assert!(assumption(&seen, false).contains("한 시점의 관측"));
    }

    #[test]
    fn a_live_pid_is_reported_with_its_own_caveat() {
        let seen = louver_cloud::audit::Seen::default();
        assert!(assumption(&seen, true).contains("PID 재사용"), "pid reuse is not hidden");
        assert!(!assumption(&seen, false).contains("PID 재사용"));
    }
}
