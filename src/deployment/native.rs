#![cfg(unix)]

//! Bounded execution of concrete NixOS and Home Manager deployment actions.
//!
//! Commands are explicit absolute argv arrays: no shell and no argument or
//! environment expansion. The environment is inherited from the caller. stdout
//! capture is capped at 64 KiB and stderr is inherited so systemd/journald owns
//! the diagnostic stream. Every child runs in its own process group; the group
//! is SIGKILLed on timeout, capture overflow, or normal exit so descendants can
//! neither outlive the command nor keep the capture pipe open.
//!
//! These helpers trust the policy layer to select absolute executables and
//! arguments. They deliberately contain no generic command abstraction.

use std::fmt;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

pub(super) fn install_signal_handlers() -> Result<(), String> {
    // The standalone worker has no other child owner. Normalize an inherited
    // ignored SIGCHLD before spawning anything, so child IDs cannot auto-reap.
    // SAFETY: setting the default disposition installs no Rust signal callback.
    if unsafe { libc::signal(libc::SIGCHLD, libc::SIG_DFL) } == libc::SIG_ERR {
        return Err(std::io::Error::last_os_error().to_string());
    }
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: the callback performs only a lock-free atomic store. Actual
        // cancellation and reaping happen outside the signal handler.
        unsafe {
            signal_hook::low_level::register(signal, || INTERRUPTED.store(true, Ordering::Relaxed))
        }
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(super) fn clear_interrupt() {
    INTERRUPTED.store(false, Ordering::Relaxed);
}

const OUTPUT_LIMIT: usize = 64 * 1024;
const KILL_GRACE: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const NIX_BASE32: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// Bounded command failure plus whether its child is known to have settled.
///
/// `settled` is false only when the process group could not be proven dead
/// after SIGKILL, so the caller must not start a mutating action that could
/// overlap a still-running child. Input validation and reaped-child failures
/// default to `true`.
#[derive(Debug)]
pub(super) struct Failure {
    message: String,
    pub settled: bool,
}

impl Failure {
    /// Prefix the message without losing whether the child settled.
    pub(super) fn context(self, prefix: impl fmt::Display) -> Self {
        Self {
            message: format!("{prefix}: {}", self.message),
            settled: self.settled,
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            message,
            settled: true,
        }
    }
}

impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Self::from(message.to_owned())
    }
}

/// Run one command with prewritten stdin and a bounded capture.
///
/// `stdout` is inherited when `capture` is false. On timeout or output overflow
/// the whole process group is killed before the bounded reader is joined.
pub(super) fn command(
    argv: &[String],
    input: &[u8],
    timeout: Duration,
    capture: bool,
) -> Result<Vec<u8>, Failure> {
    if INTERRUPTED.load(Ordering::Relaxed) {
        return Err("deployment interrupted".into());
    }
    // Library embedders must not auto-reap children either. Refuse before spawn
    // rather than signalling a PID whose ownership could have disappeared.
    // SAFETY: zero-initialized sigaction storage is filled by the query below.
    let mut disposition: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: sigaction writes into valid storage without changing the handler.
    if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut disposition) } < 0 {
        return Err(std::io::Error::last_os_error().to_string().into());
    }
    if disposition.sa_sigaction == libc::SIG_IGN || disposition.sa_flags & libc::SA_NOCLDWAIT != 0 {
        return Err("deployment commands require tracked, non-auto-reaped children".into());
    }
    let program = argv.first().ok_or("command requires an executable")?;
    if !Path::new(program).is_absolute() {
        return Err(format!("command executable must be absolute: {program:?}").into());
    }

    // A prewritten and rewound file cannot deadlock the child on a full pipe.
    let mut stdin =
        tempfile::tempfile().map_err(|error| format!("create command input: {error}"))?;
    stdin
        .write_all(input)
        .map_err(|error| format!("write command input: {error}"))?;
    stdin
        .seek(SeekFrom::Start(0))
        .map_err(|error| format!("rewind command input: {error}"))?;

    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or("command timeout is too large")?;
    let mut child = Command::new(program)
        .args(&argv[1..])
        .stdin(Stdio::from(stdin))
        .stdout(if capture {
            Stdio::piped()
        } else {
            Stdio::inherit()
        })
        .stderr(Stdio::inherit())
        .process_group(0)
        .spawn()
        .map_err(|error| format!("spawn {program}: {error}"))?;
    let group = child.id();

    let mut stdout = child.stdout.take();
    let mut output = Vec::new();
    let mut owns_group = true;
    let result: Result<(), Failure> = (|| {
        if let Some(stdout) = &stdout {
            // SAFETY: stdout owns a valid descriptor. F_SETFL changes only this
            // pipe's status flags; nonblocking reads cannot strand a thread.
            let flags = unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_GETFL) };
            if flags < 0
                || unsafe {
                    libc::fcntl(stdout.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK)
                } < 0
            {
                return Err(std::io::Error::last_os_error().to_string().into());
            }
        }
        loop {
            if INTERRUPTED.load(Ordering::Relaxed) {
                return Err("deployment interrupted".into());
            }
            drain_stdout(&mut stdout, &mut output)?;
            match exited_without_reaping(group) {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(error) => {
                    owns_group = false;
                    return Err(error.into());
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(format!("command {program} timed out after {timeout:?}").into());
            }
            std::thread::sleep(POLL_INTERVAL.min(deadline - now));
        }
    })();
    // The unreaped leader reserves its PID/PGID until after this signal. A
    // try_wait followed by kill(-pid) could signal a reused, unrelated group.
    // Waiting for the reap here also proves the child settled before `result`
    // is reported, so a timeout failure is only safe to roll back from when
    // this wait succeeded.
    if owns_group {
        kill_group(group);
    }
    let status = wait_bounded(&mut child, Instant::now() + KILL_GRACE)?;
    result?;
    drain_stdout(&mut stdout, &mut output)?;
    if !status.success() {
        return Err(format!("command {program} failed: {status}").into());
    }
    Ok(output)
}

fn drain_stdout(stdout: &mut Option<ChildStdout>, output: &mut Vec<u8>) -> Result<(), String> {
    if let Some(stdout) = stdout {
        let mut buffer = [0; 4096];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if output.len() + count > OUTPUT_LIMIT {
                        return Err(format!("command output exceeded {OUTPUT_LIMIT} bytes"));
                    }
                    output.extend_from_slice(&buffer[..count]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(format!("read command output: {error}")),
            }
        }
    }
    Ok(())
}

fn exited_without_reaping(pid: u32) -> Result<bool, String> {
    // SAFETY: waitid writes into initialized siginfo storage. WNOWAIT observes
    // only our direct child and leaves its PID reserved until Child reaps it.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(error.to_string());
    }
    // SAFETY: waitid initialized the SIGCHLD fields (or zero for WNOHANG).
    Ok(unsafe { info.si_pid() } != 0)
}

fn wait_bounded(child: &mut Child, deadline: Instant) -> Result<ExitStatus, Failure> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => {
                // The child may still be running: reaping failed.
                return Err(Failure {
                    message: format!("wait for command child: {error}"),
                    settled: false,
                });
            }
        }
        if Instant::now() >= deadline {
            // SIGKILL was sent but the child could not be reaped in time.
            return Err(Failure {
                message: "command child did not exit after SIGKILL".into(),
                settled: false,
            });
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn kill_group(pgid: u32) {
    let Ok(pid) = i32::try_from(pgid) else {
        return;
    };
    // SAFETY: `process_group(0)` made the child the leader of a fresh group.
    // A negative pid selects that group; SIGKILL reaches only this command's
    // descendants. ESRCH (the group is already gone) is an expected result.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
}

/// Canonicalize the live system and profile links. Both must resolve directly
/// to `/nix/store/<32-character-hash>-<name>`; returns
/// `(current_system, profile_target)`.
pub(super) fn observe(
    current_path: &Path,
    profile_path: &Path,
) -> Result<(String, String), Failure> {
    Ok((store_path(current_path)?, store_path(profile_path)?))
}

fn store_path(path: &Path) -> Result<String, String> {
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| format!("resolve {}: {error}", path.display()))?;
    store_identity(&canonical)?;
    if !canonical.is_dir() {
        return Err(format!(
            "store path is not a directory: {}",
            canonical.display()
        ));
    }
    canonical
        .to_str()
        .map(|text| text.to_owned())
        .ok_or_else(|| format!("store path is not UTF-8: {}", canonical.display()))
}

fn store_identity(path: &Path) -> Result<(), String> {
    if path.parent() != Some(Path::new("/nix/store")) {
        return Err(format!("not a direct /nix/store path: {}", path.display()));
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("store path has no UTF-8 name: {}", path.display()))?;
    let (hash, rest) = name
        .split_once('-')
        .ok_or_else(|| format!("store path has no hash: {name}"))?;
    if hash.len() != 32 || !hash.bytes().all(|byte| NIX_BASE32.contains(&byte)) {
        return Err(format!("invalid store hash in {name}"));
    }
    if rest.is_empty()
        || rest == "."
        || rest == ".."
        || !rest.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.' | b'_' | b'?' | b'=')
        })
    {
        return Err(format!("invalid store name in {name}"));
    }
    Ok(())
}

/// Pin already-existing paths as indirect GC roots below `root_dir`, using each
/// path's final component as the root name. Profile links are resolved to their
/// store target before realisation. Existing roots are replaced only for the
/// same name; roots are never removed.
pub(super) fn retain(
    nix_store: &str,
    root_dir: &Path,
    paths: &[&str],
    timeout: Duration,
) -> Result<(), Failure> {
    super::storage::absolute(Path::new(nix_store))?;
    super::storage::absolute(root_dir)?;
    if paths.is_empty() {
        return Ok(());
    }
    let mut names = std::collections::BTreeSet::new();
    let mut pins = Vec::new();
    for path in paths {
        let target = Path::new(*path);
        super::storage::absolute(target)?;
        let resolved = std::fs::canonicalize(target).map_err(|error| {
            format!(
                "retain requires an existing path {}: {error}",
                target.display()
            )
        })?;
        let name = target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("retain path has no UTF-8 name: {}", target.display()))?;
        if !names.insert(name) {
            return Err(format!("duplicate GC root name {name} in one retention").into());
        }
        pins.push((target, name, resolved));
    }
    std::fs::create_dir_all(root_dir)
        .map_err(|error| format!("create GC root directory {}: {error}", root_dir.display()))?;
    for (target, name, resolved) in pins {
        let root = root_dir.join(name);
        let argv = vec![
            nix_store.to_owned(),
            "--add-root".to_owned(),
            utf8(&root)?.to_owned(),
            "--indirect".to_owned(),
            "--realise".to_owned(),
            utf8(&resolved)?.to_owned(),
        ];
        command(&argv, b"", timeout, false)
            .map_err(|error| error.context(format!("retain {}", target.display())))?;
        let actual = std::fs::canonicalize(&root)
            .map_err(|error| format!("retained root {} is missing: {error}", root.display()))?;
        if actual != resolved {
            return Err(format!(
                "retained root {} points to {}, expected {}",
                root.display(),
                actual.display(),
                resolved.display()
            )
            .into());
        }
    }
    Ok(())
}

/// Activate `candidate` non-persistently: NixOS runs
/// `switch-to-configuration test`, Home Manager runs `activate`.
pub(super) fn trial(is_nixos: bool, candidate: &str, timeout: Duration) -> Result<(), Failure> {
    let candidate = Path::new(candidate);
    super::storage::absolute(candidate)?;
    let (program, argument) = if is_nixos {
        (candidate.join("bin/switch-to-configuration"), Some("test"))
    } else {
        (candidate.join("activate"), None)
    };
    let mut argv = vec![utf8(&program)?.to_owned()];
    if let Some(argument) = argument {
        argv.push(argument.to_owned());
    }
    command(&argv, b"", timeout, false)
        .map(|_| ())
        .map_err(|error| error.context(format!("trial {}", candidate.display())))
}

/// NixOS: set the system profile to `candidate`, then run the candidate's boot
/// activation. Home Manager: no-op, because activation already wrote the user
/// profile.
pub(super) fn commit(
    is_nixos: bool,
    nix_env: &str,
    profile_path: &Path,
    candidate: &str,
    timeout: Duration,
) -> Result<(), Failure> {
    if !is_nixos {
        return Ok(());
    }
    super::storage::absolute(Path::new(nix_env))?;
    super::storage::absolute(profile_path)?;
    super::storage::absolute(Path::new(candidate))?;
    let argv = vec![
        nix_env.to_owned(),
        "--profile".to_owned(),
        utf8(profile_path)?.to_owned(),
        "--set".to_owned(),
        candidate.to_owned(),
    ];
    command(&argv, b"", timeout, false).map_err(|error| error.context("set deployment profile"))?;
    let program = Path::new(candidate).join("bin/switch-to-configuration");
    command(
        &[utf8(&program)?.to_owned(), "boot".to_owned()],
        b"",
        timeout,
        false,
    )
    .map(|_| ())
    .map_err(|error| error.context(format!("boot candidate {candidate}")))
}

/// NixOS: test the previous system, set the profile to `previous_profile`, then
/// boot that profile. Home Manager: activate `previous_profile`. The caller
/// verifies live identity before and after this call.
pub(super) fn restore(
    is_nixos: bool,
    nix_env: &str,
    profile_path: &Path,
    previous_system: &str,
    previous_profile: &str,
    timeout: Duration,
) -> Result<(), Failure> {
    super::storage::absolute(profile_path)?;
    super::storage::absolute(Path::new(previous_profile))?;
    if !is_nixos {
        let program = Path::new(previous_profile).join("activate");
        return command(&[utf8(&program)?.to_owned()], b"", timeout, false)
            .map(|_| ())
            .map_err(|error| {
                error.context(format!("activate previous profile {previous_profile}"))
            });
    }
    super::storage::absolute(Path::new(nix_env))?;
    super::storage::absolute(Path::new(previous_system))?;
    let test = Path::new(previous_system).join("bin/switch-to-configuration");
    command(
        &[utf8(&test)?.to_owned(), "test".to_owned()],
        b"",
        timeout,
        false,
    )
    .map_err(|error| error.context(format!("test previous system {previous_system}")))?;
    let argv = vec![
        nix_env.to_owned(),
        "--profile".to_owned(),
        utf8(profile_path)?.to_owned(),
        "--set".to_owned(),
        previous_profile.to_owned(),
    ];
    command(&argv, b"", timeout, false)
        .map_err(|error| error.context("restore previous profile"))?;
    let boot = Path::new(previous_profile).join("bin/switch-to-configuration");
    command(
        &[utf8(&boot)?.to_owned(), "boot".to_owned()],
        b"",
        timeout,
        false,
    )
    .map(|_| ())
    .map_err(|error| error.context(format!("boot previous profile {previous_profile}")))
}

fn utf8(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("path is not UTF-8: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(directory: &Path, name: &str, body: &str) -> String {
        let path = directory.join(name);
        // Publish a closed, fsynced executable rather than racing another
        // parallel fixture's fork while the script is still open for writing.
        crate::fsutil::atomic_write(&path, format!("#!/bin/sh\nset -eu\n{body}\n").as_bytes())
            .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_str().unwrap().to_owned()
    }

    fn contents(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn command_requires_an_absolute_argv_zero() {
        assert!(
            command(&[], b"", Duration::from_secs(1), true)
                .unwrap_err()
                .to_string()
                .contains("executable")
        );
        assert!(
            command(&["sh".to_owned()], b"", Duration::from_secs(1), true)
                .unwrap_err()
                .to_string()
                .contains("absolute")
        );
    }

    #[test]
    fn command_passes_arguments_and_stdin_without_shell_expansion() {
        let temp = tempfile::tempdir().unwrap();
        let program = script(
            temp.path(),
            "echo.sh",
            "printf 'args:'; printf '%s,' \"$@\"; cat",
        );
        let output = command(
            &[
                program,
                "one".to_owned(),
                "$HOME".to_owned(),
                "two words".to_owned(),
            ],
            b"stdin-data",
            Duration::from_secs(5),
            true,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "args:one,$HOME,two words,stdin-data"
        );
    }

    #[test]
    fn uncaptured_commands_inherit_stdout_and_return_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("marker");
        let program = script(
            temp.path(),
            "write.sh",
            &format!("printf ran > {}", marker.display()),
        );
        let output = command(&[program], b"", Duration::from_secs(5), false).unwrap();
        assert!(output.is_empty());
        assert_eq!(contents(&marker), "ran");
    }

    #[test]
    fn command_timeout_kills_sleeping_descendants_holding_stdout() {
        let temp = tempfile::tempdir().unwrap();
        let program = script(temp.path(), "hang.sh", "sleep 30 & echo started; wait");
        let started = Instant::now();
        let error = command(&[program], b"", Duration::from_millis(100), true).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("timed out"), "{message}");
        // The timed-out child was SIGKILLed and reaped, so a caller may roll back.
        assert!(error.settled, "{message}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn command_kills_descendants_that_outlive_a_successful_child() {
        let temp = tempfile::tempdir().unwrap();
        let program = script(temp.path(), "orphan.sh", "sleep 30 & echo done");
        let started = Instant::now();
        let output = command(&[program], b"", Duration::from_secs(5), true).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "done\n");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn command_refuses_stdout_above_the_capture_limit() {
        let temp = tempfile::tempdir().unwrap();
        let program = script(temp.path(), "flood.sh", "head -c 65537 /dev/zero");
        let started = Instant::now();
        let error = command(&[program], b"", Duration::from_secs(5), true).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("exceeded"), "{message}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn failure_context_preserves_whether_the_child_settled() {
        let settled = Failure::from("command exited");
        assert!(settled.settled);
        assert_eq!(settled.to_string(), "command exited");

        let unsettled = Failure {
            message: "command child did not exit after SIGKILL".into(),
            settled: false,
        };
        let contextual = unsettled.context("boot candidate /nix/store/x");
        assert!(!contextual.settled);
        assert_eq!(
            contextual.to_string(),
            "boot candidate /nix/store/x: command child did not exit after SIGKILL"
        );
        let _: &dyn std::error::Error = &contextual;
        assert!(Failure::from(String::from("owned")).settled);
    }

    #[test]
    fn store_identity_requires_a_direct_hash_named_store_path() {
        assert!(
            store_identity(Path::new(
                "/nix/store/00000000000000000000000000000000-candidate"
            ))
            .is_ok()
        );
        for bad in [
            "/nix/store/00000000000000000000000000000000",
            "/nix/store/0000000000000000000000000000000-short",
            "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-candidate",
            "/nix/store/00000000000000000000000000000000-",
            "/nix/store/00000000000000000000000000000000-..",
            "/nix/store/00000000000000000000000000000000-name/sub",
            "/nix/store/../etc/passwd",
            "nix/store/00000000000000000000000000000000-candidate",
        ] {
            assert!(store_identity(Path::new(bad)).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn observe_rejects_targets_outside_the_store() {
        let temp = tempfile::tempdir().unwrap();
        let error = observe(temp.path(), temp.path()).unwrap_err();
        assert!(error.to_string().contains("/nix/store"), "{error}");
    }

    #[test]
    fn retain_pins_each_existing_path_under_its_own_root() {
        let temp = tempfile::tempdir().unwrap();
        let roots = temp.path().join("roots");
        let candidate = temp.path().join("candidate");
        let previous = temp.path().join("previous");
        std::fs::create_dir_all(&candidate).unwrap();
        std::fs::create_dir_all(&previous).unwrap();
        let nix_store = script(
            temp.path(),
            "nix-store",
            "test \"$1\" = --add-root\n\
             test \"$3\" = --indirect\n\
             test \"$4\" = --realise\n\
             ln -sfn \"$5\" \"$2\"",
        );
        retain(
            &nix_store,
            &roots,
            &[candidate.to_str().unwrap(), previous.to_str().unwrap()],
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            std::fs::canonicalize(roots.join("candidate")).unwrap(),
            std::fs::canonicalize(&candidate).unwrap()
        );
        assert_eq!(
            std::fs::canonicalize(roots.join("previous")).unwrap(),
            std::fs::canonicalize(&previous).unwrap()
        );
        let missing = temp.path().join("missing");
        let error = retain(
            &nix_store,
            &roots,
            &[missing.to_str().unwrap()],
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(error.to_string().contains("existing"), "{error}");
    }

    #[test]
    fn trial_uses_the_backend_specific_entrypoint() {
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("log");
        std::fs::write(&log, "").unwrap();
        let nixos = temp.path().join("nixos");
        std::fs::create_dir_all(nixos.join("bin")).unwrap();
        script(
            &nixos.join("bin"),
            "switch-to-configuration",
            &format!("printf 'nixos:%s\\n' \"$1\" >> {}", log.display()),
        );
        trial(true, nixos.to_str().unwrap(), Duration::from_secs(5)).unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        script(
            &home,
            "activate",
            &format!("printf 'home\\n' >> {}", log.display()),
        );
        trial(false, home.to_str().unwrap(), Duration::from_secs(5)).unwrap();
        assert_eq!(contents(&log), "nixos:test\nhome\n");
    }

    #[test]
    fn commit_sets_the_nixos_profile_before_booting_and_home_manager_is_a_noop() {
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("log");
        std::fs::write(&log, "").unwrap();
        let profile = temp.path().join("profiles/system");
        let candidate = temp.path().join("candidate");
        std::fs::create_dir_all(candidate.join("bin")).unwrap();
        script(
            &candidate.join("bin"),
            "switch-to-configuration",
            &format!("printf 'candidate:%s\\n' \"$1\" >> {}", log.display()),
        );
        let nix_env = script(
            temp.path(),
            "nix-env",
            &format!("printf 'set:%s\\n' \"$*\" >> {}", log.display()),
        );
        let candidate = candidate.to_str().unwrap();
        commit(true, &nix_env, &profile, candidate, Duration::from_secs(5)).unwrap();
        assert_eq!(
            contents(&log),
            format!(
                "set:--profile {} --set {}\ncandidate:boot\n",
                profile.display(),
                candidate
            )
        );

        std::fs::write(&log, "").unwrap();
        let failing = script(temp.path(), "nix-env-failing", "exit 7");
        assert!(commit(true, &failing, &profile, candidate, Duration::from_secs(5)).is_err());
        assert_eq!(contents(&log), "");

        assert!(
            commit(
                false,
                "/does/not/exist/nix-env",
                &profile,
                "/does/not/exist/candidate",
                Duration::from_secs(5)
            )
            .is_ok()
        );
        assert_eq!(contents(&log), "");
    }

    #[test]
    fn restore_tests_the_previous_system_before_switching_and_home_manager_activates() {
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("log");
        std::fs::write(&log, "").unwrap();
        let profile = temp.path().join("profiles/system");
        let previous_system = temp.path().join("previous-system");
        std::fs::create_dir_all(previous_system.join("bin")).unwrap();
        script(
            &previous_system.join("bin"),
            "switch-to-configuration",
            &format!("printf 'system:%s\\n' \"$1\" >> {}", log.display()),
        );
        let previous_profile = temp.path().join("previous-profile");
        std::fs::create_dir_all(previous_profile.join("bin")).unwrap();
        script(
            &previous_profile.join("bin"),
            "switch-to-configuration",
            &format!("printf 'profile:%s\\n' \"$1\" >> {}", log.display()),
        );
        script(
            &previous_profile,
            "activate",
            &format!("printf 'home\\n' >> {}", log.display()),
        );
        let nix_env = script(
            temp.path(),
            "nix-env",
            &format!("printf 'set:%s\\n' \"$*\" >> {}", log.display()),
        );
        let previous_system = previous_system.to_str().unwrap();
        let previous_profile = previous_profile.to_str().unwrap();
        restore(
            true,
            &nix_env,
            &profile,
            previous_system,
            previous_profile,
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            contents(&log),
            format!(
                "system:test\nset:--profile {} --set {}\nprofile:boot\n",
                profile.display(),
                previous_profile
            )
        );

        std::fs::write(&log, "").unwrap();
        restore(
            false,
            "/does/not/exist/nix-env",
            &profile,
            "/does/not/exist/system",
            previous_profile,
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(contents(&log), "home\n");

        let failing = temp.path().join("failing-system");
        std::fs::create_dir_all(failing.join("bin")).unwrap();
        script(&failing.join("bin"), "switch-to-configuration", "exit 9");
        std::fs::write(&log, "").unwrap();
        assert!(
            restore(
                true,
                &nix_env,
                &profile,
                failing.to_str().unwrap(),
                previous_profile,
                Duration::from_secs(5)
            )
            .is_err()
        );
        assert_eq!(contents(&log), "");
    }
}
