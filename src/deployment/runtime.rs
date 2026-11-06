//! Target-local execution of approved, configuration-only transactions.
//!
//! A privileged caller must fix the policy path. File ownership or a Nix store
//! name is not candidate authorization: the installed admission command owns
//! that decision. Untrusted requests never contain executable commands.
use super::{Admission, Backend, Event, Plan, State, Transaction, native, storage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Worker failure delivered to the process exit path. Only activation-lock
/// contention is retryable; refusals and unsafe states need an operator.
#[derive(Debug)]
pub struct WorkerError {
    pub retryable: bool,
    pub message: String,
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for WorkerError {}

impl From<String> for WorkerError {
    fn from(message: String) -> Self {
        Self {
            retryable: false,
            message,
        }
    }
}

impl From<&str> for WorkerError {
    fn from(message: &str) -> Self {
        Self::from(message.to_owned())
    }
}

/// Installed policy, not part of the user-supplied request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Policy {
    version: u32,
    target: String,
    backend: String,
    state_root: PathBuf,
    lock_path: PathBuf,
    profile_path: PathBuf,
    current_system_path: PathBuf,
    nix_store: String,
    nix_env: String,
    admission_command: Vec<String>,
    health_command: Vec<String>,
    command_timeout_seconds: u64,
    health_timeout_seconds: u64,
    health_interval_seconds: u64,
}

impl Policy {
    fn validate(&self) -> Result<(), String> {
        if self.version != 1 || !matches!(self.backend.as_str(), "nixos" | "home-manager") {
            return Err("unsupported deployment policy version or backend".into());
        }
        super::check_slug("target", &self.target, 128).map_err(|error| error.to_string())?;
        for path in [
            &self.state_root,
            &self.lock_path,
            &self.profile_path,
            &self.current_system_path,
        ] {
            storage::absolute(path)?;
        }
        for path in [&self.profile_path, &self.current_system_path] {
            if path.starts_with(&self.state_root)
                || self.state_root.starts_with(path)
                || self.lock_path == *path
            {
                return Err(
                    "deployment state, lock and generation pointers must not overlap".into(),
                );
            }
        }
        if self.lock_path == self.state_root
            || self
                .lock_path
                .starts_with(self.state_root.join("transactions"))
        {
            return Err("the activation lock must not be a journal or GC-root path".into());
        }
        for seconds in [
            self.command_timeout_seconds,
            self.health_timeout_seconds,
            self.health_interval_seconds,
        ] {
            if !(1..=3600).contains(&seconds) {
                return Err("deployment timeouts must be between 1 and 3600 seconds".into());
            }
        }
        if self.backend == "home-manager" && self.profile_path != self.current_system_path {
            return Err("Home Manager must observe the same profile it restores".into());
        }
        if self.health_interval_seconds >= self.health_timeout_seconds {
            return Err("health interval exceeds the acceptance window".into());
        }
        for command in [&self.admission_command, &self.health_command] {
            let executable = command
                .first()
                .ok_or("admission and health commands are mandatory")?;
            storage::absolute(Path::new(executable))?;
        }
        for executable in [&self.nix_store, &self.nix_env] {
            storage::absolute(Path::new(executable))?;
        }
        Ok(())
    }

    fn load(path: &Path) -> Result<Self, String> {
        let mut policy: Self = storage::read_json(path, true)?;
        policy.validate()?;
        // Freeze executable identities, not /run/current-system aliases that
        // could resolve to different code after the trial changes that link.
        for executable in [
            &mut policy.nix_store,
            &mut policy.nix_env,
            &mut policy.admission_command[0],
            &mut policy.health_command[0],
        ] {
            storage::trusted_path(Path::new(executable))?;
            *executable = freeze_executable(Path::new(executable))?
                .to_str()
                .ok_or("policy executable is not UTF-8")?
                .to_owned();
            store_root(Path::new(executable))?;
        }
        policy.validate_files()?;
        Ok(policy)
    }

    fn validate_files(&self) -> Result<(), String> {
        self.validate()?;
        // SAFETY: geteuid has no preconditions and does not mutate state.
        let root = unsafe { libc::geteuid() } == 0;
        if (self.backend == "nixos") != root {
            return Err(
                "NixOS requires a privileged worker; Home Manager requires its unprivileged owner"
                    .into(),
            );
        }
        for executable in [
            &self.nix_store,
            &self.nix_env,
            &self.admission_command[0],
            &self.health_command[0],
        ] {
            storage::trusted_path(Path::new(executable))?;
        }
        storage::trusted_path(&self.profile_path)?;
        storage::trusted_path(&self.current_system_path)?;
        Ok(())
    }

    fn command_timeout(&self) -> Duration {
        Duration::from_secs(self.command_timeout_seconds)
    }
}

fn freeze_executable(path: &Path) -> Result<PathBuf, String> {
    let resolved = std::fs::canonicalize(path).map_err(|error| error.to_string())?;
    // Multicall binaries dispatch on argv[0]. Keep their installed alias in
    // the resolved package, not the mutable profile used to find that package.
    let pinned = resolved.with_file_name(path.file_name().ok_or("executable has no name")?);
    if pinned != resolved {
        let target = std::fs::read_link(&pinned).map_err(|error| error.to_string())?;
        let direct = if target.is_absolute() {
            target
        } else {
            pinned
                .parent()
                .ok_or("executable has no parent")?
                .join(target)
        };
        if direct != resolved {
            return Err("executable alias must directly name its immutable target".into());
        }
    }
    Ok(pinned)
}

fn unix_seconds() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .map_err(|error| error.to_string())
}

/// Install graceful interruption handling for the standalone target worker.
/// Library embedders can supply equivalent signal/supervisor semantics.
pub fn install_signal_handlers() -> Result<(), String> {
    native::install_signal_handlers()
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Journal {
    version: u32,
    policy: Policy,
    policy_digest: String,
    transaction: Transaction,
}

impl Journal {
    fn save(&self, path: &Path) -> Result<(), String> {
        self.transaction
            .validate()
            .map_err(|error| error.to_string())?;
        storage::write_json(path, self)
    }

    fn advance(&mut self, path: &Path, event: Event) -> Result<(), String> {
        let result = self.transaction.advance(event, unix_seconds()?);
        // Expiry also changes state to Restoring: persist that transition even
        // when the core returns an error, before attempting restoration.
        self.save(path)?;
        result.map_err(|error| error.to_string())
    }

    fn load(path: &Path, installed: &Policy) -> Result<Self, String> {
        let journal: Self = storage::read_json(path, true)?;
        journal
            .transaction
            .validate()
            .map_err(|error| error.to_string())?;
        journal.policy.validate_files()?;
        if journal.version != 1
            || journal.policy_digest != policy_digest(&journal.policy)?
            || journal.policy.target != installed.target
            || journal.policy.backend != installed.backend
            || journal.policy.state_root != installed.state_root
            || journal.policy.lock_path != installed.lock_path
            || journal.policy.profile_path != installed.profile_path
            || journal.policy.current_system_path != installed.current_system_path
            || journal.transaction.plan.target != installed.target
            || backend_name(journal.transaction.plan.backend) != installed.backend
        {
            return Err("journal does not match its installed recovery scope".into());
        }
        Ok(journal)
    }
}

fn policy_digest(policy: &Policy) -> Result<String, String> {
    let bytes = serde_json::to_vec(policy).map_err(|error| error.to_string())?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn backend_name(backend: Backend) -> &'static str {
    match backend {
        Backend::Nixos => "nixos",
        Backend::HomeManager => "home-manager",
    }
}

fn observation(policy: &Policy) -> Result<(String, String), String> {
    native::observe(&policy.current_system_path, &policy.profile_path)
        .map_err(|error| error.to_string())
}

fn require_identity(policy: &Policy, system: &str, profile: &str) -> Result<(), String> {
    let actual = observation(policy)?;
    if actual.0 != system || actual.1 != profile {
        return Err(format!(
            "generation changed: observed {actual:?}, expected ({system}, {profile})"
        ));
    }
    Ok(())
}

fn verify_health(
    journal: &Journal,
    phase: &str,
    system: &str,
    profile: &str,
) -> Result<(), native::Failure> {
    let policy = &journal.policy;
    let input = serde_json::to_vec(&serde_json::json!({
        "version": 1, "phase": phase, "binding": journal.transaction.binding,
        "expectedSystem": system, "expectedProfile": profile,
    }))
    .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(policy.health_timeout_seconds);
    let mut successes = 0;
    let mut last_error = "health acceptance window expired".to_owned();
    loop {
        require_identity(policy, system, profile)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(last_error.into());
        }
        match native::command(
            &policy.health_command,
            &input,
            policy.command_timeout().min(remaining),
            false,
        ) {
            Ok(_) => {
                successes += 1;
                if successes == 2 {
                    require_identity(policy, system, profile)?;
                    return Ok(());
                }
            }
            Err(error) => {
                if !error.settled {
                    return Err(error);
                }
                successes = 0;
                last_error = error.to_string();
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(last_error.into());
        }
        std::thread::sleep(Duration::from_secs(policy.health_interval_seconds).min(remaining));
    }
}

fn store_root(path: &Path) -> Result<String, String> {
    let mut resolved = std::fs::canonicalize(path).map_err(|error| error.to_string())?;
    while resolved.parent() != Some(Path::new("/nix/store")) {
        if !resolved.pop() {
            return Err("worker and commands must be packaged in the Nix store".into());
        }
    }
    resolved
        .to_str()
        .map(str::to_owned)
        .ok_or("store root is not UTF-8".into())
}

fn retain(journal: &Journal, directory: &Path, policy_path: &Path) -> Result<(), String> {
    let plan = &journal.transaction.plan;
    let before = &journal.transaction.before;
    let mut paths: BTreeSet<String> = [
        Some(plan.candidate.clone()),
        before.system.clone(),
        before.profile.clone(),
    ]
    .into_iter()
    .flatten()
    .collect();
    paths.insert(store_root(
        &std::env::current_exe().map_err(|error| error.to_string())?,
    )?);
    for executable in [
        &journal.policy.nix_store,
        &journal.policy.nix_env,
        &journal.policy.admission_command[0],
        &journal.policy.health_command[0],
    ] {
        paths.insert(store_root(Path::new(executable))?);
    }
    // Non-store policies are frozen in the private journal instead. NixOS /etc
    // policies resolve to a store object, whose closure is additionally rooted.
    if let Ok(path) = store_root(policy_path) {
        paths.insert(path);
    }
    let roots = directory.join("gcroots");
    storage::private_dir(&roots)?;
    native::retain(
        &journal.policy.nix_store,
        &roots,
        &paths.iter().map(String::as_str).collect::<Vec<_>>(),
        journal.policy.command_timeout(),
    )
    .map_err(|error| error.to_string())
}

fn restore(journal: &mut Journal, path: &Path, reason: String) -> Result<(), String> {
    let untouched = !journal.transaction.trial_started;
    if !matches!(
        journal.transaction.state,
        State::Restoring | State::RecoveryFailed
    ) {
        journal.advance(path, Event::VerificationFailed { reason })?;
    }
    if journal.transaction.state == State::RecoveryFailed {
        return Err("previous restoration failed; operator inspection required".into());
    }
    if untouched {
        // Refusal before activation neither changes the host nor certifies its
        // pre-existing health. Do not run old activation hooks to undo a no-op.
        return journal.advance(path, Event::Restored);
    }
    let before = &journal.transaction.before;
    let previous_profile = before
        .profile
        .as_deref()
        .ok_or("missing immutable previous profile")?;
    let previous_system = before.system.as_deref().unwrap_or(previous_profile);
    let actual = observation(&journal.policy)?;
    if (actual.0 != previous_system
        && actual.0 != previous_profile
        && actual.0 != journal.transaction.plan.candidate)
        || (actual.1 != previous_profile && actual.1 != journal.transaction.plan.candidate)
    {
        // A reboot can legitimately start the captured boot profile instead of
        // the captured (test-activated) running generation. Both are retained.
        // Keep the unsettled record, but never overwrite an unrelated generation installed
        // by another authority. The status command identifies the blocked record.
        return Err(format!(
            "restoration of {} refused: an unrelated generation is active",
            path.display()
        ));
    }
    let result: Result<(), native::Failure> = (|| {
        let before = &journal.transaction.before;
        let previous_profile = before
            .profile
            .as_deref()
            .ok_or("missing immutable previous profile")?;
        let previous_system = before.system.as_deref().unwrap_or(previous_profile);
        // Verify the retained targets still exist before executing either one.
        require_store_identity(previous_system)?;
        require_store_identity(previous_profile)?;
        native::restore(
            journal.policy.backend == "nixos",
            &journal.policy.nix_env,
            &journal.policy.profile_path,
            previous_system,
            previous_profile,
            journal.policy.command_timeout(),
        )?;
        verify_health(journal, "restoration", previous_system, previous_profile)
    })();
    match result {
        Ok(()) => journal.advance(path, Event::Restored),
        Err(error) => {
            journal.advance(
                path,
                Event::RecoveryFailed {
                    reason: error.to_string(),
                },
            )?;
            Err(error.to_string())
        }
    }
}

fn require_store_identity(path: &str) -> Result<(), String> {
    let observed =
        native::observe(Path::new(path), Path::new(path)).map_err(|error| error.to_string())?;
    if observed.0 != path {
        return Err("generation is an alias, not an immutable store identity".into());
    }
    Ok(())
}

fn pending(policy: &Policy) -> Result<Vec<PathBuf>, String> {
    let transactions = policy.state_root.join("transactions");
    storage::private_dir(&transactions)?;
    let mut paths = Vec::new();
    // ponytail: retained history is scanned linearly; add an active index only
    // when measured history size makes this recovery scan material.
    for entry in std::fs::read_dir(transactions).map_err(|error| error.to_string())? {
        let directory = entry.map_err(|error| error.to_string())?.path();
        storage::private_dir(&directory)?;
        let path = directory.join("journal.json");
        // A crash while retaining roots can leave a directory without a journal;
        // activation is impossible until the Prepared journal has been synced.
        if !path.exists() {
            continue;
        }
        paths.push(path);
    }
    paths.sort();
    Ok(paths)
}

fn recover_locked(policy: &Policy, retry_failed: bool) -> Result<Vec<Transaction>, String> {
    let mut recovered = Vec::new();
    for path in pending(policy)? {
        let mut journal = Journal::load(&path, policy)?;
        match journal.transaction.state {
            State::Committed | State::Restored => continue,
            State::RecoveryFailed => {
                if !retry_failed {
                    return Err(format!(
                        "{} needs operator inspection; inspect status before an explicit recover --retry-failed",
                        path.display()
                    ));
                }
                journal.advance(&path, Event::RetryRestoration)?;
                restore(
                    &mut journal,
                    &path,
                    "explicit operator restoration retry".into(),
                )?;
            }
            _ => restore(
                &mut journal,
                &path,
                "target-local recovery of an unconfirmed deployment".into(),
            )?,
        }
        recovered.push(journal.transaction);
    }
    Ok(recovered)
}

/// Recover only unsettled transactions using their frozen policy. Completed
/// deployments are never rolled back merely because this service was triggered.
pub fn recover(policy_path: &Path) -> Result<String, String> {
    recover_with_retry(policy_path, false)
}

/// Explicit maintenance action after inspecting a failed restoration. The
/// unrelated-generation guard still applies; this cannot force a rollback.
pub fn retry_recovery(policy_path: &Path) -> Result<String, String> {
    recover_with_retry(policy_path, true)
}

fn recover_with_retry(policy_path: &Path, retry_failed: bool) -> Result<String, String> {
    let policy = Policy::load(policy_path)?;
    storage::private_dir(&policy.state_root)?;
    let _lock = storage::lock(&policy.lock_path).map_err(|error| error.to_string())?;
    serde_json::to_string(&recover_locked(&policy, retry_failed)?)
        .map_err(|error| error.to_string())
}

/// Read atomic snapshots without creating state, changing locks or activating.
pub fn status(policy_path: &Path) -> Result<String, String> {
    let policy = Policy::load(policy_path)?;
    if !policy.state_root.join("transactions").exists() {
        return Ok("[]".into());
    }
    let mut entries = Vec::new();
    for path in pending(&policy)? {
        let journal = Journal::load(&path, &policy)?;
        entries.push(serde_json::json!({"journalPath": path, "transaction": journal.transaction}));
    }
    serde_json::to_string(&entries).map_err(|error| error.to_string())
}

/// Run inside a target-local service with fixed policy arguments. No sudo,
/// interactive authentication or caller-provided executable is used.
pub fn run(policy_path: &Path, request_path: &Path) -> Result<String, WorkerError> {
    let policy = Policy::load(policy_path)?;
    storage::private_dir(&policy.state_root)?;
    let _lock = storage::lock(&policy.lock_path).map_err(|error| WorkerError {
        retryable: error.busy,
        message: error.message,
    })?;
    recover_locked(&policy, false)?;
    let plan: Plan = storage::read_json(request_path, false)?;
    let (system, profile) = observation(&policy)?;
    let binding = plan
        .bind(Some(system.clone()), Some(profile.clone()))
        .map_err(|error| error.to_string())?;
    if plan.target != policy.target || backend_name(plan.backend) != policy.backend {
        return Err("request scope does not match the installed target".into());
    }
    require_store_identity(&plan.candidate)?;
    let directory = policy.state_root.join("transactions").join(&plan.id);
    let path = directory.join("journal.json");
    if path.exists() {
        let journal = Journal::load(&path, &policy)?;
        if journal.transaction.plan != plan {
            return Err("attempt ID already binds a different request".into());
        }
        if journal.transaction.state != State::Committed {
            return Err("attempt was already restored; submit a new attempt".into());
        }
        if journal.policy_digest != policy_digest(&policy)? {
            return Err("installed policy changed; submit a new attempt instead of replaying old health authority".into());
        }
        verify_health(&journal, "recheck", &plan.candidate, &plan.candidate)
            .map_err(|error| error.to_string())?;
        return Ok(serde_json::to_string(&journal.transaction).map_err(|error| error.to_string())?);
    }
    if plan.expected_profile.as_deref() != Some(profile.as_str())
        || (plan.backend == Backend::Nixos
            && plan.expected_system.as_deref() != Some(system.as_str()))
    {
        return Err("request expected generation does not match the installed target".into());
    }
    let input = binding
        .canonical_json()
        .map_err(|error| error.to_string())?;
    let output = native::command(
        &policy.admission_command,
        input.as_bytes(),
        policy.command_timeout(),
        true,
    )
    .map_err(|error| error.to_string())?;
    let admission: Admission = serde_json::from_slice(&output)
        .map_err(|error| format!("invalid admission receipt: {error}"))?;
    let now = unix_seconds()?;
    plan.validate(&binding, &admission, now)
        .map_err(|error| error.to_string())?;
    if admission.expires_at_unix_seconds.saturating_sub(now) > 3600 {
        return Err("admission lifetime exceeds one hour".into());
    }
    let budget = policy.command_timeout_seconds * 3 + policy.health_timeout_seconds * 2 + 15;
    let deadline = now + budget;
    let transaction = Transaction::prepare(plan, binding, admission, deadline, now)
        .map_err(|error| error.to_string())?;
    let mut journal = Journal {
        version: 1,
        policy_digest: policy_digest(&policy)?,
        policy,
        transaction,
    };
    storage::private_dir(&directory)?;
    journal.save(&path)?;
    retain(&journal, &directory, policy_path)?;
    // Retention is non-activating and may need several bounded Nix calls. Start
    // the activation clock afterwards, and reject short-lived approvals before
    // any activation instead of predictably expiring halfway through commit.
    let now = unix_seconds()?;
    if journal.transaction.admission.expires_at_unix_seconds <= now + budget {
        return Err(format!(
            "admission must remain valid for the {budget}s activation budget after retention"
        )
        .into());
    }
    journal.transaction.deadline_unix_seconds = now + budget;
    journal.save(&path)?;
    let result: Result<(), native::Failure> = (|| {
        require_identity(&journal.policy, &system, &profile)?;
        journal.advance(&path, Event::TrialStarted)?;
        let candidate = journal.transaction.plan.candidate.clone();
        native::trial(
            journal.policy.backend == "nixos",
            &candidate,
            journal.policy.command_timeout(),
        )?;
        let trial_profile = if journal.policy.backend == "nixos" {
            &profile
        } else {
            &candidate
        };
        verify_health(&journal, "trial", &candidate, trial_profile)?;
        journal.advance(
            &path,
            Event::TrialSucceeded {
                observed_system: candidate.clone(),
                observed_profile: Some(trial_profile.clone()),
            },
        )?;
        native::commit(
            journal.policy.backend == "nixos",
            &journal.policy.nix_env,
            &journal.policy.profile_path,
            &candidate,
            journal.policy.command_timeout(),
        )?;
        verify_health(&journal, "commit", &candidate, &candidate)?;
        journal.advance(&path, Event::Committed)?;
        Ok(())
    })();
    if let Err(error) = result {
        if !error.settled {
            journal.advance(
                &path,
                Event::VerificationFailed {
                    reason: error.to_string(),
                },
            )?;
            journal.advance(
                &path,
                Event::RecoveryFailed {
                    reason: format!(
                        "action did not quiesce; automatic restoration withheld: {error}"
                    ),
                },
            )?;
            return Err(
                format!("action did not quiesce; operator inspection required: {error}").into(),
            );
        }
        // The interrupted action has been killed and reaped. A second signal
        // may interrupt restoration, but the first must not cancel it outright.
        native::clear_interrupt();
        match restore(&mut journal, &path, error.to_string()) {
            Ok(()) => {
                if !journal.transaction.trial_started {
                    return Err(format!(
                        "{error}; refused before activation, no configuration changed"
                    )
                    .into());
                }
                return Err(format!("{error}; original generations restored and verified").into());
            }
            Err(recovery) => return Err(format!("{error}; restoration failed: {recovery}").into()),
        }
    }
    Ok(serde_json::to_string(&journal.transaction).map_err(|error| error.to_string())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            version: 1,
            target: "fixture".into(),
            backend: "nixos".into(),
            state_root: "/var/lib/fleetix/deployment".into(),
            lock_path: "/run/lock/fleetix-deployment.lock".into(),
            profile_path: "/nix/var/nix/profiles/system".into(),
            current_system_path: "/run/current-system".into(),
            nix_store: "/nix/store/tool/bin/nix-store".into(),
            nix_env: "/nix/store/tool/bin/nix-env".into(),
            admission_command: vec!["/nix/store/verifier/bin/admit".into()],
            health_command: vec!["/nix/store/health/bin/check".into()],
            command_timeout_seconds: 30,
            health_timeout_seconds: 30,
            health_interval_seconds: 1,
        }
    }

    #[test]
    fn policy_requires_fixed_verification_and_bounded_commands() {
        let good = policy();
        assert!(good.validate().is_ok());
        let mut bad = good.clone();
        bad.admission_command.clear();
        assert!(bad.validate().is_err());
        bad = good.clone();
        bad.health_command.clear();
        assert!(bad.validate().is_err());
        bad = good.clone();
        bad.admission_command[0] = "verifier-from-path".into();
        assert!(bad.validate().is_err());
        bad = good.clone();
        bad.command_timeout_seconds = 0;
        assert!(bad.validate().is_err());
        bad = good;
        bad.state_root = "/var/lib/../home/can".into();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn pinning_preserves_multicall_names_without_retaining_mutable_aliases() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("package");
        std::fs::create_dir(&package).unwrap();
        std::fs::write(package.join("nix"), b"fixture binary").unwrap();
        symlink("nix", package.join("nix-store")).unwrap();
        symlink("nix", package.join("nix-env")).unwrap();
        let current = root.path().join("current");
        symlink(&package, &current).unwrap();
        assert_eq!(
            freeze_executable(&current.join("nix")).unwrap(),
            package.join("nix")
        );
        for name in ["nix-store", "nix-env"] {
            let pinned = freeze_executable(&current.join(name)).unwrap();
            assert_eq!(pinned, package.join(name));
            assert_eq!(std::fs::canonicalize(pinned).unwrap(), package.join("nix"));
        }
        // A sibling alias that delegates through mutable state is not pinning.
        symlink("current/nix", root.path().join("redirect")).unwrap();
        symlink(root.path().join("redirect"), package.join("unsafe-alias")).unwrap();
        assert!(freeze_executable(&current.join("unsafe-alias")).is_err());
    }
}
