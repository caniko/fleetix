//! Attended-transaction spool ("ops"): the file contract between a tool that
//! queues work the operator must attend and the surface that approves and runs
//! it. A step runs as the operator unless it is marked `root`; only a
//! transaction with a root step needs privilege.
//!
//! A request is one JSON file, `<spool>/pending/<id>.json`. The approving tool
//! moves it to `running/` before anything executes and writes the outcome to
//! `done/`. Nothing here runs a command, grants privilege or waits: approval,
//! sudo and host lookup belong to the caller.

use std::path::{Path, PathBuf};

use miette::{IntoDiagnostic, Result, WrapErr, miette};
use serde::{Deserialize, Serialize};
use sha2::Digest;

/// A transaction holds at most this many steps.
pub const MAX_STEPS: usize = 32;

/// A step is argv, not shell: a bare operator reaches the command as an
/// argument and turns a working step into a reported failure.
pub const SHELL_OPERATORS: &[&str] = &[
    "&&", "||", ";", "|", "|&", "&", ">", ">>", "<", "2>", "2>&1",
];

/// Queued, waiting for operator approval.
pub const PENDING: &str = "pending";
/// Approved and handed to the approver; never replayed automatically.
pub const RUNNING: &str = "running";
/// Transition state for an in-progress drop; safe to complete after a crash.
pub const DROPPING: &str = "dropping";
/// Outcome records, with the output requesters read back.
pub const DONE: &str = "done";
/// Watcher locks of requesters waiting for an outcome.
pub const WATCH: &str = "watch";

/// One queued transaction, exactly as it is stored on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Op {
    pub id: String,
    pub host: String,
    pub title: String,
    pub reason: String,
    pub requested_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// RFC 3339 time the transaction was requested.
    pub created: String,
    pub steps: Vec<Step>,
}

/// One command of a transaction, run in order and stopping at the first failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// The command as argv, never shell: see [`SHELL_OPERATORS`].
    pub argv: Vec<String>,
    /// What the step does, one plain sentence the operator reads before approving.
    pub note: String,
    /// Run as root; otherwise the step runs as the approving operator.
    pub root: bool,
}

impl Op {
    /// Whether approving this transaction needs privilege at all.
    pub fn needs_root(&self) -> bool {
        self.steps.iter().any(|step| step.root)
    }
}

/// The harness thread and session that queued a transaction, read from the
/// requester's environment so the operator can find the thread to resume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Source {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t3_thread: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_session: Option<String>,
}

impl Source {
    /// The source this process runs under, if any harness identifies it.
    pub fn from_env() -> Option<Self> {
        // The source only helps the operator find the thread, so an id the
        // spool would refuse (T3's `mcp:launch:…` threads) is left out rather
        // than failing the request.
        let var = |name| std::env::var(name).ok().filter(|value| valid_id(value));
        let source = Self {
            t3_thread: var("T3CODE_THREAD_ID"),
            claude_session: var("CLAUDE_CODE_SESSION_ID"),
        };
        (source.t3_thread.is_some() || source.claude_session.is_some()).then_some(source)
    }

    /// One line naming the thread and session, for the approval summary.
    pub fn describe(&self) -> String {
        [
            ("T3 thread", &self.t3_thread),
            ("Claude session", &self.claude_session),
        ]
        .into_iter()
        .filter_map(|(label, id)| id.as_ref().map(|id| format!("{label} {id}")))
        .collect::<Vec<_>>()
        .join(", ")
    }
}

/// A transaction to queue: already-parsed argv, no shell words and no host
/// lookup, which belong to the caller's own command surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub host: String,
    pub title: String,
    pub reason: String,
    pub steps: Vec<Step>,
    pub requested_by: String,
}

/// The spool directory canix approvals use: `$XDG_STATE_HOME/canix/ops` when
/// that variable is absolute, else `$HOME/.local/state/canix/ops`.
pub fn default_spool_dir() -> PathBuf {
    state_dir(
        std::env::var("XDG_STATE_HOME").ok(),
        std::env::var("HOME").ok(),
    )
}

fn state_dir(xdg_state_home: Option<String>, home: Option<String>) -> PathBuf {
    if let Some(dir) = xdg_state_home {
        let path = PathBuf::from(dir);
        if path.is_absolute() {
            return path.join("canix").join("ops");
        }
    }
    PathBuf::from(home.unwrap_or_default()).join(".local/state/canix/ops")
}

/// `channel:user`, the requester identity stored with a transaction.
pub fn requester(channel: &str) -> String {
    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
    format!("{channel}:{user}")
}

/// Validate and persist a request. Runs on queue and again on [`load`]: the
/// spool is writable by every process of this user, so a hand-edited file must
/// not be able to spoof the summary the operator approves.
pub fn queue(spool: &Path, request: Request) -> Result<Op> {
    let created = chrono::Utc::now();
    let mut op = Op {
        id: String::new(),
        host: request.host,
        title: request.title.trim().to_string(),
        reason: request.reason.trim().to_string(),
        requested_by: request.requested_by,
        source: Source::from_env(),
        created: created.to_rfc3339(),
        steps: request.steps,
    };
    let digest = sha2::Sha256::digest(
        serde_json::to_vec(&op)
            .into_diagnostic()
            .wrap_err("serialize the transaction")?,
    );
    op.id = format!(
        "{}-{}",
        created.format("%Y%m%dT%H%M%S"),
        &format!("{digest:x}")[..6]
    );
    validate(&op)?;
    let dir = spool.join(PENDING);
    std::fs::create_dir_all(&dir)
        .into_diagnostic()
        .wrap_err_with(|| format!("create {}", dir.display()))?;
    let mut file = tempfile::NamedTempFile::new_in(&dir)
        .into_diagnostic()
        .wrap_err_with(|| format!("create a temporary file in {}", dir.display()))?;
    serde_json::to_writer_pretty(&mut file, &op)
        .into_diagnostic()
        .wrap_err("write the transaction")?;
    file.as_file()
        .sync_all()
        .into_diagnostic()
        .wrap_err("sync the transaction")?;
    file.persist_noclobber(dir.join(format!("{}.json", op.id)))
        .into_diagnostic()
        .wrap_err("persist the transaction")?;
    Ok(op)
}

/// Whether a transaction passes every on-disk contract check.
pub fn validate(op: &Op) -> Result<()> {
    // Printable ASCII only: control, bidi and zero-width characters could make
    // the terminal show something other than what runs.
    let printable = |field: &str, value: &str| -> Result<()> {
        if value.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
            Ok(())
        } else {
            Err(miette!("{field} must be printable ASCII"))
        }
    };
    if !valid_id(&op.id) {
        return Err(miette!("invalid transaction id {:?}", op.id));
    }
    if op.host.is_empty()
        || !op
            .host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(miette!("invalid host {:?}", op.host));
    }
    if op.title.is_empty() || op.title.chars().count() > 200 {
        return Err(miette!("title must be 1-200 characters"));
    }
    printable("title", &op.title)?;
    printable("reason", &op.reason)?;
    printable("requested_by", &op.requested_by)?;
    if let Some(source) = &op.source {
        for id in [&source.t3_thread, &source.claude_session]
            .into_iter()
            .flatten()
        {
            if !valid_id(id) {
                return Err(miette!("invalid source id {id:?}"));
            }
        }
    }
    chrono::DateTime::parse_from_rfc3339(&op.created)
        .into_diagnostic()
        .wrap_err("invalid created timestamp")?;
    if op.steps.is_empty() || op.steps.len() > MAX_STEPS {
        return Err(miette!("a transaction needs 1-{MAX_STEPS} steps"));
    }
    for (index, Step { argv, note, .. }) in op.steps.iter().enumerate() {
        if note.trim().is_empty() {
            return Err(miette!("step {}: every step needs a note", index + 1));
        }
        printable("note", note)?;
        if argv.is_empty() || argv[0].is_empty() {
            return Err(miette!("empty step"));
        }
        for arg in argv {
            printable("step", arg)?;
        }
        if let Some(word) = argv
            .iter()
            .find(|word| SHELL_OPERATORS.contains(&word.as_str()))
        {
            return Err(miette!(
                "step {}: {word:?} is not shell syntax here; the command receives it as an argument. Split it into steps or wrap the step in `sh -c '...'`",
                index + 1
            ));
        }
    }
    Ok(())
}

/// A transaction id is a file name component: alphanumeric and `-`, at most 64.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The spool file of a transaction in `state` (`pending`, `running`, `done`).
pub fn spool_path(spool: &Path, state: &str, id: &str) -> Result<PathBuf> {
    if !valid_id(id) {
        return Err(miette!("invalid transaction id {id:?}"));
    }
    Ok(spool.join(state).join(format!("{id}.json")))
}

/// A validated spool entry whose file name matches its id.
pub fn load(path: &Path) -> Result<Op> {
    let bytes = std::fs::read(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", path.display()))?;
    let op: Op = serde_json::from_slice(&bytes)
        .into_diagnostic()
        .wrap_err_with(|| format!("parse {}", path.display()))?;
    validate(&op)?;
    if path.file_stem().and_then(|stem| stem.to_str()) != Some(op.id.as_str()) {
        return Err(miette!("file name does not match id {}", op.id));
    }
    Ok(op)
}

/// Debug formatting escapes control characters that a hostile file name or
/// spool content would otherwise send to the operator's terminal.
#[expect(
    clippy::unnecessary_debug_formatting,
    reason = "preserve control-character escaping for untrusted spool paths"
)]
fn ignored(path: &Path, error: &miette::Report) -> String {
    let chain: Vec<String> = error.chain().map(ToString::to_string).collect();
    format!("ops: ignoring {:?}: {:?}", path, chain.join(": "))
}

/// Pending transactions, oldest first. Invalid files are reported and never
/// run: they stay on disk for the operator to inspect.
pub fn pending(spool: &Path, host: Option<&str>) -> Result<Vec<Op>> {
    list_phase(spool, PENDING, host)
}

/// Running transactions, oldest first. Invalid files are reported and skipped.
pub fn running(spool: &Path, host: Option<&str>) -> Result<Vec<Op>> {
    list_phase(spool, RUNNING, host)
}

fn list_phase(spool: &Path, phase: &str, host: Option<&str>) -> Result<Vec<Op>> {
    let dir = spool.join(phase);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error)
                .into_diagnostic()
                .wrap_err_with(|| format!("read {}", dir.display()));
        }
    };
    let mut ops = Vec::new();
    for entry in entries {
        let path = entry
            .into_diagnostic()
            .wrap_err_with(|| format!("read {}", dir.display()))?
            .path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        match load(&path) {
            Ok(op) if host.is_none_or(|host| host == op.host) => ops.push(op),
            Ok(_) => {}
            // Debug formatting escapes control characters from spool content.
            Err(error) => eprintln!("{}", ignored(&path, &error)),
        }
    }
    ops.sort_by(|a, b| (&a.created, &a.id).cmp(&(&b.created, &b.id)));
    Ok(ops)
}

/// Whether `spool/<state>/<id>.json` currently exists.
pub fn phase_exists(spool: &Path, state: &str, id: &str) -> Result<bool> {
    spool_path(spool, state, id)?
        .try_exists()
        .into_diagnostic()
        .wrap_err_with(|| format!("check {state}/{id}"))
}

/// Move a non-terminal transaction back from `running/` to `pending/`.
///
/// Refuses to requeue if a `done/` record already exists so settled work is
/// never replayed. Returns `Ok(false)` when the `running/` file is already
/// absent (idempotent).
pub fn requeue(spool: &Path, id: &str) -> Result<bool> {
    if phase_exists(spool, DONE, id)? {
        return Err(miette!("cannot requeue {id}: done record already exists"));
    }
    let from = spool_path(spool, RUNNING, id)?;
    if !from
        .try_exists()
        .into_diagnostic()
        .wrap_err_with(|| format!("check {}", from.display()))?
    {
        return Ok(false);
    }
    // Validate the payload before placing it back into pending/.
    let _ = load(&from)?;
    let pending_dir = spool.join(PENDING);
    std::fs::create_dir_all(&pending_dir)
        .into_diagnostic()
        .wrap_err_with(|| format!("create {}", pending_dir.display()))?;
    let to = spool_path(spool, PENDING, id)?;
    if to
        .try_exists()
        .into_diagnostic()
        .wrap_err_with(|| format!("check {}", to.display()))?
    {
        return Err(miette!(
            "cannot requeue {id}: pending record already exists"
        ));
    }
    std::fs::rename(&from, &to)
        .into_diagnostic()
        .wrap_err_with(|| format!("move {} to {}", from.display(), to.display()))?;
    #[cfg(unix)]
    {
        for dir in [pending_dir, spool.join(RUNNING)] {
            if let Ok(handle) = std::fs::File::open(&dir) {
                let _ = handle.sync_all();
            }
        }
    }
    Ok(true)
}

/// Complete interrupted drops left in `dropping/` by removing valid `.json`
/// payloads whose `done/` record is absent or already written.
///
/// Returns the sorted list of recovered transaction IDs.
pub fn complete_dropping(spool: &Path) -> Result<Vec<String>> {
    let dir = spool.join(DROPPING);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error)
                .into_diagnostic()
                .wrap_err_with(|| format!("read {}", dir.display()));
        }
    };
    let mut ids = Vec::new();
    for entry in entries {
        let path = entry
            .into_diagnostic()
            .wrap_err_with(|| format!("read {}", dir.display()))?
            .path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let Some(id) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| valid_id(stem))
            .map(str::to_owned)
        else {
            continue;
        };
        match std::fs::remove_file(&path) {
            Ok(()) => ids.push(id),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .into_diagnostic()
                    .wrap_err_with(|| format!("remove {}", path.display()));
            }
        }
    }
    #[cfg(unix)]
    if !ids.is_empty() {
        if let Ok(handle) = std::fs::File::open(&dir) {
            let _ = handle.sync_all();
        }
    }
    ids.sort();
    Ok(ids)
}

/// Generic health summary of an ops/train spool directory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpoolSummary {
    pub pending: usize,
    pub running: usize,
    pub dropping: usize,
    pub done: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub malformed: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_pending_age_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_running_age_seconds: Option<u64>,
}

/// Inspect a spool's queue counts, ages, and malformed entries without mutating it.
pub fn summarize(spool: &Path, now: chrono::DateTime<chrono::Utc>) -> Result<SpoolSummary> {
    let mut summary = SpoolSummary::default();
    let scan_ops =
        |phase: &str, malformed: &mut Vec<String>, oldest_age: &mut Option<u64>| -> Result<usize> {
            let dir = spool.join(phase);
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
                Err(error) => {
                    return Err(error)
                        .into_diagnostic()
                        .wrap_err_with(|| format!("read {}", dir.display()));
                }
            };
            let mut count = 0usize;
            for entry in entries {
                let path = entry
                    .into_diagnostic()
                    .wrap_err_with(|| format!("read {}", dir.display()))?
                    .path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                match load(&path) {
                    Ok(op) => {
                        count += 1;
                        if let Ok(created) = chrono::DateTime::parse_from_rfc3339(&op.created) {
                            let elapsed = now
                                .signed_duration_since(created.with_timezone(&chrono::Utc))
                                .num_seconds();
                            let age = u64::try_from(elapsed).unwrap_or(0);
                            *oldest_age = Some(oldest_age.map_or(age, |prev| prev.max(age)));
                        }
                    }
                    Err(error) => malformed.push(ignored(&path, &error)),
                }
            }
            Ok(count)
        };

    summary.pending = scan_ops(
        PENDING,
        &mut summary.malformed,
        &mut summary.oldest_pending_age_seconds,
    )?;
    summary.running = scan_ops(
        RUNNING,
        &mut summary.malformed,
        &mut summary.oldest_running_age_seconds,
    )?;
    let count_json = |phase: &str| -> Result<usize> {
        let dir = spool.join(phase);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => {
                return Err(error)
                    .into_diagnostic()
                    .wrap_err_with(|| format!("read {}", dir.display()));
            }
        };
        let mut count = 0usize;
        for entry in entries {
            let path = entry
                .into_diagnostic()
                .wrap_err_with(|| format!("read {}", dir.display()))?
                .path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                count += 1;
            }
        }
        Ok(count)
    };
    summary.dropping = count_json(DROPPING)?;
    summary.done = count_json(DONE)?;
    summary.malformed.sort();
    Ok(summary)
}

impl From<&Op> for crate::status::PendingOp {
    fn from(op: &Op) -> Self {
        Self {
            id: op.id.clone(),
            host: op.host.clone(),
            title: op.title.clone(),
            reason: op.reason.clone(),
            requested_by: op.requested_by.clone(),
            created: op.created.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real spool file, verbatim.
    const REAL: &str = r#"{"id":"20261114T134329-d3d258","host":"atlas","title":"Restart the backup unit","reason":"The timer changed and the unit must reload it.","requested_by":"mcp:claude-code","source":{"t3Thread":"32c7f3c7-04c7-4353-8092-6eb2aa6b7121","claudeSession":"76d1ad97-dc94-4465-9b8e-8df4c99427e4"},"created":"2026-11-14T13:43:29.908777650+00:00","steps":[{"argv":["rm","-rf","--one-file-system","/data/scratch/tmp/regicide-b020"],"note":"Remove the stale scratch tree.","root":true},{"argv":["nix","store","gc"],"note":"Collect the freed store paths.","root":false}]}"#;

    fn request(argvs: &[&[&str]]) -> Request {
        Request {
            host: "atlas".into(),
            title: "Start the base backup".into(),
            reason: String::new(),
            steps: argvs
                .iter()
                .map(|argv| Step {
                    argv: argv.iter().map(ToString::to_string).collect(),
                    note: "Do the step.".into(),
                    root: true,
                })
                .collect(),
            requested_by: "cli:test".into(),
        }
    }

    #[test]
    fn ignored_files_are_reported_without_terminal_controls() {
        let spool = tempfile::tempdir().unwrap();
        let path = spool.path().join("x\u{1b}[2J\r.json");
        std::fs::write(&path, "{").unwrap();
        let message = ignored(&path, &load(&path).unwrap_err());
        assert!(!message.chars().any(char::is_control), "{message}");
    }

    #[test]
    fn load_accepts_a_real_spool_file_verbatim() {
        let spool = tempfile::tempdir().unwrap();
        let dir = spool.path().join(PENDING);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("20261114T134329-d3d258.json"), REAL).unwrap();
        let op = load(&dir.join("20261114T134329-d3d258.json")).unwrap();
        assert_eq!(op.host, "atlas");
        assert_eq!(op.requested_by, "mcp:claude-code");
        assert!(op.needs_root() && !op.steps[1].root);
        assert_eq!(
            op.source.as_ref().unwrap().describe(),
            "T3 thread 32c7f3c7-04c7-4353-8092-6eb2aa6b7121, Claude session 76d1ad97-dc94-4465-9b8e-8df4c99427e4"
        );
        // The on-disk shape survives a round trip byte for byte.
        assert_eq!(serde_json::to_string(&op).unwrap(), REAL);
        assert_eq!(pending(spool.path(), Some("atlas")).unwrap(), vec![op]);
    }

    #[test]
    fn queue_writes_a_loadable_pending_entry() {
        let spool = tempfile::tempdir().unwrap();
        let op = queue(
            spool.path(),
            request(&[&["systemctl", "start", "pg-basebackup-atlas.service"]]),
        )
        .unwrap();
        assert_eq!(op.title, "Start the base backup");
        assert!(valid_id(&op.id), "{}", op.id);
        // "%Y%m%dT%H%M%S-" + six hex digits of the request digest.
        assert_eq!(op.id.len(), 8 + 1 + 6 + 1 + 6, "{}", op.id);
        let path = spool_path(spool.path(), PENDING, &op.id).unwrap();
        assert_eq!(load(&path).unwrap(), op);
        assert_eq!(pending(spool.path(), None).unwrap(), vec![op.clone()]);
        assert_eq!(pending(spool.path(), Some("nomad")).unwrap(), []);
        // The status snapshot carries the same transaction.
        let pending_op = crate::status::PendingOp::from(&op);
        assert_eq!(pending_op.id, op.id);
        assert_eq!(pending_op.created, op.created);
    }

    #[test]
    fn invalid_transactions_never_queue_or_load() {
        let spool = tempfile::tempdir().unwrap();
        assert!(queue(spool.path(), request(&[])).is_err());
        let op = queue(spool.path(), request(&[&["true"]])).unwrap();
        let path = spool_path(spool.path(), PENDING, &op.id).unwrap();
        let write = |op: &Op| std::fs::write(&path, serde_json::to_vec(op).unwrap()).unwrap();

        let mut forged = op.clone();
        forged.title = "harmless\x1b[2K\r    1. true".into();
        write(&forged);
        assert_eq!(pending(spool.path(), None).unwrap(), []);
        forged.title = op.title.clone();
        forged.source = Some(Source {
            t3_thread: Some("x\x1b[2K".into()),
            claude_session: None,
        });
        write(&forged);
        assert_eq!(pending(spool.path(), None).unwrap(), []);
        forged.source = None;
        forged.created = "\x1b[2K".into();
        write(&forged);
        assert_eq!(pending(spool.path(), None).unwrap(), []);
        forged.created = op.created.clone();
        // Every step carries a printable note.
        forged.steps[0].note = " ".into();
        write(&forged);
        assert_eq!(pending(spool.path(), None).unwrap(), []);
        forged.steps[0].note = "x\x1b[2K".into();
        write(&forged);
        assert_eq!(pending(spool.path(), None).unwrap(), []);
        forged.steps[0].note = "Check the unit first.".into();
        forged.steps[0].root = false;
        write(&forged);
        assert_eq!(pending(spool.path(), None).unwrap(), vec![forged.clone()]);
        assert!(!forged.needs_root());
        // `root` is explicit: a step without it never loads as either.
        std::fs::write(
            &path,
            serde_json::to_string(&forged)
                .unwrap()
                .replace(",\"root\":false", ""),
        )
        .unwrap();
        assert_eq!(pending(spool.path(), None).unwrap(), []);
        forged.host = "../atlas".into();
        write(&forged);
        assert_eq!(pending(spool.path(), None).unwrap(), []);

        // The file name must be the transaction id.
        let misnamed = spool.path().join(PENDING).join("other.json");
        std::fs::write(&misnamed, serde_json::to_vec(&op).unwrap()).unwrap();
        assert!(load(&misnamed).is_err());
        assert_eq!(pending(spool.path(), None).unwrap(), []);
        std::fs::remove_file(&misnamed).unwrap();

        // A step is argv: a bare operator would reach the command as an argument.
        let error = queue(
            spool.path(),
            request(&[&[
                "systemctl",
                "is-failed",
                "x.service",
                "--quiet",
                "&&",
                "exit",
                "1",
            ]]),
        )
        .unwrap_err();
        assert!(
            format!("{error}").contains("\"&&\" is not shell syntax"),
            "{error}"
        );
        // Wrapping the step in `sh -c` is how shell syntax travels.
        queue(
            spool.path(),
            request(&[&["sh", "-c", "systemctl is-failed x || true"]]),
        )
        .unwrap();
        assert!(queue(spool.path(), request(&[&["echo", "a\nb"]])).is_err());
        // The file name is a component of the spool path, never a path.
        assert!(spool_path(spool.path(), PENDING, "../x").is_err());
        assert!(!valid_id(""));
        assert!(!valid_id(&"a".repeat(65)));
    }

    #[test]
    fn spool_dir_is_canix_state_dir_plus_ops() {
        assert_eq!(
            state_dir(Some("/var/state".into()), Some("/home/x".into())),
            PathBuf::from("/var/state/canix/ops")
        );
        // A relative XDG_STATE_HOME is ignored, as in the canix state dir.
        assert_eq!(
            state_dir(Some("state".into()), Some("/home/x".into())),
            PathBuf::from("/home/x/.local/state/canix/ops")
        );
        assert_eq!(
            state_dir(None, None),
            PathBuf::from(".local/state/canix/ops")
        );
        let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
        assert_eq!(requester("cli"), format!("cli:{user}"));
    }
}
