//! The control channel between the API and the process that owns FFmpeg.
//!
//! # Why there is one at all
//!
//! FFmpeg is a direct child of whichever process called
//! `StreamSupervisor::spawn`, its stdout carries `-progress pipe:1` and its
//! stderr is read by two threads in that same process. A "worker" is an OS
//! thread there, holding the `ProcessHandle`. So replacing the binary that
//! serves HTTP replaces the parent of every live encoder, and every broadcast
//! stops. No amount of `setsid`, `nohup` or `disown` changes that: even a
//! detached FFmpeg would be writing progress into a pipe whose reader is gone —
//! it would die on EPIPE once the buffer filled, and in the meantime nothing
//! would be reading its output or holding a handle to stop it later.
//!
//! The only structure that survives an API deployment is one where the process
//! that spawns FFmpeg is not the process being deployed. This module is the
//! seam: the API asks, the runner does.
//!
//! # What it is deliberately not
//!
//! Not a network service. A Unix socket in the data directory, which both
//! containers already share as a volume, with 0600 permissions — the
//! filesystem is the authentication, and there is no port to expose or
//! firewall. Not a serialisation format either: four verbs, one line each, so
//! the whole protocol can be read in one screen and exercised with `socat`.
//!
//! # What happens when the runner is not there
//!
//! Every call fails with a sentence naming the socket. That is the honest
//! answer and it is also the safe one: a start that cannot reach the runner
//! must not report success, and nothing here can stop a broadcast that is
//! already running — the runner holds those, and it is the one that is absent.

use crate::{CloudError, Result};
#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};

/// The socket's name inside the data directory.
pub const SOCKET_FILE: &str = "runner.sock";

/// How long a client waits for the runner to answer.
///
/// Generous, because `HALT` legitimately takes seconds: it waits for FFmpeg to
/// flush its FLV trailer. Still bounded, so a wedged runner becomes an error
/// in the dashboard rather than a request that never returns.
#[cfg(unix)]
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Which part of the system this process is.
///
/// `All` is the default and is exactly what shipped before any of this: one
/// process serving HTTP and owning FFmpeg. The split is opt-in, so a
/// deployment that sets nothing behaves as it always has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Role {
    /// HTTP and FFmpeg in one process.
    #[default]
    All,
    /// HTTP only. Asks a runner to start and stop broadcasts.
    Api,
    /// FFmpeg only. Owns the workers, the scheduler and recovery.
    Runner,
}

impl Role {
    /// Read from `LOUVER_ROLE`. Anything unrecognised is [`Role::All`], because
    /// a typo must not silently produce a server that broadcasts nothing.
    pub fn from_env() -> Self {
        match std::env::var("LOUVER_ROLE").unwrap_or_default().trim().to_ascii_lowercase().as_str() {
            "api" => Self::Api,
            "runner" | "worker" => Self::Runner,
            _ => Self::All,
        }
    }

    /// Does this process spawn FFmpeg itself?
    pub fn owns_workers(self) -> bool {
        matches!(self, Self::All | Self::Runner)
    }

    /// Does this process serve HTTP?
    pub fn serves_http(self) -> bool {
        matches!(self, Self::All | Self::Api)
    }

    /// Does this process take the one-writer lease?
    ///
    /// Whoever owns the workers. Two APIs are harmless — they spawn nothing —
    /// and making them contend for the lease would mean a rolling API
    /// deployment could not overlap even for a moment.
    pub fn takes_the_lease(self) -> bool {
        self.owns_workers()
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Api => "api",
            Self::Runner => "runner",
        }
    }
}

/// One request. Four verbs is the whole protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Spawn this broadcast's worker. The API has already validated and
    /// recorded the intent; this is the part it cannot do itself.
    Start(String),
    /// Stop this broadcast's worker and wait for it to finish.
    ///
    /// Synchronous on purpose: the caller goes on to end the YouTube broadcast,
    /// and doing that while FFmpeg is still pushing would have the encoder
    /// sending into a completed broadcast.
    Halt(String),
    /// Which broadcasts have a worker right now.
    Running,
    /// Is the runner there, and which build is it?
    Ping,
}

impl Command {
    /// The wire form: one line, verb then optional id.
    pub fn encode(&self) -> String {
        match self {
            Self::Start(id) => format!("START {id}"),
            Self::Halt(id) => format!("HALT {id}"),
            Self::Running => "RUNNING".into(),
            Self::Ping => "PING".into(),
        }
    }

    /// Parse a line. A broadcast id is hex from `create_broadcast`, so a
    /// command carrying anything else is rejected rather than passed along —
    /// the id reaches SQL and a filesystem path on the other side.
    pub fn decode(line: &str) -> Result<Self> {
        let line = line.trim();
        let (verb, rest) = match line.split_once(' ') {
            Some((v, r)) => (v, r.trim()),
            None => (line, ""),
        };
        let id = || -> Result<String> {
            if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(CloudError::Invalid(format!("방송 ID 가 올바르지 않습니다: {rest:?}")));
            }
            Ok(rest.to_string())
        };
        match verb.to_ascii_uppercase().as_str() {
            "START" => Ok(Self::Start(id()?)),
            "HALT" => Ok(Self::Halt(id()?)),
            "RUNNING" => Ok(Self::Running),
            "PING" => Ok(Self::Ping),
            other => Err(CloudError::Invalid(format!("알 수 없는 명령: {other}"))),
        }
    }
}

/// One reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Ok,
    /// A list of broadcast ids, for `RUNNING`.
    Ids(Vec<String>),
    /// A sentence for a human, kept whole: the API puts it in front of the
    /// user, so losing it would turn a specific refusal into "실패".
    Err(String),
}

impl Reply {
    pub fn encode(&self) -> String {
        match self {
            Self::Ok => "OK".into(),
            Self::Ids(ids) => format!("IDS {}", ids.join(",")),
            // Newlines would end the line and make the rest look like another
            // reply, so they are flattened rather than trusted.
            Self::Err(m) => format!("ERR {}", m.replace(['\n', '\r'], " ")),
        }
    }

    pub fn decode(line: &str) -> Self {
        let line = line.trim();
        if line == "OK" {
            return Self::Ok;
        }
        if let Some(rest) = line.strip_prefix("IDS") {
            let rest = rest.trim();
            if rest.is_empty() {
                return Self::Ids(Vec::new());
            }
            return Self::Ids(rest.split(',').map(|s| s.trim().to_string()).collect());
        }
        if let Some(rest) = line.strip_prefix("ERR ") {
            return Self::Err(rest.to_string());
        }
        Self::Err(format!("runner 의 응답을 이해할 수 없습니다: {line:?}"))
    }
}

/// The API's end of the channel.
#[derive(Debug, Clone)]
pub struct Client {
    socket: PathBuf,
}

impl Client {
    pub fn new(data_dir: &Path) -> Self {
        Self { socket: data_dir.join(SOCKET_FILE) }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Send one command and wait for its reply.
    #[cfg(unix)]
    pub fn send(&self, cmd: &Command) -> Result<Reply> {
        let stream = UnixStream::connect(&self.socket).map_err(|e| {
            CloudError::Invalid(format!(
                "방송 실행 서비스(runner)에 연결할 수 없습니다: {} ({e}). \
                 이미 송출 중인 방송은 영향을 받지 않습니다.",
                self.socket.display()
            ))
        })?;
        stream.set_read_timeout(Some(TIMEOUT)).ok();
        stream.set_write_timeout(Some(TIMEOUT)).ok();

        let mut writer = &stream;
        writeln!(writer, "{}", cmd.encode())
            .map_err(|e| CloudError::Invalid(format!("runner 에 명령을 보낼 수 없습니다: {e}")))?;
        writer.flush().ok();

        let mut line = String::new();
        BufReader::new(&stream)
            .read_line(&mut line)
            .map_err(|e| CloudError::Invalid(format!("runner 의 응답을 읽을 수 없습니다: {e}")))?;
        if line.trim().is_empty() {
            return Err(CloudError::Invalid("runner 가 응답 없이 연결을 닫았습니다".into()));
        }
        Ok(Reply::decode(&line))
    }

    #[cfg(not(unix))]
    pub fn send(&self, _cmd: &Command) -> Result<Reply> {
        Err(CloudError::Invalid("runner 분리는 Unix 에서만 지원됩니다".into()))
    }

    /// `send`, with an `ERR` reply turned into an error.
    pub fn call(&self, cmd: &Command) -> Result<Reply> {
        match self.send(cmd)? {
            Reply::Err(m) => Err(CloudError::Invalid(m)),
            other => Ok(other),
        }
    }

    /// Is a runner listening? Used for a status line, never to decide whether
    /// to stop anything.
    pub fn reachable(&self) -> bool {
        self.send(&Command::Ping).is_ok()
    }
}

/// The runner's end. Serves commands until the process stops.
#[cfg(unix)]
pub struct Server {
    listener: UnixListener,
    socket: PathBuf,
}

#[cfg(unix)]
impl Server {
    /// Bind the socket, replacing a leftover one.
    ///
    /// Unlinking first is safe here and nowhere else: the caller holds the
    /// one-writer lease, so no live runner can be using this path. Without it
    /// a crash would leave a socket file that `bind` refuses for ever.
    pub fn bind(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let socket = data_dir.join(SOCKET_FILE);
        if socket.exists() {
            std::fs::remove_file(&socket)?;
        }
        let listener = UnixListener::bind(&socket)?;
        // The filesystem is the authentication, so the mode is the access
        // control. 0600: the user the containers run as, and nobody else.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self { listener, socket })
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Serve for ever, handing each command to `handle`.
    ///
    /// One connection at a time, each carrying one command. `HALT` can take
    /// seconds, and serialising is the right trade: these are operator-rate
    /// requests, and a queue is easier to reason about than concurrent
    /// start/stop on the same broadcast.
    pub fn serve(&self, handle: impl Fn(Command) -> Reply) {
        for conn in self.listener.incoming() {
            let Ok(stream) = conn else { continue };
            stream.set_read_timeout(Some(TIMEOUT)).ok();
            stream.set_write_timeout(Some(TIMEOUT)).ok();
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_err() {
                continue;
            }
            let reply = match Command::decode(&line) {
                Ok(cmd) => handle(cmd),
                Err(e) => Reply::Err(e.to_string()),
            };
            let mut w = &stream;
            let _ = writeln!(w, "{}", reply.encode());
            let _ = w.flush();
        }
    }
}

#[cfg(unix)]
impl Drop for Server {
    fn drop(&mut self) {
        // Tidy, so the next bind does not have to unlink. Best effort: a
        // failure here is not worth failing a shutdown over.
        let _ = std::fs::remove_file(&self.socket);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_role_is_what_shipped_before_any_of_this() {
        assert_eq!(Role::default(), Role::All);
        assert!(Role::All.owns_workers() && Role::All.serves_http());
    }

    #[test]
    fn a_typo_in_the_role_does_not_produce_a_server_that_broadcasts_nothing() {
        // `Api` spawns no FFmpeg. If a misspelling landed there, a deployment
        // would come up serving pages with every broadcast silently dead.
        for bad in ["", "apii", "RUNNER ", "nonsense"] {
            let got = match bad.trim().to_ascii_lowercase().as_str() {
                "api" => Role::Api,
                "runner" | "worker" => Role::Runner,
                _ => Role::All,
            };
            if bad.trim().eq_ignore_ascii_case("runner") {
                assert_eq!(got, Role::Runner);
            } else {
                assert_eq!(got, Role::All, "{bad:?} should fall back to all");
            }
        }
    }

    #[test]
    fn only_the_worker_owner_takes_the_lease() {
        assert!(Role::All.takes_the_lease());
        assert!(Role::Runner.takes_the_lease());
        // Otherwise a rolling API deployment could not overlap for an instant.
        assert!(!Role::Api.takes_the_lease());
    }

    #[test]
    fn every_command_survives_a_round_trip() {
        for cmd in [
            Command::Start("abc123".into()),
            Command::Halt("deadbeef".into()),
            Command::Running,
            Command::Ping,
        ] {
            assert_eq!(Command::decode(&cmd.encode()).unwrap(), cmd);
        }
    }

    #[test]
    fn an_id_that_is_not_an_id_is_refused_before_it_reaches_sql_or_a_path() {
        for bad in ["START ../../etc/passwd", "START a b", "START", "HALT", "HALT x';DROP"] {
            assert!(Command::decode(bad).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn an_unknown_verb_is_refused() {
        let e = Command::decode("DESTROY abc123").unwrap_err().to_string();
        assert!(e.contains("알 수 없는 명령"), "got: {e}");
    }

    #[test]
    fn replies_survive_a_round_trip_including_an_empty_list() {
        assert_eq!(Reply::decode(&Reply::Ok.encode()), Reply::Ok);
        assert_eq!(Reply::decode(&Reply::Ids(vec![]).encode()), Reply::Ids(vec![]));
        let ids = Reply::Ids(vec!["a1".into(), "b2".into()]);
        assert_eq!(Reply::decode(&ids.encode()), ids);
    }

    #[test]
    fn an_error_message_keeps_its_sentence() {
        // The API shows this to a user, so a specific refusal must not be
        // flattened into "실패".
        let m = "요금제에 남은 동시 송출 자리가 없습니다";
        let round = Reply::decode(&Reply::Err(m.into()).encode());
        assert_eq!(round, Reply::Err(m.into()));
    }

    #[test]
    fn a_newline_in_an_error_cannot_forge_a_second_reply() {
        let sneaky = Reply::Err("broken\nOK".into());
        let line = sneaky.encode();
        assert!(!line.contains('\n'), "the wire form must be one line: {line:?}");
        assert_eq!(Reply::decode(&line), Reply::Err("broken OK".into()));
    }

    #[test]
    fn an_unparseable_reply_is_an_error_rather_than_a_success() {
        assert!(matches!(Reply::decode("hello"), Reply::Err(_)));
        assert!(matches!(Reply::decode(""), Reply::Err(_)));
    }

    #[cfg(unix)]
    #[test]
    fn a_client_with_no_runner_says_so_and_says_broadcasts_are_unaffected() {
        let dir = tempfile::tempdir().unwrap();
        let c = Client::new(dir.path());
        assert!(!c.reachable());
        let e = c.send(&Command::Ping).unwrap_err().to_string();
        assert!(e.contains("연결할 수 없습니다"), "got: {e}");
        assert!(e.contains("영향을 받지 않습니다"), "the operator is told what is safe: {e}");
    }

    #[cfg(unix)]
    #[test]
    fn a_command_reaches_the_runner_and_its_reply_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::bind(dir.path()).unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = std::sync::Arc::clone(&seen);

        let sock = server.socket().to_path_buf();
        std::thread::spawn(move || {
            server.serve(move |cmd| {
                recorder.lock().unwrap().push(cmd.clone());
                match cmd {
                    Command::Running => Reply::Ids(vec!["abc123".into()]),
                    Command::Start(_) | Command::Halt(_) | Command::Ping => Reply::Ok,
                }
            });
        });
        for _ in 0..200 {
            if sock.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let c = Client::new(dir.path());
        assert!(c.reachable());
        assert_eq!(c.call(&Command::Start("abc123".into())).unwrap(), Reply::Ok);
        assert_eq!(c.call(&Command::Running).unwrap(), Reply::Ids(vec!["abc123".into()]));
        assert_eq!(c.call(&Command::Halt("abc123".into())).unwrap(), Reply::Ok);

        let got = seen.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![
                Command::Ping,
                Command::Start("abc123".into()),
                Command::Running,
                Command::Halt("abc123".into())
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_err_reply_becomes_an_error_with_the_runners_own_words() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::bind(dir.path()).unwrap();
        let sock = server.socket().to_path_buf();
        std::thread::spawn(move || {
            server.serve(|_| Reply::Err("요금제에 남은 자리가 없습니다".into()));
        });
        for _ in 0..200 {
            if sock.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let e = Client::new(dir.path()).call(&Command::Start("abc123".into())).unwrap_err().to_string();
        assert!(e.contains("남은 자리가 없습니다"), "got: {e}");
    }

    #[cfg(unix)]
    #[test]
    fn a_leftover_socket_does_not_stop_the_runner_from_binding() {
        // A crash leaves the file behind, and `bind` refuses an existing path.
        // Without the unlink a runner could never start again.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SOCKET_FILE), b"not really a socket").unwrap();
        assert!(Server::bind(dir.path()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn the_socket_is_not_readable_by_anyone_else() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let s = Server::bind(dir.path()).unwrap();
        let mode = std::fs::metadata(s.socket()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the filesystem is the authentication here");
    }
}
