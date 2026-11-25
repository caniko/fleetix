//! Bounded retry, capped exponential backoff, and quarantine (circuit breaker).
//!
//! Transient faults (capacity contention, daemon restarts, or a single killed
//! deadlock) should heal automatically with backoff, while repeatedly failing
//! or stalling items must quarantine after a bounded number of attempts so one
//! poisoned job cannot loop forever and starve unrelated work.

use serde::{Deserialize, Serialize};

/// Bounded retry and backoff parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryPolicy {
    /// Maximum total attempts before quarantining (`1` = no retries).
    pub max_attempts: u32,
    /// Initial backoff delay in seconds after the first retryable failure.
    pub base_backoff_seconds: u64,
    /// Upper bound on backoff delay in seconds.
    pub max_backoff_seconds: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_backoff_seconds: 15,
            max_backoff_seconds: 300,
        }
    }
}

/// Classification of a failed execution attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureKind {
    /// Retryable environmental failure (capacity busy, lock contention, daemon restart).
    Transient,
    /// Killed by the stall/runtime watchdog; retryable up to `max_attempts`.
    Stalled,
    /// Deterministic failure (evaluation/compilation/validation error); never retried.
    Terminal,
}

/// Persisted per-item or per-target retry state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryState {
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_retry_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub quarantined: bool,
}

/// Outcome of recording a failure against [`RetryState`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "kebab-case")]
pub enum RetryDecision {
    RetryAfter { delay_seconds: u64, attempt: u32 },
    Quarantine { attempts: u32, reason: String },
    FailTerminal { reason: String },
}

impl RetryState {
    /// Whether this item is eligible to run at timestamp `now`.
    pub fn ready_at(&self, now: u64) -> bool {
        !self.quarantined && self.next_retry_at.is_none_or(|until| now >= until)
    }

    /// Clear transient failure counters after a successful attempt.
    pub fn record_success(&mut self) {
        self.attempts = 0;
        self.last_failure_at = None;
        self.next_retry_at = None;
        self.last_error = None;
        self.quarantined = false;
    }

    /// Record a failed attempt and decide whether to retry with backoff, fail
    /// terminally, or quarantine the item.
    pub fn record_failure(
        &mut self,
        policy: &RetryPolicy,
        kind: FailureKind,
        now: u64,
        reason: String,
    ) -> RetryDecision {
        self.attempts = self.attempts.saturating_add(1);
        self.last_failure_at = Some(now);
        self.last_error = Some(reason.clone());

        if kind == FailureKind::Terminal {
            self.next_retry_at = None;
            return RetryDecision::FailTerminal { reason };
        }

        let max_attempts = policy.max_attempts.max(1);
        if self.attempts >= max_attempts {
            self.quarantined = true;
            self.next_retry_at = None;
            return RetryDecision::Quarantine {
                attempts: self.attempts,
                reason,
            };
        }

        let shift = self.attempts.saturating_sub(1).min(20);
        let multiplier = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
        let delay = policy
            .base_backoff_seconds
            .saturating_mul(multiplier)
            .min(policy.max_backoff_seconds);
        self.next_retry_at = Some(now.saturating_add(delay));
        RetryDecision::RetryAfter {
            delay_seconds: delay,
            attempt: self.attempts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_with_capped_backoff_then_quarantines() {
        let policy = RetryPolicy {
            max_attempts: 3,
            base_backoff_seconds: 10,
            max_backoff_seconds: 25,
        };
        let mut state = RetryState::default();
        assert!(state.ready_at(100));

        assert_eq!(
            state.record_failure(&policy, FailureKind::Stalled, 100, "hung".into()),
            RetryDecision::RetryAfter {
                delay_seconds: 10,
                attempt: 1,
            }
        );
        assert!(!state.ready_at(109));
        assert!(state.ready_at(110));

        assert_eq!(
            state.record_failure(&policy, FailureKind::Transient, 110, "busy".into()),
            RetryDecision::RetryAfter {
                delay_seconds: 20,
                attempt: 2,
            }
        );
        assert!(!state.ready_at(129));
        assert!(state.ready_at(130));

        assert_eq!(
            state.record_failure(&policy, FailureKind::Stalled, 130, "hung again".into()),
            RetryDecision::Quarantine {
                attempts: 3,
                reason: "hung again".into(),
            }
        );
        assert!(!state.ready_at(10_000));
    }

    #[test]
    fn terminal_failure_never_retries() {
        let mut state = RetryState::default();
        assert_eq!(
            state.record_failure(
                &RetryPolicy::default(),
                FailureKind::Terminal,
                50,
                "assertion failed".into()
            ),
            RetryDecision::FailTerminal {
                reason: "assertion failed".into(),
            }
        );
    }
}
