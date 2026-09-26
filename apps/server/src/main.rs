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
    if sock.write_all(b"GET /api/health HTTP/1.0\r\nHost: localhost\r\n\r\n").is_err() {
        return std::process::ExitCode::FAILURE;
    }
    let mut answer = String::new();
    let _ = sock.read_to_string(&mut answer);
    if answer.starts_with("HTTP/1.") && answer.contains(" 200 ") {
        std::process::ExitCode::SUCCESS
    } else {
        eprintln!("[louver] health: 예상과 다른 응답");
        std::process::ExitCode::FAILURE
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    if std::env::args().any(|a| a == "--health-check") {
        return health_check();
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

    let addr = std::env::var("LOUVER_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[louver] {addr} 에 바인드할 수 없습니다: {e}");
            std::process::exit(1);
        }
    };
    println!("[louver] listening on {addr}, storage={}", app.storage.backend_name());

    let service = with_web_ui(router(app.clone()));
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
