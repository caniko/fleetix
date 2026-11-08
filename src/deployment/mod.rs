//! Pure contract for a recoverable, unattended deployment.
//!
//! Nothing here performs I/O: no processes, filesystem access, activation, or
//! clock reads. Runtime code captures the actual previous identities, sends the
//! canonical [`Binding`] JSON to a root-installed admission command, persists
//! the returned [`Admission`] and the [`Transaction`], and performs the trial,
//! verification, commit, or restoration itself.
//!
//! Trust boundaries:
//!
//! - [`Plan`] is the untrusted desired request. It carries no trusted booleans;
//!   only the candidate, target, backend, and expected current identities.
//! - [`Binding`] adds the worker-captured previous identities. It is the exact
//!   request presented to the trusted verifier.
//! - [`Admission`] is verifier output. Core rejects unsafe values, requires an
//!   exact field-for-field binding, the canonical binding digest, and an
//!   unexpired admission. Core cannot cryptographically authenticate where the
//!   admission came from; the runtime must obtain it from the trusted command.
//! - Store-path and slug checks are shape checks only. They do not establish
//!   that a candidate or restoration target is safe; that judgement is delegated
//!   to the external verifier, which is why arbitrary changes are never inferred
//!   safe here.
//! - [`Transaction::before`] is fixed from the worker-captured binding and is
//!   never rewritten by confirmation, retry, timeout, or resume.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

#[cfg(all(unix, feature = "deployment-runtime"))]
mod native;
#[cfg(all(unix, feature = "deployment-runtime"))]
pub mod runtime;
#[cfg(all(unix, feature = "deployment-runtime"))]
mod storage;

/// Wire and persistence schema accepted by this contract.
pub const SCHEMA_VERSION: u32 = 1;

/// Deployment backend whose activation identity is being changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    Nixos,
    HomeManager,
}

/// Untrusted desired deployment request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Plan {
    pub version: u32,
    /// Attempt slug, at most 128 characters.
    pub id: String,
    /// Target slug.
    pub target: String,
    pub backend: Backend,
    /// Immutable candidate store path.
    pub candidate: String,
    /// Exact current system identity claimed by the request.
    pub expected_system: Option<String>,
    /// Exact current profile identity claimed by the request.
    pub expected_profile: Option<String>,
}

/// Canonical request presented to the trusted admission command.
///
/// The JSON is compact, UTF-8, in struct field order, with no trailing newline.
/// The verifier's `requestDigest` must be `sha256:<hex>` over those exact bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Binding {
    pub version: u32,
    pub target: String,
    pub attempt: String,
    pub backend: Backend,
    pub candidate: String,
    /// Exact current identities claimed by the request. Included so the
    /// verifier's digest binds them even though the admission response does not
    /// repeat them.
    pub expected_system: Option<String>,
    pub expected_profile: Option<String>,
    /// Actual previous system captured by the worker, which may differ from the
    /// request's expected snapshot.
    pub previous_system: Option<String>,
    /// Actual previous profile captured by the worker.
    pub previous_profile: Option<String>,
}

/// Trusted verifier output for one canonical binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Admission {
    pub version: u32,
    pub target: String,
    pub attempt: String,
    pub backend: Backend,
    pub candidate: String,
    pub previous_system: Option<String>,
    pub previous_profile: Option<String>,
    /// `sha256:<hex>` of the canonical [`Binding`] JSON.
    pub request_digest: String,
    pub expires_at_unix_seconds: u64,
    /// Trusted verifier output, never accepted from the request.
    pub configuration_only: bool,
    /// Trusted verifier output, never accepted from the request.
    pub rollback_compatible: bool,
}

/// Previous or observed activation identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivationIdentity {
    pub system: Option<String>,
    pub profile: Option<String>,
}

/// Explicit deployment transaction states. Terminals are never inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum State {
    Prepared,
    Trial,
    Committing,
    Restoring,
    Committed,
    Restored,
    RecoveryFailed,
}

impl State {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            State::Committed | State::Restored | State::RecoveryFailed
        )
    }
}

/// Successful or failed observations reported by the runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    TrialStarted,
    /// Activation and the external health verification succeeded; commit may
    /// begin.
    TrialSucceeded {
        observed_system: String,
        observed_profile: Option<String>,
    },
    TrialFailed {
        reason: String,
    },
    VerificationFailed {
        reason: String,
    },
    CommitFailed {
        reason: String,
    },
    Committed,
    Restored,
    RecoveryFailed {
        reason: String,
    },
    /// Explicit maintenance retry; normal recovery never clears this terminal.
    RetryRestoration,
}

impl Event {
    fn name(&self) -> &'static str {
        match self {
            Event::TrialStarted => "trialStarted",
            Event::TrialSucceeded { .. } => "trialSucceeded",
            Event::TrialFailed { .. } => "trialFailed",
            Event::VerificationFailed { .. } => "verificationFailed",
            Event::CommitFailed { .. } => "commitFailed",
            Event::Committed => "committed",
            Event::Restored => "restored",
            Event::RecoveryFailed { .. } => "recoveryFailed",
            Event::RetryRestoration => "retryRestoration",
        }
    }
}

/// Persisted single transaction from prepared through trial, commit, or restore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Transaction {
    pub version: u32,
    pub plan: Plan,
    pub binding: Binding,
    pub admission: Admission,
    /// Fixed worker-captured baseline. Never replaced by a later event.
    pub before: ActivationIdentity,
    /// Observed post-trial identity, set exactly once.
    pub after: Option<ActivationIdentity>,
    /// Written before the first activation command. Recovery must distinguish
    /// a pre-trial refusal from a partially executed activation.
    pub trial_started: bool,
    pub deadline_unix_seconds: u64,
    pub state: State,
    pub failure: Option<String>,
}

impl Plan {
    /// Build the canonical binding from worker-captured previous identities.
    pub fn bind(
        &self,
        previous_system: Option<String>,
        previous_profile: Option<String>,
    ) -> Result<Binding, DeploymentError> {
        self.check()?;
        check_required_identity(
            self.backend,
            previous_system.as_deref(),
            previous_profile.as_deref(),
            "previousSystem",
            "previousProfile",
        )?;
        Ok(Binding {
            version: SCHEMA_VERSION,
            target: self.target.clone(),
            attempt: self.id.clone(),
            backend: self.backend,
            candidate: self.candidate.clone(),
            expected_system: self.expected_system.clone(),
            expected_profile: self.expected_profile.clone(),
            previous_system,
            previous_profile,
        })
    }

    /// Validate the untrusted request and the trusted admission together.
    ///
    /// The expected identities are shape-checked but intentionally not compared
    /// with the worker-captured previous identities: snapshots may differ, and
    /// compatibility is the external verifier's judgement.
    pub fn validate(
        &self,
        binding: &Binding,
        admission: &Admission,
        now_unix_seconds: u64,
    ) -> Result<(), DeploymentError> {
        check_contract(self, binding, admission)?;
        if now_unix_seconds >= admission.expires_at_unix_seconds {
            return Err(DeploymentError::AdmissionExpired {
                now_unix_seconds,
                expires_at_unix_seconds: admission.expires_at_unix_seconds,
            });
        }
        Ok(())
    }
}

impl Binding {
    pub fn previous_identity(&self) -> ActivationIdentity {
        ActivationIdentity {
            system: self.previous_system.clone(),
            profile: self.previous_profile.clone(),
        }
    }

    /// Canonical JSON bytes sent to the trusted admission command.
    pub fn canonical_json(&self) -> Result<String, DeploymentError> {
        self.check()?;
        serde_json::to_string(self).map_err(|error| DeploymentError::InvalidField {
            field: "binding",
            reason: error.to_string(),
        })
    }

    /// `sha256:<hex>` over [`Binding::canonical_json`] bytes.
    pub fn digest(&self) -> Result<String, DeploymentError> {
        let canonical = self.canonical_json()?;
        Ok(sha256_digest(canonical.as_bytes()))
    }

    fn check(&self) -> Result<(), DeploymentError> {
        check_version(self.version)?;
        check_slug("target", &self.target, 128)?;
        check_slug("attempt", &self.attempt, 128)?;
        check_store_path("candidate", &self.candidate)?;
        check_required_identity(
            self.backend,
            self.expected_system.as_deref(),
            self.expected_profile.as_deref(),
            "expectedSystem",
            "expectedProfile",
        )?;
        check_required_identity(
            self.backend,
            self.previous_system.as_deref(),
            self.previous_profile.as_deref(),
            "previousSystem",
            "previousProfile",
        )
    }

    fn check_plan(&self, plan: &Plan) -> Result<(), DeploymentError> {
        if self.target != plan.target {
            return Err(DeploymentError::BindingMismatch { field: "target" });
        }
        if self.attempt != plan.id {
            return Err(DeploymentError::BindingMismatch { field: "attempt" });
        }
        if self.backend != plan.backend {
            return Err(DeploymentError::BindingMismatch { field: "backend" });
        }
        if self.candidate != plan.candidate {
            return Err(DeploymentError::BindingMismatch { field: "candidate" });
        }
        if self.expected_system != plan.expected_system {
            return Err(DeploymentError::BindingMismatch {
                field: "expectedSystem",
            });
        }
        if self.expected_profile != plan.expected_profile {
            return Err(DeploymentError::BindingMismatch {
                field: "expectedProfile",
            });
        }
        Ok(())
    }
}

impl Admission {
    fn check(&self, plan: &Plan, binding: &Binding, digest: &str) -> Result<(), DeploymentError> {
        check_version(self.version)?;
        check_slug("target", &self.target, 128)?;
        check_slug("attempt", &self.attempt, 128)?;
        check_store_path("candidate", &self.candidate)?;
        check_identity_shape(
            self.previous_system.as_deref(),
            self.previous_profile.as_deref(),
            "previousSystem",
            "previousProfile",
        )?;
        if !is_digest(&self.request_digest) {
            return Err(DeploymentError::InvalidField {
                field: "requestDigest",
                reason: "expected sha256:<64 lowercase hex>".into(),
            });
        }
        if self.expires_at_unix_seconds == 0 {
            return Err(DeploymentError::InvalidField {
                field: "expiresAtUnixSeconds",
                reason: "must be greater than zero".into(),
            });
        }

        let mismatch = |field: &'static str| -> Result<(), DeploymentError> {
            Err(DeploymentError::AdmissionMismatch { field })
        };
        if self.target != plan.target {
            return mismatch("target");
        }
        if self.attempt != plan.id {
            return mismatch("attempt");
        }
        if self.backend != plan.backend {
            return mismatch("backend");
        }
        if self.candidate != plan.candidate {
            return mismatch("candidate");
        }
        if self.previous_system != binding.previous_system {
            return mismatch("previousSystem");
        }
        if self.previous_profile != binding.previous_profile {
            return mismatch("previousProfile");
        }
        if self.request_digest != digest {
            return mismatch("requestDigest");
        }
        Ok(())
    }
}

fn check_contract(
    plan: &Plan,
    binding: &Binding,
    admission: &Admission,
) -> Result<(), DeploymentError> {
    plan.check()?;
    let digest = binding.digest()?;
    binding.check_plan(plan)?;
    admission.check(plan, binding, &digest)?;
    if !admission.configuration_only {
        return Err(DeploymentError::UnsafeAdmission {
            field: "configurationOnly",
        });
    }
    if !admission.rollback_compatible {
        return Err(DeploymentError::UnsafeAdmission {
            field: "rollbackCompatible",
        });
    }
    Ok(())
}

impl Transaction {
    /// Validate and create the initial `Prepared` transaction.
    pub fn prepare(
        plan: Plan,
        binding: Binding,
        admission: Admission,
        deadline_unix_seconds: u64,
        now_unix_seconds: u64,
    ) -> Result<Self, DeploymentError> {
        plan.validate(&binding, &admission, now_unix_seconds)?;
        if deadline_unix_seconds <= now_unix_seconds {
            return Err(DeploymentError::InvalidDeadline {
                now_unix_seconds,
                deadline_unix_seconds,
            });
        }
        Ok(Transaction {
            version: SCHEMA_VERSION,
            before: binding.previous_identity(),
            plan,
            binding,
            admission,
            after: None,
            trial_started: false,
            deadline_unix_seconds,
            state: State::Prepared,
            failure: None,
        })
    }

    /// Re-check every persisted invariant without requiring the original
    /// admission process. Admission expiry is enforced by preparation and by
    /// `advance` for active states; restoration and terminal records stay
    /// inspectable so recovery is never blocked by a stale approval.
    pub fn validate(&self) -> Result<(), DeploymentError> {
        check_version(self.version)?;
        check_contract(&self.plan, &self.binding, &self.admission)?;
        if self.before != self.binding.previous_identity() {
            return Err(DeploymentError::InvalidField {
                field: "before",
                reason: "does not match the worker-captured previous identity".into(),
            });
        }
        if let Some(after) = &self.after {
            if after.system.as_deref() != Some(self.plan.candidate.as_str()) {
                return Err(DeploymentError::InvalidField {
                    field: "after",
                    reason: "system does not match the candidate".into(),
                });
            }
            if let Some(profile) = &after.profile {
                check_profile_path("after", profile)?;
            }
        }
        if self.deadline_unix_seconds == 0 {
            return Err(DeploymentError::InvalidField {
                field: "deadlineUnixSeconds",
                reason: "must be greater than zero".into(),
            });
        }
        match self.state {
            State::Prepared | State::Trial => {
                if self.after.is_some() {
                    return Err(DeploymentError::InvalidField {
                        field: "after",
                        reason: "must be absent before trial success".into(),
                    });
                }
                if self.failure.is_some() {
                    return Err(DeploymentError::InvalidField {
                        field: "failure",
                        reason: "must be absent before a failure transition".into(),
                    });
                }
            }
            State::Committing | State::Committed => {
                if self.after.is_none() {
                    return Err(DeploymentError::InvalidField {
                        field: "after",
                        reason: "is required after trial success".into(),
                    });
                }
                if self.failure.is_some() {
                    return Err(DeploymentError::InvalidField {
                        field: "failure",
                        reason: "must be absent while committing or committed".into(),
                    });
                }
            }
            State::Restoring | State::Restored | State::RecoveryFailed => {
                if self.failure.is_none() {
                    return Err(DeploymentError::InvalidField {
                        field: "failure",
                        reason: "is required after entering restoration".into(),
                    });
                }
            }
        }
        if (self.state == State::Prepared && self.trial_started)
            || (matches!(
                self.state,
                State::Trial | State::Committing | State::Committed
            ) && !self.trial_started)
            || (self.after.is_some() && !self.trial_started)
        {
            return Err(DeploymentError::InvalidField {
                field: "trialStarted",
                reason: "does not match the recorded phase".into(),
            });
        }
        Ok(())
    }

    /// Advance exactly one event. Timeout and unhealthy/orphaned observations
    /// move to `Restoring` before returning the error; a commit can never be
    /// recorded after either. Persisted state must validate before advancing.
    pub fn advance(&mut self, event: Event, now_unix_seconds: u64) -> Result<(), DeploymentError> {
        self.validate()?;
        if self.state.is_terminal() {
            if self.state == State::RecoveryFailed && event == Event::RetryRestoration {
                self.state = State::Restoring;
                return Ok(());
            }
            return match (&self.state, &event) {
                (State::Committed, Event::Committed)
                | (State::Restored, Event::Restored)
                | (State::RecoveryFailed, Event::RecoveryFailed { .. }) => Ok(()),
                (from, event) => Err(DeploymentError::InvalidTransition {
                    from: *from,
                    event: event.name(),
                }),
            };
        }

        if matches!(
            self.state,
            State::Prepared | State::Trial | State::Committing
        ) && now_unix_seconds >= self.deadline_unix_seconds
            && !matches!(
                &event,
                Event::TrialFailed { .. }
                    | Event::VerificationFailed { .. }
                    | Event::CommitFailed { .. }
            )
        {
            self.state = State::Restoring;
            self.failure = Some(format!(
                "deadline {} exceeded before {}",
                self.deadline_unix_seconds,
                event.name()
            ));
            return Err(DeploymentError::TimedOut {
                deadline_unix_seconds: self.deadline_unix_seconds,
            });
        }

        if matches!(
            self.state,
            State::Prepared | State::Trial | State::Committing
        ) && now_unix_seconds >= self.admission.expires_at_unix_seconds
            && !matches!(
                &event,
                Event::TrialFailed { .. }
                    | Event::VerificationFailed { .. }
                    | Event::CommitFailed { .. }
            )
        {
            self.state = State::Restoring;
            self.failure = Some(format!(
                "admission expired at {} before {}",
                self.admission.expires_at_unix_seconds,
                event.name()
            ));
            return Err(DeploymentError::AdmissionExpired {
                now_unix_seconds,
                expires_at_unix_seconds: self.admission.expires_at_unix_seconds,
            });
        }

        match (self.state, event) {
            (State::Prepared, Event::TrialStarted) => {
                self.trial_started = true;
                self.state = State::Trial;
                Ok(())
            }
            (
                State::Trial,
                Event::TrialSucceeded {
                    observed_system,
                    observed_profile,
                },
            ) => {
                if observed_system != self.plan.candidate {
                    self.restore(format!(
                        "observed system {observed_system} does not match the candidate"
                    ));
                    return Err(DeploymentError::ObservationMismatch {
                        expected: self.plan.candidate.clone(),
                        observed: observed_system,
                    });
                }
                if let Err(error) = observed_profile
                    .as_deref()
                    .map(|profile| check_profile_path("observedProfile", profile))
                    .transpose()
                {
                    self.restore(format!("invalid observed profile: {error}"));
                    return Err(error);
                }
                self.after = Some(ActivationIdentity {
                    system: Some(observed_system),
                    profile: observed_profile,
                });
                self.state = State::Committing;
                Ok(())
            }
            (State::Trial, Event::TrialFailed { reason }) => {
                self.restore(reason);
                Ok(())
            }
            (
                State::Prepared | State::Trial | State::Committing,
                Event::VerificationFailed { reason },
            ) => {
                self.restore(reason);
                Ok(())
            }
            (State::Committing, Event::CommitFailed { reason }) => {
                self.restore(reason);
                Ok(())
            }
            (State::Committing, Event::Committed) => {
                self.state = State::Committed;
                Ok(())
            }
            (State::Restoring, Event::Restored) => {
                self.state = State::Restored;
                Ok(())
            }
            (State::Restoring, Event::RecoveryFailed { reason }) => {
                self.failure = Some(reason);
                self.state = State::RecoveryFailed;
                Ok(())
            }
            (from, event) => Err(DeploymentError::InvalidTransition {
                from,
                event: event.name(),
            }),
        }
    }

    fn restore(&mut self, reason: String) {
        self.failure = Some(reason);
        self.state = State::Restoring;
    }
}

/// Fail-closed contract errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeploymentError {
    UnsupportedSchema {
        found: u32,
    },
    InvalidField {
        field: &'static str,
        reason: String,
    },
    BindingMismatch {
        field: &'static str,
    },
    AdmissionMismatch {
        field: &'static str,
    },
    UnsafeAdmission {
        field: &'static str,
    },
    AdmissionExpired {
        now_unix_seconds: u64,
        expires_at_unix_seconds: u64,
    },
    InvalidDeadline {
        now_unix_seconds: u64,
        deadline_unix_seconds: u64,
    },
    InvalidTransition {
        from: State,
        event: &'static str,
    },
    TimedOut {
        deadline_unix_seconds: u64,
    },
    ObservationMismatch {
        expected: String,
        observed: String,
    },
}

impl fmt::Display for DeploymentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeploymentError::UnsupportedSchema { found } => {
                write!(f, "unsupported deployment schema version {found}")
            }
            DeploymentError::InvalidField { field, reason } => {
                write!(f, "invalid {field}: {reason}")
            }
            DeploymentError::BindingMismatch { field } => {
                write!(f, "binding {field} does not match the request")
            }
            DeploymentError::AdmissionMismatch { field } => {
                write!(f, "admission {field} is not bound to this request")
            }
            DeploymentError::UnsafeAdmission { field } => {
                write!(f, "trusted admission rejected {field}")
            }
            DeploymentError::AdmissionExpired {
                now_unix_seconds,
                expires_at_unix_seconds,
            } => write!(
                f,
                "admission expired at {expires_at_unix_seconds} (now {now_unix_seconds})"
            ),
            DeploymentError::InvalidDeadline {
                now_unix_seconds,
                deadline_unix_seconds,
            } => write!(
                f,
                "deadline {deadline_unix_seconds} is not after {now_unix_seconds}"
            ),
            DeploymentError::InvalidTransition { from, event } => {
                write!(f, "event {event} is invalid in state {from:?}")
            }
            DeploymentError::TimedOut {
                deadline_unix_seconds,
            } => write!(f, "deadline {deadline_unix_seconds} exceeded"),
            DeploymentError::ObservationMismatch { expected, observed } => {
                write!(f, "observed {observed}, expected candidate {expected}")
            }
        }
    }
}

impl std::error::Error for DeploymentError {}

fn check_version(found: u32) -> Result<(), DeploymentError> {
    if found == SCHEMA_VERSION {
        Ok(())
    } else {
        Err(DeploymentError::UnsupportedSchema { found })
    }
}

impl Plan {
    fn check(&self) -> Result<(), DeploymentError> {
        check_version(self.version)?;
        check_slug("id", &self.id, 128)?;
        check_slug("target", &self.target, 128)?;
        check_store_path("candidate", &self.candidate)?;
        check_required_identity(
            self.backend,
            self.expected_system.as_deref(),
            self.expected_profile.as_deref(),
            "expectedSystem",
            "expectedProfile",
        )
    }
}

fn check_required_identity(
    backend: Backend,
    system: Option<&str>,
    profile: Option<&str>,
    system_field: &'static str,
    profile_field: &'static str,
) -> Result<(), DeploymentError> {
    match backend {
        Backend::Nixos if system.is_none() => {
            return Err(DeploymentError::InvalidField {
                field: system_field,
                reason: "a NixOS identity requires a system".into(),
            });
        }
        Backend::HomeManager if profile.is_none() => {
            return Err(DeploymentError::InvalidField {
                field: profile_field,
                reason: "a home-manager identity requires a profile".into(),
            });
        }
        _ => {}
    }
    check_identity_shape(system, profile, system_field, profile_field)
}

fn check_identity_shape(
    system: Option<&str>,
    profile: Option<&str>,
    system_field: &'static str,
    profile_field: &'static str,
) -> Result<(), DeploymentError> {
    if system.is_none() && profile.is_none() {
        return Err(DeploymentError::InvalidField {
            field: system_field,
            reason: "at least one activation identity is required".into(),
        });
    }
    if let Some(system) = system {
        check_store_path(system_field, system)?;
    }
    if let Some(profile) = profile {
        check_profile_path(profile_field, profile)?;
    }
    Ok(())
}

fn check_slug(field: &'static str, value: &str, max_len: usize) -> Result<(), DeploymentError> {
    let valid = !value.is_empty()
        && value.len() <= max_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && value
            .as_bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric());
    if valid {
        Ok(())
    } else {
        Err(DeploymentError::InvalidField {
            field,
            reason: format!("expected a lowercase alphanumeric/hyphen slug of at most {max_len}"),
        })
    }
}

fn check_store_path(field: &'static str, value: &str) -> Result<(), DeploymentError> {
    if is_store_path(value) {
        Ok(())
    } else {
        Err(DeploymentError::InvalidField {
            field,
            reason: "expected an immutable /nix/store/<32-hash>-<name> path".into(),
        })
    }
}

fn check_profile_path(field: &'static str, value: &str) -> Result<(), DeploymentError> {
    // A profile pointer can advance during the transaction. Recovery must bind
    // its captured immutable target, never the mutable pointer itself.
    check_store_path(field, value)
}

fn is_store_path(value: &str) -> bool {
    let Some(name) = value.strip_prefix("/nix/store/") else {
        return false;
    };
    let bytes = name.as_bytes();
    if bytes.len() < 34 || bytes[32] != b'-' || name.contains('/') {
        return false;
    }
    let nix_base32 = |byte: u8| {
        matches!(
            byte,
            b'0'..=b'9' | b'a'..=b'd' | b'f'..=b'n' | b'p'..=b's' | b'v'..=b'z'
        )
    };
    if !bytes[..32].iter().copied().all(nix_base32) {
        return false;
    }
    name[33..].bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.' | b'_' | b'?' | b'=')
    })
}

fn is_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

fn sha256_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{digest:x}")
}
