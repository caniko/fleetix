#![cfg(all(unix, feature = "service-surfaces-runtime"))]

use fleetix::service_surfaces::runtime::{
    self, Admission, Backend, CommandEvidence, HostLock, OperationReport,
};
use fleetix::service_surfaces::{
    Containers, Manifest, Policy, Surface, Tools, validate_store_executable_path,
    validate_store_path,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use tempfile::TempDir;

const HASH_ALPHABET: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";

fn hash(seed: u32) -> String {
    let mut chars = HASH_ALPHABET.to_vec();
    for index in 0..4 {
        chars[index] = HASH_ALPHABET[((seed >> (index * 5)) & 31) as usize];
    }
    String::from_utf8(chars).unwrap()
}

fn artifact(seed: u32, name: &str) -> String {
    format!("/nix/store/{}-{name}", hash(seed))
}

fn test_policy(root: &Path) -> Policy {
    let state_dir = root.join("state");
    if !state_dir.exists() {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&state_dir)
            .unwrap();
    }
    Policy {
        version: 1,
        host: "atlas".into(),
        state_dir,
        lock_path: root.join("switch.lock"),
        containers: Containers {
            dev: "fleetix-dev".into(),
            prod: "fleetix-prod".into(),
        },
        required_services: vec!["database".into(), "web".into()],
        tools: Tools {
            systemctl: PathBuf::from("/bin/true"),
            nixos_container: PathBuf::from("/bin/true"),
            nix_store: PathBuf::from("/bin/true"),
        },
        admission_check: format!("/nix/store/{}-coreutils/bin/false", hash(999)).into(),
        container_config_dir: root.join("containers"),
        command_timeout_seconds: 10,
        health_timeout_seconds: 1,
        health_interval_seconds: 1,
    }
}

fn evidence() -> CommandEvidence {
    CommandEvidence {
        command: vec!["fake".into()],
        exit_code: Some(0),
        stdout: String::new(),
        stderr: String::new(),
    }
}

#[test]
fn unrecorded_prod_is_not_overwritten_when_observation_fails() {
    let root = tempfile::tempdir().unwrap();
    let policy = test_policy(root.path());
    let candidate = artifact(801, "candidate");
    let foreign = artifact(802, "unrecorded-prod");
    let backend = Fake::new(&[&candidate, &foreign]);
    runtime::deploy_dev(&policy, &backend, &candidate).unwrap();
    backend.set_live(Surface::Prod, &foreign);
    *backend.observe_failure.lock().unwrap() = Some(Surface::Prod);
    let admissions = backend.admissions().len();

    assert!(runtime::promote(&policy, &backend, &candidate).is_err());
    assert_eq!(
        backend.live_of(Surface::Prod).as_deref(),
        Some(foreign.as_str())
    );
    assert_eq!(backend.admissions().len(), admissions);
    assert!(!runtime::pending_path(&policy).exists());
}

#[test]
fn initial_cancellation_requires_confirmed_inactivity() {
    let root = tempfile::tempdir().unwrap();
    let policy = test_policy(root.path());
    let candidate = artifact(803, "candidate");
    let foreign = artifact(804, "unrecorded-prod");
    let backend = Fake::new(&[&candidate, &foreign]);
    runtime::deploy_dev(&policy, &backend, &candidate).unwrap();
    backend.set_admission_failure(Some("not ready"));
    assert!(runtime::promote(&policy, &backend, &candidate).is_err());
    backend.set_admission_failure(None);
    backend.set_live(Surface::Prod, &foreign);
    *backend.observe_failure.lock().unwrap() = Some(Surface::Prod);

    assert!(runtime::recover(&policy, &backend, None).is_err());
    assert!(runtime::pending_path(&policy).exists());
    assert_eq!(
        backend.live_of(Surface::Prod).as_deref(),
        Some(foreign.as_str())
    );
}

#[test]
fn initial_resume_does_not_treat_an_observation_error_as_absence() {
    let root = tempfile::tempdir().unwrap();
    let policy = test_policy(root.path());
    let candidate = artifact(805, "candidate");
    let backend = Fake::new(&[&candidate]);
    backend.set_activate_failure(Some((Surface::Dev, &candidate)));
    assert!(runtime::deploy_dev(&policy, &backend, &candidate).is_err());
    backend.set_activate_failure(None);
    *backend.observe_failure.lock().unwrap() = Some(Surface::Dev);
    let activations = backend.activations(Surface::Dev).len();

    assert!(runtime::resume(&policy, &backend, None).is_err());
    assert_eq!(backend.activations(Surface::Dev).len(), activations);
    assert!(runtime::pending_path(&policy).exists());
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AdmissionRecord {
    operation: String,
    surface: Surface,
    previous: Option<String>,
    candidate: String,
}

#[derive(Default)]
struct Fake {
    valid: Mutex<BTreeSet<String>>,
    live: Mutex<BTreeMap<Surface, String>>,
    retained: Mutex<BTreeSet<String>>,
    validations: Mutex<Vec<String>>,
    admissions: Mutex<Vec<AdmissionRecord>>,
    activations: Mutex<Vec<(Surface, String)>>,
    starts: Mutex<Vec<(Surface, String)>>,
    stops: Mutex<Vec<Surface>>,
    health_calls: Mutex<Vec<(Surface, String)>>,
    admission_failure: Mutex<Option<String>>,
    health_failure: Mutex<Option<(Surface, String)>>,
    health_transient: Mutex<BTreeMap<(Surface, String), u32>>,
    activate_failure: Mutex<Option<(Surface, String)>>,
    observe_failure: Mutex<Option<Surface>>,
    idle_failure: Mutex<Option<Surface>>,
}

impl Fake {
    fn new(artifacts: &[&str]) -> Self {
        let fake = Self::default();
        for artifact in artifacts {
            fake.add(artifact);
        }
        fake
    }

    fn add(&self, artifact: &str) {
        self.valid.lock().unwrap().insert(artifact.to_owned());
    }

    fn set_live(&self, surface: Surface, artifact: &str) {
        self.live
            .lock()
            .unwrap()
            .insert(surface, artifact.to_owned());
    }

    fn live_of(&self, surface: Surface) -> Option<String> {
        self.live.lock().unwrap().get(&surface).cloned()
    }

    fn activations(&self, surface: Surface) -> Vec<String> {
        self.activations
            .lock()
            .unwrap()
            .iter()
            .filter(|(current, _)| *current == surface)
            .map(|(_, artifact)| artifact.clone())
            .collect()
    }

    fn admissions(&self) -> Vec<AdmissionRecord> {
        self.admissions.lock().unwrap().clone()
    }

    fn set_admission_failure(&self, failure: Option<&str>) {
        *self.admission_failure.lock().unwrap() = failure.map(str::to_owned);
    }

    fn set_health_failure(&self, failure: Option<(Surface, &str)>) {
        *self.health_failure.lock().unwrap() =
            failure.map(|(surface, artifact)| (surface, artifact.to_owned()));
    }

    fn set_activate_failure(&self, failure: Option<(Surface, &str)>) {
        *self.activate_failure.lock().unwrap() =
            failure.map(|(surface, artifact)| (surface, artifact.to_owned()));
    }

    fn fail_health_times(&self, surface: Surface, artifact: &str, times: u32) {
        self.health_transient
            .lock()
            .unwrap()
            .insert((surface, artifact.to_owned()), times);
    }

    fn fail_idle(&self, surface: Option<Surface>) {
        *self.idle_failure.lock().unwrap() = surface;
    }
}

impl Backend for Fake {
    fn inactive(&self, surface: Surface) -> Result<bool, String> {
        self.idle(surface)?;
        Ok(!self.live.lock().unwrap().contains_key(&surface))
    }

    fn validate(&self, artifact: &str) -> Result<(), String> {
        self.validations.lock().unwrap().push(artifact.to_owned());
        if self.valid.lock().unwrap().contains(artifact) {
            Ok(())
        } else {
            Err(format!("fake release {artifact} is missing"))
        }
    }

    fn retain(&self, artifact: &str) -> Result<(), String> {
        self.retained.lock().unwrap().insert(artifact.to_owned());
        Ok(())
    }

    fn admission(&self, request: &Admission<'_>) -> Result<CommandEvidence, String> {
        self.admissions.lock().unwrap().push(AdmissionRecord {
            operation: request.operation.to_owned(),
            surface: request.surface,
            previous: request.previous.map(str::to_owned),
            candidate: request.candidate.to_owned(),
        });
        match self.admission_failure.lock().unwrap().as_ref() {
            Some(failure) => Err(failure.clone()),
            None => Ok(evidence()),
        }
    }

    fn activate(&self, surface: Surface, artifact: &str) -> Result<CommandEvidence, String> {
        self.activations
            .lock()
            .unwrap()
            .push((surface, artifact.to_owned()));
        self.set_live(surface, artifact);
        match self.activate_failure.lock().unwrap().as_ref() {
            Some((failed_surface, failed_artifact))
                if *failed_surface == surface && failed_artifact == artifact =>
            {
                Err("simulated crash after activation".into())
            }
            _ => Ok(evidence()),
        }
    }

    fn start(&self, surface: Surface, artifact: &str) -> Result<CommandEvidence, String> {
        self.starts
            .lock()
            .unwrap()
            .push((surface, artifact.to_owned()));
        self.set_live(surface, artifact);
        Ok(evidence())
    }

    fn observe(&self, surface: Surface) -> Result<String, String> {
        if self.observe_failure.lock().unwrap().as_ref() == Some(&surface) {
            return Err("simulated observation failure".into());
        }
        self.live
            .lock()
            .unwrap()
            .get(&surface)
            .cloned()
            .ok_or_else(|| "container is down".into())
    }

    fn idle(&self, surface: Surface) -> Result<(), String> {
        if self.idle_failure.lock().unwrap().as_ref() == Some(&surface) {
            return Err("simulated active systemd job".into());
        }
        Ok(())
    }

    fn stop(&self, surface: Surface) -> Result<CommandEvidence, String> {
        self.stops.lock().unwrap().push(surface);
        self.live.lock().unwrap().remove(&surface);
        Ok(evidence())
    }

    fn health(&self, surface: Surface, artifact: &str) -> Result<CommandEvidence, String> {
        self.health_calls
            .lock()
            .unwrap()
            .push((surface, artifact.to_owned()));
        let key = (surface, artifact.to_owned());
        let transient = self.health_transient.lock().unwrap().get(&key).copied();
        if let Some(remaining) = transient {
            if remaining > 0 {
                self.health_transient
                    .lock()
                    .unwrap()
                    .insert(key, remaining - 1);
                return Err("simulated readiness delay".into());
            }
        }
        match self.health_failure.lock().unwrap().as_ref() {
            Some((failed_surface, failed_artifact))
                if *failed_surface == surface && failed_artifact == artifact =>
            {
                Err("simulated unhealthy stack".into())
            }
            _ => Ok(evidence()),
        }
    }
}

fn state_of(policy: &Policy, fake: &Fake) -> runtime::StatusReport {
    runtime::status(policy, fake).unwrap()
}

fn pending_of(policy: &Policy, fake: &Fake) -> Option<fleetix::service_surfaces::Pending> {
    runtime::status(policy, fake).unwrap().pending
}

fn establish_pair(policy: &Policy, fake: &Fake, artifact: &str) {
    runtime::deploy_dev(policy, fake, artifact).unwrap();
    runtime::promote(policy, fake, artifact).unwrap();
}

fn install_boot_artifacts(policy: &Policy, artifact: &str) {
    fs::DirBuilder::new()
        .mode(0o700)
        .create(runtime::roots_dir(policy))
        .unwrap();
    symlink(artifact, runtime::root_path(policy, artifact).unwrap()).unwrap();
    for surface in [Surface::Dev, Surface::Prod] {
        let directory = runtime::surface_dir(policy, surface);
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        symlink(artifact, runtime::surface_link(policy, surface)).unwrap();
    }
}

#[test]
fn dev_update_never_touches_prod_and_promotion_uses_the_exact_artifact() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let second = artifact(2, "guest-system");
    let fake = Fake::new(&[&first, &second]);

    let deployed = runtime::deploy_dev(&policy, &fake, &first).unwrap();
    assert_eq!(deployed.surface, Surface::Dev);
    assert_eq!(deployed.artifact, first);
    let promoted = runtime::promote(&policy, &fake, &first).unwrap();
    assert_eq!(promoted.surface, Surface::Prod);

    let status = state_of(&policy, &fake);
    assert_eq!(status.state.dev.artifact.as_deref(), Some(first.as_str()));
    assert_eq!(status.state.prod.artifact.as_deref(), Some(first.as_str()));
    assert!(status.pending.is_none());
    let prod_activations = fake.activations(Surface::Prod);
    assert_eq!(prod_activations, vec![first.clone()]);
    assert_eq!(fake.live_of(Surface::Prod).as_deref(), Some(first.as_str()));

    runtime::deploy_dev(&policy, &fake, &second).unwrap();
    let status = state_of(&policy, &fake);
    assert_eq!(status.state.dev.artifact.as_deref(), Some(second.as_str()));
    assert_eq!(status.state.prod.artifact.as_deref(), Some(first.as_str()));
    assert_eq!(
        fake.activations(Surface::Prod),
        vec![first.clone()],
        "dev update must not restart or swap prod"
    );
    assert_eq!(fake.live_of(Surface::Prod).as_deref(), Some(first.as_str()));
    assert!(fake.retained.lock().unwrap().contains(&first));

    let promotions = fake
        .admissions()
        .into_iter()
        .filter(|admission| admission.operation == "promote")
        .collect::<Vec<_>>();
    assert_eq!(promotions.len(), 1);
    assert_eq!(promotions[0].candidate, first);
    assert_eq!(promotions[0].previous, None);
}

#[test]
fn promote_rejects_stale_expected_dev_before_any_prod_action() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let second = artifact(2, "guest-system");
    let fake = Fake::new(&[&first, &second]);
    runtime::deploy_dev(&policy, &fake, &first).unwrap();

    let error = runtime::promote(&policy, &fake, &second).unwrap_err();
    assert!(error.contains("stale expected dev release"), "{error}");
    assert!(fake.activations(Surface::Prod).is_empty());
    assert!(pending_of(&policy, &fake).is_none());
}

#[test]
fn promote_rejects_fresh_dev_health_failure_without_journal() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let fake = Fake::new(&[&first]);
    runtime::deploy_dev(&policy, &fake, &first).unwrap();
    fake.set_health_failure(Some((Surface::Dev, &first)));

    let error = runtime::promote(&policy, &fake, &first).unwrap_err();
    assert!(error.contains("health failed"), "{error}");
    assert!(fake.activations(Surface::Prod).is_empty());
    assert!(pending_of(&policy, &fake).is_none());
}

#[test]
fn promote_rejects_live_prod_drift_before_journal() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let drifted = artifact(2, "guest-system");
    let next = artifact(3, "guest-system");
    let fake = Fake::new(&[&first, &drifted, &next]);
    establish_pair(&policy, &fake, &first);
    fake.set_live(Surface::Prod, &drifted);
    runtime::deploy_dev(&policy, &fake, &next).unwrap();

    let error = runtime::promote(&policy, &fake, &next).unwrap_err();
    assert!(error.contains("prod drift"), "{error}");
    assert_eq!(
        fake.activations(Surface::Prod),
        vec![first.clone()],
        "drift refusal must not activate prod"
    );
    assert!(pending_of(&policy, &fake).is_none());
    assert_eq!(
        state_of(&policy, &fake).state.prod.artifact.as_deref(),
        Some(first.as_str())
    );
}

#[test]
fn repeat_promotion_is_refused_and_preserves_the_rollback_baseline() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let second = artifact(2, "guest-system");
    let fake = Fake::new(&[&first, &second]);
    establish_pair(&policy, &fake, &first);
    runtime::deploy_dev(&policy, &fake, &second).unwrap();
    runtime::promote(&policy, &fake, &second).unwrap();

    let error = runtime::promote(&policy, &fake, &second).unwrap_err();
    assert!(error.contains("rollback baseline"), "{error}");
    assert!(pending_of(&policy, &fake).is_none());
    let status = state_of(&policy, &fake);
    assert_eq!(status.state.prod.artifact.as_deref(), Some(second.as_str()));
    assert_eq!(status.state.prod.previous.as_deref(), Some(first.as_str()));
    assert_eq!(
        fake.activations(Surface::Prod),
        vec![first.clone(), second.clone()],
        "a repeat promotion must not restart prod"
    );

    runtime::rollback(&policy, &fake, Some(&second)).unwrap();
    let status = state_of(&policy, &fake);
    assert_eq!(status.state.prod.artifact.as_deref(), Some(first.as_str()));
    assert_eq!(status.state.prod.previous.as_deref(), Some(second.as_str()));
}

#[test]
fn admission_failure_is_durable_blocks_changes_and_requires_explicit_recovery() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let second = artifact(2, "guest-system");
    let third = artifact(3, "guest-system");
    let fake = Fake::new(&[&first, &second, &third]);
    establish_pair(&policy, &fake, &first);
    runtime::deploy_dev(&policy, &fake, &second).unwrap();
    fake.set_admission_failure(Some("database backup is stale"));

    let error = runtime::promote(&policy, &fake, &second).unwrap_err();
    assert!(error.contains("database backup is stale"), "{error}");
    let status = state_of(&policy, &fake);
    let pending = status.pending.as_ref().expect("failure must be journaled");
    assert_eq!(pending.phase.as_str(), "prepared");
    assert_eq!(
        pending.failure.as_ref().unwrap().message,
        "database backup is stale"
    );
    assert_eq!(
        status.state.prod.artifact.as_deref(),
        Some(first.as_str()),
        "failed admission leaves prod committed state untouched"
    );
    assert_eq!(fake.live_of(Surface::Prod).as_deref(), Some(first.as_str()));
    assert!(
        fake.stops.lock().unwrap().is_empty(),
        "admission refusal before activation must not stop healthy prod"
    );

    let blocked = runtime::deploy_dev(&policy, &fake, &third).unwrap_err();
    assert!(blocked.contains("pending"), "{blocked}");
    assert!(blocked.contains(&pending.id), "{blocked}");

    fake.set_admission_failure(None);
    let prod_activations = fake.activations(Surface::Prod);
    let recovered = runtime::recover(&policy, &fake, Some(&pending.id)).unwrap();
    assert_eq!(
        fake.activations(Surface::Prod),
        prod_activations,
        "recovering a refused admission must not restart healthy prod"
    );
    assert!(recovered.aborted);
    assert_eq!(recovered.surface, Surface::Prod);
    assert!(
        fake.stops.lock().unwrap().is_empty(),
        "recovering a refused admission must not stop healthy prod"
    );
    let status = state_of(&policy, &fake);
    assert!(status.pending.is_none());
    assert_eq!(status.state.dev.artifact.as_deref(), Some(second.as_str()));
    assert_eq!(status.state.prod.artifact.as_deref(), Some(first.as_str()));
    let last = fake.admissions().pop().unwrap();
    assert_eq!(last.operation, "abort");
    assert_eq!(last.surface, Surface::Prod);
    assert_eq!(last.previous.as_deref(), Some(first.as_str()));
    assert_eq!(last.candidate, second);

    // The same non-activating cancel path covers a refused deploy-dev: dev is
    // live on its committed release and must not be restarted or stopped.
    let dev_activations = fake.activations(Surface::Dev);
    fake.set_admission_failure(Some("dev database backup is stale"));
    let refused = runtime::deploy_dev(&policy, &fake, &third).unwrap_err();
    assert!(
        refused.contains("dev database backup is stale"),
        "{refused}"
    );
    fake.set_admission_failure(None);
    let aborted = runtime::recover(&policy, &fake, None).unwrap();
    assert_eq!(
        fake.activations(Surface::Dev),
        dev_activations,
        "recovering a refused deploy-dev must not restart healthy dev"
    );
    assert!(aborted.aborted);
    assert_eq!(aborted.surface, Surface::Dev);
    assert_eq!(
        fake.activations(Surface::Prod),
        prod_activations,
        "recovering a refused deploy-dev must not touch prod"
    );
    assert!(fake.stops.lock().unwrap().is_empty());
    assert_eq!(
        state_of(&policy, &fake).state.dev.artifact.as_deref(),
        Some(second.as_str())
    );
}

#[test]
fn refused_admission_after_reboot_aborts_without_activation_and_boot_restores_prod() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let second = artifact(2, "guest-system");
    let fake = Fake::new(&[&first, &second]);
    establish_pair(&policy, &fake, &first);
    runtime::deploy_dev(&policy, &fake, &second).unwrap();
    fake.set_admission_failure(Some("database backup is stale"));
    runtime::promote(&policy, &fake, &second).unwrap_err();
    fake.set_admission_failure(None);
    install_boot_artifacts(&policy, &first);
    fs::remove_file(runtime::surface_link(&policy, Surface::Dev)).unwrap();
    symlink(&second, runtime::surface_link(&policy, Surface::Dev)).unwrap();
    symlink(&second, runtime::root_path(&policy, &second).unwrap()).unwrap();
    let activations = fake.activations(Surface::Prod);

    // A reboot stops both guests while the refused journal stays prepared.
    fake.stop(Surface::Prod).unwrap();
    fake.stop(Surface::Dev).unwrap();
    let aborted = runtime::recover(&policy, &fake, None).unwrap();
    assert_eq!(
        fake.activations(Surface::Prod),
        activations,
        "aborting a refused admission must not restart prod"
    );
    assert!(aborted.aborted);

    // With the journal cleared, boot restores the committed prod release.
    let report = runtime::boot(&policy, &fake).unwrap();
    assert!(report.ok(), "{:?}", report.errors);
    assert_eq!(report.started, vec![Surface::Dev, Surface::Prod]);
    assert_eq!(fake.live_of(Surface::Prod).as_deref(), Some(first.as_str()));
    assert_eq!(
        state_of(&policy, &fake).state.prod.artifact.as_deref(),
        Some(first.as_str())
    );
}

#[test]
fn crash_after_activation_keeps_evidence_and_recovery_is_guard_gated() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let second = artifact(2, "guest-system");
    let third = artifact(3, "guest-system");
    let fake = Fake::new(&[&first, &second, &third]);
    establish_pair(&policy, &fake, &first);
    runtime::deploy_dev(&policy, &fake, &second).unwrap();
    fake.set_activate_failure(Some((Surface::Dev, &third)));

    let error = runtime::deploy_dev(&policy, &fake, &third).unwrap_err();
    assert!(error.contains("simulated crash"), "{error}");
    let status = state_of(&policy, &fake);
    let pending = status.pending.as_ref().expect("crash must stay pending");
    assert_eq!(pending.phase.as_str(), "activating");
    assert_eq!(status.state.dev.artifact.as_deref(), Some(second.as_str()));
    assert_eq!(fake.live_of(Surface::Dev).as_deref(), Some(third.as_str()));

    fake.set_admission_failure(Some("recovery admission rejected"));
    let refused = runtime::recover(&policy, &fake, Some(&pending.id)).unwrap_err();
    assert!(refused.contains("recovery admission rejected"), "{refused}");
    let status = state_of(&policy, &fake);
    assert_eq!(
        status.pending.as_ref().unwrap().phase.as_str(),
        "recovery-failed"
    );
    assert_eq!(status.state.dev.artifact.as_deref(), Some(second.as_str()));
    assert_eq!(fake.live_of(Surface::Dev).as_deref(), Some(third.as_str()));

    fake.set_admission_failure(None);
    let recovered = runtime::recover(&policy, &fake, Some(&pending.id)).unwrap();
    assert_eq!(recovered.artifact, second);
    let status = state_of(&policy, &fake);
    assert!(status.pending.is_none());
    assert_eq!(status.state.dev.artifact.as_deref(), Some(second.as_str()));
    assert_eq!(fake.live_of(Surface::Dev).as_deref(), Some(second.as_str()));
    let last = fake.admissions().pop().unwrap();
    assert_eq!(last.operation, "recover");
    assert_eq!(last.previous.as_deref(), Some(third.as_str()));
    assert_eq!(last.candidate, second);
}

#[test]
fn interrupted_initial_prod_install_refuses_recovery_until_safe_procedure() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let fake = Fake::new(&[&first]);
    runtime::deploy_dev(&policy, &fake, &first).unwrap();
    fake.set_activate_failure(Some((Surface::Prod, &first)));

    let error = runtime::promote(&policy, &fake, &first).unwrap_err();
    assert!(error.contains("simulated crash"), "{error}");
    let status = state_of(&policy, &fake);
    let pending = status.pending.as_ref().unwrap();
    assert!(pending.previous().is_none());
    assert_eq!(status.state.prod.artifact, None);

    let refused = runtime::recover(&policy, &fake, Some(&pending.id)).unwrap_err();
    assert!(refused.contains("explicit safe procedure"), "{refused}");
    assert!(state_of(&policy, &fake).pending.is_some());
    assert_eq!(fake.live_of(Surface::Prod).as_deref(), Some(first.as_str()));
}

#[test]
fn rollback_is_guard_gated_journaled_and_restores_the_previous_release() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let second = artifact(2, "guest-system");
    let fake = Fake::new(&[&first, &second]);
    establish_pair(&policy, &fake, &first);
    runtime::deploy_dev(&policy, &fake, &second).unwrap();
    runtime::promote(&policy, &fake, &second).unwrap();
    assert_eq!(
        state_of(&policy, &fake).state.prod.artifact.as_deref(),
        Some(second.as_str())
    );

    let stale = runtime::rollback(&policy, &fake, Some(&first)).unwrap_err();
    assert!(stale.contains("stale expected prod release"), "{stale}");
    assert!(pending_of(&policy, &fake).is_none());

    fake.set_admission_failure(Some("rollback would corrupt data"));
    let refused = runtime::rollback(&policy, &fake, Some(&second)).unwrap_err();
    assert!(refused.contains("rollback would corrupt data"), "{refused}");
    let status = state_of(&policy, &fake);
    let pending = status.pending.as_ref().unwrap();
    assert_eq!(status.state.prod.artifact.as_deref(), Some(second.as_str()));
    assert_eq!(
        fake.live_of(Surface::Prod).as_deref(),
        Some(second.as_str())
    );

    fake.set_admission_failure(None);
    runtime::recover(&policy, &fake, Some(&pending.id)).unwrap();
    runtime::rollback(&policy, &fake, Some(&second)).unwrap();
    let status = state_of(&policy, &fake);
    assert!(status.pending.is_none());
    assert_eq!(status.state.prod.artifact.as_deref(), Some(first.as_str()));
    assert_eq!(status.state.prod.previous.as_deref(), Some(second.as_str()));
    assert_eq!(fake.live_of(Surface::Prod).as_deref(), Some(first.as_str()));
    let last = fake.admissions().pop().unwrap();
    assert_eq!(last.operation, "rollback");
    assert_eq!(last.previous.as_deref(), Some(second.as_str()));
    assert_eq!(last.candidate, first);
}

#[test]
fn unknown_fields_and_policy_digest_mismatches_are_refused() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let fake = Fake::new(&[&first]);
    runtime::deploy_dev(&policy, &fake, &first).unwrap();

    let state_path = runtime::state_path(&policy);
    let mut state: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    state["unknown"] = json!(true);
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    let error = runtime::status(&policy, &fake).unwrap_err();
    assert!(error.contains("unknown field"), "{error}");

    let mut state: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    state.as_object_mut().unwrap().remove("unknown");
    state["policy_digest"] =
        json!("sha256:0000000000000000000000000000000000000000000000000000000000000000");
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    let error = runtime::status(&policy, &fake).unwrap_err();
    assert!(error.contains("different policy"), "{error}");
}

#[test]
fn unknown_pending_journal_fields_are_refused() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let fake = Fake::new(&[&first]);
    runtime::deploy_dev(&policy, &fake, &first).unwrap();
    fake.set_admission_failure(Some("reject"));
    runtime::promote(&policy, &fake, &first).unwrap_err();
    fake.set_admission_failure(None);

    let pending_path = runtime::pending_path(&policy);
    let mut pending: Value = serde_json::from_slice(&fs::read(&pending_path).unwrap()).unwrap();
    pending["unexpected"] = json!("value");
    fs::write(&pending_path, serde_json::to_vec_pretty(&pending).unwrap()).unwrap();
    let error = runtime::recover(&policy, &fake, None).unwrap_err();
    assert!(error.contains("unknown field"), "{error}");
}

#[test]
fn unsafe_state_directories_and_lock_parents_are_refused() {
    let root = TempDir::new().unwrap();
    let mut policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let fake = Fake::new(&[&first]);

    fs::set_permissions(&policy.state_dir, fs::Permissions::from_mode(0o755)).unwrap();
    let error = runtime::deploy_dev(&policy, &fake, &first).unwrap_err();
    assert!(error.contains("private directory"), "{error}");
    fs::set_permissions(&policy.state_dir, fs::Permissions::from_mode(0o700)).unwrap();

    let real = root.path().join("real-state");
    fs::DirBuilder::new().mode(0o700).create(&real).unwrap();
    let link = root.path().join("state-link");
    symlink(&real, &link).unwrap();
    policy.state_dir = link;
    let error = runtime::deploy_dev(&policy, &fake, &first).unwrap_err();
    assert!(error.contains("symlink"), "{error}");

    let mut policy = test_policy(root.path());
    let writable = root.path().join("writable");
    fs::DirBuilder::new().mode(0o700).create(&writable).unwrap();
    fs::set_permissions(&writable, fs::Permissions::from_mode(0o777)).unwrap();
    policy.lock_path = writable.join("switch.lock");
    let error = runtime::deploy_dev(&policy, &fake, &first).unwrap_err();
    assert!(error.contains("trusted directory"), "{error}");
}

#[test]
fn policy_and_manifest_reject_empty_self_attested_coverage() {
    let root = TempDir::new().unwrap();
    let mut policy = test_policy(root.path());
    policy.required_services.clear();
    assert!(policy.validate().unwrap_err().contains("nonempty"));

    let policy = test_policy(root.path());
    let mut manifest = Manifest {
        version: 1,
        services: policy.required_services.clone(),
        units: vec!["database.service".into(), "web.service".into()],
    };
    manifest.validate(&policy).unwrap();

    manifest.services.clear();
    assert!(manifest.validate(&policy).unwrap_err().contains("nonempty"));
    manifest.services = policy.required_services.clone();
    manifest.units.clear();
    assert!(manifest.validate(&policy).unwrap_err().contains("nonempty"));
    manifest.units = vec!["web.service".into()];
    manifest.services = vec!["web".into()];
    assert!(
        manifest
            .validate(&policy)
            .unwrap_err()
            .contains("exactly cover")
    );
    manifest.services = vec!["database".into(), "web".into(), "extra".into()];
    assert!(
        manifest
            .validate(&policy)
            .unwrap_err()
            .contains("exactly cover")
    );
}

#[test]
fn policy_admission_accepts_store_files_and_subpaths_while_releases_are_exact_outputs() {
    let hash = hash(7);
    let release = format!("/nix/store/{hash}-coreutils");
    let executable = format!("{release}/bin/false");
    assert!(validate_store_path("release", Path::new(&executable)).is_err());
    assert!(validate_store_path("release", Path::new(&release)).is_ok());
    assert!(validate_store_executable_path("admission", Path::new(&executable)).is_ok());
    assert!(validate_store_executable_path("admission", Path::new(&release)).is_ok());

    let root = TempDir::new().unwrap();
    let mut policy = test_policy(root.path());
    policy.admission_check = PathBuf::from(&executable);
    policy.validate().unwrap();
    policy.admission_check = PathBuf::from(&release);
    policy.validate().unwrap(); // Native validation still requires a real executable file.
    policy.admission_check = PathBuf::from("/run/mutable-admit");
    assert!(policy.validate().is_err());
}

#[test]
fn policy_file_rejects_unknown_fields() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let mut value = serde_json::to_value(&policy).unwrap();
    value["unexpected"] = json!(true);
    let path = root.path().join("policy.json");
    fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let error = runtime::load_policy(&path).unwrap_err();
    assert!(error.contains("unknown field"), "{error}");
}

#[test]
fn host_lock_is_one_stable_anchor_across_threads() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let held = HostLock::acquire(&policy).unwrap();
    let inode = fs::metadata(&policy.lock_path).unwrap().ino();
    let contender = policy.clone();
    let busy = thread::spawn(move || HostLock::acquire(&contender).is_err())
        .join()
        .unwrap();
    assert!(busy, "a second owner must not enter the host lock");
    drop(held);
    let reacquired = HostLock::acquire(&policy).unwrap();
    assert_eq!(
        fs::metadata(&policy.lock_path).unwrap().ino(),
        inode,
        "the lock anchor must never be replaced"
    );
    drop(reacquired);
    assert!(policy.lock_path.exists());
}

#[test]
fn host_lock_accepts_the_existing_read_only_canix_anchor_without_changing_it() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    fs::write(&policy.lock_path, "").unwrap();
    fs::set_permissions(&policy.lock_path, fs::Permissions::from_mode(0o444)).unwrap();
    let before = fs::metadata(&policy.lock_path).unwrap();
    let held = HostLock::acquire(&policy).unwrap();
    assert!(HostLock::acquire(&policy).is_err());
    drop(held);
    let after = fs::metadata(&policy.lock_path).unwrap();
    assert_eq!(before.ino(), after.ino());
    assert_eq!(after.mode() & 0o777, 0o444);
    HostLock::acquire(&policy).unwrap();
}

#[test]
fn health_readiness_retries_are_bounded_and_can_recover_after_restart() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let fake = Fake::new(&[&first]);
    fake.fail_health_times(Surface::Dev, &first, 1);

    runtime::deploy_dev(&policy, &fake, &first).unwrap();
    let calls = fake
        .health_calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(surface, artifact)| *surface == Surface::Dev && artifact == &first)
        .count();
    assert!(
        calls >= 2,
        "readiness retry must re-check health, got {calls}"
    );
    assert_eq!(
        state_of(&policy, &fake).state.dev.artifact.as_deref(),
        Some(first.as_str())
    );
}

#[test]
fn status_is_read_only() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let fake = Fake::new(&[&first]);
    runtime::deploy_dev(&policy, &fake, &first).unwrap();
    let before = fs::read(runtime::state_path(&policy)).unwrap();
    let inode = fs::metadata(&policy.lock_path).unwrap().ino();
    assert!(!runtime::pending_path(&policy).exists());

    // Status must not need or take the mutation lock.
    let held = HostLock::acquire(&policy).unwrap();
    let status = state_of(&policy, &fake);
    drop(held);
    assert_eq!(status.live.len(), 2);
    assert_eq!(fs::read(runtime::state_path(&policy)).unwrap(), before);
    assert!(!runtime::pending_path(&policy).exists());
    assert_eq!(fs::metadata(&policy.lock_path).unwrap().ino(), inode);
}

#[test]
fn boot_no_state_is_a_safe_noop() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let fake = Fake::new(&[]);
    let report = runtime::boot(&policy, &fake).unwrap();
    assert!(report.ok());
    assert!(report.started.is_empty());
    assert!(fake.starts.lock().unwrap().is_empty());
}

#[test]
fn boot_skips_the_pending_surface_and_starts_only_durable_verified_releases() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let second = artifact(2, "guest-system");
    let fake = Fake::new(&[&first, &second]);
    establish_pair(&policy, &fake, &first);

    // A crashed deploy-dev leaves a pending dev operation. Boot starts the
    // committed prod release, skips dev, and reports the refusal visibly.
    fake.set_activate_failure(Some((Surface::Dev, &second)));
    runtime::deploy_dev(&policy, &fake, &second).unwrap_err();
    install_boot_artifacts(&policy, &first);
    let report = runtime::boot(&policy, &fake).unwrap();
    assert!(!report.ok());
    assert_eq!(report.started, vec![Surface::Prod]);
    assert_eq!(
        fake.starts.lock().unwrap().clone(),
        vec![(Surface::Prod, first.clone())]
    );
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].starts_with("dev:"), "{:?}", report.errors);
    assert!(
        report.errors[0].contains("refusing unattended boot"),
        "{:?}",
        report.errors
    );
    assert_eq!(fake.live_of(Surface::Prod).as_deref(), Some(first.as_str()));
    assert_eq!(fake.live_of(Surface::Dev).as_deref(), Some(second.as_str()));

    // The converse: a pending prod operation must not keep dev down.
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let second = artifact(2, "guest-system");
    let fake = Fake::new(&[&first, &second]);
    establish_pair(&policy, &fake, &first);
    runtime::deploy_dev(&policy, &fake, &second).unwrap();
    fake.set_activate_failure(Some((Surface::Prod, &second)));
    runtime::promote(&policy, &fake, &second).unwrap_err();
    install_boot_artifacts(&policy, &second);
    let report = runtime::boot(&policy, &fake).unwrap();
    assert!(!report.ok());
    assert_eq!(report.started, vec![Surface::Dev]);
    assert_eq!(
        fake.starts.lock().unwrap().clone(),
        vec![(Surface::Dev, second.clone())]
    );
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].starts_with("prod:"), "{:?}", report.errors);
    assert!(
        report.errors[0].contains("refusing unattended boot"),
        "{:?}",
        report.errors
    );
    assert_eq!(fake.live_of(Surface::Dev).as_deref(), Some(second.as_str()));

    // With no pending journal boot starts and verifies both recorded releases.
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let fake = Fake::new(&[&first]);
    establish_pair(&policy, &fake, &first);
    install_boot_artifacts(&policy, &first);
    let report = runtime::boot(&policy, &fake).unwrap();
    assert!(report.ok(), "{:?}", report.errors);
    assert_eq!(report.started, vec![Surface::Dev, Surface::Prod]);
    assert_eq!(
        fake.starts.lock().unwrap().clone(),
        vec![
            (Surface::Dev, first.clone()),
            (Surface::Prod, first.clone())
        ]
    );
    assert_eq!(fake.live_of(Surface::Dev).as_deref(), Some(first.as_str()));
    assert_eq!(fake.live_of(Surface::Prod).as_deref(), Some(first.as_str()));
}

#[test]
fn operation_report_ids_are_stable_and_explicit() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let fake = Fake::new(&[&first]);
    let OperationReport { id, .. } = runtime::deploy_dev(&policy, &fake, &first).unwrap();
    assert_eq!(id.len(), 16);
    assert!(id.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(
        state_of(&policy, &fake).state.last_operation.as_deref(),
        Some(id.as_str())
    );
}

#[test]
fn resume_completes_initial_dev_after_activation_failure() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let fake = Fake::new(&[&first]);
    fake.set_activate_failure(Some((Surface::Dev, &first)));
    runtime::deploy_dev(&policy, &fake, &first).unwrap_err();
    fake.set_activate_failure(None);

    let report = runtime::resume(&policy, &fake, None).unwrap();
    assert!(!report.aborted);
    assert_eq!(report.artifact, first);
    let status = state_of(&policy, &fake);
    assert!(status.pending.is_none());
    assert_eq!(status.state.dev.artifact.as_deref(), Some(first.as_str()));
    assert_eq!(fake.live_of(Surface::Dev).as_deref(), Some(first.as_str()));
}

#[test]
fn resume_completes_initial_prod_after_activation_failure() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let fake = Fake::new(&[&first]);
    runtime::deploy_dev(&policy, &fake, &first).unwrap();
    fake.set_activate_failure(Some((Surface::Prod, &first)));
    runtime::promote(&policy, &fake, &first).unwrap_err();
    fake.set_activate_failure(None);

    let report = runtime::resume(&policy, &fake, None).unwrap();
    assert_eq!(report.surface, Surface::Prod);
    assert_eq!(report.artifact, first);
    let status = state_of(&policy, &fake);
    assert!(status.pending.is_none());
    assert_eq!(status.state.prod.artifact.as_deref(), Some(first.as_str()));
    assert_eq!(fake.live_of(Surface::Prod).as_deref(), Some(first.as_str()));
}

#[test]
fn resume_restarts_committed_dev_after_reboot_during_initial_promotion() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let fake = Fake::new(&[&first]);
    runtime::deploy_dev(&policy, &fake, &first).unwrap();
    fake.set_activate_failure(Some((Surface::Prod, &first)));
    runtime::promote(&policy, &fake, &first).unwrap_err();
    fake.set_activate_failure(None);
    install_boot_artifacts(&policy, &first);

    // A reboot stops both guests. If unattended boot never ran (it would
    // start dev and skip the pending prod surface), resume must restart the
    // committed dev release from its unchanged pin before forward-completing.
    fake.stop(Surface::Prod).unwrap();
    fake.stop(Surface::Dev).unwrap();
    assert!(runtime::recover(&policy, &fake, None).is_err());
    assert!(fake.starts.lock().unwrap().is_empty());

    let report = runtime::resume(&policy, &fake, None).unwrap();
    assert_eq!(report.surface, Surface::Prod);
    assert_eq!(report.artifact, first);
    assert_eq!(
        fake.starts.lock().unwrap().clone(),
        vec![(Surface::Dev, first.clone())],
        "resume must restart only the unchanged committed dev release"
    );
    let status = state_of(&policy, &fake);
    assert!(status.pending.is_none());
    assert_eq!(status.state.dev.artifact.as_deref(), Some(first.as_str()));
    assert_eq!(status.state.prod.artifact.as_deref(), Some(first.as_str()));
    assert_eq!(fake.live_of(Surface::Prod).as_deref(), Some(first.as_str()));
}

#[test]
fn resume_rejects_mutated_dev_and_admission_refusal_keeps_pending() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let second = artifact(2, "guest-system");
    let mutated = artifact(3, "guest-system");
    let fake = Fake::new(&[&first, &second, &mutated]);
    establish_pair(&policy, &fake, &first);
    runtime::deploy_dev(&policy, &fake, &second).unwrap();
    fake.set_activate_failure(Some((Surface::Prod, &second)));
    runtime::promote(&policy, &fake, &second).unwrap_err();
    fake.set_activate_failure(None);

    fake.set_live(Surface::Dev, &mutated);
    let error = runtime::resume(&policy, &fake, None).unwrap_err();
    assert!(error.contains("live dev system"), "{error}");
    assert!(pending_of(&policy, &fake).is_some());

    fake.set_live(Surface::Dev, &second);
    fake.set_admission_failure(Some("resume admission rejected"));
    let refused = runtime::resume(&policy, &fake, None).unwrap_err();
    assert!(refused.contains("resume admission rejected"), "{refused}");
    assert!(pending_of(&policy, &fake).is_some());

    fake.set_admission_failure(None);
    runtime::resume(&policy, &fake, None).unwrap();
    let status = state_of(&policy, &fake);
    assert!(status.pending.is_none());
    assert_eq!(status.state.prod.artifact.as_deref(), Some(second.as_str()));
}

#[test]
fn resume_finalizes_committed_pending_without_reactivating() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let second = artifact(2, "guest-system");
    let fake = Fake::new(&[&first, &second]);
    establish_pair(&policy, &fake, &first);
    fake.set_activate_failure(Some((Surface::Dev, &second)));
    runtime::deploy_dev(&policy, &fake, &second).unwrap_err();
    fake.set_activate_failure(None);

    let pending = pending_of(&policy, &fake).unwrap();
    let after = pending.expected_after_commit().unwrap();
    fs::write(
        runtime::state_path(&policy),
        serde_json::to_vec_pretty(&after).unwrap(),
    )
    .unwrap();
    let activations = fake.activations(Surface::Dev).len();

    let report = runtime::resume(&policy, &fake, Some(&pending.id)).unwrap();
    assert_eq!(report.artifact, second);
    assert_eq!(fake.activations(Surface::Dev).len(), activations);
    assert!(pending_of(&policy, &fake).is_none());
    assert_eq!(
        state_of(&policy, &fake).state.dev.artifact.as_deref(),
        Some(second.as_str())
    );
}

#[test]
fn failed_initial_admission_can_be_safely_aborted_then_deploy_different() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let second = artifact(2, "guest-system");
    let fake = Fake::new(&[&first, &second]);
    fake.set_admission_failure(Some("candidate rejected"));
    runtime::deploy_dev(&policy, &fake, &first).unwrap_err();

    fake.fail_idle(Some(Surface::Dev));
    let busy = runtime::recover(&policy, &fake, None).unwrap_err();
    assert!(busy.contains("systemd job"), "{busy}");
    assert!(pending_of(&policy, &fake).is_some());
    fake.fail_idle(None);

    let refused = runtime::recover(&policy, &fake, None).unwrap_err();
    assert!(refused.contains("candidate rejected"), "{refused}");
    let pending = pending_of(&policy, &fake).expect("denied abort keeps journal");
    assert_eq!(pending.phase.as_str(), "prepared");
    fake.set_admission_failure(None);

    let aborted = runtime::recover(&policy, &fake, Some(&pending.id)).unwrap();
    assert!(aborted.aborted);
    let status = state_of(&policy, &fake);
    assert!(status.pending.is_none());
    assert_eq!(status.state.dev.artifact, None);
    assert!(fake.retained.lock().unwrap().contains(&first));

    runtime::deploy_dev(&policy, &fake, &second).unwrap();
    assert_eq!(
        state_of(&policy, &fake).state.dev.artifact.as_deref(),
        Some(second.as_str())
    );
}

#[test]
fn activating_initial_install_cannot_use_noop_abort() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let fake = Fake::new(&[&first]);
    fake.set_activate_failure(Some((Surface::Dev, &first)));
    runtime::deploy_dev(&policy, &fake, &first).unwrap_err();
    fake.set_activate_failure(None);

    let error = runtime::recover(&policy, &fake, None).unwrap_err();
    assert!(error.contains("resume"), "{error}");
    assert!(pending_of(&policy, &fake).is_some());
    runtime::resume(&policy, &fake, None).unwrap();
    assert!(pending_of(&policy, &fake).is_none());
}

#[test]
fn boot_stops_a_surface_when_poststart_verification_fails() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let fake = Fake::new(&[&first]);
    establish_pair(&policy, &fake, &first);
    install_boot_artifacts(&policy, &first);
    *fake.observe_failure.lock().unwrap() = Some(Surface::Dev);

    let report = runtime::boot(&policy, &fake).unwrap();
    assert!(!report.ok());
    assert_eq!(report.started, vec![Surface::Prod]);
    assert_eq!(*fake.stops.lock().unwrap(), vec![Surface::Dev]);
    assert_eq!(fake.live_of(Surface::Dev), None);
    assert_eq!(fake.live_of(Surface::Prod).as_deref(), Some(first.as_str()));
}

#[test]
fn post_activation_health_failure_stops_candidate_and_keeps_pending() {
    let root = TempDir::new().unwrap();
    let policy = test_policy(root.path());
    let first = artifact(1, "guest-system");
    let second = artifact(2, "guest-system");
    let fake = Fake::new(&[&first, &second]);
    runtime::deploy_dev(&policy, &fake, &first).unwrap();
    fake.set_health_failure(Some((Surface::Dev, &second)));

    let error = runtime::deploy_dev(&policy, &fake, &second).unwrap_err();
    assert!(error.contains("simulated unhealthy stack"), "{error}");
    assert!(!error.is_empty());
    let status = state_of(&policy, &fake);
    assert_eq!(status.pending.as_ref().unwrap().phase.as_str(), "activated");
    assert_eq!(status.state.dev.artifact.as_deref(), Some(first.as_str()));
    assert_eq!(fake.live_of(Surface::Dev), None);
    assert_eq!(*fake.stops.lock().unwrap(), vec![Surface::Dev]);

    fake.set_health_failure(None);
    let directory = runtime::surface_dir(&policy, Surface::Dev);
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    symlink(&second, directory.join("system")).unwrap();
    runtime::resume(&policy, &fake, None).unwrap();
    let status = state_of(&policy, &fake);
    assert!(status.pending.is_none());
    assert_eq!(status.state.dev.artifact.as_deref(), Some(second.as_str()));
}
