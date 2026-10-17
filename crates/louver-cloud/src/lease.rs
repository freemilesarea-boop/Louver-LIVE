//! One writer per data directory, enforced by the kernel.
//!
//! Two `louver-server` processes pointed at the same data directory both call
//! `recover_all`, and both spawn FFmpeg for every broadcast that wants to run.
//! Two encoders then push to the same YouTube stream key, which YouTube
//! answers by alternating between them or dropping both — and the cause is
//! invisible from either process, because each one is behaving correctly.
//!
//! This is not hypothetical and it is not only about the worker split. It is
//! what a deployment looks like when the old container has not finished dying
//! as the new one starts, and what a hand-run binary does to a live server
//! while an operator is diagnosing something.
//!
//! So: an advisory `flock` on a file in the data directory, taken for the life
//! of the process. The kernel owns it, which gives two properties a pid file
//! cannot:
//!
//! * a process that is killed, panics, or is SIGKILLed releases it
//!   immediately, because the lock belongs to the open file description and
//!   the kernel closes that. There is no stale lock to clean up and no
//!   "is pid 1234 still alive?" guesswork;
//! * it works across containers sharing the volume, because it is a property
//!   of the inode rather than of any process table.
//!
//! `flock` is declared here the way `kill` is declared in
//! `louver-core/src/streaming/supervisor.rs` — directly, rather than by taking
//! a dependency for two constants and one call.

#[cfg(unix)]
use crate::CloudError;
use crate::Result;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
const LOCK_EX: i32 = 2;
#[cfg(unix)]
const LOCK_NB: i32 = 4;

#[cfg(unix)]
extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

/// The file the lease lives in, inside the data directory.
pub const LEASE_FILE: &str = "runner.lock";

/// Proof that this process is the only one working this data directory.
///
/// Held for as long as the work lasts. Dropping it releases the lease, so it
/// belongs in a binding that lives as long as the process — binding it to `_`
/// drops it immediately and silently gives the lease away.
#[derive(Debug)]
#[must_use = "dropping the lease releases it, and another process may then start workers"]
pub struct Lease {
    path: PathBuf,
    /// Kept open because the lock is tied to this open file description.
    file: File,
}

impl Lease {
    /// Take the lease, or say who has it.
    ///
    /// Non-blocking on purpose: a second server starting up should fail with a
    /// sentence an operator can act on, not hang looking healthy.
    pub fn acquire(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join(LEASE_FILE);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // SAFETY: `file` owns a valid descriptor for the duration of the
            // call, and `flock` only consults it.
            let rc = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
            if rc != 0 {
                let held_by = read_holder(&path).unwrap_or_else(|| "알 수 없는 프로세스".into());
                return Err(CloudError::Invalid(format!(
                    "이 데이터 디렉터리는 이미 다른 프로세스가 사용 중입니다 ({held_by}). \
                     같은 데이터로 두 개를 실행하면 방송 하나에 FFmpeg 두 개가 붙습니다. \
                     ({})",
                    path.display()
                )));
            }
        }

        // Who holds it, for the error message the *next* process will print.
        // Written after the lock is held, so the contents always describe the
        // holder rather than the last hopeful.
        file.seek(SeekFrom::Start(0))?;
        file.set_len(0)?;
        writeln!(file, "pid={} role={}", std::process::id(), role_hint())?;
        file.flush()?;

        Ok(Self { path, file })
    }

    /// Where the lease file is, for an operator who wants to look.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What this process wrote into the lease file.
    pub fn holder(&self) -> String {
        read_holder(&self.path).unwrap_or_default()
    }

    /// Give up the lease deliberately.
    ///
    /// Equivalent to dropping it; named so a caller can be explicit at the end
    /// of a shutdown rather than relying on a scope.
    pub fn release(self) {
        drop(self);
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        // The lock goes when the descriptor does, which `File`'s own `Drop`
        // handles. The file is left in place deliberately: it is not a pid
        // file, nothing reads it to decide whether the holder is alive, and
        // removing it would race another process that has already opened it.
        let _ = self.file.flush();
    }
}

fn read_holder(path: &Path) -> Option<String> {
    let mut s = String::new();
    File::open(path).ok()?.read_to_string(&mut s).ok()?;
    let line = s.lines().next()?.trim().to_string();
    if line.is_empty() {
        return None;
    }
    Some(line)
}

/// A hint about which role took the lease, for the error message.
///
/// Read from the environment rather than passed in, because the lease is taken
/// before the role has been parsed into anything.
fn role_hint() -> String {
    std::env::var("LOUVER_ROLE").unwrap_or_else(|_| "all".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    // `flock` is what refuses the second one, so this is a Unix property.
    #[cfg(unix)]
    #[test]
    fn the_second_process_is_refused_rather_than_left_to_collide() {
        let dir = tempfile::tempdir().unwrap();
        let first = Lease::acquire(dir.path()).unwrap();

        // A separate `open`, so this is a different open file description —
        // which is what makes it conflict even inside one process.
        let second = Lease::acquire(dir.path());
        let err = second.expect_err("two leases on one directory").to_string();
        assert!(err.contains("이미 다른 프로세스가 사용 중"), "got: {err}");
        assert!(err.contains("FFmpeg 두 개"), "the message says why it matters: {err}");
        drop(first);
    }

    // `flock` is what refuses the second one, so this is a Unix property.
    #[cfg(unix)]
    #[test]
    fn the_refusal_names_the_holder() {
        let dir = tempfile::tempdir().unwrap();
        let held = Lease::acquire(dir.path()).unwrap();
        assert!(held.holder().contains(&format!("pid={}", std::process::id())));

        let err = Lease::acquire(dir.path()).unwrap_err().to_string();
        assert!(err.contains(&format!("pid={}", std::process::id())), "got: {err}");
        drop(held);
    }

    #[test]
    fn releasing_it_lets_the_next_one_in() {
        let dir = tempfile::tempdir().unwrap();
        let first = Lease::acquire(dir.path()).unwrap();
        first.release();
        let second = Lease::acquire(dir.path());
        assert!(second.is_ok(), "a released lease must be takeable");
    }

    #[test]
    fn a_lease_file_left_behind_by_a_dead_process_is_not_a_stale_lock() {
        // The property a pid file does not have. The file is still there with
        // somebody else's pid in it, and that must not block anything: the
        // kernel released the lock when the holder's descriptor closed.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LEASE_FILE), "pid=999999 role=runner\n").unwrap();

        let lease = Lease::acquire(dir.path()).expect("a stale file must not block the lease");
        assert!(lease.holder().contains(&format!("pid={}", std::process::id())), "it was rewritten");
    }

    #[test]
    fn it_does_not_need_the_directory_to_exist_already() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("var").join("lib").join("louver");
        let lease = Lease::acquire(&nested).unwrap();
        assert!(lease.path().exists());
    }

    #[test]
    fn two_different_directories_do_not_contend() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let one = Lease::acquire(a.path()).unwrap();
        let two = Lease::acquire(b.path()).unwrap();
        assert_ne!(one.path(), two.path());
    }
}
