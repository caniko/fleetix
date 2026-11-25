//! Multi-signal stall and hang watchdog for supervised builds, trains, and jobs.
//!
//! Pure wall-clock timeouts either kill legitimate long builds too early or let
//! deadlocked processes wedge queues for hours. Observing multiple independent
//! progress signals (output/log growth, CPU ticks, I/O bytes, and process-tree
//! churn) keeps real compilations running while catching deadlocks (such as a
//! hung pre-build hook, deadlocked `flock`, or wedged daemon RPC) promptly and
//! capturing kernel wait-channel (`wchan`) diagnostics.

use super::liveness::{
    cmdline_summary, descendant_pids, parse_proc_stat_bytes, read_proc_stat, wait_channel,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Bounds governing when a supervised job is classified as stalled or overtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchdogPolicy {
    /// Hard wall-clock runtime ceiling in seconds (`0` disables the ceiling).
    pub max_runtime_seconds: u64,
    /// Maximum silent window with zero observable progress (`0` disables stall detection).
    pub stall_timeout_seconds: u64,
    /// Initial grace window after start during which stall detection does not fire.
    #[serde(default)]
    pub startup_grace_seconds: u64,
}

impl WatchdogPolicy {
    /// Common-sense default for Nix derivation builds: 4 h hard cap, 20 min
    /// zero-progress stall timeout, 2 min startup grace.
    pub const fn build_default() -> Self {
        Self {
            max_runtime_seconds: 4 * 3600,
            stall_timeout_seconds: 20 * 60,
            startup_grace_seconds: 120,
        }
    }

    /// Common-sense default for pin/switch train steps: 2 h hard cap, 15 min
    /// zero-progress stall timeout, 90 s startup grace.
    pub const fn train_step_default() -> Self {
        Self {
            max_runtime_seconds: 2 * 3600,
            stall_timeout_seconds: 15 * 60,
            startup_grace_seconds: 90,
        }
    }

    /// Disabled watchdog (never fires).
    pub const fn disabled() -> Self {
        Self {
            max_runtime_seconds: 0,
            stall_timeout_seconds: 0,
            startup_grace_seconds: 0,
        }
    }

    /// Validate that the configured windows are internally consistent.
    pub fn validate(&self) -> Result<(), String> {
        if self.max_runtime_seconds > 0
            && self.stall_timeout_seconds > 0
            && self.stall_timeout_seconds > self.max_runtime_seconds
        {
            return Err("stall_timeout_seconds must not exceed max_runtime_seconds".into());
        }
        Ok(())
    }
}

/// Snapshot of observable forward-progress counters for a supervised run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressSample {
    /// Cumulative bytes across observed log/output files.
    pub output_bytes: u64,
    /// Latest modification timestamp (nanoseconds since UNIX epoch) across observed files.
    pub output_mtime_nanos: u128,
    /// Cumulative user + system CPU ticks (or microseconds for cgroups).
    pub cpu_ticks: u64,
    /// Cumulative read + write I/O bytes across the process tree or cgroup.
    pub io_bytes: u64,
    /// Number of live non-zombie processes currently in the supervised tree/cgroup.
    pub live_processes: usize,
}

impl ProgressSample {
    /// Whether this observation shows forward progress relative to `previous`.
    ///
    /// A genuinely deadlocked process tree has static logs, zero CPU advance,
    /// zero I/O advance, and a static set of blocked processes. Any advance in
    /// output, CPU, I/O, or subprocess churn counts as progress so slow builds
    /// are never killed spuriously.
    pub fn advanced_from(&self, previous: &Self) -> bool {
        self.output_bytes > previous.output_bytes
            || self.output_mtime_nanos > previous.output_mtime_nanos
            || self.cpu_ticks > previous.cpu_ticks
            || self.io_bytes > previous.io_bytes
            || self.live_processes != previous.live_processes
    }
}

/// Diagnostic snapshot of a blocked process captured when a stall fires.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StuckProcess {
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wchan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmdline: Option<String>,
}

/// Verdict produced by [`StallWatchdog::observe`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum WatchdogVerdict {
    Healthy,
    Stalled {
        silent_seconds: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        stuck_processes: Vec<StuckProcess>,
    },
    RuntimeExceeded {
        elapsed_seconds: u64,
        limit_seconds: u64,
    },
}

impl WatchdogVerdict {
    pub fn is_healthy(&self) -> bool {
        matches!(self, Self::Healthy)
    }
}

/// Pure, clock-injected stall detector for a single supervised execution.
#[derive(Clone, Debug)]
pub struct StallWatchdog {
    policy: WatchdogPolicy,
    started_at: u64,
    last_progress_at: u64,
    last_sample: Option<ProgressSample>,
}

impl StallWatchdog {
    pub fn new(policy: WatchdogPolicy, started_at: u64) -> Self {
        Self {
            policy,
            started_at,
            last_progress_at: started_at,
            last_sample: None,
        }
    }

    pub fn policy(&self) -> WatchdogPolicy {
        self.policy
    }

    pub fn elapsed_seconds(&self, now: u64) -> u64 {
        now.saturating_sub(self.started_at)
    }

    pub fn silent_seconds(&self, now: u64) -> u64 {
        now.saturating_sub(self.last_progress_at)
    }

    /// Feed a new observation at monotonic/epoch second `now`.
    ///
    /// `collect_diagnostics` is only invoked when a stall actually triggers, so
    /// healthy ticks pay zero diagnostic overhead.
    pub fn observe(
        &mut self,
        now: u64,
        sample: ProgressSample,
        collect_diagnostics: impl FnOnce() -> Vec<StuckProcess>,
    ) -> WatchdogVerdict {
        let elapsed = self.elapsed_seconds(now);
        if self.policy.max_runtime_seconds > 0 && elapsed >= self.policy.max_runtime_seconds {
            return WatchdogVerdict::RuntimeExceeded {
                elapsed_seconds: elapsed,
                limit_seconds: self.policy.max_runtime_seconds,
            };
        }
        match self.last_sample {
            None => {
                self.last_sample = Some(sample);
                self.last_progress_at = now;
                return WatchdogVerdict::Healthy;
            }
            Some(previous) if sample.advanced_from(&previous) => {
                // Keep monotonic peaks for CPU/IO so short-lived child exits do
                // not look like zero-baseline oscillation on the following tick.
                self.last_sample = Some(ProgressSample {
                    output_bytes: sample.output_bytes.max(previous.output_bytes),
                    output_mtime_nanos: sample.output_mtime_nanos.max(previous.output_mtime_nanos),
                    cpu_ticks: sample.cpu_ticks.max(previous.cpu_ticks),
                    io_bytes: sample.io_bytes.max(previous.io_bytes),
                    live_processes: sample.live_processes,
                });
                self.last_progress_at = now;
                return WatchdogVerdict::Healthy;
            }
            Some(_) => {}
        }
        if elapsed < self.policy.startup_grace_seconds {
            return WatchdogVerdict::Healthy;
        }
        let silent = self.silent_seconds(now);
        if self.policy.stall_timeout_seconds > 0 && silent >= self.policy.stall_timeout_seconds {
            return WatchdogVerdict::Stalled {
                silent_seconds: silent,
                stuck_processes: collect_diagnostics(),
            };
        }
        WatchdogVerdict::Healthy
    }
}

/// Observe combined file size and latest modification time across `paths`.
/// Missing files are ignored so newly starting units work cleanly.
pub fn observe_files(paths: &[&Path]) -> (u64, u128) {
    let mut total_bytes = 0u64;
    let mut latest_mtime = 0u128;
    for path in paths {
        let Ok(meta) = fs::metadata(path) else {
            continue;
        };
        total_bytes = total_bytes.saturating_add(meta.len());
        if let Ok(modified) = meta.modified() {
            if let Ok(duration) = modified.duration_since(UNIX_EPOCH) {
                latest_mtime = latest_mtime.max(duration.as_nanos());
            }
        }
    }
    (total_bytes, latest_mtime)
}

/// Observe progress of a Linux process tree rooted at `root_pid` plus any log files.
pub fn observe_process_tree(root_pid: u32, log_paths: &[&Path]) -> ProgressSample {
    let (output_bytes, output_mtime_nanos) = observe_files(log_paths);
    let pids = descendant_pids(root_pid);
    let mut cpu_ticks = 0u64;
    let mut io_bytes = 0u64;
    for &pid in &pids {
        if let Ok(Some(stat)) = read_proc_stat(pid) {
            cpu_ticks = cpu_ticks.saturating_add(stat.total_cpu_ticks());
        }
        io_bytes = io_bytes.saturating_add(read_proc_io_bytes(pid));
    }
    ProgressSample {
        output_bytes,
        output_mtime_nanos,
        cpu_ticks,
        io_bytes,
        live_processes: pids.len(),
    }
}

/// Observe progress of a Linux cgroup v2 directory (e.g. a systemd transient
/// unit cgroup) plus any log files.
///
/// Uses kernel-maintained monotonic `cpu.stat` (`usage_usec`) and `io.stat`
/// counters so child process exits never drop cumulative usage.
pub fn observe_cgroup(cgroup_dir: &Path, log_paths: &[&Path]) -> Option<ProgressSample> {
    if !cgroup_dir.is_dir() {
        return None;
    }
    let (output_bytes, output_mtime_nanos) = observe_files(log_paths);
    let cpu_ticks = fs::read_to_string(cgroup_dir.join("cpu.stat"))
        .ok()
        .and_then(|text| parse_cgroup_cpu_usec(&text))
        .unwrap_or(0);
    let io_bytes = fs::read_to_string(cgroup_dir.join("io.stat"))
        .ok()
        .map(|text| parse_cgroup_io_bytes(&text))
        .unwrap_or(0);
    let live_processes = fs::read_to_string(cgroup_dir.join("cgroup.procs"))
        .ok()
        .map(|text| {
            text.lines()
                .filter(|line| line.trim().parse::<u32>().is_ok())
                .count()
        })
        .unwrap_or(0);
    Some(ProgressSample {
        output_bytes,
        output_mtime_nanos,
        cpu_ticks,
        io_bytes,
        live_processes,
    })
}

/// Capture bounded diagnostic snapshots (PID, kernel `wchan`, sanitized cmdline)
/// for up to `limit` sleeping/blocked processes from `pids`.
pub fn diagnose_stuck_pids(pids: &[u32], limit: usize) -> Vec<StuckProcess> {
    let mut out = Vec::new();
    for &pid in pids.iter().take(limit) {
        let wchan = wait_channel(pid);
        let cmdline = cmdline_summary(pid);
        out.push(StuckProcess {
            pid,
            wchan,
            cmdline,
        });
    }
    out
}

/// Parse `/proc/<pid>/io` `rchar` + `wchar` bytes (readable for same-UID processes).
fn read_proc_io_bytes(pid: u32) -> u64 {
    let Ok(text) = fs::read_to_string(format!("/proc/{pid}/io")) else {
        return 0;
    };
    parse_proc_io_bytes(&text)
}

pub(crate) fn parse_proc_io_bytes(text: &str) -> u64 {
    let mut total = 0u64;
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(':') {
            if matches!(key.trim(), "rchar" | "wchar" | "read_bytes" | "write_bytes") {
                if let Ok(n) = value.trim().parse::<u64>() {
                    total = total.saturating_add(n);
                }
            }
        }
    }
    total
}

pub(crate) fn parse_cgroup_cpu_usec(text: &str) -> Option<u64> {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() == Some("usage_usec") {
            return parts.next()?.parse::<u64>().ok();
        }
    }
    None
}

pub(crate) fn parse_cgroup_io_bytes(text: &str) -> u64 {
    let mut total = 0u64;
    for line in text.lines() {
        for token in line.split_whitespace().skip(1) {
            if let Some((key, value)) = token.split_once('=') {
                if matches!(key, "rbytes" | "wbytes") {
                    if let Ok(n) = value.parse::<u64>() {
                        total = total.saturating_add(n);
                    }
                }
            }
        }
    }
    total
}

/// Collect PIDs from a cgroup v2 `cgroup.procs` file that are still non-zombie.
pub fn cgroup_live_pids(cgroup_dir: &Path) -> Vec<u32> {
    let Ok(text) = fs::read_to_string(cgroup_dir.join("cgroup.procs")) else {
        return Vec::new();
    };
    let mut pids = Vec::new();
    for line in text.lines() {
        let Ok(pid) = line.trim().parse::<u32>() else {
            continue;
        };
        let Ok(bytes) = fs::read(format!("/proc/{pid}/stat")) else {
            continue;
        };
        if parse_proc_stat_bytes(&bytes).is_some_and(|stat| stat.state != b'Z') {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_resets_on_any_signal_and_fires_when_completely_silent() {
        let policy = WatchdogPolicy {
            max_runtime_seconds: 600,
            stall_timeout_seconds: 60,
            startup_grace_seconds: 20,
        };
        policy.validate().unwrap();
        let mut wd = StallWatchdog::new(policy, 1000);
        let base = ProgressSample {
            output_bytes: 100,
            output_mtime_nanos: 1,
            cpu_ticks: 50,
            io_bytes: 1024,
            live_processes: 2,
        };
        assert_eq!(wd.observe(1000, base, Vec::new), WatchdogVerdict::Healthy);
        // Silent during startup grace does not trip.
        assert_eq!(wd.observe(1015, base, Vec::new), WatchdogVerdict::Healthy);
        // CPU-only progress (quiet compiler LTO) resets stall window.
        let compiling = ProgressSample {
            cpu_ticks: 200,
            ..base
        };
        assert_eq!(
            wd.observe(1055, compiling, Vec::new),
            WatchdogVerdict::Healthy
        );
        // 59s after last progress: still healthy.
        assert_eq!(
            wd.observe(1114, compiling, Vec::new),
            WatchdogVerdict::Healthy
        );
        // 60s of total silence across all signals: stalled with diagnostics.
        let verdict = wd.observe(1115, compiling, || {
            vec![StuckProcess {
                pid: 999,
                wchan: Some("unix_stream_read_generic".into()),
                cmdline: Some("nix derivation show".into()),
            }]
        });
        assert_eq!(
            verdict,
            WatchdogVerdict::Stalled {
                silent_seconds: 60,
                stuck_processes: vec![StuckProcess {
                    pid: 999,
                    wchan: Some("unix_stream_read_generic".into()),
                    cmdline: Some("nix derivation show".into()),
                }],
            }
        );
        // Hard runtime ceiling takes precedence.
        assert_eq!(
            wd.observe(1600, compiling, Vec::new),
            WatchdogVerdict::RuntimeExceeded {
                elapsed_seconds: 600,
                limit_seconds: 600,
            }
        );
    }

    #[test]
    fn parses_cgroup_and_proc_counters() {
        assert_eq!(
            parse_cgroup_cpu_usec("usage_usec 123456\nuser_usec 100000\n"),
            Some(123456)
        );
        assert_eq!(
            parse_cgroup_io_bytes("259:0 rbytes=4096 wbytes=8192 rios=2 wios=3\n"),
            12288
        );
        assert_eq!(
            parse_proc_io_bytes("rchar: 10\nwchar: 20\nread_bytes: 30\nwrite_bytes: 40\n"),
            100
        );
    }
}
