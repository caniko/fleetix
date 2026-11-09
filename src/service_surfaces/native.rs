//! Native argv-only backend for persistent NixOS containers.
//!
//! Never calls `nixos-container update`: it evaluates and builds. Activation
//! atomically replaces the declarative container's stable `system` symlink and
//! restarts `container@<name>.service`, which reboots the exact candidate
//! closure into the same persistent root filesystem. Admission is a host-side
//! operator executable independent of the candidate.

use super::runtime::{self, Admission, Backend, CommandEvidence};
use super::*;
use std::fs;
use std::io;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::symlink;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_MANIFEST_BYTES: u64 = 128 * 1024;
const MAX_CONTAINER_CONFIG_BYTES: u64 = 64 * 1024;
const MAX_CAPTURE_BYTES: usize = 8192;
const MAX_CLOSURE_BYTES: usize = 8 * 1024 * 1024;
const MAX_CLOSURE_PATHS: usize = 100_000;
const VALIDITY_CHUNK: usize = 256;
const POLL_INTERVAL: Duration = Duration::from_millis(25);
/// Bound for the cgroup kill that follows a timed-out guest command. The lease
/// stays held while it runs, so it must not be able to wait indefinitely.
const SCOPE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const CONTAINER_ROOT: &str = "/var/lib/nixos-containers";
const GUEST_READLINK: &str = "/run/current-system/sw/bin/readlink";

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static SCOPE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct Native {
    policy: Policy,
}

impl Native {
    pub fn new(policy: &Policy) -> Result<Self, String> {
        policy.validate()?;
        for (field, path) in policy.tools.all() {
            validate_tool(field, path)?;
        }
        // systemd-run ships in the same systemd output as the configured
        // systemctl, so no separate policy field is needed; validate the
        // derived tool here with the others rather than when a scope is built.
        validate_tool(
            "systemd_run",
            &policy.tools.systemctl.with_file_name("systemd-run"),
        )?;
        validate_executable("admission_check", &policy.admission_check)?;
        Ok(Self {
            policy: policy.clone(),
        })
    }

    fn container(&self, surface: Surface) -> &str {
        self.policy.surface_container(surface)
    }

    fn health_hook(&self, artifact: &str) -> Result<PathBuf, String> {
        let hook = Path::new(artifact).join(HEALTH_BIN);
        validate_executable("release health hook", &hook)?;
        Ok(hook)
    }

    fn manifest(&self, artifact: &str) -> Result<Manifest, String> {
        let path = Path::new(artifact).join(MANIFEST_FILE);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("release manifest {}: {error}", path.display()))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(format!(
                "release manifest {} must be one regular file",
                path.display()
            ));
        }
        if metadata.len() > MAX_MANIFEST_BYTES {
            return Err(format!(
                "release manifest {} exceeds {MAX_MANIFEST_BYTES} bytes",
                path.display()
            ));
        }
        let bytes = fs::read(&path)
            .map_err(|error| format!("read manifest {}: {error}", path.display()))?;
        let manifest: Manifest = serde_json::from_slice(&bytes)
            .map_err(|error| format!("parse manifest {}: {error}", path.display()))?;
        manifest.validate(&self.policy)?;
        Ok(manifest)
    }

    fn check_closure(&self, artifact: &str) -> Result<(), String> {
        self.check_closure_with_limit(artifact, MAX_CLOSURE_BYTES)
    }

    fn check_closure_with_limit(&self, artifact: &str, max_bytes: usize) -> Result<(), String> {
        let nix_store = path_string("nix_store", &self.policy.tools.nix_store)?;
        let query = vec![
            nix_store.clone(),
            "--query".into(),
            "--requisites".into(),
            artifact.to_owned(),
        ];
        let (evidence, truncated) = run_with_limit(
            &query,
            None,
            runtime::command_timeout(&self.policy),
            "nix-store --query --requisites",
            max_bytes,
            None,
        )?;
        if truncated {
            return Err(format!(
                "release closure query exceeded the {max_bytes} byte bound (or the {MAX_CAPTURE_BYTES} byte diagnostic bound)"
            ));
        }
        let mut paths = Vec::new();
        for line in evidence.stdout.lines() {
            let path = line.trim();
            if path.is_empty() {
                continue;
            }
            // Derivations may be retained as data by build/cache services;
            // only the executable release root must not itself be a .drv.
            validate_store_path(
                "release closure path",
                Path::new(path.strip_suffix(".drv").unwrap_or(path)),
            )?;
            paths.push(path.to_owned());
            if paths.len() > MAX_CLOSURE_PATHS {
                return Err(format!(
                    "release closure exceeds the {MAX_CLOSURE_PATHS} path bound"
                ));
            }
        }
        if paths.is_empty() {
            return Err("release closure query returned no store paths".into());
        }
        if !paths.iter().any(|path| path == artifact) {
            return Err(format!("release closure does not include {artifact}"));
        }
        for chunk in paths.chunks(VALIDITY_CHUNK) {
            let mut argv = vec![nix_store.clone(), "--check-validity".into()];
            argv.extend(chunk.iter().cloned());
            run(
                &argv,
                None,
                runtime::command_timeout(&self.policy),
                "nix-store --check-validity",
            )?;
        }
        Ok(())
    }

    fn retain_root(&self, artifact: &str) -> Result<PathBuf, String> {
        let roots = runtime::ensure_roots_dir(&self.policy)?;
        let name = artifact
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .ok_or("release has no store basename")?;
        let root = roots.join(name);
        // A symlink alone is not proof of an indirect root registration.
        // Validate an existing link, then always ask Nix to (re-)register it.
        match fs::symlink_metadata(&root) {
            Ok(metadata) => {
                if !metadata.file_type().is_symlink() || metadata.uid() != euid() {
                    return Err(format!(
                        "release root {} is not an owned symlink; refusing to replace it",
                        root.display()
                    ));
                }
                let target = fs::read_link(&root)
                    .map_err(|error| format!("read release root {}: {error}", root.display()))?;
                if target != Path::new(artifact) {
                    return Err(format!(
                        "release root {} points to {}, not {artifact}",
                        root.display(),
                        target.display()
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("inspect release root {}: {error}", root.display())),
        }
        let argv = vec![
            path_string("nix_store", &self.policy.tools.nix_store)?,
            "--add-root".into(),
            root.to_str()
                .ok_or("release root path is not UTF-8")?
                .to_owned(),
            "--indirect".into(),
            "--realise".into(),
            artifact.to_owned(),
        ];
        run(
            &argv,
            None,
            runtime::command_timeout(&self.policy),
            "nix-store --add-root",
        )?;
        let metadata = fs::symlink_metadata(&root)
            .map_err(|error| format!("inspect release root {}: {error}", root.display()))?;
        if !metadata.file_type().is_symlink() || metadata.uid() != euid() {
            return Err(format!(
                "release root {} is not an owned symlink after registration",
                root.display()
            ));
        }
        let target = fs::read_link(&root)
            .map_err(|error| format!("read release root {}: {error}", root.display()))?;
        if target != Path::new(artifact) {
            return Err(format!(
                "release root {} did not pin {artifact}",
                root.display()
            ));
        }
        Ok(root)
    }

    fn unit(&self, surface: Surface) -> String {
        format!("container@{}.service", self.container(surface))
    }

    /// A killed `systemctl` client does not cancel the job already submitted to
    /// PID 1. Fail closed until that unit has no active systemd job.
    fn unit_job(&self, surface: Surface) -> Result<Option<String>, String> {
        let argv = vec![
            path_string("systemctl", &self.policy.tools.systemctl)?,
            "show".into(),
            "--property=Job".into(),
            "--value".into(),
            self.unit(surface),
        ];
        let evidence = run(
            &argv,
            None,
            runtime::command_timeout(&self.policy),
            "systemctl show Job",
        )?;
        let value = evidence.stdout.trim();
        if value.is_empty() || value == "0" {
            return Ok(None);
        }
        if value.lines().count() != 1 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(format!(
                "container {} returned an ambiguous systemd job value {:?}",
                self.container(surface),
                value
            ));
        }
        Ok(Some(value.to_owned()))
    }

    fn ensure_unit_idle(&self, surface: Surface) -> Result<(), String> {
        if let Some(job) = self.unit_job(surface)? {
            return Err(format!(
                "container {} still has active systemd job {job}; refusing until the \
                 owning job finishes or an explicit recovery releases it",
                self.container(surface)
            ));
        }
        Ok(())
    }

    fn wait_unit_idle(&self, surface: Surface) -> Result<(), String> {
        let deadline = Instant::now() + runtime::command_timeout(&self.policy);
        loop {
            if self.unit_job(surface)?.is_none() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "container {} still has an active systemd job after {}s",
                    self.container(surface),
                    self.policy.command_timeout_seconds
                ));
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    /// Parse only the declarative generator's known `SYSTEM_PATH=` line. No
    /// shell evaluation, no flake mode, no duplicate definitions.
    fn check_container_config(&self, surface: Surface) -> Result<(), String> {
        let path = self
            .policy
            .container_config_dir
            .join(format!("{}.conf", self.container(surface)));
        let canonical = fs::canonicalize(&path)
            .map_err(|error| format!("resolve container config {}: {error}", path.display()))?;
        let metadata = fs::metadata(&canonical)
            .map_err(|error| format!("inspect container config {}: {error}", path.display()))?;
        if !metadata.is_file()
            || metadata.mode() & 0o022 != 0
            || metadata.len() > MAX_CONTAINER_CONFIG_BYTES
        {
            return Err(format!(
                "container config {} must be one non-writable regular file",
                path.display()
            ));
        }
        // SAFETY: geteuid has no preconditions and does not mutate state.
        if unsafe { libc::geteuid() } == 0 {
            if !canonical.starts_with("/nix/store/") {
                return Err(format!(
                    "container config {} must resolve into the immutable Nix store",
                    path.display()
                ));
            }
            if metadata.uid() != 0 {
                return Err(format!(
                    "container config {} is not root-owned",
                    path.display()
                ));
            }
        } else if metadata.uid() != euid() {
            return Err(format!(
                "container config {} is not owned by this user",
                path.display()
            ));
        }
        let text = fs::read_to_string(&canonical)
            .map_err(|error| format!("read container config {}: {error}", path.display()))?;
        parse_system_path(&text, &runtime::surface_link(&self.policy, surface))
    }

    fn install_link(&self, surface: Surface, artifact: &str) -> Result<(), String> {
        let directory = runtime::ensure_surface_dir(&self.policy, surface)?;
        let link = directory.join("system");
        if let Ok(metadata) = fs::symlink_metadata(&link) {
            if !metadata.file_type().is_symlink() {
                return Err(format!(
                    "surface link {} is not a symlink; refusing to replace it",
                    link.display()
                ));
            }
        }
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = directory.join(format!(".system.{}.{}", std::process::id(), sequence));
        symlink(artifact, &temporary)
            .map_err(|error| format!("create {}: {error}", temporary.display()))?;
        if let Err(error) = fs::rename(&temporary, &link) {
            let _ = fs::remove_file(&temporary);
            return Err(format!("replace surface link {}: {error}", link.display()));
        }
        fs::File::open(&directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("sync {}: {error}", directory.display()))?;
        let pinned = fs::read_link(&link)
            .map_err(|error| format!("read surface link {}: {error}", link.display()))?;
        if pinned != Path::new(artifact) {
            return Err(format!(
                "surface link {} targets {}, not {artifact}",
                link.display(),
                pinned.display()
            ));
        }
        Ok(())
    }

    fn activate_inner(&self, surface: Surface, artifact: &str) -> Result<CommandEvidence, String> {
        self.ensure_unit_idle(surface)?;
        self.check_container_config(surface)?;
        self.install_link(surface, artifact)?;
        self.start_unit(surface, "restart").map_err(|error| {
            format!("{error}; surface link now targets {artifact}, explicit recovery is required")
        })
    }

    fn admission_inner(&self, request: &Admission<'_>) -> Result<CommandEvidence, String> {
        self.ensure_unit_idle(request.surface)?;
        let container = self.container(request.surface);
        let argv = vec![
            path_string("admission_check", &self.policy.admission_check)?,
            request.operation.to_owned(),
            request.surface.as_str().to_owned(),
            request.previous.unwrap_or("").to_owned(),
            request.candidate.to_owned(),
            container.to_owned(),
        ];
        let root = format!("{CONTAINER_ROOT}/{container}");
        run(
            &argv,
            Some(("FLEETIX_SURFACE_ROOT", root)),
            runtime::command_timeout(&self.policy),
            "operator admission check",
        )
    }

    fn start_unit(&self, surface: Surface, action: &str) -> Result<CommandEvidence, String> {
        let argv = vec![
            path_string("systemctl", &self.policy.tools.systemctl)?,
            action.into(),
            self.unit(surface),
        ];
        run(
            &argv,
            None,
            runtime::command_timeout(&self.policy),
            &format!("systemctl {action}"),
        )
    }

    /// Run one guest health attempt inside a transient systemd scope.
    ///
    /// `nixos-container run` reaches the release hook through `su -l -c`, which
    /// calls setsid(): a hung hook leaves the spawned process group, so only a
    /// cgroup-wide kill contains it. The scope is named so the timeout path can
    /// kill exactly this attempt without touching any other unit.
    fn run_health_scoped(
        &self,
        argv: &[String],
        timeout: Duration,
        label: &str,
    ) -> Result<CommandEvidence, String> {
        let systemctl = path_string("systemctl", &self.policy.tools.systemctl)?;
        // systemd-run ships in the same systemd output as the configured
        // systemctl, so no separate policy field is needed.
        let systemd_run = path_string(
            "systemd_run",
            &self.policy.tools.systemctl.with_file_name("systemd-run"),
        )?;
        let unit = format!(
            "fleetix-surfaces-health-{}-{}-{}.scope",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            SCOPE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let mut scoped = vec![
            systemd_run,
            "--scope".into(),
            "--collect".into(),
            "--quiet".into(),
            format!("--unit={unit}"),
            // If this process dies before cleanup, systemd SIGKILLs the scope
            // 500 ms after the command timeout: after the live timeout path,
            // before the crash keeper releases the lease at one second.
            // ponytail: the keeper's clock starts before the scope's, so slow
            // scope startup can let an escaped hook outlive the lease after a
            // CLI crash; have the keeper confirm the scope cgroup is empty if
            // that overlap must be impossible.
            format!("--property=RuntimeMaxSec={}ms", timeout.as_millis() + 500),
            "--property=KillSignal=SIGKILL".into(),
            "--".into(),
        ];
        scoped.extend_from_slice(argv);
        let cleanup = vec![
            systemctl,
            "kill".into(),
            "--kill-whom=all".into(),
            "--signal=KILL".into(),
            unit,
        ];
        run_bounded(&scoped, None, timeout, label, Some(&cleanup))
    }
}

impl Backend for Native {
    fn validate(&self, artifact: &str) -> Result<(), String> {
        validate_store_path("release", Path::new(artifact))?;
        validate_release_directory(Path::new(artifact))?;
        self.check_closure(artifact)?;
        self.manifest(artifact)?;
        self.health_hook(artifact)?;
        Ok(())
    }

    fn retain(&self, artifact: &str) -> Result<(), String> {
        self.validate(artifact)?;
        self.retain_root(artifact).map(|_| ())
    }

    fn admission(&self, request: &Admission<'_>) -> Result<CommandEvidence, String> {
        validate_store_path("admission candidate", Path::new(request.candidate))?;
        self.validate(request.candidate)?;
        self.admission_inner(request)
    }

    fn activate(&self, surface: Surface, artifact: &str) -> Result<CommandEvidence, String> {
        self.validate(artifact)?;
        self.activate_inner(surface, artifact)
    }

    fn start(&self, surface: Surface, artifact: &str) -> Result<CommandEvidence, String> {
        validate_store_path("booted release", Path::new(artifact))?;
        self.check_container_config(surface)?;
        self.ensure_unit_idle(surface)?;
        let link = runtime::surface_link(&self.policy, surface);
        let pinned = fs::read_link(&link)
            .map_err(|error| format!("read surface link {}: {error}", link.display()))?;
        if pinned != Path::new(artifact) {
            return Err(format!(
                "surface link {} targets {}, not durably verified {artifact}",
                link.display(),
                pinned.display()
            ));
        }
        self.start_unit(surface, "start")
    }

    fn stop(&self, surface: Surface) -> Result<CommandEvidence, String> {
        let evidence = self.start_unit(surface, "stop")?;
        self.wait_unit_idle(surface)?;
        Ok(evidence)
    }

    fn idle(&self, surface: Surface) -> Result<(), String> {
        self.ensure_unit_idle(surface)
    }

    fn inactive(&self, surface: Surface) -> Result<bool, String> {
        self.ensure_unit_idle(surface)?;
        let argv = vec![
            path_string("systemctl", &self.policy.tools.systemctl)?,
            "show".into(),
            "--property=ActiveState".into(),
            "--value".into(),
            self.unit(surface),
        ];
        let evidence = run(
            &argv,
            None,
            runtime::command_timeout(&self.policy),
            "systemctl show ActiveState",
        )?;
        match evidence.stdout.trim() {
            "inactive" => Ok(true),
            "active" | "activating" | "deactivating" | "reloading" | "failed" | "maintenance"
            | "refreshing" => Ok(false),
            value => Err(format!(
                "unknown {surface} ActiveState {value:?}; refusing absence assumption"
            )),
        }
    }

    fn observe(&self, surface: Surface) -> Result<String, String> {
        self.ensure_unit_idle(surface)?;
        let argv = vec![
            path_string("nixos_container", &self.policy.tools.nixos_container)?,
            "run".into(),
            self.container(surface).to_owned(),
            "--".into(),
            GUEST_READLINK.into(),
            "--canonicalize".into(),
            "/run/current-system".into(),
        ];
        let evidence = run(
            &argv,
            None,
            runtime::command_timeout(&self.policy),
            "observe current system",
        )?;
        let current = evidence.stdout.trim();
        if current.is_empty() || current.lines().count() != 1 {
            return Err(format!(
                "container {} did not report one canonical system path: {}",
                self.container(surface),
                evidence.output()
            ));
        }
        validate_store_path("observed current system", Path::new(current))?;
        Ok(current.to_owned())
    }

    fn health(&self, surface: Surface, artifact: &str) -> Result<CommandEvidence, String> {
        let hook = self.health_hook(artifact)?;
        let argv = vec![
            path_string("nixos_container", &self.policy.tools.nixos_container)?,
            "run".into(),
            self.container(surface).to_owned(),
            "--".into(),
            hook.to_str()
                .ok_or("health hook path is not UTF-8")?
                .to_owned(),
            surface.as_str().to_owned(),
            artifact.to_owned(),
        ];
        self.run_health_scoped(
            &argv,
            runtime::command_timeout(&self.policy),
            "release health check",
        )
    }
}

fn parse_system_path(text: &str, expected: &Path) -> Result<(), String> {
    let mut found: Option<&str> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(value) = line.strip_prefix("SYSTEM_PATH=") {
            if found.is_some() {
                return Err("container config defines SYSTEM_PATH more than once".into());
            }
            let value = unquote(value.trim())
                .ok_or("container config has an ambiguous quoted SYSTEM_PATH")?;
            found = Some(value);
        } else if line.starts_with("FLAKE=") {
            return Err("container config selects a flake; expected an exact SYSTEM_PATH".into());
        }
    }
    let found = found.ok_or("container config does not define SYSTEM_PATH")?;
    if Path::new(found) != expected {
        return Err(format!(
            "container config SYSTEM_PATH {found:?} does not match expected {}",
            expected.display()
        ));
    }
    Ok(())
}

fn unquote(value: &str) -> Option<&str> {
    if let Some(value) = value.strip_prefix('\'') {
        return value.strip_suffix('\'');
    }
    if let Some(value) = value.strip_prefix('"') {
        return value.strip_suffix('"');
    }
    if value.contains('\'') || value.contains('"') {
        return None;
    }
    Some(value)
}

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and does not mutate state.
    unsafe { libc::geteuid() }
}

fn validate_tool(field: &str, path: &Path) -> Result<(), String> {
    validate_absolute_path(field, path)?;
    if euid() == 0 {
        return validate_executable(field, path);
    }
    let metadata =
        fs::metadata(path).map_err(|error| format!("{field} {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.mode() & 0o111 == 0 || metadata.mode() & 0o022 != 0 {
        return Err(format!("{field} {} is not executable", path.display()));
    }
    Ok(())
}

fn validate_release_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("release {}: {error}", path.display()))?;
    if !metadata.is_dir() || metadata.mode() & 0o222 != 0 || (euid() == 0 && metadata.uid() != 0) {
        return Err(format!(
            "release {} must be an immutable directory, not a symlink or mutable alias",
            path.display()
        ));
    }
    Ok(())
}

fn validate_executable(field: &str, path: &Path) -> Result<(), String> {
    validate_store_executable_path(field, path)?;
    let canonical =
        fs::canonicalize(path).map_err(|error| format!("{field} {}: {error}", path.display()))?;
    if !canonical.starts_with("/nix/store/") {
        return Err(format!("{field} {} escapes the Nix store", path.display()));
    }
    let metadata =
        fs::metadata(&canonical).map_err(|error| format!("{field} executable: {error}"))?;
    if !metadata.is_file()
        || metadata.mode() & 0o111 == 0
        || metadata.mode() & 0o222 != 0
        || (euid() == 0 && metadata.uid() != 0)
    {
        return Err(format!("{field} {} is not executable", path.display()));
    }
    Ok(())
}

fn path_string(field: &str, path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{field} is not valid UTF-8"))
}

fn run(
    argv: &[String],
    environment: Option<(&str, String)>,
    timeout: Duration,
    label: &str,
) -> Result<CommandEvidence, String> {
    run_bounded(argv, environment, timeout, label, None)
}

fn run_bounded(
    argv: &[String],
    environment: Option<(&str, String)>,
    timeout: Duration,
    label: &str,
    cleanup: Option<&[String]>,
) -> Result<CommandEvidence, String> {
    let (evidence, truncated) = run_with_limit(
        argv,
        environment,
        timeout,
        label,
        MAX_CAPTURE_BYTES,
        cleanup,
    )?;
    if truncated {
        return Err(format!(
            "{label}: output exceeded the {MAX_CAPTURE_BYTES} byte bound or could not be read completely"
        ));
    }
    Ok(evidence)
}

fn run_with_limit(
    argv: &[String],
    environment: Option<(&str, String)>,
    timeout: Duration,
    label: &str,
    capture_limit: usize,
    cleanup: Option<&[String]>,
) -> Result<(CommandEvidence, bool), String> {
    let executable = argv.first().ok_or("command has no executable")?;
    let mut command = Command::new(executable);
    command
        // Never carry host credentials, Nix overrides, or bus/socket overrides
        // across the guest boundary. Explicit admission context is added below.
        .env_clear()
        .env("PATH", "/run/current-system/sw/bin:/usr/bin:/bin")
        .current_dir("/")
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some((key, value)) = environment {
        command.env(key, value);
    }
    let leases = runtime::lease_descriptors()
        .map_err(|error| format!("{label}: retain host descriptors: {error}"))?;
    let mut lease_fds: Vec<_> = leases.iter().map(AsRawFd::as_raw_fd).collect();
    lease_fds.sort_unstable();
    // Let the live caller report its normal timeout first. If that caller is
    // killed, the host-only keeper still releases the lease within this grace.
    let timeout_ms = i32::try_from(timeout.as_millis().saturating_add(1000))
        .map_err(|_| "command timeout exceeds poll bound")?;
    // Give the action its own process group so a timeout or error kills the
    // whole command tree, not only the direct client. PID 1 jobs submitted by
    // systemctl are separately guarded by ensure_unit_idle().
    // SAFETY: the callback uses only syscalls after fork. The bounded keeper
    // stays in host namespaces; CLOEXEC fences every extra fd from the payload.
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            prepare_command_child(&lease_fds, timeout_ms)
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("{label}: spawn {executable}: {error}"))?;
    let process_group = child.id();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{label}: missing stdout pipe"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("{label}: missing stderr pipe"))?;
    let (sender, receiver) = mpsc::channel();
    let stdout_sender = sender.clone();
    let stdout_thread = thread::spawn(move || {
        let _ = stdout_sender.send((0u8, drain(stdout, capture_limit)));
    });
    let stderr_thread = thread::spawn(move || {
        let _ = sender.send((1u8, drain(stderr, MAX_CAPTURE_BYTES)));
    });
    let deadline = Instant::now() + timeout;
    let mut containment = Ok(());
    let (status, timed_out) = loop {
        match child_exited(process_group) {
            Ok(true) => {
                // Keep the zombie leader unreaped until after terminating its
                // group: its PID/PGID cannot be reused for an unrelated owner.
                let _ = terminate(process_group, cleanup);
                let status = child
                    .wait()
                    .map_err(|error| format!("{label}: reap child: {error}"))?;
                break (Some(status), false);
            }
            Ok(false) => {
                if Instant::now() >= deadline {
                    containment = terminate(process_group, cleanup);
                    let _ = child.wait();
                    break (None, true);
                }
                thread::sleep(POLL_INTERVAL);
            }
            Err(error) => {
                let _ = terminate(process_group, cleanup);
                let _ = child.wait();
                return Err(format!("{label}: wait for {executable}: {error}"));
            }
        }
    };
    // Every exit path killed the group before reaping the leader. No detached
    // work may keep capture pipes or the inherited host lock alive.
    let drain_deadline = if timed_out {
        Instant::now() + Duration::from_secs(1)
    } else {
        deadline
    };
    let mut stdout = None;
    let mut stderr = None;
    while stdout.is_none() || stderr.is_none() {
        let now = Instant::now();
        if now >= drain_deadline {
            return Err(format!(
                "{label}: descendant output drain exceeded the command deadline"
            ));
        }
        match receiver.recv_timeout(drain_deadline - now) {
            Ok((0, output)) => stdout = Some(output),
            Ok((1, output)) => stderr = Some(output),
            Ok(_) => {}
            Err(error) => return Err(format!("{label}: descendant output drain failed: {error}")),
        }
    }
    let _ = stdout_thread.join();
    let _ = stderr_thread.join();
    let (stdout, stdout_truncated) = stdout.ok_or("missing stdout capture")?;
    let (stderr, stderr_truncated) = stderr.ok_or("missing stderr capture")?;
    let (stdout, stderr) = (
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    );
    let evidence = CommandEvidence {
        command: argv.to_vec(),
        exit_code: status.as_ref().and_then(|status| status.code()),
        stdout,
        stderr,
    };
    let truncated = stdout_truncated || stderr_truncated;
    if timed_out {
        let containment = match containment {
            Ok(()) => String::new(),
            Err(error) => format!("; additionally failed to kill the command's cgroup: {error}"),
        };
        return Err(format!(
            "{label} timed out after {}s: {}{containment}",
            timeout.as_secs(),
            evidence.output()
        ));
    }
    if status.is_some_and(|status| status.success()) {
        return Ok((evidence, truncated));
    }
    Err(format!(
        "{label} failed (exit {:?}): {}{}",
        evidence.exit_code,
        evidence.output(),
        if truncated {
            "\n[output truncated]"
        } else {
            ""
        }
    ))
}

#[cfg(target_os = "linux")]
fn prepare_command_child(leases: &[i32], timeout_ms: i32) -> io::Result<()> {
    // SAFETY: called only in pre_exec. No allocation, locks or Rust destructors
    // run in the keeper. Its pidfd refers to the unreaped command leader, and
    // membership reserves that process group even if the CLI is killed.
    unsafe {
        if !leases.is_empty() {
            let pidfd = libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0) as i32;
            if pidfd < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut ready = [-1; 2];
            if libc::pipe2(ready.as_mut_ptr(), libc::O_CLOEXEC) < 0 {
                return Err(io::Error::last_os_error());
            }
            let keeper = libc::fork();
            if keeper < 0 {
                return Err(io::Error::last_os_error());
            }
            if keeper == 0 {
                if libc::dup2(pidfd, 0) < 0 || libc::dup2(ready[1], 1) < 0 {
                    libc::_exit(125);
                }
                // Keep only the pidfd, readiness pipe and reserved lease copies;
                // in particular, close Rust's exec-error and output pipes.
                let mut first = 2u32;
                for &fd in leases {
                    if first < fd as u32
                        && libc::syscall(libc::SYS_close_range, first, fd as u32 - 1, 0) < 0
                    {
                        libc::_exit(125);
                    }
                    first = fd as u32 + 1;
                }
                if libc::syscall(libc::SYS_close_range, first, u32::MAX, 0) < 0 {
                    libc::_exit(125);
                }
                if libc::write(1, b"R".as_ptr().cast(), 1) != 1 {
                    libc::_exit(125);
                }
                libc::close(1);
                let mut poll = libc::pollfd {
                    fd: 0,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // Timeout, leader exit, or an unexpected signal all fail closed.
                libc::poll(&mut poll, 1, timeout_ms);
                libc::kill(-libc::getpgrp(), libc::SIGKILL);
                libc::_exit(125);
            }
            libc::close(ready[1]);
            libc::close(pidfd);
            let mut byte = 0u8;
            let read = libc::read(ready[0], (&mut byte as *mut u8).cast(), 1);
            libc::close(ready[0]);
            if read != 1 || byte != b'R' {
                return Err(io::Error::from_raw_os_error(libc::EIO));
            }
        }
        if libc::syscall(
            libc::SYS_close_range,
            3u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        ) < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn prepare_command_child(_: &[i32], _: i32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native service surfaces require Linux pidfds and close_range",
    ))
}

fn kill_process_group(process_group: u32) {
    if process_group <= i32::MAX as u32 {
        // SAFETY: kill with a negative pid targets the child's own process
        // group. Failure (for example ESRCH) is harmless here.
        let _ = unsafe { libc::kill(-(process_group as i32), libc::SIGKILL) };
    }
}

/// Kill the command's process group and, when the command is bounded by a
/// transient systemd scope, its whole cgroup.
///
/// Guest commands reach the release hook through `su -l -c`, which calls
/// setsid() for a command invocation. The hook therefore leaves the process
/// group and would survive a group-only kill while the lease is released; only
/// the scope's cgroup still contains it.
fn terminate(process_group: u32, cleanup: Option<&[String]>) -> Result<(), String> {
    kill_process_group(process_group);
    let Some(argv) = cleanup else {
        return Ok(());
    };
    run(argv, None, SCOPE_CLEANUP_TIMEOUT, "kill command scope").map(|_| ())
}

fn child_exited(pid: u32) -> io::Result<bool> {
    // SAFETY: waitid writes a siginfo_t for our child. WNOWAIT reserves its PID
    // until the process group has been terminated and Child::wait reaps it.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok(info.si_signo != 0)
}

fn drain(mut stream: impl Read, limit: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut buffer = [0u8; 4096];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                if kept.len() < limit {
                    let remaining = limit - kept.len();
                    let take = read.min(remaining);
                    kept.extend_from_slice(&buffer[..take]);
                    if take < read {
                        truncated = true;
                    }
                } else {
                    truncated = true;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => {
                truncated = true;
                break;
            }
        }
    }
    (kept, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service_surfaces::runtime::HostLock;
    use crate::service_surfaces::{Containers, Tools};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};

    fn script(path: &Path, body: &str) {
        let harness_path = std::env::var("PATH").expect("test harness PATH");
        let quoted_path = harness_path.replace('\'', "'\\''");
        let body = body.strip_prefix("#!/bin/sh\n").unwrap_or(body);
        fs::write(
            path,
            format!("#!/bin/sh\nexport PATH='{quoted_path}'\n{body}"),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn fixture_policy(root: &Path) -> Policy {
        let state = root.join("state");
        fs::DirBuilder::new().mode(0o700).create(&state).unwrap();
        let containers = root.join("containers");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&containers)
            .unwrap();
        let bin = root.join("bin");
        fs::DirBuilder::new().mode(0o700).create(&bin).unwrap();
        Policy {
            version: 1,
            host: "atlas".into(),
            state_dir: state,
            lock_path: root.join("switch.lock"),
            containers: Containers {
                dev: "fleetix-dev".into(),
                prod: "fleetix-prod".into(),
            },
            required_services: vec!["database".into(), "web".into()],
            tools: Tools {
                systemctl: bin.join("systemctl"),
                nixos_container: bin.join("nixos-container"),
                nix_store: bin.join("nix-store"),
            },
            admission_check: PathBuf::from(
                "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-admission/bin/admit",
            ),
            container_config_dir: containers,
            command_timeout_seconds: 2,
            health_timeout_seconds: 1,
            health_interval_seconds: 1,
        }
    }

    fn systemctl_script(policy: &Policy, log: &Path, job: &Path) {
        script(
            &policy.tools.systemctl,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n\
                 if [ \"$1\" = show ]; then cat '{}' 2>/dev/null || echo 0; fi\n",
                log.display(),
                job.display()
            ),
        );
    }

    #[test]
    fn inactivity_is_positive_controller_evidence_not_an_empty_response() {
        let root = tempfile::tempdir().unwrap();
        let policy = fixture_policy(root.path());
        let state = root.path().join("active-state");
        script(
            &policy.tools.systemctl,
            &format!(
                "#!/bin/sh\nif [ \"$2\" = --property=Job ]; then echo 0; else cat '{}'; fi\n",
                state.display()
            ),
        );
        let native = Native { policy };
        fs::write(&state, "inactive\n").unwrap();
        assert!(native.inactive(Surface::Prod).unwrap());
        fs::write(&state, "active\n").unwrap();
        assert!(!native.inactive(Surface::Prod).unwrap());
        fs::write(&state, "").unwrap();
        assert!(native.inactive(Surface::Prod).is_err());
        fs::write(&state, "unavailable\n").unwrap();
        assert!(native.inactive(Surface::Prod).is_err());
    }

    #[test]
    fn release_directory_cannot_be_a_mutable_or_symlinked_alias() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("release");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o555)).unwrap();
        validate_release_directory(&directory).unwrap();
        let alias = root.path().join("alias");
        symlink(&directory, &alias).unwrap();
        assert!(validate_release_directory(&alias).is_err());
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(validate_release_directory(&directory).is_err());
    }

    #[test]
    fn successful_commands_cannot_return_silently_truncated_protocol_data() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("large-response");
        fs::write(&data, vec![b'x'; MAX_CAPTURE_BYTES + 1]).unwrap();
        let executable = root.path().join("emit");
        script(
            &executable,
            &format!("#!/bin/sh\ncat '{}'\n", data.display()),
        );
        let error = run(
            &[executable.to_str().unwrap().into()],
            None,
            Duration::from_secs(2),
            "response",
        )
        .unwrap_err();
        assert!(error.contains("exceeded"), "{error}");
    }

    #[test]
    fn native_new_validates_the_derived_systemd_run_tool() {
        if unsafe { libc::geteuid() } == 0 {
            // Root validation requires every tool to be an immutable Nix store
            // executable, which this fixture cannot provide.
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let policy = fixture_policy(root.path());
        for tool in [
            &policy.tools.systemctl,
            &policy.tools.nixos_container,
            &policy.tools.nix_store,
        ] {
            script(tool, "#!/bin/sh\nexit 0\n");
        }
        let Err(error) = Native::new(&policy) else {
            panic!("Native::new must reject a missing systemd-run");
        };
        assert!(error.contains("systemd_run"), "{error}");
        script(
            &policy.tools.systemctl.with_file_name("systemd-run"),
            "#!/bin/sh\nexit 0\n",
        );
        let Err(error) = Native::new(&policy) else {
            panic!("Native::new must still reject the fixture admission_check");
        };
        assert!(!error.contains("systemd_run"), "{error}");
    }

    #[test]
    fn systemctl_argv_is_exact_and_activation_is_atomic() {
        let root = tempfile::tempdir().unwrap();
        let policy = fixture_policy(root.path());
        let log = root.path().join("systemctl.log");
        let job = root.path().join("job");
        fs::write(&job, "0\n").unwrap();
        systemctl_script(&policy, &log, &job);
        let native = Native {
            policy: policy.clone(),
        };
        native.start_unit(Surface::Dev, "restart").unwrap();
        let recorded = fs::read_to_string(&log).unwrap();
        assert!(recorded.contains("restart container@fleetix-dev.service"));

        let first = root.path().join("first");
        let second = root.path().join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        native
            .install_link(Surface::Dev, first.to_str().unwrap())
            .unwrap();
        assert_eq!(
            fs::read_link(runtime::surface_link(&policy, Surface::Dev)).unwrap(),
            first
        );
        native
            .install_link(Surface::Dev, second.to_str().unwrap())
            .unwrap();
        assert_eq!(
            fs::read_link(runtime::surface_link(&policy, Surface::Dev)).unwrap(),
            second
        );
    }

    #[test]
    fn active_systemd_job_blocks_admission_and_activation() {
        let root = tempfile::tempdir().unwrap();
        let policy = fixture_policy(root.path());
        let log = root.path().join("systemctl.log");
        let job = root.path().join("job");
        fs::write(&job, "42\n").unwrap();
        systemctl_script(&policy, &log, &job);
        let admission_log = root.path().join("admission.log");
        let mut policy = policy;
        policy.admission_check = root.path().join("bin/admit");
        script(
            &policy.admission_check,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
                admission_log.display()
            ),
        );
        let native = Native {
            policy: policy.clone(),
        };
        let candidate = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-guest";
        let error = native
            .admission_inner(&Admission {
                operation: "promote",
                surface: Surface::Prod,
                previous: None,
                candidate,
            })
            .unwrap_err();
        assert!(error.contains("active systemd job"), "{error}");
        assert!(!admission_log.exists());

        let error = native.activate_inner(Surface::Prod, candidate).unwrap_err();
        assert!(error.contains("active systemd job"), "{error}");
        assert!(!runtime::surface_link(&policy, Surface::Prod).exists());
        assert!(!fs::read_to_string(&log).unwrap().contains("restart"));
    }

    #[test]
    fn container_config_requires_one_exact_system_path() {
        let expected = Path::new("/var/lib/fleetix-surfaces/dev/system");
        parse_system_path(
            "SYSTEM_PATH=/var/lib/fleetix-surfaces/dev/system\nPRIVATE_NETWORK=1\n",
            expected,
        )
        .unwrap();
        assert!(parse_system_path("FLAKE=github:caniko/fleetix\n", expected).is_err());
        assert!(
            parse_system_path(
                "SYSTEM_PATH=/var/lib/fleetix-surfaces/dev/system\nSYSTEM_PATH=/var/lib/fleetix-surfaces/dev/system\n",
                expected
            )
            .is_err()
        );
        assert!(parse_system_path("SYSTEM_PATH=/tmp/other\n", expected).is_err());

        if unsafe { libc::geteuid() } != 0 {
            let root = tempfile::tempdir().unwrap();
            let policy = fixture_policy(root.path());
            let config = policy.container_config_dir.join("fleetix-dev.conf");
            fs::write(
                &config,
                format!(
                    "SYSTEM_PATH={}\n",
                    runtime::surface_link(&policy, Surface::Dev).display()
                ),
            )
            .unwrap();
            let native = Native { policy };
            native.check_container_config(Surface::Dev).unwrap();
        }
    }

    #[test]
    fn observe_requires_one_canonical_store_path() {
        let root = tempfile::tempdir().unwrap();
        let policy = fixture_policy(root.path());
        let log = root.path().join("systemctl.log");
        let job = root.path().join("job");
        fs::write(&job, "0\n").unwrap();
        systemctl_script(&policy, &log, &job);
        let observed = root.path().join("observed");
        script(
            &policy.tools.nixos_container,
            &format!("#!/bin/sh\ncat '{}'\n", observed.display()),
        );
        let native = Native { policy };
        fs::write(
            &observed,
            "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-guest\n",
        )
        .unwrap();
        assert_eq!(
            native.observe(Surface::Dev).unwrap(),
            "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-guest"
        );
        fs::write(
            &observed,
            "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-guest\n/nix/store/extra\n",
        )
        .unwrap();
        assert!(native.observe(Surface::Dev).is_err());
    }

    #[test]
    fn closure_query_and_validity_use_exact_paths() {
        let root = tempfile::tempdir().unwrap();
        let policy = fixture_policy(root.path());
        let log = root.path().join("nix-store.log");
        let closure = root.path().join("closure");
        let artifact = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-guest";
        let dependency = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-dependency.drv";
        fs::write(&closure, format!("{artifact}\n{dependency}\n")).unwrap();
        script(
            &policy.tools.nix_store,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n\
                 if [ \"$1\" = --query ]; then cat '{}'; fi\n",
                log.display(),
                closure.display()
            ),
        );
        let native = Native { policy };
        native.check_closure(artifact).unwrap();
        let recorded = fs::read_to_string(&log).unwrap();
        assert!(recorded.contains(&format!("--query --requisites {artifact}")));
        assert!(recorded.contains(&format!("--check-validity {artifact} {dependency}")));

        fs::write(&closure, format!("{artifact}\nnot-a-store-path\n")).unwrap();
        let error = native.check_closure(artifact).unwrap_err();
        assert!(error.contains("store path"), "{error}");
    }

    #[test]
    fn retain_always_reregisters_an_existing_exact_root() {
        let root = tempfile::tempdir().unwrap();
        let policy = fixture_policy(root.path());
        let log = root.path().join("nix-store.log");
        let artifact = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-guest";
        script(
            &policy.tools.nix_store,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n\
                 if [ \"$1\" = --add-root ]; then ln -sfn \"$5\" \"$2\"; fi\n",
                log.display()
            ),
        );
        let native = Native {
            policy: policy.clone(),
        };
        let root_path = runtime::root_path(&policy, artifact).unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(runtime::roots_dir(&policy))
            .unwrap();
        symlink(artifact, &root_path).unwrap();
        native.retain_root(artifact).unwrap();
        let recorded = fs::read_to_string(&log).unwrap();
        assert!(recorded.contains("--add-root"));
        assert_eq!(fs::read_link(&root_path).unwrap(), Path::new(artifact));

        fs::write(log.clone(), "").unwrap();
        native.retain_root(artifact).unwrap();
        assert!(fs::read_to_string(&log).unwrap().contains("--add-root"));

        let wrong = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-other";
        fs::remove_file(&root_path).unwrap();
        symlink(wrong, &root_path).unwrap();
        assert!(native.retain_root(artifact).is_err());
    }

    #[test]
    fn closure_above_generic_capture_limit_is_fully_checked_in_batches() {
        let root = tempfile::tempdir().unwrap();
        let policy = fixture_policy(root.path());
        let log = root.path().join("nix-store.log");
        let closure = root.path().join("closure");
        let artifact = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-guest";
        let hash = "0123456789abcdfghijklmnpqrsvwxyz";
        let mut paths = vec![artifact.to_owned()];
        for index in 0..600 {
            paths.push(format!("/nix/store/{hash}-dependency-{index}"));
        }
        let content = format!("{}\n", paths.join("\n"));
        assert!(
            content.len() > MAX_CAPTURE_BYTES,
            "fixture must exceed the generic diagnostic capture"
        );
        assert!(content.len() < MAX_CLOSURE_BYTES);
        fs::write(&closure, &content).unwrap();
        script(
            &policy.tools.nix_store,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n\
                 if [ \"$1\" = --query ]; then cat '{}'; fi\n",
                log.display(),
                closure.display()
            ),
        );
        let native = Native { policy };
        native.check_closure(artifact).unwrap();

        let recorded = fs::read_to_string(&log).unwrap();
        let checked: std::collections::BTreeSet<String> = recorded
            .lines()
            .filter_map(|line| line.strip_prefix("--check-validity "))
            .flat_map(|rest| rest.split_whitespace().map(str::to_owned))
            .collect();
        assert_eq!(checked.len(), paths.len());
        for path in &paths {
            assert!(checked.contains(path), "missing closure path {path}");
        }
        let batches = recorded
            .lines()
            .filter(|line| line.starts_with("--check-validity "))
            .count();
        assert_eq!(batches, paths.len().div_ceil(VALIDITY_CHUNK));
        assert!(batches > 1, "fixture must span multiple argv batches");

        fs::write(&log, "").unwrap();
        let error = native.check_closure_with_limit(artifact, 1024).unwrap_err();
        assert!(error.contains("exceeded the 1024 byte bound"), "{error}");
        let recorded = fs::read_to_string(&log).unwrap();
        assert!(!recorded.contains("--check-validity"));
    }

    #[test]
    fn leader_exit_with_pipe_holding_descendant_is_bounded() {
        let root = tempfile::tempdir().unwrap();
        let policy = fixture_policy(root.path());
        let pid_file = root.path().join("grandchild.pid");
        let script_path = root.path().join("exit-tree.sh");
        script(
            &script_path,
            &format!(
                "#!/bin/sh\nsleep 300 &\necho $! > '{}'\nexit 0\n",
                pid_file.display()
            ),
        );
        let held = HostLock::acquire(&policy).unwrap();
        let argv = vec![script_path.to_str().unwrap().to_owned()];
        let started = Instant::now();
        run(&argv, None, Duration::from_secs(5), "exit tree probe").unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "leader exit must not wait for detached pipe holders"
        );
        drop(held);

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match HostLock::acquire(&policy) {
                Ok(lock) => {
                    drop(lock);
                    break;
                }
                Err(error) => {
                    assert!(Instant::now() < deadline, "lock never released: {error}");
                    thread::sleep(POLL_INTERVAL);
                }
            }
        }
        if let Ok(pid) = fs::read_to_string(&pid_file) {
            if let Ok(pid) = pid.trim().parse::<i32>() {
                let deadline = Instant::now() + Duration::from_secs(3);
                while unsafe { libc::kill(pid, 0) } == 0 {
                    assert!(
                        Instant::now() < deadline,
                        "grandchild {pid} survived a successful leader exit"
                    );
                    thread::sleep(POLL_INTERVAL);
                }
            }
        }
    }

    #[test]
    fn stop_waits_for_the_systemd_job_to_drain() {
        let root = tempfile::tempdir().unwrap();
        let policy = fixture_policy(root.path());
        let log = root.path().join("systemctl.log");
        let job = root.path().join("job");
        fs::write(&job, "42\n").unwrap();
        script(
            &policy.tools.systemctl,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n\
                 if [ \"$1\" = show ]; then cat '{}'; fi\n\
                 if [ \"$1\" = stop ]; then echo 0 > '{}'; fi\n",
                log.display(),
                job.display(),
                job.display()
            ),
        );
        let native = Native { policy };
        native.stop(Surface::Dev).unwrap();
        assert_eq!(fs::read_to_string(&job).unwrap().trim(), "0");
        assert!(
            fs::read_to_string(&log)
                .unwrap()
                .contains("stop container@fleetix-dev.service")
        );
    }

    #[test]
    fn timeout_kills_the_process_group_and_releases_the_host_lock() {
        let root = tempfile::tempdir().unwrap();
        let policy = fixture_policy(root.path());
        let pid_file = root.path().join("grandchild.pid");
        let script_path = root.path().join("tree.sh");
        script(
            &script_path,
            &format!(
                "#!/bin/sh\nsleep 30 &\necho $! > '{}'\nwait\n",
                pid_file.display()
            ),
        );
        let held = HostLock::acquire(&policy).unwrap();
        let argv = vec![script_path.to_str().unwrap().to_owned()];
        let error = run(&argv, None, Duration::from_millis(300), "tree probe").unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        drop(held);

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match HostLock::acquire(&policy) {
                Ok(lock) => {
                    drop(lock);
                    break;
                }
                Err(error) => {
                    assert!(Instant::now() < deadline, "lock never released: {error}");
                    thread::sleep(POLL_INTERVAL);
                }
            }
        }
        if let Ok(pid) = fs::read_to_string(&pid_file) {
            if let Ok(pid) = pid.trim().parse::<i32>() {
                let deadline = Instant::now() + Duration::from_secs(3);
                while unsafe { libc::kill(pid, 0) } == 0 {
                    assert!(
                        Instant::now() < deadline,
                        "grandchild {pid} survived the process-group timeout"
                    );
                    thread::sleep(POLL_INTERVAL);
                }
            }
        }
    }

    #[test]
    fn setsid_escape_worker() {
        let Some(marker) = std::env::var_os("FLEETIX_ESCAPED_MARKER") else {
            return;
        };
        // SAFETY: setsid has no preconditions and only detaches this worker.
        let session = unsafe { libc::setsid() };
        assert!(
            session >= 0,
            "setsid escape worker: {}",
            io::Error::last_os_error()
        );
        fs::write(marker, std::process::id().to_string()).unwrap();
        thread::sleep(Duration::from_secs(300));
    }

    #[test]
    fn scoped_guest_command_kills_a_setsid_escaped_health_process() {
        let root = tempfile::tempdir().unwrap();
        let policy = fixture_policy(root.path());
        let unit_log = root.path().join("scope.unit");
        let property_log = root.path().join("scope.properties");
        let escaped_pid = root.path().join("escaped.pid");
        let escaped_marker = root.path().join("escaped.session");
        let kill_log = root.path().join("systemctl.kill");
        let systemd_run = policy.tools.systemctl.with_file_name("systemd-run");
        script(
            &systemd_run,
            &format!(
                "#!/bin/sh\n\
                 while [ $# -gt 0 ]; do\n\
                 case \"$1\" in\n\
                 --scope|--collect|--quiet) shift ;;\n\
                 --unit=*) echo \"${{1#--unit=}}\" > '{}'; shift ;;\n\
                 --property=*) echo \"${{1#--property=}}\" >> '{}'; shift ;;\n\
                 --) shift; break ;;\n\
                 *) break ;;\n\
                 esac\n\
                 done\n\
                 exec \"$@\"\n",
                unit_log.display(),
                property_log.display()
            ),
        );
        script(
            &policy.tools.systemctl,
            &format!(
                "#!/bin/sh\n\
                 printf '%s\\n' \"$*\" >> '{}'\n\
                 if [ \"$1\" = kill ]; then\n\
                 pid=$(cat '{}' 2>/dev/null)\n\
                 [ -n \"$pid\" ] && kill -KILL \"$pid\" 2>/dev/null\n\
                 fi\n",
                kill_log.display(),
                escaped_pid.display()
            ),
        );
        // `nixos-container run` reaches the hook through `su -l -c`, which
        // calls setsid(); this worker reproduces that detachment for real.
        script(
            &policy.tools.nixos_container,
            &format!(
                "#!/bin/sh\nFLEETIX_ESCAPED_MARKER='{}' '{}' --exact \
                 service_surfaces::native::tests::setsid_escape_worker &\n\
                 echo $! > '{}'\nwait\n",
                escaped_marker.display(),
                std::env::current_exe().unwrap().display(),
                escaped_pid.display()
            ),
        );
        let argv = vec![
            policy.tools.nixos_container.to_str().unwrap().to_owned(),
            "run".into(),
            "fleetix-dev".into(),
            "--".into(),
            "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-guest/bin/fleetix-surface-health".into(),
        ];
        let native = Native { policy };

        let error = native
            .run_health_scoped(&argv, Duration::from_secs(2), "release health check")
            .unwrap_err();
        assert!(
            error.contains("release health check timed out after"),
            "{error}"
        );
        assert!(
            fs::read_to_string(&escaped_marker).is_ok(),
            "escape worker never completed setsid()"
        );
        let unit = fs::read_to_string(&unit_log).unwrap();
        assert!(unit.starts_with("fleetix-surfaces-health-"), "{unit}");
        assert!(unit.trim_end().ends_with(".scope"), "{unit}");
        // Crash-safe bound: systemd stops the scope even if the CLI is killed.
        assert_eq!(
            fs::read_to_string(&property_log).unwrap(),
            "RuntimeMaxSec=2500ms\nKillSignal=SIGKILL\n"
        );
        let killed = fs::read_to_string(&kill_log).unwrap_or_default();
        assert!(
            killed.contains("kill --kill-whom=all --signal=KILL"),
            "scope kill was not issued: {killed:?}"
        );
        assert!(killed.contains(unit.trim()), "{killed}");

        let escaped: i32 = fs::read_to_string(&escaped_pid)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while unsafe { libc::kill(escaped, 0) } == 0 {
            assert!(
                Instant::now() < deadline,
                "setsid-escaped health process {escaped} survived the scope kill"
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    #[test]
    fn commands_cannot_access_or_unlock_the_inherited_host_lease() {
        let root = tempfile::tempdir().unwrap();
        let policy = fixture_policy(root.path());
        let executable = root.path().join("probe-descriptors");
        script(
            &executable,
            &format!(
                "#!/bin/sh\nfor fd in /proc/self/fd/*; do\n  if [ \"$(readlink \"$fd\")\" = '{}' ]; then echo 'host lease exposed'; exit 17; fi\ndone\n",
                policy.lock_path.display()
            ),
        );
        let held = HostLock::acquire(&policy).unwrap();
        run(
            &[executable.to_str().unwrap().into()],
            None,
            Duration::from_secs(2),
            "guest fd probe",
        )
        .unwrap();
        assert!(
            HostLock::acquire(&policy).is_err(),
            "command must not unlock the caller's lease"
        );
        drop(held);
        // The host-only keeper releases its lease copy once the group kill
        // that ended the command reaches it; under load that takes a moment.
        let deadline = Instant::now() + Duration::from_secs(2);
        while let Err(error) = HostLock::acquire(&policy) {
            assert!(Instant::now() < deadline, "{error}");
            thread::sleep(POLL_INTERVAL);
        }
    }

    #[test]
    fn lease_keeper_crash_worker() {
        let Some(root) = std::env::var_os("FLEETIX_LEASE_CRASH_FIXTURE") else {
            return;
        };
        let root = Path::new(&root);
        let policy = fixture_policy(root);
        fs::write(
            root.join("policy.json"),
            serde_json::to_vec(&policy).unwrap(),
        )
        .unwrap();
        let _held = HostLock::acquire(&policy).unwrap();
        let executable = root.join("wait-for-crash");
        script(
            &executable,
            &format!(
                "#!/bin/sh\ntest -z \"${{FLEETIX_HOST_ONLY_SECRET-}}\" || exit 23\ntest \"$FLEETIX_ALLOWED_CONTEXT\" = authorized || exit 24\ntouch '{}'\nsleep 30\n",
                root.join("ready").display()
            ),
        );
        let _ = run(
            &[executable.to_str().unwrap().into()],
            Some(("FLEETIX_ALLOWED_CONTEXT", "authorized".into())),
            Duration::from_secs(2),
            "crash fixture",
        );
    }

    #[test]
    fn host_only_keeper_bounds_the_lease_after_worker_sigkill() {
        let root = tempfile::tempdir().unwrap();
        let mut worker = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "service_surfaces::native::tests::lease_keeper_crash_worker",
            ])
            .env("FLEETIX_LEASE_CRASH_FIXTURE", root.path())
            .env("FLEETIX_HOST_ONLY_SECRET", "must-not-reach-command")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !root.path().join("ready").exists() {
            assert!(
                worker.try_wait().unwrap().is_none(),
                "fixture worker exited before starting the command"
            );
            assert!(Instant::now() < deadline, "fixture command never started");
            thread::sleep(POLL_INTERVAL);
        }
        let policy: Policy =
            serde_json::from_slice(&fs::read(root.path().join("policy.json")).unwrap()).unwrap();
        worker.kill().unwrap();
        worker.wait().unwrap();
        assert!(
            HostLock::acquire(&policy).is_err(),
            "worker crash must not immediately release a running action's lease"
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match HostLock::acquire(&policy) {
                Ok(lock) => {
                    drop(lock);
                    break;
                }
                Err(error) => {
                    assert!(
                        Instant::now() < deadline,
                        "keeper did not release its bounded lease: {error}"
                    );
                    thread::sleep(POLL_INTERVAL);
                }
            }
        }
    }
}

#[cfg(test)]
mod activation_precheck_tests {
    use super::*;
    use crate::service_surfaces::{Containers, Tools};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    #[test]
    fn activation_refuses_before_link_change_when_container_config_is_missing() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        fs::DirBuilder::new().mode(0o700).create(&state).unwrap();
        let bin = root.path().join("bin");
        fs::DirBuilder::new().mode(0o700).create(&bin).unwrap();
        let systemctl = bin.join("systemctl");
        let log = root.path().join("systemctl.log");
        fs::write(
            &systemctl,
            format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()),
        )
        .unwrap();
        fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o755)).unwrap();
        let policy = Policy {
            version: 1,
            host: "atlas".into(),
            state_dir: state,
            lock_path: root.path().join("switch.lock"),
            containers: Containers {
                dev: "fleetix-dev".into(),
                prod: "fleetix-prod".into(),
            },
            required_services: vec!["database".into()],
            tools: Tools {
                systemctl,
                nixos_container: bin.join("nixos-container"),
                nix_store: bin.join("nix-store"),
            },
            admission_check: PathBuf::from(
                "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-admission/bin/admit",
            ),
            container_config_dir: root.path().join("containers"),
            command_timeout_seconds: 2,
            health_timeout_seconds: 1,
            health_interval_seconds: 1,
        };
        let native = Native {
            policy: policy.clone(),
        };
        let error = native
            .activate_inner(Surface::Dev, "/fixture/candidate")
            .unwrap_err();
        assert!(error.contains("container config"), "{error}");
        assert!(!runtime::surface_link(&policy, Surface::Dev).exists());
        assert!(
            !fs::read_to_string(&log)
                .unwrap_or_default()
                .contains("restart")
        );
    }
}
