//! Durable paired-surface lifecycle. Every external mutation is preceded by a
//! persisted journal phase, and every success is derived from observed live
//! identity plus mandatory health, never from a missing file.
//!
//! Runtime policy paths are trusted by the host module, but this module still
//! refuses symlinked components, non-private state, foreign ownership and
//! mismatched journals before opening commands. In fixture mode (unprivileged)
//! the same checks bind the current user and a private temp directory.

use super::*;
use fs2::FileExt;
use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const STATE_FILE: &str = "state.json";
pub const PENDING_FILE: &str = "pending.json";

const MAX_JSON_BYTES: u64 = 256 * 1024;

/// Canonical admission request passed to the host-side operator executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission<'a> {
    pub operation: &'a str,
    pub surface: Surface,
    /// Empty for an initial install.
    pub previous: Option<&'a str>,
    pub candidate: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandEvidence {
    pub command: Vec<String>,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CommandEvidence {
    pub fn output(&self) -> String {
        match (self.stdout.is_empty(), self.stderr.is_empty()) {
            (true, true) => String::new(),
            (false, true) => self.stdout.clone(),
            (true, false) => self.stderr.clone(),
            (false, false) => format!("{}\n{}", self.stdout, self.stderr),
        }
    }
}

/// Side-effect boundary used by the lifecycle. The native implementation
/// invokes argv-only host tools; tests use an in-memory fake.
pub trait Backend: Send + Sync {
    /// Validate an exact realized output, closure presence and release manifest
    /// coverage. Must not build, substitute or evaluate.
    fn validate(&self, artifact: &str) -> Result<(), String>;
    /// Retain an immutable GC root for this exact output.
    fn retain(&self, artifact: &str) -> Result<(), String>;
    /// Run the trusted host-side operator admission command.
    fn admission(&self, request: &Admission<'_>) -> Result<CommandEvidence, String>;
    /// Atomically pin the surface link to the artifact and restart the guest.
    fn activate(&self, surface: Surface, artifact: &str) -> Result<CommandEvidence, String>;
    /// Start the already-pinned guest without changing the surface link.
    fn start(&self, surface: Surface, artifact: &str) -> Result<CommandEvidence, String>;
    /// Return the live guest's canonical `/run/current-system`.
    fn observe(&self, surface: Surface) -> Result<String, String>;
    /// Fail closed while PID 1 still owns a job for this surface.
    fn idle(&self, surface: Surface) -> Result<(), String>;
    /// Positively confirm that the guest is inactive. An observation error is
    /// not proof of absence; unknown controller state must return an error.
    fn inactive(&self, surface: Surface) -> Result<bool, String>;
    /// Stop the guest and wait for the stop job to drain.
    fn stop(&self, surface: Surface) -> Result<CommandEvidence, String>;
    /// Run mandatory release-owned health inside the selected guest.
    fn health(&self, surface: Surface, artifact: &str) -> Result<CommandEvidence, String>;
}

pub fn command_timeout(policy: &Policy) -> Duration {
    Duration::from_secs(policy.command_timeout_seconds)
}

pub fn health_timeout(policy: &Policy) -> Duration {
    Duration::from_secs(policy.health_timeout_seconds)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceObservation {
    pub surface: Surface,
    pub container: String,
    pub expected: Option<String>,
    pub current: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReport {
    pub policy_digest: String,
    pub state: State,
    pub pending: Option<Pending>,
    pub pending_committed: bool,
    pub live: Vec<SurfaceObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationReport {
    pub id: String,
    pub surface: Surface,
    pub artifact: String,
    pub recovered: bool,
    pub aborted: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootReport {
    pub started: Vec<Surface>,
    pub errors: Vec<String>,
}

impl BootReport {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
}

pub fn state_path(policy: &Policy) -> PathBuf {
    policy.state_dir.join(STATE_FILE)
}

pub fn pending_path(policy: &Policy) -> PathBuf {
    policy.state_dir.join(PENDING_FILE)
}

pub fn surface_dir(policy: &Policy, surface: Surface) -> PathBuf {
    policy.state_dir.join(surface.as_str())
}

pub fn surface_link(policy: &Policy, surface: Surface) -> PathBuf {
    surface_dir(policy, surface).join("system")
}

pub fn roots_dir(policy: &Policy) -> PathBuf {
    policy.state_dir.join("roots")
}

pub fn root_path(policy: &Policy, artifact: &str) -> Result<PathBuf, String> {
    validate_store_path("release root", Path::new(artifact))?;
    let name = artifact
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .ok_or("release has no store basename")?;
    Ok(roots_dir(policy).join(name))
}

pub(super) fn ensure_surface_dir(policy: &Policy, surface: Surface) -> Result<PathBuf, String> {
    let directory = surface_dir(policy, surface);
    ensure_private_dir(&directory)?;
    Ok(directory)
}

pub(super) fn ensure_roots_dir(policy: &Policy) -> Result<PathBuf, String> {
    let directory = roots_dir(policy);
    ensure_private_dir(&directory)?;
    Ok(directory)
}

/// Shared host activation lock. The anchor is never unlinked: every frontend
/// takes an exclusive kernel lock on the same inode.
/// Keep it on the acquiring thread: synchronous Native commands retain its
/// lease in a bounded host-only keeper, never in a guest command's fd table.
pub struct HostLock {
    _file: File,
    // Native commands execute synchronously on the acquiring thread.
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

thread_local! {
    static HOST_LEASES: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn lease_descriptors() -> std::io::Result<Vec<OwnedFd>> {
    HOST_LEASES.with_borrow(|fds| {
        fds.iter()
            .map(|&fd| {
                // SAFETY: the thread-bound HostLock still owns fd. The returned owned
                // CLOEXEC copy reserves its number and retains that exact flock lease.
                let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
                if copy < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(unsafe { OwnedFd::from_raw_fd(copy) })
                }
            })
            .collect()
    })
}

impl Drop for HostLock {
    fn drop(&mut self) {
        HOST_LEASES.with_borrow_mut(|fds| fds.retain(|&fd| fd != self._file.as_raw_fd()));
    }
}

impl HostLock {
    pub fn acquire(policy: &Policy) -> Result<Self, String> {
        policy.validate()?;
        let parent = policy
            .lock_path
            .parent()
            .ok_or("activation lock has no parent")?;
        check_trusted_parent(parent)?;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        let file = match options.create_new(true).open(&policy.lock_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
                .open(&policy.lock_path)
                .map_err(|error| {
                    format!(
                        "open activation lock {}: {error}",
                        policy.lock_path.display()
                    )
                })?,
            Err(error) => {
                return Err(format!(
                    "create activation lock {}: {error}",
                    policy.lock_path.display()
                ));
            }
        };
        let metadata = file
            .metadata()
            .map_err(|error| format!("inspect activation lock: {error}"))?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != trusted_uid()
            || metadata.mode() & 0o022 != 0
        {
            return Err(format!(
                "activation lock {} is not an owned, non-publicly-writable regular anchor",
                policy.lock_path.display()
            ));
        }
        file.try_lock_exclusive().map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                format!(
                    "another service-surface operation holds {}",
                    policy.lock_path.display()
                )
            } else {
                format!("lock {}: {error}", policy.lock_path.display())
            }
        })?;
        // Keep CLOEXEC: only Native's bounded host-only keeper receives copies,
        // never guest payloads or unrelated processes/other worker threads.
        HOST_LEASES.with_borrow_mut(|fds| fds.push(file.as_raw_fd()));
        Ok(Self {
            _file: file,
            _thread_bound: std::marker::PhantomData,
        })
    }
}

pub fn load_policy(path: &Path) -> Result<Policy, String> {
    load_policy_with_store_root(path, Path::new("/nix/store"))
}

fn load_policy_with_store_root(path: &Path, store_root: &Path) -> Result<Policy, String> {
    validate_absolute_path("policy path", path)?;
    // NixOS renders environment.etc through symlinked directories and files.
    // Resolve the complete chain, then apply the same immutable-store rules to
    // the canonical target. Journals, state and locks never use this path.
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("resolve policy {}: {error}", path.display()))?;
    let symlinked = canonical != path;
    let metadata = fs::metadata(&canonical)
        .map_err(|error| format!("inspect policy {}: {error}", canonical.display()))?;
    check_policy_target(
        path,
        &canonical,
        &metadata,
        symlinked,
        store_root,
        trusted_uid(),
    )?;
    let file = open_nofollow(&canonical)?;
    let opened = file.metadata().map_err(|error| error.to_string())?;
    if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
        return Err(format!(
            "policy {} changed while it was opened",
            path.display()
        ));
    }
    let policy: Policy = serde_json::from_slice(&read_bounded(file)?)
        .map_err(|error| format!("parse policy {}: {error}", path.display()))?;
    policy.validate()?;
    Ok(policy)
}

fn check_policy_target(
    original: &Path,
    canonical: &Path,
    metadata: &fs::Metadata,
    symlinked: bool,
    store_root: &Path,
    owner: u32,
) -> Result<(), String> {
    let in_store = canonical.starts_with(store_root);
    if symlinked && !in_store {
        return Err(format!(
            "policy symlink {} must resolve to an immutable file under {}",
            original.display(),
            store_root.display()
        ));
    }
    if !metadata.is_file() || metadata.mode() & (if in_store { 0o222 } else { 0o022 }) != 0 {
        return Err(format!(
            "policy target {} must be a non-writable regular file",
            canonical.display()
        ));
    }
    if in_store {
        // Nix store optimisation may hardlink regular files, so nlink is not a
        // trust signal here. Elevated callers still require root ownership.
        if owner == 0 && metadata.uid() != 0 {
            return Err(format!(
                "immutable policy target {} is not root-owned",
                canonical.display()
            ));
        }
        return Ok(());
    }
    if symlinked || metadata.nlink() != 1 || metadata.uid() != owner {
        return Err(format!(
            "policy {} must be a trusted owned regular file",
            original.display()
        ));
    }
    Ok(())
}

/// Explicit deploy of one exact output to dev. Replaces dev only; prod is never
/// touched by this operation.
pub fn deploy_dev(
    policy: &Policy,
    backend: &dyn Backend,
    artifact: &str,
) -> Result<OperationReport, String> {
    policy.validate()?;
    validate_store_path("deploy-dev artifact", Path::new(artifact))?;
    let (_lock, state, _) = load_for_change(policy)?;
    check_baseline(policy, backend, Surface::Dev, state.dev.artifact())?;
    backend.validate(artifact)?;
    backend.retain(artifact)?;
    let mut pending = Pending::new(
        policy,
        &state,
        Operation::DeployDev {
            artifact: artifact.to_owned(),
        },
        now()?,
    )?;
    save_pending(policy, &pending)?;

    let previous = pending.previous().map(str::to_owned);
    let admission = Admission {
        operation: "deploy-dev",
        surface: Surface::Dev,
        previous: previous.as_deref(),
        candidate: artifact,
    };
    let evidence = match backend.admission(&admission) {
        Ok(evidence) => evidence,
        Err(error) => return Err(fail(policy, &mut pending, error)),
    };
    record_evidence(&mut pending, "admission", evidence)?;
    pending.phase = Phase::Admitted;
    save_pending(policy, &pending)?;

    pending.phase = Phase::Activating;
    pending.updated_unix_seconds = now()?;
    save_pending(policy, &pending)?;
    let evidence = match backend.activate(Surface::Dev, artifact) {
        Ok(evidence) => evidence,
        Err(error) => return Err(fail(policy, &mut pending, error)),
    };
    record_evidence(&mut pending, "activate", evidence)?;
    pending.phase = Phase::Activated;
    save_pending(policy, &pending)?;

    let observed = match backend.observe(Surface::Dev) {
        Ok(observed) => observed,
        Err(error) => {
            return Err(fail_after_activation(
                policy,
                backend,
                Surface::Dev,
                &mut pending,
                error,
            ));
        }
    };
    if observed != artifact {
        return Err(fail_after_activation(
            policy,
            backend,
            Surface::Dev,
            &mut pending,
            format!("live dev system {observed} does not match deployed {artifact}"),
        ));
    }
    let evidence = match health_bounded(policy, backend, Surface::Dev, artifact) {
        Ok(evidence) => evidence,
        Err(error) => {
            return Err(fail_after_activation(
                policy,
                backend,
                Surface::Dev,
                &mut pending,
                error,
            ));
        }
    };
    record_evidence(&mut pending, "health", evidence)?;
    pending.phase = Phase::Verified;
    save_pending(policy, &pending)?;
    commit(policy, &mut pending)
}

/// Promote the exact durably verified dev output to prod. Promotion performs
/// no evaluation, build, copy or rootfs swap.
pub fn promote(
    policy: &Policy,
    backend: &dyn Backend,
    expected_dev: &str,
) -> Result<OperationReport, String> {
    policy.validate()?;
    validate_store_path("expected dev release", Path::new(expected_dev))?;
    let (_lock, state, _) = load_for_change(policy)?;
    let dev = state
        .dev
        .artifact()
        .ok_or("dev has no durably verified release to promote")?;
    if dev != expected_dev {
        return Err(format!(
            "stale expected dev release: recorded {dev}, requested {expected_dev}"
        ));
    }
    // A repeat promotion would record the current prod release as its own
    // rollback baseline, making the last good release unreachable.
    if state.prod.artifact() == Some(expected_dev) {
        return Err(format!(
            "prod already records {expected_dev} as its current release; a repeat \
             promotion would overwrite the rollback baseline"
        ));
    }
    // Manifest, closure and exact-output checks happen before any observation,
    // admission or root mutation.
    backend.validate(expected_dev)?;

    let live_dev = backend.observe(Surface::Dev)?;
    if live_dev != expected_dev {
        return Err(format!(
            "live dev system {live_dev} does not match durably verified {expected_dev}"
        ));
    }
    // Fresh health before the operation journal: dev must be healthy now, not
    // merely at deploy time.
    health_bounded(policy, backend, Surface::Dev, expected_dev)?;

    check_baseline(policy, backend, Surface::Prod, state.prod.artifact())?;
    if let Some(prod) = state.prod.artifact() {
        // Preserve the currently recorded prod release before the write-ahead
        // journal and before any prod action.
        backend.retain(prod)?;
    }

    let mut pending = Pending::new(
        policy,
        &state,
        Operation::Promote {
            expected_dev: expected_dev.to_owned(),
        },
        now()?,
    )?;
    save_pending(policy, &pending)?;

    let previous = pending.previous().map(str::to_owned);
    let admission = Admission {
        operation: "promote",
        surface: Surface::Prod,
        previous: previous.as_deref(),
        candidate: expected_dev,
    };
    let evidence = match backend.admission(&admission) {
        Ok(evidence) => evidence,
        Err(error) => return Err(fail(policy, &mut pending, error)),
    };
    record_evidence(&mut pending, "admission", evidence)?;
    pending.phase = Phase::Admitted;
    save_pending(policy, &pending)?;

    pending.phase = Phase::Activating;
    pending.updated_unix_seconds = now()?;
    save_pending(policy, &pending)?;
    let evidence = match backend.activate(Surface::Prod, expected_dev) {
        Ok(evidence) => evidence,
        Err(error) => return Err(fail(policy, &mut pending, error)),
    };
    record_evidence(&mut pending, "activate", evidence)?;
    pending.phase = Phase::Activated;
    save_pending(policy, &pending)?;

    let observed = match backend.observe(Surface::Prod) {
        Ok(observed) => observed,
        Err(error) => {
            return Err(fail_after_activation(
                policy,
                backend,
                Surface::Prod,
                &mut pending,
                error,
            ));
        }
    };
    if observed != expected_dev {
        return Err(fail_after_activation(
            policy,
            backend,
            Surface::Prod,
            &mut pending,
            format!("live prod system {observed} does not match promoted {expected_dev}"),
        ));
    }
    let evidence = match health_bounded(policy, backend, Surface::Prod, expected_dev) {
        Ok(evidence) => evidence,
        Err(error) => {
            return Err(fail_after_activation(
                policy,
                backend,
                Surface::Prod,
                &mut pending,
                error,
            ));
        }
    };
    record_evidence(&mut pending, "health", evidence)?;
    pending.phase = Phase::Verified;
    save_pending(policy, &pending)?;
    commit(policy, &mut pending)
}

/// Restore prod to the release recorded before the most recent successful
/// promotion. This restores the system closure only; data safety is owned by the
/// admission command and is never inferred here.
pub fn rollback(
    policy: &Policy,
    backend: &dyn Backend,
    expected_prod: Option<&str>,
) -> Result<OperationReport, String> {
    policy.validate()?;
    let (_lock, state, _) = load_for_change(policy)?;
    let current = state
        .prod
        .artifact()
        .ok_or("prod has no durably verified release to roll back")?
        .to_owned();
    if let Some(expected) = expected_prod {
        validate_store_path("expected prod release", Path::new(expected))?;
        if expected != current {
            return Err(format!(
                "stale expected prod release: recorded {current}, requested {expected}"
            ));
        }
    }
    let target = state
        .prod
        .previous
        .clone()
        .ok_or("prod has no previous release recorded")?;
    if target == current {
        return Err("prod rollback target equals the current release".into());
    }
    backend.validate(&target)?;
    let live = backend.observe(Surface::Prod)?;
    if live != current {
        return Err(format!(
            "prod drift: live prod system {live} does not match recorded {current}"
        ));
    }
    backend.retain(&target)?;

    let mut pending = Pending::new(
        policy,
        &state,
        Operation::RollbackProd {
            expected_prod: current.clone(),
            target: target.clone(),
        },
        now()?,
    )?;
    save_pending(policy, &pending)?;
    let admission = Admission {
        operation: "rollback",
        surface: Surface::Prod,
        previous: Some(&current),
        candidate: &target,
    };
    let evidence = match backend.admission(&admission) {
        Ok(evidence) => evidence,
        Err(error) => return Err(fail(policy, &mut pending, error)),
    };
    record_evidence(&mut pending, "admission", evidence)?;
    pending.phase = Phase::Admitted;
    save_pending(policy, &pending)?;

    pending.phase = Phase::Activating;
    pending.updated_unix_seconds = now()?;
    save_pending(policy, &pending)?;
    let evidence = match backend.activate(Surface::Prod, &target) {
        Ok(evidence) => evidence,
        Err(error) => return Err(fail(policy, &mut pending, error)),
    };
    record_evidence(&mut pending, "activate", evidence)?;
    pending.phase = Phase::Activated;
    save_pending(policy, &pending)?;

    let observed = match backend.observe(Surface::Prod) {
        Ok(observed) => observed,
        Err(error) => {
            return Err(fail_after_activation(
                policy,
                backend,
                Surface::Prod,
                &mut pending,
                error,
            ));
        }
    };
    if observed != target {
        return Err(fail_after_activation(
            policy,
            backend,
            Surface::Prod,
            &mut pending,
            format!("live prod system {observed} does not match rollback {target}"),
        ));
    }
    let evidence = match health_bounded(policy, backend, Surface::Prod, &target) {
        Ok(evidence) => evidence,
        Err(error) => {
            return Err(fail_after_activation(
                policy,
                backend,
                Surface::Prod,
                &mut pending,
                error,
            ));
        }
    };
    record_evidence(&mut pending, "health", evidence)?;
    pending.phase = Phase::Verified;
    save_pending(policy, &pending)?;
    commit(policy, &mut pending)
}

/// Explicit recovery. Restores the exact committed state snapshot captured by
/// the pending operation, but only after the operator admission command
/// authorizes the restore. Before activation no operation has touched the live
/// surface, so any journal can be cancelled against its unchanged baseline;
/// after activation an initial install must forward-complete through `resume`.
pub fn recover(
    policy: &Policy,
    backend: &dyn Backend,
    operation: Option<&str>,
) -> Result<OperationReport, String> {
    policy.validate()?;
    let (_lock, state, pending) = load_locked(policy)?;
    let mut pending = pending.ok_or("no pending operation to recover")?;
    if let Some(operation) = operation {
        validate_operation_id(operation)?;
        if operation != pending.id {
            return Err(format!(
                "pending operation is {}, not requested {operation}",
                pending.id
            ));
        }
    }
    pending.validate(policy)?;
    if pending.committed(&state)? {
        let surface = pending.surface();
        let artifact = if pending.recovered {
            pending
                .previous()
                .ok_or("recovery journal has no previous release")?
                .to_owned()
        } else {
            pending.candidate().to_owned()
        };
        let id = pending.id.clone();
        remove_pending(policy)?;
        return Ok(OperationReport {
            id,
            surface,
            artifact,
            recovered: pending.recovered,
            aborted: false,
        });
    }
    let surface = pending.surface();
    // Never activated: admission refused before any activation intent, so the
    // live surface is still on its recorded baseline. Restoring here would
    // restart a healthy prod (or dev) for a refusal that changed nothing.
    if matches!(pending.phase, Phase::Prepared | Phase::Admitted) {
        return cancel_pre_activation(policy, backend, pending);
    }
    let target = pending.previous().map(str::to_owned).ok_or_else(|| {
        format!(
            "pending operation {} is an initial {} install with no previous release; \
                 recovery refuses until an explicit safe procedure is performed; \
                 use `fleetix-surfaces --policy <path> resume --operation {}` to \
                 forward-complete the exact pending candidate",
            pending.id, surface, pending.id
        )
    })?;
    backend.validate(&target)?;
    backend.retain(&target)?;

    pending.recovered = true;
    pending.phase = Phase::Recovering;
    pending.failure = None;
    pending.updated_unix_seconds = now()?;
    save_pending(policy, &pending)?;

    let attempted = pending.candidate().to_owned();
    let admission = Admission {
        operation: "recover",
        surface,
        previous: Some(&attempted),
        candidate: &target,
    };
    let evidence = match backend.admission(&admission) {
        Ok(evidence) => evidence,
        Err(error) => return Err(fail_recovery(policy, &mut pending, error)),
    };
    record_evidence(&mut pending, "admission", evidence)?;
    save_pending(policy, &pending)?;

    let evidence = match backend.activate(surface, &target) {
        Ok(evidence) => evidence,
        Err(error) => return Err(fail_recovery(policy, &mut pending, error)),
    };
    record_evidence(&mut pending, "activate", evidence)?;
    save_pending(policy, &pending)?;

    let observed = match backend.observe(surface) {
        Ok(observed) => observed,
        Err(error) => {
            return Err(fail_recovery_after_activation(
                policy,
                backend,
                surface,
                &mut pending,
                error,
            ));
        }
    };
    if observed != target {
        return Err(fail_recovery_after_activation(
            policy,
            backend,
            surface,
            &mut pending,
            format!("live {surface} system {observed} does not match restored {target}"),
        ));
    }
    let evidence = match health_bounded(policy, backend, surface, &target) {
        Ok(evidence) => evidence,
        Err(error) => {
            return Err(fail_recovery_after_activation(
                policy,
                backend,
                surface,
                &mut pending,
                error,
            ));
        }
    };
    record_evidence(&mut pending, "health", evidence)?;
    save_pending(policy, &pending)?;
    commit(policy, &mut pending)
}

/// No-op cancellation for any journal that provably never started activation
/// (`Prepared` or `Admitted`). The live surface must still match the journal's
/// recorded baseline: the previous release when one exists, otherwise a
/// confirmed inactive, unselected guest. Not a general journal clear: later
/// phases must use resume or recover.
fn cancel_pre_activation(
    policy: &Policy,
    backend: &dyn Backend,
    mut pending: Pending,
) -> Result<OperationReport, String> {
    let surface = pending.surface();
    if pending.phase.activation_may_have_started() {
        return Err(format!(
            "pending operation {} may have started activation; refusing no-op cancellation",
            pending.id
        ));
    }
    if let Some(previous) = pending.previous() {
        // A live surface must still be the recorded previous release. A
        // confirmed inactive surface is also an unchanged baseline (for
        // example after a reboot): aborting leaves committed state
        // authoritative for `boot`.
        backend.idle(surface)?;
        match backend.observe(surface) {
            Ok(live) if live == previous => {}
            Ok(live) => {
                return Err(format!(
                    "{surface} drift: live system {live} does not match recorded {previous}"
                ));
            }
            Err(error) => {
                if !backend.inactive(surface)? {
                    return Err(format!(
                        "cannot verify live {surface} baseline before abort: {error}; \
                         guest is not confirmed inactive"
                    ));
                }
            }
        }
    } else {
        check_baseline(policy, backend, surface, None)?;
    }
    let candidate = pending.candidate().to_owned();
    let previous = pending.previous().map(str::to_owned);
    let admission = Admission {
        operation: "abort",
        surface,
        previous: previous.as_deref(),
        candidate: &candidate,
    };
    if let Err(error) = backend.admission(&admission) {
        return Err(fail(policy, &mut pending, error));
    }
    // State-before is unchanged and candidate roots are deliberately retained.
    remove_pending(policy)?;
    Ok(OperationReport {
        id: pending.id,
        surface,
        artifact: candidate,
        recovered: false,
        aborted: true,
    })
}

/// Forward-complete one exact pending candidate. This is the safe path for an
/// initial install with no previous release; it never evaluates source, copies
/// state or fabricates a baseline.
pub fn resume(
    policy: &Policy,
    backend: &dyn Backend,
    operation: Option<&str>,
) -> Result<OperationReport, String> {
    policy.validate()?;
    let (_lock, state, pending) = load_locked(policy)?;
    let mut pending = pending.ok_or("no pending operation to resume")?;
    if let Some(operation) = operation {
        validate_operation_id(operation)?;
        if operation != pending.id {
            return Err(format!(
                "pending operation is {}, not requested {operation}",
                pending.id
            ));
        }
    }
    pending.validate(policy)?;
    if pending.committed(&state)? {
        let surface = pending.surface();
        let artifact = if pending.recovered {
            pending
                .previous()
                .ok_or("recovery journal has no previous release")?
                .to_owned()
        } else {
            pending.candidate().to_owned()
        };
        let id = pending.id.clone();
        remove_pending(policy)?;
        return Ok(OperationReport {
            id,
            surface,
            artifact,
            recovered: pending.recovered,
            aborted: false,
        });
    }
    if pending.recovered {
        return Err(format!(
            "pending operation {} already entered recovery; use recover, not resume",
            pending.id
        ));
    }
    let surface = pending.surface();
    let candidate = pending.candidate().to_owned();
    let previous = pending.previous().map(str::to_owned);
    backend.validate(&candidate)?;
    match &pending.operation {
        Operation::Promote { expected_dev } => {
            if state.dev.artifact() != Some(expected_dev.as_str()) {
                return Err(format!(
                    "promote resume refused: recorded dev release {} does not match pending \
                     expected dev {expected_dev}",
                    state.dev.artifact().unwrap_or("none")
                ));
            }
            match backend.observe(Surface::Dev) {
                Ok(live_dev) => {
                    if live_dev != *expected_dev {
                        return Err(format!(
                            "promote resume refused: live dev system {live_dev} does not match \
                             expected dev {expected_dev}"
                        ));
                    }
                }
                Err(error) => {
                    // A reboot stops both guests while the journal stays
                    // pending. Only a provably inactive guest is restarted,
                    // from its unchanged pinned release, and verified before
                    // the promotion continues; nothing is evaluated or rebuilt.
                    backend.idle(Surface::Dev)?;
                    if !backend.inactive(Surface::Dev)? {
                        return Err(format!(
                            "promote resume refused: cannot verify live dev system before \
                             resume: {error}; the guest is not confirmed inactive"
                        ));
                    }
                    boot_surface(policy, backend, Surface::Dev, expected_dev).map_err(|boot| {
                        format!(
                            "promote resume refused: could not restart the committed dev \
                             release after observation failed ({error}): {boot}"
                        )
                    })?;
                }
            }
            health_bounded(policy, backend, Surface::Dev, expected_dev)?;
        }
        Operation::RollbackProd { expected_prod, .. } => {
            if state.prod.artifact() != Some(expected_prod.as_str()) {
                return Err(format!(
                    "rollback resume refused: recorded prod release {} does not match pending \
                     expected prod {expected_prod}",
                    state.prod.artifact().unwrap_or("none")
                ));
            }
        }
        Operation::DeployDev { .. } => {}
    }
    match backend.observe(surface) {
        Ok(live) => {
            let matches_candidate = live == candidate;
            let matches_previous = previous.as_deref() == Some(live.as_str());
            if !matches_candidate && !matches_previous {
                return Err(format!(
                    "live {surface} system {live} is neither pending candidate {candidate} nor \
                     previous {}; refusing resume",
                    previous.as_deref().unwrap_or("none")
                ));
            }
        }
        Err(error) => {
            backend.idle(surface)?;
            if !backend.inactive(surface)? {
                return Err(format!(
                    "cannot verify live {surface} baseline before resume: {error}; guest is not confirmed inactive"
                ));
            }
            let selected = selected_link(policy, surface)?;
            let known_pin = selected.as_deref().is_some_and(|pin| {
                pin == Path::new(&candidate)
                    || previous.as_deref().is_some_and(|old| pin == Path::new(old))
            });
            if !known_pin && (selected.is_some() || previous.is_some()) {
                return Err(format!(
                    "cannot verify selected {surface} baseline before resume: {error}; refusing an unknown pin"
                ));
            }
        }
    }
    backend.retain(&candidate)?;
    if let Some(failure) = pending.failure.take() {
        pending.evidence.push(Evidence {
            action: "previous-failure".into(),
            command: Vec::new(),
            exit_code: None,
            output: truncate(failure.message, 4096),
            unix_seconds: failure.unix_seconds,
        });
        if pending.evidence.len() > 16 {
            pending.evidence.remove(0);
        }
    }
    let admission = Admission {
        operation: pending.operation.label(),
        surface,
        previous: previous.as_deref(),
        candidate: &candidate,
    };
    let evidence = match backend.admission(&admission) {
        Ok(evidence) => evidence,
        Err(error) => return Err(fail(policy, &mut pending, error)),
    };
    record_evidence(&mut pending, "admission", evidence)?;
    pending.phase = Phase::Activating;
    pending.updated_unix_seconds = now()?;
    save_pending(policy, &pending)?;

    let evidence = match backend.activate(surface, &candidate) {
        Ok(evidence) => evidence,
        Err(error) => return Err(fail(policy, &mut pending, error)),
    };
    record_evidence(&mut pending, "activate", evidence)?;
    pending.phase = Phase::Activated;
    save_pending(policy, &pending)?;

    let observed = match backend.observe(surface) {
        Ok(observed) => observed,
        Err(error) => {
            return Err(fail_after_activation(
                policy,
                backend,
                surface,
                &mut pending,
                error,
            ));
        }
    };
    if observed != candidate {
        return Err(fail_after_activation(
            policy,
            backend,
            surface,
            &mut pending,
            format!("live {surface} system {observed} does not match resumed {candidate}"),
        ));
    }
    let evidence = match health_bounded(policy, backend, surface, &candidate) {
        Ok(evidence) => evidence,
        Err(error) => {
            return Err(fail_after_activation(
                policy,
                backend,
                surface,
                &mut pending,
                error,
            ));
        }
    };
    record_evidence(&mut pending, "health", evidence)?;
    pending.phase = Phase::Verified;
    save_pending(policy, &pending)?;
    commit(policy, &mut pending)
}

/// Boot known durable deployments only. No journal creation, initialization,
/// build or new release selection happens here. A pending transaction refuses
/// unattended boot; a missing state directory is a safe no-op.
pub fn boot(policy: &Policy, backend: &dyn Backend) -> Result<BootReport, String> {
    policy.validate()?;
    let _lock = HostLock::acquire(policy)?;
    let state = load_state_readonly(policy)?;
    if let Some(pending) = load_pending_readonly(policy)? {
        pending.validate(policy)?;
        if !pending.committed(&state)? {
            return Err(format!(
                "refusing unattended boot: pending {} operation {} at {}",
                pending.operation.label(),
                pending.id,
                pending.phase
            ));
        }
    }
    let mut report = BootReport::default();
    for surface in [Surface::Dev, Surface::Prod] {
        let Some(artifact) = state.record(surface).artifact() else {
            continue;
        };
        let outcome = boot_surface(policy, backend, surface, artifact);
        match outcome {
            Ok(()) => report.started.push(surface),
            Err(error) => report.errors.push(format!("{surface}: {error}")),
        }
    }
    Ok(report)
}

/// Bounded readiness retry: a container may be systemd-started before its
/// application and database are ready. Every attempt is itself bounded by the
/// backend command timeout; this loop bounds the total window.
fn health_bounded(
    policy: &Policy,
    backend: &dyn Backend,
    surface: Surface,
    artifact: &str,
) -> Result<CommandEvidence, String> {
    let window = health_timeout(policy);
    let interval = Duration::from_secs(policy.health_interval_seconds);
    let started = Instant::now();
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        match backend.health(surface, artifact) {
            Ok(evidence) => return Ok(evidence),
            Err(error) => {
                let elapsed = started.elapsed();
                if elapsed >= window {
                    return Err(format!(
                        "{surface} health failed after {attempts} attempt(s) within {}s: {error}",
                        policy.health_timeout_seconds
                    ));
                }
                thread::sleep(interval.min(window - elapsed));
            }
        }
    }
}

fn boot_surface(
    policy: &Policy,
    backend: &dyn Backend,
    surface: Surface,
    artifact: &str,
) -> Result<(), String> {
    backend.validate(artifact)?;
    let link = surface_link(policy, surface);
    let pinned = fs::read_link(&link)
        .map_err(|error| format!("read pinned system link {}: {error}", link.display()))?;
    if pinned != Path::new(artifact) {
        return Err(format!(
            "pinned {} link {} targets {}, not durably verified {artifact}",
            surface,
            link.display(),
            pinned.display()
        ));
    }
    let root = root_path(policy, artifact)?;
    let rooted = fs::read_link(&root)
        .map_err(|error| format!("read release root {}: {error}", root.display()))?;
    if rooted != Path::new(artifact) {
        return Err(format!(
            "release root {} targets {}, not durably verified {artifact}",
            root.display(),
            rooted.display()
        ));
    }
    backend.start(surface, artifact)?;
    let verification = (|| -> Result<(), String> {
        let observed = backend.observe(surface)?;
        if observed != artifact {
            return Err(format!(
                "live {surface} system {observed} does not match durably verified {artifact}"
            ));
        }
        health_bounded(policy, backend, surface, artifact)?;
        Ok(())
    })();
    if let Err(error) = verification {
        return match backend.stop(surface) {
            Ok(_) => Err(format!(
                "{error}; stopped {surface} after failed boot verification"
            )),
            Err(stop) => Err(format!(
                "{error}; additionally failed to stop {surface}: {stop}; \
                 containment is incomplete"
            )),
        };
    }
    Ok(())
}

/// Read-only status. Does not create directories, acquire the mutation lock, or
/// run admission/health.
pub fn status(policy: &Policy, backend: &dyn Backend) -> Result<StatusReport, String> {
    policy.validate()?;
    let state = load_state_readonly(policy)?;
    let pending = load_pending_readonly(policy)?;
    let pending_committed = match &pending {
        Some(pending) => {
            pending.validate(policy)?;
            pending.committed(&state)?
        }
        None => false,
    };
    let mut live = Vec::with_capacity(2);
    for surface in [Surface::Dev, Surface::Prod] {
        let expected = state.record(surface).artifact().map(str::to_owned);
        let observation = match backend.observe(surface) {
            Ok(current) => SurfaceObservation {
                surface,
                container: policy.surface_container(surface).to_owned(),
                expected,
                current: Some(current),
                error: None,
            },
            Err(error) => SurfaceObservation {
                surface,
                container: policy.surface_container(surface).to_owned(),
                expected,
                current: None,
                error: Some(error),
            },
        };
        live.push(observation);
    }
    Ok(StatusReport {
        policy_digest: policy.digest()?,
        state,
        pending,
        pending_committed,
        live,
    })
}

fn load_for_change(policy: &Policy) -> Result<(HostLock, State, Option<Pending>), String> {
    let lock = HostLock::acquire(policy)?;
    ensure_private_dir(&policy.state_dir)?;
    let state = load_state(policy)?.unwrap_or(State::new(policy)?);
    state.validate(policy)?;
    if let Some(pending) = load_pending(policy)? {
        pending.validate(policy)?;
        if pending.committed(&state)? {
            remove_pending(policy)?;
        } else {
            return Err(format!(
                "pending {} operation {} at {} blocks further changes; run \
                 `fleetix-surfaces --policy <path> recover --operation {}`",
                pending.operation.label(),
                pending.id,
                pending.phase,
                pending.id
            ));
        }
    }
    Ok((lock, state, None))
}

fn selected_link(policy: &Policy, surface: Surface) -> Result<Option<PathBuf>, String> {
    let link = surface_link(policy, surface);
    match fs::symlink_metadata(&link) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("inspect {}: {error}", link.display())),
        Ok(metadata) if metadata.file_type().is_symlink() => fs::read_link(&link)
            .map(Some)
            .map_err(|error| format!("read {}: {error}", link.display())),
        Ok(_) => Err(format!("selected link {} is not a symlink", link.display())),
    }
}

fn check_baseline(
    policy: &Policy,
    backend: &dyn Backend,
    surface: Surface,
    expected: Option<&str>,
) -> Result<(), String> {
    backend.idle(surface)?;
    if let Some(expected) = expected {
        let live = backend.observe(surface)?;
        if live != expected {
            return Err(format!(
                "{surface} drift: live system {live} does not match recorded {expected}"
            ));
        }
    } else {
        if !backend.inactive(surface)? {
            return Err(format!(
                "unrecorded {surface} guest is not confirmed inactive"
            ));
        }
        if selected_link(policy, surface)?.is_some() {
            return Err(format!(
                "unrecorded {surface} has an existing selected link; refusing adoption"
            ));
        }
    }
    Ok(())
}

fn load_locked(policy: &Policy) -> Result<(HostLock, State, Option<Pending>), String> {
    let lock = HostLock::acquire(policy)?;
    ensure_private_dir(&policy.state_dir)?;
    let state = load_state(policy)?.unwrap_or(State::new(policy)?);
    state.validate(policy)?;
    let pending = load_pending(policy)?;
    if let Some(pending) = &pending {
        pending.validate(policy)?;
    }
    Ok((lock, state, pending))
}

fn load_state_readonly(policy: &Policy) -> Result<State, String> {
    match fs::symlink_metadata(&policy.state_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => State::new(policy),
        Err(error) => Err(format!(
            "inspect state directory {}: {error}",
            policy.state_dir.display()
        )),
        Ok(_) => {
            check_private_dir(&policy.state_dir)?;
            Ok(load_state(policy)?.unwrap_or(State::new(policy)?))
        }
    }
}

fn load_pending_readonly(policy: &Policy) -> Result<Option<Pending>, String> {
    match fs::symlink_metadata(&policy.state_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "inspect state directory {}: {error}",
            policy.state_dir.display()
        )),
        Ok(_) => {
            check_private_dir(&policy.state_dir)?;
            load_pending(policy)
        }
    }
}

fn load_state(policy: &Policy) -> Result<Option<State>, String> {
    let path = state_path(policy);
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            let state: State = read_private_json(&path)?;
            state.validate(policy)?;
            Ok(Some(state))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("inspect state {}: {error}", path.display())),
    }
}

fn load_pending(policy: &Policy) -> Result<Option<Pending>, String> {
    let path = pending_path(policy);
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            let pending: Pending = read_private_json(&path)?;
            pending.validate(policy)?;
            Ok(Some(pending))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "inspect pending journal {}: {error}",
            path.display()
        )),
    }
}

fn save_pending(policy: &Policy, pending: &Pending) -> Result<(), String> {
    pending.validate(policy)?;
    write_private_json(&pending_path(policy), pending)
}

fn save_state(policy: &Policy, state: &State) -> Result<(), String> {
    state.validate(policy)?;
    write_private_json(&state_path(policy), state)
}

fn remove_pending(policy: &Policy) -> Result<(), String> {
    let path = pending_path(policy);
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("remove {}: {error}", path.display())),
    }
    File::open(&policy.state_dir)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync state directory: {error}"))
}

fn commit(policy: &Policy, pending: &mut Pending) -> Result<OperationReport, String> {
    let after = pending.expected_after_commit()?;
    if let Err(error) = save_state(policy, &after) {
        return Err(fail(
            policy,
            pending,
            format!("persist committed state: {error}"),
        ));
    }
    pending.phase = Phase::Committed;
    pending.updated_unix_seconds = now()?;
    if let Err(error) = save_pending(policy, pending) {
        return Err(format!(
            "committed state is durable but journal finalization failed: {error}; \
             rerun status or recover to finalize"
        ));
    }
    remove_pending(policy)?;
    let artifact = if pending.recovered {
        pending
            .previous()
            .ok_or("recovery committed without a previous release")?
            .to_owned()
    } else {
        pending.candidate().to_owned()
    };
    Ok(OperationReport {
        id: pending.id.clone(),
        surface: pending.surface(),
        artifact,
        recovered: pending.recovered,
        aborted: false,
    })
}

fn record_evidence(
    pending: &mut Pending,
    action: &str,
    evidence: CommandEvidence,
) -> Result<(), String> {
    let output = truncate(evidence.output(), 8192);
    pending.evidence.push(Evidence {
        action: action.to_owned(),
        command: evidence.command,
        exit_code: evidence.exit_code,
        output,
        unix_seconds: now()?,
    });
    if pending.evidence.len() > 16 {
        pending.evidence.remove(0);
    }
    pending.updated_unix_seconds = now()?;
    Ok(())
}

fn fail(policy: &Policy, pending: &mut Pending, message: String) -> String {
    let phase = pending.phase;
    pending.failure = Some(Failure {
        phase,
        message: truncate(message.clone(), 4096),
        unix_seconds: now().unwrap_or(1),
    });
    pending.updated_unix_seconds = now().unwrap_or(1);
    match save_pending(policy, pending) {
        Ok(()) => format!(
            "operation {} ({}) failed at {}: {message}; pending journal {} blocks further \
             changes; run `fleetix-surfaces --policy <path> recover --operation {}`",
            pending.id,
            pending.operation.label(),
            phase,
            pending.id,
            pending.id
        ),
        Err(error) => format!(
            "operation {} ({}) failed at {}: {message}; additionally failed to persist the \
             failure: {error}",
            pending.id,
            pending.operation.label(),
            phase
        ),
    }
}

fn fail_after_activation(
    policy: &Policy,
    backend: &dyn Backend,
    surface: Surface,
    pending: &mut Pending,
    message: String,
) -> String {
    let message = match backend.stop(surface) {
        Ok(_) => {
            format!("{message}; stopped {surface} so an unverified closure is not left running")
        }
        Err(error) => format!(
            "{message}; additionally failed to stop {surface}: {error}; \
             containment is incomplete"
        ),
    };
    fail(policy, pending, message)
}

fn fail_recovery_after_activation(
    policy: &Policy,
    backend: &dyn Backend,
    surface: Surface,
    pending: &mut Pending,
    message: String,
) -> String {
    pending.phase = Phase::RecoveryFailed;
    fail_after_activation(policy, backend, surface, pending, message)
}

fn fail_recovery(policy: &Policy, pending: &mut Pending, message: String) -> String {
    pending.phase = Phase::RecoveryFailed;
    fail(policy, pending, message)
}

fn truncate(value: String, max: usize) -> String {
    if value.len() <= max {
        return value;
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut value = value;
    value.truncate(end);
    value
}

fn now() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .map_err(|error| error.to_string())
}

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and does not mutate state.
    unsafe { libc::geteuid() }
}

fn trusted_uid() -> u32 {
    euid()
}

pub(super) fn ensure_private_dir(path: &Path) -> Result<(), String> {
    validate_absolute_path("private directory", path)?;
    match fs::symlink_metadata(path) {
        Ok(_) => check_private_dir(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or("private directory has no parent")?;
            check_trusted_parent(parent)?;
            fs::DirBuilder::new()
                .mode(0o700)
                .create(path)
                .map_err(|error| format!("create {}: {error}", path.display()))?;
            check_private_dir(path)
        }
        Err(error) => Err(format!("inspect {}: {error}", path.display())),
    }
}

fn check_private_dir(path: &Path) -> Result<(), String> {
    reject_symlink_ancestors(path)?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != trusted_uid()
        || metadata.mode() & 0o077 != 0
    {
        return Err(format!(
            "state directory {} must be a private directory owned by this user",
            path.display()
        ));
    }
    Ok(())
}

fn check_trusted_parent(path: &Path) -> Result<(), String> {
    reject_symlink_ancestors(path)?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != trusted_uid()
        || metadata.mode() & 0o022 != 0
    {
        return Err(format!(
            "parent {} must be a trusted directory not writable by others",
            path.display()
        ));
    }
    Ok(())
}

fn reject_symlink_ancestors(path: &Path) -> Result<(), String> {
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        let metadata = fs::symlink_metadata(ancestor)
            .map_err(|error| format!("inspect {}: {error}", ancestor.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "{} must not contain a symlinked component ({})",
                path.display(),
                ancestor.display()
            ));
        }
    }
    Ok(())
}

fn read_private_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let file = open_nofollow(path)?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != trusted_uid()
        || metadata.mode() & 0o077 != 0
    {
        return Err(format!(
            "{} must be a private owned regular file",
            path.display()
        ));
    }
    serde_json::from_slice(&read_bounded(file)?)
        .map_err(|error| format!("parse {}: {error}", path.display()))
}

fn write_private_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path.parent().ok_or("journal has no parent")?;
    check_private_dir(parent)?;
    if fs::symlink_metadata(path).is_ok() {
        let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.nlink() != 1
            || metadata.uid() != trusted_uid()
            || metadata.mode() & 0o077 != 0
        {
            return Err(format!(
                "{} must be a private owned regular file",
                path.display()
            ));
        }
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    crate::fsutil::atomic_write(path, &bytes).map_err(|error| error.to_string())
}

fn open_nofollow(path: &Path) -> Result<File, String> {
    validate_absolute_path("trusted file", path)?;
    let components: Vec<_> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    if components.is_empty() {
        return Err("trusted file path is empty".into());
    }
    let mut directory = File::open("/").map_err(|error| error.to_string())?;
    for (index, name) in components.iter().enumerate() {
        let name = std::ffi::CString::new(name.as_bytes())
            .map_err(|error| format!("invalid path component: {error}"))?;
        let final_component = index + 1 == components.len();
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if final_component {
                libc::O_NONBLOCK
            } else {
                libc::O_DIRECTORY
            };
        // SAFETY: the directory descriptor is open and name is a live,
        // NUL-terminated single component. No creation mode is used.
        let descriptor = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if descriptor < 0 {
            return Err(format!(
                "open {} without symlinks: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: openat returned a newly owned descriptor.
        directory = unsafe { File::from_raw_fd(descriptor) };
    }
    Ok(directory)
}

fn read_bounded(mut file: File) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_JSON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_JSON_BYTES {
        return Err(format!("JSON input exceeds {} bytes", MAX_JSON_BYTES));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn fixture_policy() -> Policy {
        Policy {
            version: 1,
            host: "atlas".into(),
            state_dir: PathBuf::from("/var/lib/fleetix-surfaces"),
            lock_path: PathBuf::from("/run/lock/fleetix/switch.lock"),
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
            admission_check: PathBuf::from(
                "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-coreutils/bin/false",
            ),
            container_config_dir: PathBuf::from("/etc/nixos-containers"),
            command_timeout_seconds: 10,
            health_timeout_seconds: 1,
            health_interval_seconds: 1,
        }
    }

    fn write_policy(path: &Path) -> Vec<u8> {
        let bytes = serde_json::to_vec(&fixture_policy()).unwrap();
        fs::write(path, &bytes).unwrap();
        bytes
    }

    #[test]
    fn immutable_store_policy_symlink_accepts_hardlinked_target() {
        let root = tempfile::tempdir().unwrap();
        let store = root.path().join("store");
        let etc = root.path().join("etc");
        fs::create_dir(&store).unwrap();
        fs::create_dir(&etc).unwrap();
        let target = store.join("0123456789abcdfghijklmnpqrsvwxyz-policy.json");
        write_policy(&target);
        fs::set_permissions(&target, fs::Permissions::from_mode(0o444)).unwrap();
        fs::hard_link(&target, store.join("alias-policy.json")).unwrap();
        let link = etc.join("policy.json");
        symlink(&target, &link).unwrap();

        let policy = load_policy_with_store_root(&link, &store).unwrap();
        assert_eq!(policy.host, "atlas");
    }

    #[test]
    fn non_store_policy_symlink_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let store = root.path().join("store");
        fs::create_dir(&store).unwrap();
        let target = root.path().join("policy.json");
        write_policy(&target);
        let link = root.path().join("policy-link.json");
        symlink(&target, &link).unwrap();
        let error = load_policy_with_store_root(&link, &store).unwrap_err();
        assert!(error.contains("immutable file"), "{error}");
    }

    #[test]
    fn dangling_and_writable_policy_targets_are_refused() {
        let root = tempfile::tempdir().unwrap();
        let store = root.path().join("store");
        fs::create_dir(&store).unwrap();
        let dangling = root.path().join("dangling.json");
        symlink(root.path().join("missing.json"), &dangling).unwrap();
        assert!(load_policy_with_store_root(&dangling, &store).is_err());

        let writable = store.join("0123456789abcdfghijklmnpqrsvwxyz-policy.json");
        write_policy(&writable);
        let link = root.path().join("writable-link.json");
        symlink(&writable, &link).unwrap();
        for mode in [0o644, 0o666] {
            fs::set_permissions(&writable, fs::Permissions::from_mode(mode)).unwrap();
            let error = load_policy_with_store_root(&link, &store).unwrap_err();
            assert!(error.contains("non-writable"), "{error}");
        }
    }

    #[test]
    fn non_store_private_fixture_policy_is_accepted() {
        let root = tempfile::tempdir().unwrap();
        let store = root.path().join("store");
        fs::create_dir(&store).unwrap();
        let path = root.path().join("policy.json");
        write_policy(&path);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        load_policy_with_store_root(&path, &store).unwrap();
    }
}
