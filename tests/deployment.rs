//! Contract tests for the pure recoverable-deployment state machine.

use fleetix::deployment::{
    ActivationIdentity, Admission, Backend, Binding, DeploymentError, Event, Plan, SCHEMA_VERSION,
    State, Transaction,
};

const CANDIDATE: &str = "/nix/store/00000000000000000000000000000000-candidate";
const PREVIOUS: &str = "/nix/store/11111111111111111111111111111111-previous";
const OTHER: &str = "/nix/store/22222222222222222222222222222222-other";
const PROFILE: &str = "/nix/store/33333333333333333333333333333333-boot-generation";
const OTHER_PROFILE: &str = "/nix/store/44444444444444444444444444444444-other-boot-generation";

#[test]
fn restoration_references_must_be_immutable_not_mutable_profile_links() {
    let mut request = plan();
    request.expected_profile = Some("/nix/var/nix/profiles/system".into());
    assert!(
        request
            .bind(Some(PREVIOUS.into()), Some(PROFILE.into()))
            .is_err()
    );
    assert!(
        plan()
            .bind(
                Some(PREVIOUS.into()),
                Some("/nix/var/nix/profiles/system".into())
            )
            .is_err()
    );
}

#[test]
fn pretrial_expiry_retains_proof_that_activation_never_started() {
    let mut transaction = prepared_at(1_010, 1_000);
    assert!(transaction.advance(Event::TrialStarted, 1_010).is_err());
    assert_eq!(transaction.state, State::Restoring);
    assert!(!transaction.trial_started);
}

#[test]
fn only_an_explicit_maintenance_event_can_retry_failed_restoration() {
    let mut transaction = prepared();
    let original = transaction.before.clone();
    transaction.advance(Event::TrialStarted, 1_001).unwrap();
    transaction
        .advance(
            Event::TrialFailed {
                reason: "unhealthy".into(),
            },
            1_002,
        )
        .unwrap();
    transaction
        .advance(
            Event::RecoveryFailed {
                reason: "restore hook failed".into(),
            },
            1_003,
        )
        .unwrap();
    assert!(transaction.advance(Event::TrialStarted, 1_004).is_err());
    transaction.advance(Event::RetryRestoration, 1_004).unwrap();
    assert_eq!(transaction.before, original);
    assert_eq!(transaction.state, State::Restoring);
    transaction.advance(Event::Restored, 1_005).unwrap();
}

fn plan() -> Plan {
    Plan {
        version: SCHEMA_VERSION,
        id: "deploy-2026-11-06".into(),
        target: "atlas".into(),
        backend: Backend::Nixos,
        candidate: CANDIDATE.into(),
        expected_system: Some(PREVIOUS.into()),
        expected_profile: Some(PROFILE.into()),
    }
}

fn binding() -> Binding {
    plan()
        .bind(Some(PREVIOUS.into()), Some(PROFILE.into()))
        .expect("fixture must bind")
}

fn admission_for(binding: &Binding) -> Admission {
    Admission {
        version: SCHEMA_VERSION,
        target: binding.target.clone(),
        attempt: binding.attempt.clone(),
        backend: binding.backend,
        candidate: binding.candidate.clone(),
        previous_system: binding.previous_system.clone(),
        previous_profile: binding.previous_profile.clone(),
        request_digest: binding.digest().expect("fixture must digest"),
        expires_at_unix_seconds: 10_000,
        configuration_only: true,
        rollback_compatible: true,
    }
}

fn admission() -> Admission {
    admission_for(&binding())
}

fn prepared_at(deadline_unix_seconds: u64, now_unix_seconds: u64) -> Transaction {
    Transaction::prepare(
        plan(),
        binding(),
        admission(),
        deadline_unix_seconds,
        now_unix_seconds,
    )
    .expect("fixture must prepare")
}

fn prepared() -> Transaction {
    prepared_at(2_000, 1_000)
}

fn assert_invalid_field<T: std::fmt::Debug>(result: Result<T, DeploymentError>, expected: &str) {
    match result {
        Err(DeploymentError::InvalidField { field, .. }) => assert_eq!(field, expected),
        other => panic!("expected invalid field {expected}, got {other:?}"),
    }
}

fn digest_of_zeros() -> String {
    format!("sha256:{}", "0".repeat(64))
}

#[test]
fn unknown_schema_unsafe_admission_and_request_booleans_are_rejected() {
    let mut candidate = plan();
    candidate.version = 99;
    assert_eq!(
        candidate
            .validate(&binding(), &admission(), 1_000)
            .unwrap_err(),
        DeploymentError::UnsupportedSchema { found: 99 }
    );

    let mut candidate = admission();
    candidate.version = 99;
    assert_eq!(
        plan().validate(&binding(), &candidate, 1_000).unwrap_err(),
        DeploymentError::UnsupportedSchema { found: 99 }
    );

    let mut unknown_binding = binding();
    unknown_binding.version = 99;
    assert_eq!(
        plan()
            .validate(&unknown_binding, &admission(), 1_000)
            .unwrap_err(),
        DeploymentError::UnsupportedSchema { found: 99 }
    );

    let mut denied = admission();
    denied.configuration_only = false;
    assert_eq!(
        plan().validate(&binding(), &denied, 1_000).unwrap_err(),
        DeploymentError::UnsafeAdmission {
            field: "configurationOnly"
        }
    );

    let mut unrollbackable = admission();
    unrollbackable.rollback_compatible = false;
    assert_eq!(
        plan()
            .validate(&binding(), &unrollbackable, 1_000)
            .unwrap_err(),
        DeploymentError::UnsafeAdmission {
            field: "rollbackCompatible"
        }
    );

    for forbidden in ["configurationOnly", "rollbackCompatible", "effects"] {
        let mut request = serde_json::to_value(plan()).unwrap();
        request[forbidden] = true.into();
        assert!(
            serde_json::from_value::<Plan>(request).is_err(),
            "request must not accept trusted field {forbidden}"
        );
    }

    let mut trusted = serde_json::to_value(admission()).unwrap();
    trusted
        .as_object_mut()
        .unwrap()
        .remove("rollbackCompatible");
    assert!(
        serde_json::from_value::<Admission>(trusted).is_err(),
        "the trusted verifier must return rollbackCompatible explicitly"
    );
}

#[test]
fn invalid_ids_paths_and_baselines_are_rejected() {
    for id in [
        String::new(),
        "Deploy".into(),
        "deploy_one".into(),
        "deploy/edge".into(),
        "-deploy".into(),
        "deploy-".into(),
        "a".repeat(129),
    ] {
        let mut candidate = plan();
        candidate.id = id;
        assert_invalid_field(candidate.validate(&binding(), &admission(), 1_000), "id");
    }

    for target in ["", "Atlas", "atlas.edge", "atlas/edge", "-atlas", "atlas-"] {
        let mut candidate = plan();
        candidate.target = target.into();
        assert_invalid_field(
            candidate.validate(&binding(), &admission(), 1_000),
            "target",
        );
    }

    for candidate in [
        "nix/store/00000000000000000000000000000000-candidate",
        "/nix/store/../etc/passwd",
        "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-candidate",
        "/nix/store/0000000000000000000000000000000-short",
        "/nix/store/00000000000000000000000000000000-",
    ] {
        let mut selected = plan();
        selected.candidate = candidate.into();
        assert_invalid_field(
            selected.validate(&binding(), &admission(), 1_000),
            "candidate",
        );
    }

    let mut selected = plan();
    selected.expected_system = Some("relative/system".into());
    assert_invalid_field(
        selected.validate(&binding(), &admission(), 1_000),
        "expectedSystem",
    );

    let mut selected = plan();
    selected.expected_profile = Some("relative/profile".into());
    assert_invalid_field(
        selected.validate(&binding(), &admission(), 1_000),
        "expectedProfile",
    );

    let mut selected = plan();
    selected.expected_profile = Some("/nix/var/nix/profiles/../escape".into());
    assert_invalid_field(
        selected.validate(&binding(), &admission(), 1_000),
        "expectedProfile",
    );

    let mut selected = plan();
    selected.expected_system = None;
    assert_invalid_field(
        selected.validate(&binding(), &admission(), 1_000),
        "expectedSystem",
    );

    let mut selected = plan();
    selected.backend = Backend::HomeManager;
    selected.expected_system = None;
    selected.expected_profile = None;
    assert_invalid_field(
        selected.validate(&binding(), &admission(), 1_000),
        "expectedProfile",
    );

    assert_invalid_field(plan().bind(None, Some(PROFILE.into())), "previousSystem");
    assert_invalid_field(
        plan().bind(Some("relative/system".into()), Some(PROFILE.into())),
        "previousSystem",
    );
    assert_invalid_field(
        plan().bind(Some(PREVIOUS.into()), Some("relative/profile".into())),
        "previousProfile",
    );

    let system_only = plan()
        .bind(Some(PREVIOUS.into()), None)
        .expect("NixOS may capture a system identity without a persisted profile");
    assert_eq!(system_only.previous_system.as_deref(), Some(PREVIOUS));
    assert!(system_only.previous_profile.is_none());
    let admission = admission_for(&system_only);
    plan()
        .validate(&system_only, &admission, 1_000)
        .expect("a system-only previous identity is a complete NixOS baseline");
}

#[test]
fn binding_and_admission_must_be_exactly_bound_to_the_request() {
    for field in [
        "target",
        "attempt",
        "backend",
        "candidate",
        "expectedSystem",
        "expectedProfile",
    ] {
        let mut proof = binding();
        match field {
            "target" => proof.target = "other-host".into(),
            "attempt" => proof.attempt = "other-attempt".into(),
            "backend" => proof.backend = Backend::HomeManager,
            "candidate" => proof.candidate = OTHER.into(),
            "expectedSystem" => proof.expected_system = Some(OTHER.into()),
            "expectedProfile" => proof.expected_profile = Some(OTHER_PROFILE.into()),
            _ => unreachable!(),
        }
        assert_eq!(
            plan().validate(&proof, &admission(), 1_000).unwrap_err(),
            DeploymentError::BindingMismatch { field }
        );
    }

    for field in [
        "target",
        "attempt",
        "backend",
        "candidate",
        "previousSystem",
        "previousProfile",
        "requestDigest",
    ] {
        let mut proof = admission();
        match field {
            "target" => proof.target = "other-host".into(),
            "attempt" => proof.attempt = "other-attempt".into(),
            "backend" => proof.backend = Backend::HomeManager,
            "candidate" => proof.candidate = OTHER.into(),
            "previousSystem" => proof.previous_system = Some(OTHER.into()),
            "previousProfile" => proof.previous_profile = Some(OTHER_PROFILE.into()),
            "requestDigest" => proof.request_digest = digest_of_zeros(),
            _ => unreachable!(),
        }
        assert_eq!(
            plan().validate(&binding(), &proof, 1_000).unwrap_err(),
            DeploymentError::AdmissionMismatch { field }
        );
    }
}

#[test]
fn admission_requires_an_unexpired_well_formed_digest() {
    let mut expired = admission();
    expired.expires_at_unix_seconds = 1_000;
    assert_eq!(
        plan().validate(&binding(), &expired, 1_000).unwrap_err(),
        DeploymentError::AdmissionExpired {
            now_unix_seconds: 1_000,
            expires_at_unix_seconds: 1_000
        }
    );

    let mut malformed = admission();
    malformed.request_digest = "not-a-digest".into();
    assert_invalid_field(
        plan().validate(&binding(), &malformed, 1_000),
        "requestDigest",
    );

    let mut malformed = admission();
    malformed.previous_profile = Some("relative/profile".into());
    assert_invalid_field(
        plan().validate(&binding(), &malformed, 1_000),
        "previousProfile",
    );

    let mut expired = admission();
    expired.expires_at_unix_seconds = 0;
    assert_invalid_field(
        plan().validate(&binding(), &expired, 1_000),
        "expiresAtUnixSeconds",
    );
}

#[test]
fn actual_previous_identities_may_differ_from_the_expected_snapshot() {
    let binding = plan()
        .bind(Some(OTHER.into()), Some(OTHER_PROFILE.into()))
        .expect("worker-captured previous identities must bind");
    let admission = admission_for(&binding);

    plan()
        .validate(&binding, &admission, 1_000)
        .expect("admission is bound to the actual previous identities");

    let transaction = Transaction::prepare(plan(), binding, admission, 2_000, 1_000).unwrap();
    assert_eq!(
        transaction.before,
        ActivationIdentity {
            system: Some(OTHER.into()),
            profile: Some(OTHER_PROFILE.into()),
        }
    );
    assert_ne!(
        transaction.before.system, transaction.plan.expected_system,
        "the baseline is the captured previous identity, not the expected snapshot"
    );
}

#[test]
fn home_manager_accepts_a_profile_only_baseline() {
    let mut request = plan();
    request.backend = Backend::HomeManager;
    request.expected_system = None;
    request.expected_profile = Some(PROFILE.into());
    let binding = request.bind(None, Some(PROFILE.into())).unwrap();
    let admission = admission_for(&binding);
    assert_eq!(
        serde_json::to_value(Backend::HomeManager).unwrap(),
        serde_json::json!("home-manager")
    );

    request
        .validate(&binding, &admission, 1_000)
        .expect("profile baseline is complete for home-manager");
    assert_eq!(request.backend, Backend::HomeManager);
    assert_eq!(request.id, "deploy-2026-11-06");
    assert!(admission.rollback_compatible);
}

#[test]
fn prepare_requires_a_live_admission_and_a_future_deadline() {
    assert_eq!(
        Transaction::prepare(plan(), binding(), admission(), 1_000, 1_000).unwrap_err(),
        DeploymentError::InvalidDeadline {
            now_unix_seconds: 1_000,
            deadline_unix_seconds: 1_000
        }
    );

    assert_eq!(
        Transaction::prepare(plan(), binding(), admission(), 20_000, 10_000).unwrap_err(),
        DeploymentError::AdmissionExpired {
            now_unix_seconds: 10_000,
            expires_at_unix_seconds: 10_000
        }
    );
}

#[test]
fn prepared_trial_committing_committed_is_the_only_success_path() {
    let mut transaction = prepared();
    let before = transaction.before.clone();
    assert_eq!(transaction.state, State::Prepared);

    assert!(matches!(
        transaction.advance(
            Event::TrialSucceeded {
                observed_system: CANDIDATE.into(),
                observed_profile: Some(PROFILE.into()),
            },
            1_100
        ),
        Err(DeploymentError::InvalidTransition {
            from: State::Prepared,
            ..
        })
    ));

    transaction.advance(Event::TrialStarted, 1_101).unwrap();
    assert_eq!(transaction.state, State::Trial);
    assert!(matches!(
        transaction.advance(Event::Committed, 1_102),
        Err(DeploymentError::InvalidTransition {
            from: State::Trial,
            ..
        })
    ));

    transaction
        .advance(
            Event::TrialSucceeded {
                observed_system: CANDIDATE.into(),
                observed_profile: Some(PROFILE.into()),
            },
            1_103,
        )
        .unwrap();
    assert_eq!(transaction.state, State::Committing);
    assert_eq!(
        transaction.after,
        Some(ActivationIdentity {
            system: Some(CANDIDATE.into()),
            profile: Some(PROFILE.into()),
        })
    );

    transaction.advance(Event::Committed, 1_104).unwrap();
    assert_eq!(transaction.state, State::Committed);
    transaction.advance(Event::Committed, 1_105).unwrap();
    assert_eq!(transaction.state, State::Committed);
    assert_eq!(transaction.before, before);
    assert_eq!(transaction.before.system.as_deref(), Some(PREVIOUS));
    assert!(transaction.validate().is_ok());
}

#[test]
fn trial_health_or_commit_failure_requires_restore_and_blocks_commit() {
    let mut activation_failed = prepared();
    activation_failed
        .advance(Event::TrialStarted, 1_100)
        .unwrap();
    activation_failed
        .advance(
            Event::TrialFailed {
                reason: "activation rejected".into(),
            },
            1_101,
        )
        .unwrap();
    assert_eq!(activation_failed.state, State::Restoring);
    assert_eq!(
        activation_failed.failure.as_deref(),
        Some("activation rejected")
    );
    assert_eq!(activation_failed.after, None);
    assert!(matches!(
        activation_failed.advance(Event::Committed, 1_102),
        Err(DeploymentError::InvalidTransition {
            from: State::Restoring,
            ..
        })
    ));
    activation_failed.advance(Event::Restored, 1_103).unwrap();
    assert_eq!(activation_failed.state, State::Restored);
    assert_eq!(activation_failed.before.system.as_deref(), Some(PREVIOUS));

    let mut unhealthy = prepared();
    unhealthy.advance(Event::TrialStarted, 1_100).unwrap();
    unhealthy
        .advance(
            Event::TrialSucceeded {
                observed_system: CANDIDATE.into(),
                observed_profile: Some(PROFILE.into()),
            },
            1_101,
        )
        .unwrap();
    unhealthy
        .advance(
            Event::VerificationFailed {
                reason: "health check failed".into(),
            },
            1_102,
        )
        .unwrap();
    assert_eq!(unhealthy.state, State::Restoring);
    assert!(matches!(
        unhealthy.advance(
            Event::TrialSucceeded {
                observed_system: CANDIDATE.into(),
                observed_profile: Some(PROFILE.into()),
            },
            1_103
        ),
        Err(DeploymentError::InvalidTransition {
            from: State::Restoring,
            ..
        })
    ));
    assert!(matches!(
        unhealthy.advance(Event::Committed, 1_104),
        Err(DeploymentError::InvalidTransition {
            from: State::Restoring,
            ..
        })
    ));
    assert_eq!(unhealthy.before.system.as_deref(), Some(PREVIOUS));

    let mut commit_failed = prepared();
    commit_failed.advance(Event::TrialStarted, 1_100).unwrap();
    commit_failed
        .advance(
            Event::TrialSucceeded {
                observed_system: CANDIDATE.into(),
                observed_profile: Some(PROFILE.into()),
            },
            1_101,
        )
        .unwrap();
    commit_failed
        .advance(
            Event::CommitFailed {
                reason: "profile pin failed".into(),
            },
            1_102,
        )
        .unwrap();
    assert_eq!(commit_failed.state, State::Restoring);
    assert_eq!(commit_failed.failure.as_deref(), Some("profile pin failed"));
}

#[test]
fn timeout_forces_restore_and_never_commits() {
    let mut timed_out = prepared_at(1_010, 1_000);
    timed_out.advance(Event::TrialStarted, 1_001).unwrap();
    assert_eq!(
        timed_out
            .advance(
                Event::TrialSucceeded {
                    observed_system: CANDIDATE.into(),
                    observed_profile: Some(PROFILE.into()),
                },
                1_011
            )
            .unwrap_err(),
        DeploymentError::TimedOut {
            deadline_unix_seconds: 1_010
        }
    );
    assert_eq!(timed_out.state, State::Restoring);
    assert_eq!(timed_out.after, None);
    assert!(matches!(
        timed_out.advance(Event::Committed, 1_012),
        Err(DeploymentError::InvalidTransition {
            from: State::Restoring,
            ..
        })
    ));
    timed_out.advance(Event::Restored, 1_013).unwrap();
    assert_eq!(timed_out.state, State::Restored);

    let mut late_commit = prepared_at(1_010, 1_000);
    late_commit.advance(Event::TrialStarted, 1_001).unwrap();
    late_commit
        .advance(
            Event::TrialSucceeded {
                observed_system: CANDIDATE.into(),
                observed_profile: Some(PROFILE.into()),
            },
            1_005,
        )
        .unwrap();
    assert_eq!(
        late_commit.advance(Event::Committed, 1_011).unwrap_err(),
        DeploymentError::TimedOut {
            deadline_unix_seconds: 1_010
        }
    );
    assert_eq!(late_commit.state, State::Restoring);
    assert_eq!(
        late_commit
            .after
            .as_ref()
            .and_then(|after| after.system.as_deref()),
        Some(CANDIDATE)
    );
    assert!(matches!(
        late_commit.advance(Event::Committed, 1_012),
        Err(DeploymentError::InvalidTransition {
            from: State::Restoring,
            ..
        })
    ));
    assert_eq!(late_commit.before.system.as_deref(), Some(PREVIOUS));
}

#[test]
fn admission_expiry_forces_restore_and_never_commits() {
    let mut admission = admission();
    admission.expires_at_unix_seconds = 1_010;
    let mut transaction = Transaction::prepare(plan(), binding(), admission, 2_000, 1_000).unwrap();
    transaction.advance(Event::TrialStarted, 1_001).unwrap();
    transaction
        .advance(
            Event::TrialSucceeded {
                observed_system: CANDIDATE.into(),
                observed_profile: Some(PROFILE.into()),
            },
            1_005,
        )
        .unwrap();
    assert_eq!(
        transaction.advance(Event::Committed, 1_010).unwrap_err(),
        DeploymentError::AdmissionExpired {
            now_unix_seconds: 1_010,
            expires_at_unix_seconds: 1_010
        }
    );
    assert_eq!(transaction.state, State::Restoring);
    assert!(matches!(
        transaction.advance(Event::Committed, 1_011),
        Err(DeploymentError::InvalidTransition {
            from: State::Restoring,
            ..
        })
    ));
    transaction.advance(Event::Restored, 1_012).unwrap();
    assert_eq!(transaction.state, State::Restored);
    assert_eq!(transaction.before.system.as_deref(), Some(PREVIOUS));
}

#[test]
fn a_different_observed_system_cannot_be_recorded_as_the_trial_result() {
    let mut transaction = prepared();
    transaction.advance(Event::TrialStarted, 1_100).unwrap();
    assert_eq!(
        transaction
            .advance(
                Event::TrialSucceeded {
                    observed_system: OTHER.into(),
                    observed_profile: Some(PROFILE.into()),
                },
                1_101
            )
            .unwrap_err(),
        DeploymentError::ObservationMismatch {
            expected: CANDIDATE.into(),
            observed: OTHER.into()
        }
    );
    assert_eq!(transaction.state, State::Restoring);
    assert_eq!(transaction.after, None);
    assert!(matches!(
        transaction.advance(Event::Committed, 1_102),
        Err(DeploymentError::InvalidTransition {
            from: State::Restoring,
            ..
        })
    ));
}

#[test]
fn resume_preserves_the_original_baseline_and_after_identity() {
    let mut transaction = prepared();
    let original = transaction.before.clone();
    transaction.advance(Event::TrialStarted, 1_100).unwrap();
    transaction
        .advance(
            Event::TrialSucceeded {
                observed_system: CANDIDATE.into(),
                observed_profile: Some(PROFILE.into()),
            },
            1_101,
        )
        .unwrap();

    let encoded = serde_json::to_string(&transaction).unwrap();
    let mut resumed: Transaction = serde_json::from_str(&encoded).unwrap();
    resumed.validate().unwrap();
    assert_eq!(resumed.before, original);
    assert_eq!(resumed.plan.expected_system.as_deref(), Some(PREVIOUS));
    assert_eq!(resumed.state, State::Committing);

    resumed.advance(Event::Committed, 1_102).unwrap();
    let encoded = serde_json::to_string(&resumed).unwrap();
    let mut confirmed: Transaction = serde_json::from_str(&encoded).unwrap();
    confirmed.validate().unwrap();
    confirmed.advance(Event::Committed, 1_103).unwrap();
    assert_eq!(confirmed.state, State::Committed);
    assert_eq!(confirmed.before, original);
    assert_eq!(
        confirmed
            .after
            .as_ref()
            .and_then(|after| after.system.as_deref()),
        Some(CANDIDATE)
    );
}

#[test]
fn resume_validation_rejects_tampered_or_inconsistent_state() {
    let mut tampered = prepared();
    tampered.before.system = Some(OTHER.into());
    assert_invalid_field(tampered.validate(), "before");

    let mut missing_after = prepared();
    missing_after.state = State::Committing;
    assert_invalid_field(missing_after.validate(), "after");

    let mut wrong_binding = prepared();
    wrong_binding.binding.candidate = OTHER.into();
    assert_eq!(
        wrong_binding.validate().unwrap_err(),
        DeploymentError::BindingMismatch { field: "candidate" }
    );

    let mut unsafe_admission = prepared();
    unsafe_admission.admission.configuration_only = false;
    assert_eq!(
        unsafe_admission.validate().unwrap_err(),
        DeploymentError::UnsafeAdmission {
            field: "configurationOnly"
        }
    );

    let mut bad_digest = prepared();
    bad_digest.admission.request_digest = digest_of_zeros();
    assert_eq!(
        bad_digest.validate().unwrap_err(),
        DeploymentError::AdmissionMismatch {
            field: "requestDigest"
        }
    );

    let mut wrong_schema = prepared();
    wrong_schema.version = 2;
    assert_eq!(
        wrong_schema.validate().unwrap_err(),
        DeploymentError::UnsupportedSchema { found: 2 }
    );

    let mut committed_failure = prepared();
    committed_failure.state = State::Committed;
    committed_failure.after = Some(ActivationIdentity {
        system: Some(CANDIDATE.into()),
        profile: Some(PROFILE.into()),
    });
    committed_failure.failure = Some("stale failure".into());
    assert_invalid_field(committed_failure.validate(), "failure");
}

#[test]
fn recovery_failed_is_terminal_and_retains_the_original_baseline() {
    let mut transaction = prepared();
    transaction.advance(Event::TrialStarted, 1_100).unwrap();
    transaction
        .advance(
            Event::TrialFailed {
                reason: "activation rejected".into(),
            },
            1_101,
        )
        .unwrap();
    transaction
        .advance(
            Event::RecoveryFailed {
                reason: "restore did not boot".into(),
            },
            1_102,
        )
        .unwrap();
    assert_eq!(transaction.state, State::RecoveryFailed);
    assert_eq!(transaction.failure.as_deref(), Some("restore did not boot"));
    assert_eq!(transaction.before.system.as_deref(), Some(PREVIOUS));
    transaction
        .advance(
            Event::RecoveryFailed {
                reason: "restore did not boot".into(),
            },
            1_103,
        )
        .unwrap();
    assert!(matches!(
        transaction.advance(Event::Restored, 1_104),
        Err(DeploymentError::InvalidTransition {
            from: State::RecoveryFailed,
            ..
        })
    ));
    assert!(transaction.validate().is_ok());
}

#[test]
fn canonical_binding_json_and_digest_are_stable() {
    let first = binding();
    let second = plan()
        .bind(Some(PREVIOUS.into()), Some(PROFILE.into()))
        .unwrap();
    assert_eq!(
        first.canonical_json().unwrap(),
        second.canonical_json().unwrap()
    );
    assert_eq!(first.digest().unwrap(), second.digest().unwrap());

    let changed = plan()
        .bind(Some(OTHER.into()), Some(PROFILE.into()))
        .unwrap();
    assert_ne!(first.digest().unwrap(), changed.digest().unwrap());

    let value: serde_json::Value = serde_json::from_str(&first.canonical_json().unwrap()).unwrap();
    assert_eq!(value["version"], serde_json::json!(1));
    assert_eq!(value["attempt"], "deploy-2026-11-06");
    assert_eq!(value["expectedSystem"], PREVIOUS);
    assert_eq!(value["expectedProfile"], PROFILE);
    assert_eq!(value["previousSystem"], PREVIOUS);
}
