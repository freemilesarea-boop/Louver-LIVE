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

/// `--set-admin <email>` / `--drop-admin <email>` / `--list-admins`
///
/// The only way an operator is made. There is deliberately no API for it: a
/// route that can grant admin is a route that has to be perfect for ever, and
/// this service does not need one. Somebody with a shell on the server is
/// already the most privileged party there is.
///
/// Not run automatically by any deployment. The command exists; using it is a
/// decision.
fn set_admin(args: &[String]) -> std::process::ExitCode {
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

    if args.iter().any(|a| a == "--list-admins") {
        match db.admins() {
            Ok(list) if list.is_empty() => println!("관리자 계정이 없습니다."),
            Ok(list) => {
                println!("관리자 {}명:", list.len());
                for email in list {
                    println!("  {email}");
                }
            }
            Err(e) => {
                eprintln!("[louver] 관리자 목록을 읽을 수 없습니다: {e}");
                return std::process::ExitCode::FAILURE;
            }
        }
        return std::process::ExitCode::SUCCESS;
    }

    let (flag, role) = match value_of(args, "--set-admin") {
        Some(email) => (email, louver_cloud::ROLE_ADMIN),
        None => match value_of(args, "--drop-admin") {
            Some(email) => (email, louver_cloud::ROLE_USER),
            None => {
                eprintln!("사용법: louver-server --set-admin <email> | --drop-admin <email> | --list-admins");
                return std::process::ExitCode::FAILURE;
            }
        },
    };
    match db.set_role(&flag, role) {
        Ok(u) => {
            println!("[louver] {} → {}", u.email, if u.is_admin() { "관리자" } else { "일반 사용자" });
            println!("관리 콘솔: <서비스 주소>/admin");
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("[louver] 권한을 바꾸지 못했습니다: {e}");
            std::process::ExitCode::FAILURE
        }
    }
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

/// `--audit-media-storage [--json]`
///
/// Every file in the object store, what points at it, and whether anything is
/// reading it right now.
///
/// **It deletes nothing, and there is no flag that makes it delete anything.**
/// That is not an oversight. The files it lists are hours of somebody's video
/// and the cost of being wrong is a broadcast that dies at the next restart, so
/// the command's whole job is to let a person look. Removing a file it calls
/// safe is a separate decision, made by a person, with `rm`.
///
/// A file is called safe only when no media row names it, no manifest under the
/// work directory names it, no process holds it open, **and** open files could
/// be listed at all. Anything unknown reads as not safe.
fn audit_media_storage(args: &[String]) -> std::process::ExitCode {
    use louver_cloud::media_audit::human;

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
    let report = match louver_cloud::media_audit::audit(&db, &data) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[louver] 저장소를 읽지 못했습니다: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    if args.iter().any(|a| a == "--json") {
        match serde_json::to_string_pretty(&report) {
            Ok(j) => println!("{j}"),
            Err(e) => {
                eprintln!("[louver] {e}");
                return std::process::ExitCode::FAILURE;
            }
        }
        return std::process::ExitCode::SUCCESS;
    }

    println!("미디어 저장소: {}", report.media_root);
    let v = &report.volume;
    if v.known {
        println!();
        println!("물리 디스크");
        println!("  전체        {}", human(v.total_bytes));
        println!("  사용 중     {}", human(v.used_bytes));
        println!("  여유        {}", human(v.free_bytes));
        println!("  최소 확보   {}  (이 아래로는 업로드와 변환을 거부합니다)", human(v.floor_bytes));
        println!("  쓸 수 있음  {}", human(v.usable_bytes));
        if v.usable_bytes == 0 {
            println!("  ⚠ 여유가 최소 확보선 아래입니다. 새 업로드와 변환이 거부됩니다.");
        }
    } else {
        println!();
        println!("물리 디스크: 알 수 없음 (볼륨을 식별하지 못했습니다)");
    }
    if !report.open_files_known {
        println!();
        println!("  ⚠ 열려 있는 파일 목록을 읽지 못했습니다. 어떤 파일도 삭제 가능으로 판정하지 않습니다.");
    }
    println!();
    println!("  {:<10} {:>10}  경로", "역할", "크기");
    println!("  {}", "-".repeat(76));
    for o in &report.objects {
        let marks = format!(
            "{}{}{}",
            if o.referenced_by_db { "" } else { " db✗" },
            if o.in_manifest { " manifest" } else { "" },
            if o.open_now { " OPEN" } else { "" },
        );
        println!(
            "  {:<10} {:>10}  {}{}{}",
            o.role.label(),
            human(o.bytes),
            o.key,
            if marks.is_empty() { String::new() } else { format!("  ({})", marks.trim()) },
            if o.safe_to_delete { "  [정리 가능]" } else { "" },
        );
        if let (Some(f), Some(id)) = (&o.filename, &o.media_id) {
            println!("  {:<22}└ {f} ({})", "", &id[..8.min(id.len())]);
        }
    }

    println!();
    println!("합계");
    println!("  파일 {}개, {}", report.totals.files, human(report.totals.bytes));
    println!("  원본        {}", human(report.totals.source_bytes));
    println!("  변환본      {}", human(report.totals.prepared_bytes));
    println!("  교체된 변환본 {}개, {}", report.totals.retired_files, human(report.totals.retired_bytes));
    println!("  미참조      {}개, {}", report.totals.orphan_files, human(report.totals.orphan_bytes));
    println!("  작업 중(.scratch) {}", human(report.scratch_bytes));
    if !report.manifests.is_empty() {
        let names: Vec<String> =
            report.manifests.iter().map(|(b, n)| format!("{}({}개)", &b[..8.min(b.len())], n)).collect();
        println!("  실행 중 manifest: {}", names.join(", "));
    }

    if !report.accounting_drift.is_empty() {
        println!();
        println!("DB 값과 실제 파일 크기가 다른 항목");
        for d in &report.accounting_drift {
            println!(
                "  {} {}  DB {} / 디스크 {}",
                &d.media_id[..8.min(d.media_id.len())],
                d.filename,
                human(d.db_bytes.max(0) as u64),
                human(d.disk_bytes.max(0) as u64),
            );
        }
    }

    println!();
    if report.totals.reclaimable_files == 0 {
        println!("정리 가능한 파일이 없습니다.");
    } else {
        println!(
            "정리 가능 후보: {}개, {} — 아래 파일은 DB가 참조하지 않고, manifest 에도 없고, 열려 있지도 않습니다.",
            report.totals.reclaimable_files,
            human(report.totals.reclaimable_bytes),
        );
        for o in report.objects.iter().filter(|o| o.safe_to_delete) {
            println!("  {}/{}", report.media_root, o.key);
        }
        println!();
        println!("이 명령은 아무것도 지우지 않습니다. 삭제는 위 목록을 직접 확인한 뒤 사람이 결정합니다.");
        println!("방송이 하나라도 돌고 있는 동안에는, 그 방송이 끝난 뒤 다시 실행해서 재확인하세요.");
    }
    std::process::ExitCode::SUCCESS
}

/// `--media-check <file> [--run] [--compare]`
///
/// Why a file was prepared the way it was, and what that cost.
///
/// Written in Rust and shipped in the server binary on purpose: the production
/// image has no Node, and the moment somebody needs this is the moment a user
/// is asking why their upload took three hours. Without `--run` it only probes
/// — safe on a live server, milliseconds, touches nothing.
///
/// `--run` prepares the file into a temporary directory and reports what it
/// cost. `--compare` does it twice, once the way this release does it and once
/// the way every upload used to be done, which is the measurement behind the
/// change. Use `--compare` on a short clip: the old way is the slow way.
fn media_check(args: &[String]) -> std::process::ExitCode {
    use louver_core::media::normalize::{normalize_one_with, plan_preparation, CancelToken};
    use louver_core::media::probe::probe;
    use louver_core::streaming::ffmpeg::{FfmpegCommandBuilder, FfmpegTools};

    let Some(path) = value_of(args, "--media-check") else {
        eprintln!("사용법: louver-server --media-check <파일> [--run] [--compare]");
        return std::process::ExitCode::FAILURE;
    };
    let path = std::path::PathBuf::from(path);
    let tools = match FfmpegTools::discover(
        std::env::var("LOUVER_FFMPEG_DIR").ok().map(std::path::PathBuf::from).as_deref(),
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[louver] FFmpeg를 찾지 못했습니다: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let profile = louver_cloud::ingest::CLOUD_PROFILE;
    let encoder = std::env::var("LOUVER_ENCODER").unwrap_or_else(|_| "libx264".into());
    let builder = FfmpegCommandBuilder::new(tools, profile).with_encoder(encoder.clone());

    let info = match probe(&builder, &path) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("[louver] 파일을 읽을 수 없습니다: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("=== 247streams 미디어 점검 ===");
    println!("파일     : {}", path.display());
    println!(
        "영상     : {} {}x{} @{:.2}fps {} profile={} level={}",
        info.video_codec,
        info.width,
        info.height,
        info.fps,
        info.pixel_format,
        if info.video_profile.is_empty() { "?" } else { &info.video_profile },
        info.video_level.map(|l| format!("{}.{}", l / 10, l % 10)).unwrap_or_else(|| "?".into()),
    );
    println!(
        "소리     : {} {}Hz {}ch",
        info.audio_codec.clone().unwrap_or_else(|| "없음".into()),
        info.audio_sample_rate.unwrap_or(0),
        info.audio_channels.unwrap_or(0),
    );
    println!("길이     : {:.1}초 ({:.2}시간)", info.duration_secs, info.duration_secs / 3600.0);

    let native = plan_preparation(&builder, &path, &info, profile, false);
    let canonical = plan_preparation(&builder, &path, &info, profile, true);
    let say = |name: &str, prep: &louver_core::media::normalize::Preparation| {
        println!("\n{name}: {}", prep.summary());
        if let Some(g) = prep.measured_gop_secs {
            println!("    키프레임 간격: {g:.1}초 (한계 {:.1}초)", profile.max_copy_gop_secs());
        }
        if let Some(v) = &prep.video {
            println!(
                "    영상 재인코딩: {}x{} @{:.2}fps 유지, 키프레임 {:.1}초마다, {}kbps",
                info.width, info.height, info.fps, v.keyframe_secs, v.kbps,
            );
        }
        for r in prep.plan.video_reasons.iter().chain(prep.plan.audio_reasons.iter()) {
            println!("    reason: {r}");
        }
    };
    say("이 버전", &native);
    if args.iter().any(|a| a == "--compare") {
        say("이전 버전(항상 1080p30)", &canonical);
    }

    if !args.iter().any(|a| a == "--run") {
        println!("\n실제로 준비해 보려면 --run 을 붙이세요 (임시 폴더에 만들고 지웁니다).");
        return std::process::ExitCode::SUCCESS;
    }

    // A scratch directory of our own rather than a crate: this runs in the
    // production image, and a diagnostic is not a reason to add a dependency.
    let dir = std::env::temp_dir().join(format!("louver-media-check-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("[louver] 임시 폴더를 만들 수 없습니다: {e}");
        return std::process::ExitCode::FAILURE;
    }
    let mut runs = vec![("이 버전", native)];
    if args.iter().any(|a| a == "--compare") {
        runs.push(("이전 버전", canonical));
    }
    let summarise = |prep: &louver_core::media::normalize::Preparation| {
        (
            if prep.plan.video.is_copy() { "copy" } else { "encode" },
            if prep.plan.audio.is_copy() { "copy" } else { "encode" },
        )
    };
    println!(
        "\n{:<12} {:>9} {:>9} {:>10} {:>8} {:>8}  결과",
        "", "걸린시간", "배속", "출력크기", "영상", "소리"
    );
    for (name, prep) in runs {
        let cache = louver_core::media::cache::MediaCache::new(dir.join(name));
        let started = std::time::Instant::now();
        let out = normalize_one_with(
            &builder,
            &cache,
            &path,
            "media-check",
            &info,
            profile,
            Some(&prep),
            &CancelToken::new(),
            |_| {},
        );
        let (v, au) = summarise(&prep);
        match out {
            Ok(o) => {
                let shape = louver_core::media::probe::probe(&builder, &o.output_path).ok();
                let gop = louver_core::media::probe::probe_max_keyframe_gap(&builder, &o.output_path, 60);
                println!(
                    "{:<12} {:>8.1}s {:>8.1}x {:>10} {:>8} {:>8}  {}",
                    name,
                    started.elapsed().as_secs_f64(),
                    o.speed_x,
                    louver_core::system::format_bytes(o.bytes),
                    v,
                    au,
                    match shape {
                        Some(s) => format!(
                            "{}x{}@{:.2}fps gop={} {}Hz",
                            s.width,
                            s.height,
                            s.fps,
                            gop.map(|g| format!("{g:.1}s")).unwrap_or_else(|| "?".into()),
                            s.audio_sample_rate.unwrap_or(0),
                        ),
                        None => String::new(),
                    },
                );
            }
            Err(e) => println!("{name:<12} 실패: {e}"),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    std::process::ExitCode::SUCCESS
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
    // Who may open the admin console.
    if args.iter().any(|a| a == "--set-admin" || a == "--drop-admin" || a == "--list-admins") {
        return set_admin(&args);
    }
    // Why one file was prepared the way it was, and what it cost.
    if args.iter().any(|a| a == "--media-check") {
        return media_check(&args);
    }
    // What the storage ceilings are, and what this build would set them to.
    if args.iter().any(|a| a == "--audit-storage") {
        return audit_storage(&args);
    }
    // What is actually on the disk, and what still points at it.
    if args.iter().any(|a| a == "--audit-media-storage") {
        return audit_media_storage(&args);
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
