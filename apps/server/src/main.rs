//! The process. Everything it does lives in the library beside it, so the
//! router can be driven by a test without a socket.

use louver_server::state::App;
use louver_server::{router, with_web_ui};

/// `--health-check`: is this container's server answering?
///
/// A plain TCP request rather than a curl in the image, because the smallest
/// runtime image has no HTTP client and adding one to run a healthcheck is a
/// larger attack surface than writing eleven lines.
fn health_check() -> std::process::ExitCode {
    use std::io::{Read, Write};
    let bind = std::env::var("LOUVER_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let port = bind.rsplit(':').next().unwrap_or("8080").to_string();
    let Ok(mut sock) = std::net::TcpStream::connect(format!("127.0.0.1:{port}")) else {
        eprintln!("[louver] health: {port} 포트에 연결할 수 없습니다");
        return std::process::ExitCode::FAILURE;
    };
    let _ = sock.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    if sock.write_all(b"GET /health HTTP/1.0\r\nHost: localhost\r\n\r\n").is_err() {
        return std::process::ExitCode::FAILURE;
    }
    let mut answer = String::new();
    let _ = sock.read_to_string(&mut answer);
    // The body, so an operator running this by hand — or a deploy script — sees
    // which check failed rather than only that something did.
    if let Some(body) = answer.split("\r\n\r\n").nth(1) {
        println!("{}", body.trim());
    }
    if answer.starts_with("HTTP/1.") && answer.contains(" 200 ") {
        std::process::ExitCode::SUCCESS
    } else {
        eprintln!("[louver] health: 예상과 다른 응답");
        std::process::ExitCode::FAILURE
    }
}

/// `--create-user <email> [--plan <id>]`: the way an account comes into being
/// on a fresh install.
///
/// The password is never an argument. Arguments are visible in `ps` and in a
/// shell history file, so it comes from `LOUVER_BOOTSTRAP_PASSWORD` or from
/// stdin, and there is no default: an install with a password this code chose
/// would be an install every reader of this repository can sign into.
fn create_user(args: &[String]) -> std::process::ExitCode {
    let email = match value_of(args, "--create-user") {
        Some(e) => e,
        None => {
            eprintln!("사용법: louver-server --create-user <email> [--plan <plan-id>]");
            return std::process::ExitCode::FAILURE;
        }
    };
    // `LOUVER_DEFAULT_PLAN` used to be what a public signup got. Signups are
    // unsubscribed now, so this is where it still means something: the plan an
    // operator's account gets when `--plan` is not given.
    let plan = value_of(args, "--plan")
        .or_else(|| std::env::var("LOUVER_DEFAULT_PLAN").ok().filter(|p| !p.trim().is_empty()))
        .unwrap_or_else(|| "basic".into());

    let data = std::path::PathBuf::from(
        std::env::var("LOUVER_DATA_DIR").unwrap_or_else(|_| "/var/lib/louver".into()),
    );
    let db = match louver_cloud::CloudDb::open(&data.join("cloud.db")) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("[louver] 데이터베이스를 열 수 없습니다 ({}): {e}", data.display());
            return std::process::ExitCode::FAILURE;
        }
    };

    let known = db.plan_ids().unwrap_or_default();
    if !known.iter().any(|p| p == &plan) {
        eprintln!("[louver] '{plan}' 요금제가 없습니다. 사용 가능: {}", known.join(", "));
        return std::process::ExitCode::FAILURE;
    }

    // An account that already exists has its plan changed rather than being
    // refused: re-running the bootstrap to grant Business is the common case.
    if let Ok(existing) = db.user_by_email(&email) {
        match db.set_plan(&existing.id, &plan) {
            Ok(()) => {
                println!("[louver] 기존 계정 {} 의 요금제를 {plan} 으로 변경했습니다", existing.email);
                return std::process::ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("[louver] 요금제를 변경할 수 없습니다: {e}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }

    let password = match read_password() {
        Some(p) => p,
        None => return std::process::ExitCode::FAILURE,
    };
    if password.chars().count() < 10 {
        eprintln!("[louver] 비밀번호는 10자 이상이어야 합니다");
        return std::process::ExitCode::FAILURE;
    }

    let hash = match louver_cloud::credentials::hash_password(&password) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[louver] 비밀번호를 저장할 수 없습니다: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match db.create_user(email.trim().to_lowercase().as_str(), &hash, &plan) {
        Ok(u) => {
            println!("[louver] 계정을 만들었습니다: {} (요금제 {plan})", u.email);
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("[louver] 계정을 만들 수 없습니다: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// `--audit-plans [--revoke-unpaid-grants --yes]`
///
/// Answers a question a production operator actually had: why does a brand-new
/// account show Basic?
///
/// The release before paid plans put every public signup on
/// `LOUVER_DEFAULT_PLAN`, which was Basic. The current code cannot do that —
/// `register_user` has no plan argument — but the rows it already wrote are still
/// there, and they are indistinguishable from a paid Basic except by how the
/// account was made: only the browser's signup form records consent, so an
/// account with a consent timestamp and a paid plan is an entitlement nobody
/// asked for and nobody paid for.
///
/// Read-only unless both `--revoke-unpaid-grants` and `--yes` are given. Taking
/// an entitlement away from somebody who is using it is not something to do
/// without looking at the list first.
fn audit_plans(args: &[String]) -> std::process::ExitCode {
    let data = std::path::PathBuf::from(
        std::env::var("LOUVER_DATA_DIR").unwrap_or_else(|_| "/var/lib/louver".into()),
    );
    let db = match louver_cloud::CloudDb::open(&data.join("cloud.db")) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("[louver] 데이터베이스를 열 수 없습니다 ({}): {e}", data.display());
            return std::process::ExitCode::FAILURE;
        }
    };
    let rows = match db.plan_audit() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[louver] 계정 목록을 읽을 수 없습니다: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let none = louver_cloud::db::UNSUBSCRIBED_PLAN;
    // No width on the Korean column: those glyphs are double-width in a terminal,
    // so `{:<9}` counts nine characters and draws eighteen columns, and
    // everything after it lands somewhere different on every row.
    println!("{:<34} {:<10} {:<13} 가입경로", "email", "plan", "status");
    for r in &rows {
        println!(
            "{:<34} {:<10} {:<13} {}{}",
            r.email,
            r.plan_id,
            r.status,
            if r.from_public_signup { "회원가입" } else { "관리자" },
            if r.is_unpaid_grant(none) { "   ← 자동 부여" } else { "" }
        );
    }

    let flagged: Vec<&louver_cloud::PlanAudit> = rows.iter().filter(|r| r.is_unpaid_grant(none)).collect();
    println!("\n계정 {}개, 자동 부여로 보이는 계정 {}개", rows.len(), flagged.len());
    if flagged.is_empty() {
        println!("조치할 것이 없습니다.");
        return std::process::ExitCode::SUCCESS;
    }

    let revoke = args.iter().any(|a| a == "--revoke-unpaid-grants");
    let confirmed = args.iter().any(|a| a == "--yes");
    if !revoke {
        println!("위 계정을 미구독으로 되돌리려면:");
        println!("  louver-server --audit-plans --revoke-unpaid-grants --yes");
        return std::process::ExitCode::SUCCESS;
    }
    if !confirmed {
        // Naming the count rather than just asking: the operator should see how
        // many people are about to lose access before they type --yes.
        eprintln!(
            "[louver] {}개 계정의 요금제를 회수합니다. 확인했다면 --yes 를 함께 주세요.",
            flagged.len()
        );
        return std::process::ExitCode::FAILURE;
    }

    let mut done = 0;
    for r in &flagged {
        match db.revoke_unpaid_grant(&r.user_id) {
            Ok(()) => {
                done += 1;
                println!("[louver] {} → 미구독", r.email);
            }
            Err(e) => eprintln!("[louver] {} 회수 실패: {e}", r.email),
        }
    }
    println!("[louver] {done}/{}개 계정을 미구독으로 되돌렸습니다", flagged.len());
    std::process::ExitCode::SUCCESS
}

/// `--audit-billing [--fix --yes]`
///
/// The mirror image of `--audit-plans`, for the other way an entitlement can be
/// wrong: the billing record says the recurring payment was cancelled, but the
/// account is still on the plan it paid for.
///
/// Rows like this exist because cancellation used to keep the entitlement until
/// the end of the paid period. That policy is gone, and the code no longer
/// writes such a row — but the rows written before it changed are still in the
/// production database, and the accounts they belong to still hold a plan they
/// have cancelled.
///
/// This is deliberately *not* a boot migration. A migration that moved accounts
/// between plans on every start would be a migration nobody reads the output of,
/// running against a database with paying customers in it. So: read-only unless
/// both `--fix` and `--yes` are given, and even then it only ever touches the
/// accounts printed above — the query finds cancelled-but-still-entitled, never
/// "everyone on Basic".
fn audit_billing(args: &[String]) -> std::process::ExitCode {
    let data = std::path::PathBuf::from(
        std::env::var("LOUVER_DATA_DIR").unwrap_or_else(|_| "/var/lib/louver".into()),
    );
    let db = match louver_cloud::CloudDb::open(&data.join("cloud.db")) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("[louver] 데이터베이스를 열 수 없습니다 ({}): {e}", data.display());
            return std::process::ExitCode::FAILURE;
        }
    };
    let rows = match db.billing_mismatches() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[louver] 결제 기록을 읽을 수 없습니다: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    if rows.is_empty() {
        println!("해지됐는데도 유료 권한이 남아 있는 계정은 없습니다.");
        return std::process::ExitCode::SUCCESS;
    }
    println!("{:<34} {:<10} {:<22} 해지 시각", "email", "plan", "billing status");
    for r in &rows {
        println!(
            "{:<34} {:<10} {:<22} {}",
            r.email,
            r.plan_id,
            r.billing_status,
            r.cancelled_at.as_deref().unwrap_or("-")
        );
    }
    println!("\n{}개 계정이 해지 상태인데 아직 유료 권한을 갖고 있습니다", rows.len());

    let fix = args.iter().any(|a| a == "--fix");
    let confirmed = args.iter().any(|a| a == "--yes");
    if !fix {
        println!("위 계정의 권한을 회수하려면:");
        println!("  louver-server --audit-billing --fix --yes");
        return std::process::ExitCode::SUCCESS;
    }
    if !confirmed {
        eprintln!(
            "[louver] {}개 계정의 유료 권한을 회수합니다. 확인했다면 --yes 를 함께 주세요.",
            rows.len()
        );
        return std::process::ExitCode::FAILURE;
    }

    let mut done = 0;
    for r in &rows {
        // Each row is fixed by its own billing id, and `fix_billing_mismatch`
        // re-checks that the row is still a mismatch before it writes. Nothing
        // here can touch an account that is not in the list above.
        match db.fix_billing_mismatch(&r.billing_id) {
            Ok(_) => {
                done += 1;
                println!("[louver] {} → 미구독", r.email);
            }
            Err(e) => eprintln!("[louver] {} 회수 실패: {e}", r.email),
        }
    }
    println!("[louver] {done}/{}개 계정의 유료 권한을 회수했습니다", rows.len());
    std::process::ExitCode::SUCCESS
}

/// `--audit-storage [--apply --yes]`
///
/// The storage ceilings that ship in this build, against the ones the database
/// is actually using, plus who is storing how much.
///
/// It exists because `seed_plans` leaves a plan's `limits` alone on conflict —
/// an operator may have raised one for a customer and a restart must not undo
/// that — so editing `SEED_PLANS` changes nothing for a database whose rows are
/// already there. Lowering a ceiling can put an existing account over it, so
/// somebody has to read the list first and then say so.
///
/// `--apply` writes only `max_storage_bytes` and `max_upload_bytes`. The price
/// column is not in the statement; `max_concurrent_streams` and `max_broadcasts`
/// are read back from the row and written unchanged. **No file is ever deleted**
/// and no account is put off air: an account over the new ceiling keeps
/// everything it has, keeps broadcasting, and is refused its next upload.
fn audit_storage(args: &[String]) -> std::process::ExitCode {
    let data = std::path::PathBuf::from(
        std::env::var("LOUVER_DATA_DIR").unwrap_or_else(|_| "/var/lib/louver".into()),
    );
    let db = match louver_cloud::CloudDb::open(&data.join("cloud.db")) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("[louver] 데이터베이스를 열 수 없습니다 ({}): {e}", data.display());
            return std::process::ExitCode::FAILURE;
        }
    };
    let plans = match db.storage_audit() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[louver] 요금제를 읽을 수 없습니다: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let gb = |b: i64| format!("{:.0}GB", b as f64 / 1_073_741_824.0);

    println!(
        "{:<10} {:>9} {:>6}  {:>16}  {:>16}",
        "plan", "월요금", "동시", "저장(현재→변경)", "한파일(현재→변경)"
    );
    let mut pending = 0;
    for p in &plans {
        let moves = p.storage_now != p.storage_target || p.upload_now != p.upload_target;
        if moves {
            pending += 1;
        }
        println!(
            "{:<10} {:>9} {:>6}  {:>7} → {:<6}  {:>7} → {:<6}{}",
            p.plan_id,
            p.monthly_price_krw,
            p.concurrent_streams,
            gb(p.storage_now),
            gb(p.storage_target),
            gb(p.upload_now),
            gb(p.upload_target),
            if moves { "  ← 변경됨" } else { "" }
        );
    }
    println!("\n월요금과 동시 송출 수는 이 명령으로 바뀌지 않습니다.");

    match db.storage_usage() {
        Ok(rows) => {
            let over: Vec<_> = rows.iter().filter(|r| r.over()).collect();
            println!("\n계정 {}개, 변경 후 한도를 넘는 계정 {}개", rows.len(), over.len());
            for r in rows.iter().filter(|r| r.used_bytes > 0) {
                println!(
                    "  {:<34} {:<10} 사용 {:>7} / 한도 {:<7}{}",
                    r.email,
                    r.plan_id,
                    gb(r.used_bytes),
                    gb(r.ceiling_after),
                    if r.over() { "  ← 추가 업로드만 차단됨 (파일은 그대로)" } else { "" }
                );
            }
            if !over.is_empty() {
                println!("\n한도를 넘는 계정의 파일은 삭제되지 않습니다. 방송도 계속됩니다.");
                println!("추가 업로드만 거부되며, 영상을 지우면 다시 올릴 수 있습니다.");
            }
        }
        Err(e) => eprintln!("[louver] 사용량을 읽을 수 없습니다: {e}"),
    }

    if pending == 0 {
        println!("\n요금제 한도는 이미 이 버전과 같습니다. 조치할 것이 없습니다.");
        return std::process::ExitCode::SUCCESS;
    }
    let apply = args.iter().any(|a| a == "--apply");
    let confirmed = args.iter().any(|a| a == "--yes");
    if !apply {
        println!("\n위 한도를 적용하려면:");
        println!("  louver-server --audit-storage --apply --yes");
        return std::process::ExitCode::SUCCESS;
    }
    if !confirmed {
        eprintln!("[louver] 요금제 {pending}개의 저장 한도를 변경합니다. 확인했다면 --yes 를 함께 주세요.");
        return std::process::ExitCode::FAILURE;
    }
    match db.apply_seed_storage_limits() {
        Ok(changed) => {
            println!(
                "[louver] 요금제 {}개의 저장 한도를 적용했습니다: {}",
                changed.len(),
                changed.join(", ")
            );
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("[louver] 한도를 적용하지 못했습니다: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn value_of(args: &[String], flag: &str) -> Option<String> {
    let i = args.iter().position(|a| a == flag)?;
    args.get(i + 1).filter(|v| !v.starts_with("--")).cloned()
}

/// The environment first, then stdin. Never an argument.
fn read_password() -> Option<String> {
    if let Ok(p) = std::env::var("LOUVER_BOOTSTRAP_PASSWORD") {
        if !p.trim().is_empty() {
            return Some(p);
        }
    }
    eprint!("비밀번호(10자 이상, 화면에 표시됩니다): ");
    let _ = std::io::Write::flush(&mut std::io::stderr());
    let mut line = String::new();
    match std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line) {
        Ok(0) | Err(_) => {
            eprintln!("\n[louver] 비밀번호를 읽지 못했습니다. LOUVER_BOOTSTRAP_PASSWORD 를 사용하세요.");
            None
        }
        Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_string()),
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--health-check") {
        return health_check();
    }
    if args.iter().any(|a| a == "--create-user") {
        return create_user(&args);
    }
    // Who is on which plan, and how they got there.
    if args.iter().any(|a| a == "--audit-plans") {
        return audit_plans(&args);
    }
    // What the storage ceilings are, and what this build would set them to.
    if args.iter().any(|a| a == "--audit-storage") {
        return audit_storage(&args);
    }
    // Cancelled at the provider, but still holding the plan here.
    if args.iter().any(|a| a == "--audit-billing") {
        return audit_billing(&args);
    }
    // What is running, where it is sending, and what FFmpeg has said about it.
    if args.iter().any(|a| a == "--diagnose") {
        return louver_server::diagnose::run();
    }

    let app = match App::boot() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[louver] 서버를 시작할 수 없습니다: {e}");
            std::process::exit(1);
        }
    };

    // §6: whatever was meant to be running, runs again. A broadcast the user
    // stopped stays stopped, because the decision is read from desired_state.
    match app.mgr.recover_all() {
        Ok(n) if n > 0 => println!("[louver] 방송 {n}개를 복구했습니다"),
        Ok(_) => println!("[louver] 복구할 방송이 없습니다"),
        Err(e) => eprintln!("[louver] 복구 실패: {e}"),
    }

    // §8: the clock is watched by the server, not by a browser. It holds no
    // state, so it needs nothing from recovery.
    let _scheduler = app.mgr.spawn_scheduler();

    let addr = std::env::var("LOUVER_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[louver] {addr} 에 바인드할 수 없습니다: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "[louver] listening on {addr} ({}), storage={}",
        match louver_server::health::deployment().as_str() {
            "cloud" => "REMOTE CLOUD SERVER",
            _ => "LOCAL DEVELOPMENT — 이 컴퓨터를 끄면 방송도 끝납니다",
        },
        app.storage.backend_name()
    );

    // `with_connect_info` so the login throttle can tell one caller from
    // another when there is no proxy in front to say.
    let service =
        with_web_ui(router(app.clone())).into_make_service_with_connect_info::<std::net::SocketAddr>();
    let shutdown = async move {
        let _ = tokio::signal::ctrl_c().await;
        println!("[louver] 종료 신호를 받았습니다. 방송을 정리합니다");
    };
    if let Err(e) = axum::serve(listener, service).with_graceful_shutdown(shutdown).await {
        eprintln!("[louver] server error: {e}");
    }

    // Ask every worker to finish. `desired_state` is untouched, so the next boot
    // brings these same broadcasts back.
    app.mgr.shutdown();
    std::process::ExitCode::SUCCESS
}
