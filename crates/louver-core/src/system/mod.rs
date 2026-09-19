//! System integration: metrics, disk, sleep prevention, autostart (§21, §23, §28).
//!
//! Sleep prevention and autostart are traits here; the desktop crate provides
//! the real OS implementations. Keeping them abstract lets the core be tested
//! on any platform and keeps the UI decoupled from the OS layer (§68).

use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Process and machine metrics for the dashboard (§24, §28).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SystemMetrics {
    /// This process's CPU usage, as a percentage of one core's worth of work
    /// normalized to the whole machine.
    pub app_cpu_percent: f32,
    pub app_memory_bytes: u64,
    pub system_cpu_percent: f32,
    pub total_memory_bytes: u64,
    pub available_memory_bytes: u64,
    /// FFmpeg's own CPU, which is the number that matters for §65.
    pub ffmpeg_cpu_percent: f32,
    pub ffmpeg_memory_bytes: u64,
}

/// Samples CPU/RAM. `sysinfo` needs two samples spaced apart to report CPU, so
/// this holds state between calls.
pub struct MetricsCollector {
    sys: sysinfo::System,
    pid: sysinfo::Pid,
}

impl std::fmt::Debug for MetricsCollector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetricsCollector").field("pid", &self.pid).finish()
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsCollector {
    pub fn new() -> Self {
        let mut sys = sysinfo::System::new();
        sys.refresh_all();
        Self { sys, pid: sysinfo::Pid::from_u32(std::process::id()) }
    }

    /// Sample now. `ffmpeg_pid` is included separately so the UI can show what
    /// the broadcast itself costs.
    pub fn sample(&mut self, ffmpeg_pid: Option<u32>) -> SystemMetrics {
        self.sys.refresh_memory();
        self.sys.refresh_cpu();
        self.sys.refresh_processes();

        let cores = self.sys.cpus().len().max(1) as f32;
        let me = self.sys.process(self.pid);
        let ff = ffmpeg_pid.map(sysinfo::Pid::from_u32).and_then(|p| self.sys.process(p));

        SystemMetrics {
            // sysinfo reports per-core percentages; normalize to the machine.
            app_cpu_percent: me.map(|p| p.cpu_usage() / cores).unwrap_or(0.0),
            app_memory_bytes: me.map(|p| p.memory()).unwrap_or(0),
            system_cpu_percent: self.sys.global_cpu_info().cpu_usage(),
            total_memory_bytes: self.sys.total_memory(),
            available_memory_bytes: self.sys.available_memory(),
            ffmpeg_cpu_percent: ff.map(|p| p.cpu_usage() / cores).unwrap_or(0.0),
            ffmpeg_memory_bytes: ff.map(|p| p.memory()).unwrap_or(0),
        }
    }

    /// Whether a process with this pid exists *and* looks like our FFmpeg.
    ///
    /// Pids are reused, so §33 asks for identity verification rather than a
    /// bare pid check before killing anything.
    pub fn is_ffmpeg_process(&mut self, pid: u32) -> bool {
        self.sys.refresh_processes();
        self.sys
            .process(sysinfo::Pid::from_u32(pid))
            .map(|p| {
                let name = p.name().to_ascii_lowercase();
                name.starts_with("ffmpeg") || name == "ffmpeg.exe"
            })
            .unwrap_or(false)
    }

    /// Kill an orphaned FFmpeg left by a crashed run, after verifying identity.
    pub fn kill_if_ffmpeg(&mut self, pid: u32) -> bool {
        if !self.is_ffmpeg_process(pid) {
            return false;
        }
        self.sys.process(sysinfo::Pid::from_u32(pid)).map(|p| p.kill()).unwrap_or(false)
    }
}

/// Free bytes on the volume containing `path` (§10).
pub fn available_disk_bytes(path: &Path) -> u64 {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    // Longest matching mount point wins, so /home beats / for a path under /home.
    disks
        .iter()
        .filter(|d| path.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space())
        .unwrap_or(0)
}

/// Keeps the machine awake while broadcasting (§23).
///
/// Closing a laptop lid is explicitly *not* worked around; the UI says so.
pub trait SleepPreventer: Send + Sync + std::fmt::Debug {
    fn prevent_sleep(&self, reason: &str) -> Result<()>;
    fn allow_sleep(&self) -> Result<()>;
    fn is_preventing(&self) -> bool;
}

/// No-op preventer used in tests and on platforms without an implementation.
#[derive(Debug, Default)]
pub struct NoopSleepPreventer {
    active: Arc<AtomicBool>,
}

impl SleepPreventer for NoopSleepPreventer {
    fn prevent_sleep(&self, _reason: &str) -> Result<()> {
        self.active.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn allow_sleep(&self) -> Result<()> {
        self.active.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn is_preventing(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}

/// "Launch at startup" (§21).
pub trait AutostartManager: Send + Sync + std::fmt::Debug {
    fn enable(&self) -> Result<()>;
    fn disable(&self) -> Result<()>;
    fn is_enabled(&self) -> bool;
}

#[derive(Debug, Default)]
pub struct NoopAutostart {
    enabled: Arc<AtomicBool>,
}

impl AutostartManager for NoopAutostart {
    fn enable(&self) -> Result<()> {
        self.enabled.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn disable(&self) -> Result<()> {
        self.enabled.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }
}

/// Network reachability for preflight (§29).
///
/// No fake Mbps figure is ever produced: §29 forbids inventing a speed test.
/// [`SpeedTestProvider`] is the seam a real provider would plug into later.
pub trait NetworkChecker: Send + Sync + std::fmt::Debug {
    /// Can we open a TCP connection to the ingest host?
    fn can_reach(&self, host: &str, port: u16, timeout: std::time::Duration) -> bool;
}

#[derive(Debug, Default)]
pub struct TcpNetworkChecker;

impl NetworkChecker for TcpNetworkChecker {
    fn can_reach(&self, host: &str, port: u16, timeout: std::time::Duration) -> bool {
        use std::net::ToSocketAddrs;
        let Ok(mut addrs) = (host, port).to_socket_addrs() else { return false };
        addrs.any(|a| std::net::TcpStream::connect_timeout(&a, timeout).is_ok())
    }
}

/// Interface reserved for a future speed-test provider (§29). V1 ships none.
pub trait SpeedTestProvider: Send + Sync {
    fn measure_upload_mbps(&self) -> Option<f64>;
    fn provider_name(&self) -> &'static str;
}

/// Extract host and port from an `rtmps://host/app` URL.
pub fn parse_rtmp_host(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let default_port = match scheme {
        "rtmps" => 443,
        "rtmp" => 1935,
        _ => return None,
    };
    let authority = rest.split('/').next()?;
    match authority.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() => Some((h.to_string(), p.parse().ok()?)),
        _ => (!authority.is_empty()).then(|| (authority.to_string(), default_port)),
    }
}

/// Format bytes for the UI.
pub fn format_bytes(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// Format a duration as `HH:MM:SS`.
pub fn format_duration(secs: i64) -> String {
    let s = secs.max(0);
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

/// Korean "3시간 00분 37초" for playlist totals (§25).
pub fn format_duration_ko(secs: i64) -> String {
    let s = secs.max(0);
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}시간 {m:02}분 {sec:02}초")
    } else if m > 0 {
        format!("{m}분 {sec:02}초")
    } else {
        format!("{sec}초")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_report_something_for_this_process() {
        let mut c = MetricsCollector::new();
        let m = c.sample(None);
        assert!(m.total_memory_bytes > 0);
        assert!(m.app_memory_bytes > 0, "we are running, so we use memory");
        assert_eq!(m.ffmpeg_memory_bytes, 0, "no ffmpeg pid was given");
    }

    #[test]
    fn cpu_percentages_are_normalized_to_the_machine() {
        let mut c = MetricsCollector::new();
        let m = c.sample(None);
        assert!((0.0..=100.0).contains(&m.app_cpu_percent), "{}", m.app_cpu_percent);
    }

    #[test]
    fn a_bogus_pid_is_not_mistaken_for_ffmpeg() {
        let mut c = MetricsCollector::new();
        assert!(!c.is_ffmpeg_process(0));
        // Our own process is not ffmpeg, so an orphan check must not kill us.
        assert!(!c.is_ffmpeg_process(std::process::id()));
        assert!(!c.kill_if_ffmpeg(std::process::id()), "must refuse to kill a non-ffmpeg pid");
    }

    #[test]
    fn disk_space_is_reported_for_a_real_path() {
        let d = tempfile::tempdir().unwrap();
        // Some sandboxes expose no disks at all; the call must still not panic.
        let _ = available_disk_bytes(d.path());
    }

    #[test]
    fn sleep_preventer_tracks_its_state() {
        let p = NoopSleepPreventer::default();
        assert!(!p.is_preventing());
        p.prevent_sleep("방송 중").unwrap();
        assert!(p.is_preventing());
        p.allow_sleep().unwrap();
        assert!(!p.is_preventing(), "power policy must be restored after a broadcast");
    }

    #[test]
    fn autostart_toggles() {
        let a = NoopAutostart::default();
        assert!(!a.is_enabled());
        a.enable().unwrap();
        assert!(a.is_enabled());
        a.disable().unwrap();
        assert!(!a.is_enabled());
    }

    #[test]
    fn rtmp_host_parsing_defaults_ports_by_scheme() {
        assert_eq!(
            parse_rtmp_host("rtmps://a.rtmps.youtube.com/live2"),
            Some(("a.rtmps.youtube.com".into(), 443))
        );
        assert_eq!(
            parse_rtmp_host("rtmp://a.rtmp.youtube.com/live2"),
            Some(("a.rtmp.youtube.com".into(), 1935))
        );
        assert_eq!(parse_rtmp_host("rtmps://host.example:1936/live2"), Some(("host.example".into(), 1936)));
        assert_eq!(parse_rtmp_host("https://example.com"), None);
        assert_eq!(parse_rtmp_host("not a url"), None);
    }

    #[test]
    fn byte_formatting() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(24_800_000_000), "23.1 GB");
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(format_duration(0), "00:00:00");
        assert_eq!(format_duration(3661), "01:01:01");
        assert_eq!(format_duration(15678), "04:21:18");
        assert_eq!(format_duration(-5), "00:00:00");
    }

    #[test]
    fn korean_duration_matches_the_spec_example() {
        // §25: "3시간 00분 37초"
        assert_eq!(format_duration_ko(3 * 3600 + 37), "3시간 00분 37초");
        assert_eq!(format_duration_ko(62), "1분 02초");
        assert_eq!(format_duration_ko(9), "9초");
    }
}
