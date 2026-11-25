//! Safe stranded-work classification and interactive-priority yield fairness.
//!
//! [`classify_stranded`] separates provably safe auto-healing transitions
//! (harvesting a finished detached unit's status, requeuing a non-mutating job
//! whose driver died before launch, completing an interrupted drop, or stopping
//! a stalled non-mutating worker) from states that MUST fail closed for
//! operator inspection (unverified dispatched mutations or failed probes).
//!
//! [`YieldTracker`] lets background trains step aside at safe boundaries when
//! higher-priority attended operator work is waiting, with a bounded cap so
//! neither class starves.

use super::liveness::Liveness;
use super::watchdog::{StuckProcess, WatchdogVerdict};
use serde::{Deserialize, Serialize};

/// Observed state of an in-flight spool entry or train reservation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrandedObservation {
    /// A verified terminal `done/` record already exists for this entry.
    pub has_done_record: bool,
    /// The entry is sitting in `dropping/` from an interrupted drop.
    pub dropping: bool,
    /// An irreversible or external mutating phase (e.g. host activation,
    /// checkout commit, attended root step) was dispatched without a verified
    /// outcome yet.
    pub mutating_dispatched: bool,
    /// Liveness of the driver/coordinator process holding the reservation.
    pub owner_liveness: Liveness,
    /// Whether the detached worker unit (if any) is still active.
    /// `None` means the probe failed and callers must fail closed.
    pub worker_active: Option<bool>,
    /// Recorded exit status of the detached worker unit, if any.
    pub exit_status: Option<i32>,
    /// Current stall/runtime watchdog verdict for an active worker.
    pub watchdog: WatchdogVerdict,
}

/// Safe, auditable auto-healing action for a stranded or supervised entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum RecoveryAction {
    /// Entry already has a terminal record; clean up residual running/lease state.
    AlreadyTerminal,
    /// Complete an interrupted drop cleanly.
    CompleteDrop,
    /// Owner or worker is alive and healthy; leave untouched.
    KeepRunning,
    /// Detached worker exited with a recorded status before its driver died;
    /// settle the entry from `exit_code` and release its lease.
    HarvestOutcome { exit_code: i32 },
    /// Driver died with no active worker and no unverified mutation; safe to
    /// move back to `pending/` and release the stale reservation.
    RequeueAndRelease { reason: String },
    /// Active non-mutating worker tripped the stall or runtime watchdog; stop
    /// it cleanly, record diagnostics, and apply retry/quarantine policy.
    StopStalledWorker {
        reason: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        diagnostics: Vec<StuckProcess>,
    },
    /// Fail closed: retain the reservation and require operator reconciliation.
    RequireOperator { reason: String },
}

/// Classify the safest automatic recovery action for an observed in-flight item.
pub fn classify_stranded(obs: &StrandedObservation) -> RecoveryAction {
    if obs.has_done_record {
        return RecoveryAction::AlreadyTerminal;
    }

    match obs.worker_active {
        None => {
            return RecoveryAction::RequireOperator {
                reason: "cannot verify detached worker unit state; reservation retained".into(),
            };
        }
        Some(true) => {
            return match &obs.watchdog {
                WatchdogVerdict::Healthy => RecoveryAction::KeepRunning,
                WatchdogVerdict::Stalled {
                    silent_seconds,
                    stuck_processes,
                } => {
                    if obs.mutating_dispatched {
                        RecoveryAction::RequireOperator {
                            reason: format!(
                                "mutating worker appears stalled for {silent_seconds}s; refusing automatic kill without checkpoint verification"
                            ),
                        }
                    } else {
                        RecoveryAction::StopStalledWorker {
                            reason: format!(
                                "non-mutating worker stalled with no observable progress for {silent_seconds}s"
                            ),
                            diagnostics: stuck_processes.clone(),
                        }
                    }
                }
                WatchdogVerdict::RuntimeExceeded {
                    elapsed_seconds,
                    limit_seconds,
                } => {
                    if obs.mutating_dispatched {
                        RecoveryAction::RequireOperator {
                            reason: format!(
                                "mutating worker exceeded runtime budget ({elapsed_seconds}s >= {limit_seconds}s); refusing automatic kill"
                            ),
                        }
                    } else {
                        RecoveryAction::StopStalledWorker {
                            reason: format!(
                                "non-mutating worker exceeded runtime budget ({elapsed_seconds}s >= {limit_seconds}s)"
                            ),
                            diagnostics: Vec::new(),
                        }
                    }
                }
            };
        }
        Some(false) => {}
    }

    // Detached worker is confirmed inactive.
    if obs.dropping && obs.owner_liveness.is_dead() {
        return RecoveryAction::CompleteDrop;
    }

    match obs.owner_liveness {
        Liveness::Alive => RecoveryAction::KeepRunning,
        Liveness::Indeterminate => RecoveryAction::RequireOperator {
            reason: "cannot verify owner process liveness; reservation retained".into(),
        },
        Liveness::Dead(dead_reason) => {
            if let Some(exit_code) = obs.exit_status {
                return RecoveryAction::HarvestOutcome { exit_code };
            }
            if obs.mutating_dispatched {
                return RecoveryAction::RequireOperator {
                    reason: format!(
                        "owner died ({dead_reason}) after mutating phase was dispatched with no recorded exit status"
                    ),
                };
            }
            RecoveryAction::RequeueAndRelease {
                reason: format!(
                    "owner died ({dead_reason}) with inactive worker and no dispatched mutation"
                ),
            }
        }
    }
}

/// Fairness policy for yielding background work to interactive/attended queues.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YieldPolicy {
    /// Maximum consecutive seconds background work yields before taking a turn
    /// (`0` disables yielding).
    pub max_yield_seconds: u64,
}

impl Default for YieldPolicy {
    fn default() -> Self {
        Self {
            max_yield_seconds: 15 * 60,
        }
    }
}

/// Decision returned by [`YieldTracker::evaluate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "kebab-case")]
pub enum YieldDecision {
    Proceed,
    Yield { remaining_seconds: u64 },
}

/// Pure, clock-injected tracker preventing interactive-op starvation while
/// capping indefinite background starvation.
#[derive(Clone, Debug, Default)]
pub struct YieldTracker {
    policy: YieldPolicy,
    yielding_since: Option<u64>,
}

impl YieldTracker {
    pub fn new(policy: YieldPolicy) -> Self {
        Self {
            policy,
            yielding_since: None,
        }
    }

    pub fn evaluate(&mut self, higher_priority_waiting: bool, now: u64) -> YieldDecision {
        if !higher_priority_waiting || self.policy.max_yield_seconds == 0 {
            self.yielding_since = None;
            return YieldDecision::Proceed;
        }
        let since = *self.yielding_since.get_or_insert(now);
        let elapsed = now.saturating_sub(since);
        if elapsed >= self.policy.max_yield_seconds {
            self.yielding_since = None;
            YieldDecision::Proceed
        } else {
            YieldDecision::Yield {
                remaining_seconds: self.policy.max_yield_seconds - elapsed,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::healing::liveness::DeadReason;

    fn base() -> StrandedObservation {
        StrandedObservation {
            has_done_record: false,
            dropping: false,
            mutating_dispatched: false,
            owner_liveness: Liveness::Dead(DeadReason::NotFound),
            worker_active: Some(false),
            exit_status: None,
            watchdog: WatchdogVerdict::Healthy,
        }
    }

    #[test]
    fn auto_heals_safe_stranded_states_and_fails_closed_on_mutations() {
        assert_eq!(
            classify_stranded(&StrandedObservation {
                has_done_record: true,
                ..base()
            }),
            RecoveryAction::AlreadyTerminal
        );
        assert_eq!(
            classify_stranded(&StrandedObservation {
                dropping: true,
                ..base()
            }),
            RecoveryAction::CompleteDrop
        );
        assert_eq!(
            classify_stranded(&StrandedObservation {
                exit_status: Some(0),
                ..base()
            }),
            RecoveryAction::HarvestOutcome { exit_code: 0 }
        );
        assert!(matches!(
            classify_stranded(&base()),
            RecoveryAction::RequeueAndRelease { .. }
        ));
        assert!(matches!(
            classify_stranded(&StrandedObservation {
                mutating_dispatched: true,
                ..base()
            }),
            RecoveryAction::RequireOperator { .. }
        ));
        assert!(matches!(
            classify_stranded(&StrandedObservation {
                worker_active: None,
                ..base()
            }),
            RecoveryAction::RequireOperator { .. }
        ));
        assert!(matches!(
            classify_stranded(&StrandedObservation {
                owner_liveness: Liveness::Indeterminate,
                ..base()
            }),
            RecoveryAction::RequireOperator { .. }
        ));
        assert!(matches!(
            classify_stranded(&StrandedObservation {
                worker_active: Some(true),
                watchdog: WatchdogVerdict::Stalled {
                    silent_seconds: 900,
                    stuck_processes: vec![],
                },
                ..base()
            }),
            RecoveryAction::StopStalledWorker { .. }
        ));
        assert!(matches!(
            classify_stranded(&StrandedObservation {
                worker_active: Some(true),
                mutating_dispatched: true,
                watchdog: WatchdogVerdict::Stalled {
                    silent_seconds: 900,
                    stuck_processes: vec![],
                },
                ..base()
            }),
            RecoveryAction::RequireOperator { .. }
        ));
    }

    #[test]
    fn yield_tracker_defers_for_interactive_work_up_to_fairness_cap() {
        let mut tracker = YieldTracker::new(YieldPolicy {
            max_yield_seconds: 60,
        });
        assert_eq!(tracker.evaluate(false, 100), YieldDecision::Proceed);
        assert_eq!(
            tracker.evaluate(true, 100),
            YieldDecision::Yield {
                remaining_seconds: 60
            }
        );
        assert_eq!(
            tracker.evaluate(true, 145),
            YieldDecision::Yield {
                remaining_seconds: 15
            }
        );
        assert_eq!(tracker.evaluate(true, 160), YieldDecision::Proceed);
        // After taking a turn, a fresh interactive wait starts a new bounded window.
        assert_eq!(
            tracker.evaluate(true, 170),
            YieldDecision::Yield {
                remaining_seconds: 60
            }
        );
        assert_eq!(tracker.evaluate(false, 180), YieldDecision::Proceed);
    }
}
