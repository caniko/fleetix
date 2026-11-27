//! PID-reuse-safe and reboot-safe process liveness checks over Linux `/proc`.
//!
//! A bare PID cannot prove a lease holder is still alive: after a crash or
//! reboot the kernel recycles PIDs. Recording `(pid, start_ticks, boot_id)`
//! makes dead-owner detection exact while failing closed ([`Liveness::Indeterminate`])
//! whenever `/proc` cannot be inspected.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

/// Durable, PID-reuse-safe process identity suitable for leases and markers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessIdentity {
    pub pid: u32,
    /// Kernel process start time in clock ticks (`/proc/<pid>/stat` field 22).
    pub start_ticks: u64,
    /// Optional host boot UUID (`/proc/sys/kernel/random/boot_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_id: Option<String>,
}

/// Why a previously recorded process is provably no longer running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeadReason {
    /// Host boot ID changed since the identity was captured.
    Rebooted,
    /// No `/proc/<pid>` entry exists for the PID.
    NotFound,
    /// Process is a zombie (`Z`) awaiting parent reap and cannot make progress.
    Zombie,
    /// PID now belongs to a different process (`start_ticks` mismatch).
    PidRecycled,
}

impl std::fmt::Display for DeadReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rebooted => f.write_str("host rebooted"),
            Self::NotFound => f.write_str("process exited"),
            Self::Zombie => f.write_str("process is a zombie"),
            Self::PidRecycled => f.write_str("PID was recycled by another process"),
        }
    }
}

/// Result of checking whether a recorded process identity is still alive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "reason", rename_all = "kebab-case")]
pub enum Liveness {
    Alive,
    Dead(DeadReason),
    /// Could not inspect process state; callers must fail closed.
    Indeterminate,
}

impl Liveness {
    pub fn is_alive(self) -> bool {
        matches!(self, Self::Alive)
    }

    pub fn is_dead(self) -> bool {
        matches!(self, Self::Dead(_))
    }

    pub fn is_indeterminate(self) -> bool {
        matches!(self, Self::Indeterminate)
    }
}

/// Parsed fields from Linux `/proc/<pid>/stat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcStat {
    /// Single-byte process state (`b'R'`, `b'S'`, `b'D'`, `b'Z'`, ...).
    pub state: u8,
    pub ppid: u32,
    pub pgrp: u32,
    /// User-mode CPU ticks consumed by this process.
    pub utime: u64,
    /// Kernel-mode CPU ticks consumed by this process.
    pub stime: u64,
    /// User-mode CPU ticks consumed by waited-for children.
    pub cutime: u64,
    /// Kernel-mode CPU ticks consumed by waited-for children.
    pub cstime: u64,
    /// Process start time in ticks since boot (field 22).
    pub start_ticks: u64,
}

impl ProcStat {
    /// Total CPU ticks (self plus reaped children).
    pub fn total_cpu_ticks(&self) -> u64 {
        self.utime
            .saturating_add(self.stime)
            .saturating_add(self.cutime)
            .saturating_add(self.cstime)
    }
}

/// Parse `/proc/<pid>/stat` using raw bytes so arbitrary `comm` names
/// containing spaces, parentheses, or non-UTF-8 bytes never fail or misalign.
pub fn parse_proc_stat_bytes(bytes: &[u8]) -> Option<ProcStat> {
    // Locate the LAST ") " delimiter closing the parenthesized comm field.
    let split = bytes.windows(2).rposition(|window| window == b") ")?;
    let tail = std::str::from_utf8(&bytes[split + 2..]).ok()?;
    let mut fields = tail.split_whitespace();
    let state_str = fields.next()?;
    let state = *state_str.as_bytes().first()?;
    let ppid = fields.next()?.parse::<u32>().ok()?;
    let pgrp = fields.next()?.parse::<u32>().ok()?;
    // Skip fields 3..=10 (session, tty_nr, tpgid, flags, minflt, cminflt, majflt, cmajflt).
    let user_ticks = fields.nth(8)?.parse::<u64>().ok()?;
    let system_ticks = fields.next()?.parse::<u64>().ok()?;
    let child_user_ticks = fields.next()?.parse::<u64>().ok()?;
    let child_system_ticks = fields.next()?.parse::<u64>().ok()?;
    // Skip fields 15..=18 (priority, nice, num_threads, itrealvalue).
    let start_ticks = fields.nth(4)?.parse::<u64>().ok()?;
    Some(ProcStat {
        state,
        ppid,
        pgrp,
        utime: user_ticks,
        stime: system_ticks,
        cutime: child_user_ticks,
        cstime: child_system_ticks,
        start_ticks,
    })
}

/// Pure, testable liveness verdict from pre-collected host observations.
#[expect(
    clippy::needless_pass_by_value,
    reason = "keep accepting an owned observation result from the procfs reader"
)]
pub fn evaluate_liveness<E>(
    recorded: &ProcessIdentity,
    current_boot_id: Option<&str>,
    observed_stat: Result<Option<ProcStat>, E>,
) -> Liveness {
    if recorded.pid == 0 {
        return Liveness::Dead(DeadReason::NotFound);
    }
    if let (Some(expected), Some(actual)) = (recorded.boot_id.as_deref(), current_boot_id) {
        if expected != actual {
            return Liveness::Dead(DeadReason::Rebooted);
        }
    }
    match observed_stat {
        Ok(None) => Liveness::Dead(DeadReason::NotFound),
        Ok(Some(stat)) if stat.state == b'Z' => Liveness::Dead(DeadReason::Zombie),
        Ok(Some(stat)) if stat.start_ticks != recorded.start_ticks => {
            Liveness::Dead(DeadReason::PidRecycled)
        }
        Ok(Some(_)) => Liveness::Alive,
        Err(_) => Liveness::Indeterminate,
    }
}

/// Read the host boot UUID from `/proc/sys/kernel/random/boot_id`.
pub fn read_boot_id() -> Option<String> {
    read_boot_id_at(Path::new("/proc/sys/kernel/random/boot_id"))
}

pub(crate) fn read_boot_id_at(path: &Path) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Read `/proc/<pid>/stat` for a process.
///
/// Returns `Ok(None)` when the process does not exist (`NotFound`), or `Err`
/// when `/proc` is unavailable or unreadable (so callers fail closed).
pub fn read_proc_stat(pid: u32) -> std::io::Result<Option<ProcStat>> {
    read_proc_stat_in(Path::new("/proc"), pid)
}

pub(crate) fn read_proc_stat_in(proc_root: &Path, pid: u32) -> std::io::Result<Option<ProcStat>> {
    if pid == 0 {
        return Ok(None);
    }
    let path = proc_root.join(pid.to_string()).join("stat");
    match fs::read(&path) {
        Ok(bytes) => parse_proc_stat_bytes(&bytes).map(Some).ok_or_else(|| {
            std::io::Error::new(
                ErrorKind::InvalidData,
                format!("unparseable proc stat at {}", path.display()),
            )
        }),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            // Confirm `/proc` itself is mounted so a missing `/proc` mount
            // fails closed rather than claiming every PID is dead.
            if proc_root.join("self").exists() {
                Ok(None)
            } else {
                Err(error)
            }
        }
        // ESRCH can surface as `Uncategorized` / raw OS error 3 when racing exit.
        Err(error) if error.raw_os_error() == Some(3) => Ok(None),
        Err(error) => Err(error),
    }
}

impl ProcessIdentity {
    /// Capture the current process's durable identity.
    pub fn current() -> Result<Self, String> {
        Self::capture(std::process::id())
    }

    /// Capture a live process's durable identity from `/proc`.
    pub fn capture(pid: u32) -> Result<Self, String> {
        let stat = read_proc_stat(pid)
            .map_err(|error| format!("cannot inspect /proc/{pid}/stat: {error}"))?
            .ok_or_else(|| format!("process {pid} does not exist"))?;
        if stat.state == b'Z' {
            return Err(format!("process {pid} is already a zombie"));
        }
        Ok(Self {
            pid,
            start_ticks: stat.start_ticks,
            boot_id: read_boot_id(),
        })
    }

    /// Check whether this process identity is still alive on the current host.
    pub fn check(&self) -> Liveness {
        let boot_id = read_boot_id();
        let observed = read_proc_stat(self.pid);
        evaluate_liveness(self, boot_id.as_deref(), observed)
    }
}

/// Read the kernel wait channel (`/proc/<pid>/wchan`) for stall diagnostics.
pub fn wait_channel(pid: u32) -> Option<String> {
    let raw = fs::read_to_string(format!("/proc/{pid}/wchan")).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "0" {
        None
    } else {
        Some(sanitize_diagnostic(trimmed, 128))
    }
}

/// Read a bounded, printable-ASCII summary of `/proc/<pid>/cmdline`.
pub fn cmdline_summary(pid: u32) -> Option<String> {
    let raw = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    if raw.is_empty() {
        return None;
    }
    let joined = raw
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    if joined.is_empty() {
        None
    } else {
        Some(sanitize_diagnostic(&joined, 200))
    }
}

/// Collect `root_pid` and all non-zombie recursive descendant PIDs in a single
/// `/proc` scan.
pub fn descendant_pids(root_pid: u32) -> Vec<u32> {
    descendant_pids_in(Path::new("/proc"), root_pid)
}

pub(crate) fn descendant_pids_in(proc_root: &Path, root_pid: u32) -> Vec<u32> {
    if root_pid == 0 {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let mut alive = BTreeSet::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(bytes) = fs::read(entry.path().join("stat")) else {
            continue;
        };
        let Some(stat) = parse_proc_stat_bytes(&bytes) else {
            continue;
        };
        if stat.state == b'Z' {
            continue;
        }
        alive.insert(pid);
        children.entry(stat.ppid).or_default().push(pid);
    }
    if !alive.contains(&root_pid) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut stack = vec![root_pid];
    let mut visited = BTreeSet::new();
    while let Some(pid) = stack.pop() {
        if !visited.insert(pid) {
            continue;
        }
        out.push(pid);
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids.iter().copied());
        }
    }
    out.sort_unstable();
    out
}

pub(crate) fn sanitize_diagnostic(input: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        if out.len() >= max_chars {
            out.push_str("...");
            break;
        }
        if ch.is_ascii_graphic() || ch == ' ' {
            out.push(ch);
        } else {
            out.push('?');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_stat_with_parens_spaces_and_non_utf8_comm() {
        let mut raw = b"12345 (weird ) name ".to_vec();
        raw.extend_from_slice(&[0xff, 0xfe]);
        raw.extend_from_slice(
            b") S 99 12345 12345 0 -1 4194304 10 0 0 0 111 222 33 44 20 0 1 0 9876543 1000 50\n",
        );
        let stat = parse_proc_stat_bytes(&raw).unwrap();
        assert_eq!(stat.state, b'S');
        assert_eq!(stat.ppid, 99);
        assert_eq!(stat.pgrp, 12345);
        assert_eq!(stat.utime, 111);
        assert_eq!(stat.stime, 222);
        assert_eq!(stat.cutime, 33);
        assert_eq!(stat.cstime, 44);
        assert_eq!(stat.start_ticks, 9_876_543);
        assert_eq!(stat.total_cpu_ticks(), 410);
    }

    #[test]
    fn evaluates_all_liveness_outcomes_and_fails_closed() {
        let id = ProcessIdentity {
            pid: 42,
            start_ticks: 1000,
            boot_id: Some("boot-a".into()),
        };
        let live_stat = ProcStat {
            state: b'S',
            ppid: 1,
            pgrp: 42,
            utime: 1,
            stime: 2,
            cutime: 0,
            cstime: 0,
            start_ticks: 1000,
        };
        assert_eq!(
            evaluate_liveness::<&str>(&id, Some("boot-a"), Ok(Some(live_stat))),
            Liveness::Alive
        );
        assert_eq!(
            evaluate_liveness::<&str>(&id, Some("boot-b"), Ok(Some(live_stat))),
            Liveness::Dead(DeadReason::Rebooted)
        );
        assert_eq!(
            evaluate_liveness::<&str>(&id, Some("boot-a"), Ok(None)),
            Liveness::Dead(DeadReason::NotFound)
        );
        assert_eq!(
            evaluate_liveness::<&str>(
                &id,
                Some("boot-a"),
                Ok(Some(ProcStat {
                    state: b'Z',
                    ..live_stat
                }))
            ),
            Liveness::Dead(DeadReason::Zombie)
        );
        assert_eq!(
            evaluate_liveness::<&str>(
                &id,
                Some("boot-a"),
                Ok(Some(ProcStat {
                    start_ticks: 2000,
                    ..live_stat
                }))
            ),
            Liveness::Dead(DeadReason::PidRecycled)
        );
        assert_eq!(
            evaluate_liveness(&id, Some("boot-a"), Err("procfs unavailable")),
            Liveness::Indeterminate
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn captures_and_verifies_current_process_identity() {
        let current = ProcessIdentity::current().unwrap();
        assert_eq!(current.pid, std::process::id());
        assert!(current.start_ticks > 0);
        assert_eq!(current.check(), Liveness::Alive);
    }
}
