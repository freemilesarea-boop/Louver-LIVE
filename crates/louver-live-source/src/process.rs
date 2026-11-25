//! Running a child and not being hostage to it.
//!
//! The same shape as `louver_cloud::cctv`'s private helper, which could not be
//! reused because it is not public — and copying ten lines is better than
//! widening production's API surface for this worker's benefit.
//!
//! Both pipes are drained by their own thread. A child that writes more than a
//! pipe buffer would otherwise block forever and never be seen to exit, which
//! is the deadlock this shape exists to avoid.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct Finished {
    pub ok: bool,
    pub timed_out: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub fn run_with_timeout(mut cmd: Command, limit: Duration) -> std::io::Result<Finished> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let mut out = child.stdout.take().expect("piped");
    let mut err = child.stderr.take().expect("piped");
    let out_t = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let err_t = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        b
    });
    let deadline = Instant::now() + limit;
    let (ok, timed_out) = loop {
        match child.try_wait()? {
            Some(status) => break (status.success(), false),
            None => {
                if Instant::now() >= deadline {
                    // This child, by handle. Never a pid search.
                    let _ = child.kill();
                    let _ = child.wait();
                    break (false, true);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    Ok(Finished {
        ok,
        timed_out,
        stdout: out_t.join().unwrap_or_default(),
        stderr: err_t.join().unwrap_or_default(),
    })
}
