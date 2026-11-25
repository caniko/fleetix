//! Generic, topology-neutral auto-healing primitives for Fleetix coordinators,
//! trains, and attended-operation spools.
//!
//! These building blocks encode the common-sense failure recovery policies that
//! prevent background trains and coordinators from wedging behind dead owners,
//! stalled child trees, superseded request backlogs, or abandoned leases —
//! while preserving fail-closed boundaries whenever an external mutation has an
//! unverified outcome.
//!
//! All core decision engines ([`liveness::evaluate_liveness`],
//! [`watchdog::StallWatchdog`], [`coalesce::coalesce_keyed`],
//! [`retry::RetryState`], [`reconcile::classify_stranded`], and
//! [`reconcile::YieldTracker`]) are pure, deterministic, and clock-injected so
//! consumers and tests can verify every recovery transition without live host
//! state.

pub mod coalesce;
pub mod liveness;
pub mod reconcile;
pub mod retry;
pub mod watchdog;

pub use coalesce::{
    CoalescePlan, CoalescePolicy, KeyedItem, SoloGroup, SupersededItem, coalesce_keyed,
};
pub use liveness::{DeadReason, Liveness, ProcStat, ProcessIdentity, evaluate_liveness};
pub use reconcile::{
    RecoveryAction, StrandedObservation, YieldDecision, YieldPolicy, YieldTracker,
    classify_stranded,
};
pub use retry::{FailureKind, RetryDecision, RetryPolicy, RetryState};
pub use watchdog::{ProgressSample, StallWatchdog, StuckProcess, WatchdogPolicy, WatchdogVerdict};

/// Send a `sd_notify(3)` datagram to `$NOTIFY_SOCKET` when running under
/// systemd (`READY=1`, `WATCHDOG=1`, `STOPPING=1`, `STATUS=...`).
///
/// Returns `Ok(true)` when a notification was sent, `Ok(false)` when
/// `NOTIFY_SOCKET` is unset or empty, and `Err` on transport errors.
/// Callers typically treat notification errors as non-fatal diagnostics so
/// standalone (non-systemd) execution works identically.
pub fn sd_notify(state: &str) -> Result<bool, String> {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixDatagram;

        let Some(socket_path) = std::env::var_os("NOTIFY_SOCKET") else {
            return Ok(false);
        };
        if socket_path.is_empty() {
            return Ok(false);
        }
        // Abstract sockets (`@name`) are Linux-specific; standard systemd unit
        // notification sockets use filesystem paths under `/run/systemd/notify`
        // or `$XDG_RUNTIME_DIR/systemd/notify`.
        let path = std::path::PathBuf::from(&socket_path);
        if path.to_string_lossy().starts_with('@') {
            return Ok(false);
        }
        let socket = UnixDatagram::unbound().map_err(|e| e.to_string())?;
        socket
            .send_to(state.as_bytes(), &path)
            .map_err(|e| e.to_string())?;
        Ok(true)
    }
    #[cfg(not(unix))]
    {
        let _ = state;
        Ok(false)
    }
}
