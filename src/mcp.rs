//! Reconcile rendered MCP registrations without taking ownership of a user's whole config.
use crate::fsutil::atomic_write;
use miette::{IntoDiagnostic, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

mod document;
use document::Document;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Json,
    Toml,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    pub path: PathBuf,
    pub format: Format,
    pub root: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Retirement {
    pub key: String,
    pub command_suffix: String,
    #[serde(default)]
    pub args_prefix: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub name: String,
    #[serde(flatten)]
    pub destination: Destination,
    pub servers: BTreeMap<String, Value>,
    /// Explicit first-migration ownership transfer, only used without an existing ledger entry.
    #[serde(default)]
    pub adopt: Vec<String>,
    #[serde(default)]
    pub retire: Vec<Retirement>,
    #[serde(default)]
    pub auto_detect: bool,
    #[serde(default)]
    pub commands: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub targets: Vec<Target>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Owned {
    destination: Destination,
    /// During a transaction either the old or new value may be on disk.
    entries: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    targets: BTreeMap<String, Owned>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            targets: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Serialize, Default)]
pub struct Report {
    pub changed: Vec<PathBuf>,
    pub configured: Vec<String>,
    pub skipped: Vec<String>,
}

fn fingerprint(value: &Value) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).into_diagnostic()?)
    ))
}

fn read_regular(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => {
            bail!(
                "MCP destination {} is not a regular file; use native delivery for declarative files",
                path.display()
            );
        }
        Ok(_) => Ok(Some(fs::read(path).into_diagnostic()?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).into_diagnostic(),
    }
}

fn validate_destination(destination: &Destination, state: &Path) -> Result<()> {
    if !destination.path.is_absolute()
        || destination
            .path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        || destination.root.is_empty()
        || destination.root.iter().any(String::is_empty)
        || destination.path == state
        || destination.path == state.with_extension("lock")
    {
        bail!("invalid MCP destination {}", destination.path.display());
    }
    Ok(())
}

fn detected(target: &Target) -> bool {
    if !target.auto_detect {
        return true;
    }
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            target.commands.iter().any(|command| {
                let path = dir.join(command);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::metadata(path)
                        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                }
                #[cfg(not(unix))]
                {
                    path.is_file()
                }
            })
        })
    })
}

/// Dry-run performs no writes. Apply takes a persistent kernel lock, checks all files
/// before mutation, and journals both accepted versions before replacing any file.
pub fn reconcile(manifest: &Manifest, state_path: &Path, dry_run: bool) -> Result<Report> {
    if manifest.version != 1 {
        bail!("unsupported MCP manifest version {}", manifest.version);
    }
    if !state_path.is_absolute() || state_path == state_path.with_extension("lock") {
        bail!("MCP state path must be absolute and distinct from its .lock anchor");
    }
    read_regular(state_path)?;
    let _lock = if dry_run {
        None
    } else {
        let parent = state_path
            .parent()
            .ok_or_else(|| miette::miette!("MCP state path has no parent"))?;
        fs::create_dir_all(parent).into_diagnostic()?;
        let lock_path = state_path.with_extension("lock");
        read_regular(&lock_path)?;
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(&lock_path).into_diagnostic()?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|error| {
            miette::miette!(
                "MCP reconciliation already running ({}): {error}",
                lock_path.display()
            )
        })?;
        Some(lock)
    };
    let state: State = match read_regular(state_path)? {
        Some(bytes) => serde_json::from_slice(&bytes).into_diagnostic()?,
        None => State::default(),
    };
    if state.version != 1 {
        bail!("unsupported MCP state version {}", state.version);
    }
    let mut report = Report::default();
    let mut desired = BTreeMap::new();
    let mut names = BTreeSet::new();
    let mut destinations = BTreeSet::new();
    for target in &manifest.targets {
        validate_destination(&target.destination, state_path)?;
        if !names.insert(&target.name) || !destinations.insert(&target.destination.path) {
            bail!("duplicate MCP writer for {}", target.name);
        }
        if target.servers.values().any(|v| !v.is_object()) {
            bail!("MCP server descriptors must be objects");
        }
        if target
            .retire
            .iter()
            .any(|entry| entry.command_suffix.is_empty() || target.servers.contains_key(&entry.key))
        {
            bail!("invalid MCP retirement for {}", target.name);
        }
        if detected(target) {
            desired.insert(target.name.clone(), target);
            report.configured.push(target.name.clone());
        } else {
            report.skipped.push(target.name.clone());
        }
    }
    let mut docs: BTreeMap<PathBuf, (Format, Option<Vec<u8>>, Document)> = BTreeMap::new();
    for destination in state
        .targets
        .values()
        .map(|owned| &owned.destination)
        .chain(desired.values().map(|target| &target.destination))
    {
        validate_destination(destination, state_path)?;
        if let Some((format, _, _)) = docs.get(&destination.path) {
            if format != &destination.format {
                bail!("conflicting MCP formats for {}", destination.path.display());
            }
        } else {
            let before = read_regular(&destination.path)?;
            let document = Document::parse(&destination.format, before.as_deref())?;
            docs.insert(
                destination.path.clone(),
                (destination.format.clone(), before, document),
            );
        }
    }
    let mut next = State::default();
    let mut pending = state.clone();
    // Validate old ownership before touching the in-memory documents.
    for (name, owned) in &state.targets {
        let (_, _, doc) = &docs[&owned.destination.path];
        for (key, hashes) in &owned.entries {
            if let Some(value) = doc.get(&owned.destination.root, key)? {
                let desired_value = desired
                    .get(name)
                    .filter(|t| t.destination == owned.destination)
                    .and_then(|t| t.servers.get(key));
                if !hashes.contains(&fingerprint(&value)?) && desired_value != Some(&value) {
                    bail!(
                        "MCP ownership conflict: {name}/{key} in {} was edited; preserving it",
                        owned.destination.path.display()
                    );
                }
            }
        }
    }
    for (name, owned) in &state.targets {
        let (_, _, doc) = docs
            .get_mut(&owned.destination.path)
            .expect("loaded destination");
        for key in owned.entries.keys() {
            if !desired
                .get(name)
                .is_some_and(|t| t.destination == owned.destination && t.servers.contains_key(key))
            {
                doc.remove(&owned.destination.root, key)?;
            }
        }
    }
    for (name, target) in &desired {
        let (_, _, doc) = docs
            .get_mut(&target.destination.path)
            .expect("loaded destination");
        let previous = state
            .targets
            .get(name)
            .filter(|s| s.destination == target.destination);
        let mut entries = BTreeMap::new();
        let mut adopted_hashes = BTreeMap::new();
        for (key, value) in &target.servers {
            if let Some(existing) = doc.get(&target.destination.root, key)? {
                if previous.and_then(|s| s.entries.get(key)).is_none()
                    && existing != *value
                    && !target.adopt.contains(key)
                {
                    bail!(
                        "MCP ownership conflict: {name}/{key} already exists in {}; declare an explicit migration adoption",
                        target.destination.path.display()
                    );
                }
                adopted_hashes.insert(key.clone(), fingerprint(&existing)?);
            }
            doc.set(&target.destination.root, key, value)?;
            entries.insert(key.clone(), vec![fingerprint(value)?]);
        }
        for retired in &target.retire {
            if let Some(value) = doc.get(&target.destination.root, &retired.key)? {
                let command_matches = value
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.ends_with(&retired.command_suffix));
                let args: Vec<String> = serde_json::from_value(
                    value
                        .get("args")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!([])),
                )
                .unwrap_or_default();
                if command_matches && args.starts_with(&retired.args_prefix) {
                    doc.remove(&target.destination.root, &retired.key)?;
                }
            }
        }
        let owned = Owned {
            destination: target.destination.clone(),
            entries,
        };
        // Keep a moved destination in the journal until its old entries are removed.
        if let Some(previous) = pending.targets.get(name) {
            if previous.destination != target.destination {
                pending.targets.insert(
                    format!(
                        "{name}@retired:{}",
                        fingerprint(
                            &serde_json::to_value(&previous.destination).into_diagnostic()?
                        )?
                    ),
                    previous.clone(),
                );
            }
        }
        let mut journal = owned.clone();
        for (key, hash) in adopted_hashes {
            let hashes = journal.entries.entry(key).or_default();
            if !hashes.contains(&hash) {
                hashes.push(hash);
            }
        }
        if let Some(previous) = previous {
            for (key, hashes) in &previous.entries {
                let accepted = journal.entries.entry(key.clone()).or_default();
                for hash in hashes {
                    if !accepted.contains(hash) {
                        accepted.push(hash.clone());
                    }
                }
            }
        }
        pending.targets.insert(name.clone(), journal);
        next.targets.insert(name.clone(), owned);
    }
    // A renamed target may reuse the same destination. Its retired journal
    // record must also accept the newly written values until finalization.
    for owned in pending.targets.values_mut() {
        for current in next
            .targets
            .values()
            .filter(|t| t.destination == owned.destination)
        {
            for (key, hashes) in &mut owned.entries {
                if let Some(written) = current.entries.get(key) {
                    for hash in written {
                        if !hashes.contains(hash) {
                            hashes.push(hash.clone());
                        }
                    }
                }
            }
        }
    }
    let mut writes = Vec::new();
    for (path, (format, before, doc)) in docs {
        let after = doc.render()?;
        // An absent file with no entries to remove remains absent.
        if before.is_none() && doc.is_empty() {
            continue;
        }
        if before.is_some() && Document::parse(&format, before.as_deref())?.render()? == after {
            continue;
        }
        if before.as_deref() != Some(after.as_bytes()) {
            report.changed.push(path.clone());
            writes.push((path, before, after));
        }
    }
    if dry_run {
        return Ok(report);
    }
    // Detect non-cooperating client edits made during planning.
    for (path, before, _) in &writes {
        if read_regular(path)? != *before {
            bail!("MCP config changed during planning: {}", path.display());
        }
    }
    save_state(state_path, &pending)?;
    for (path, before, after) in writes {
        if read_regular(&path)? != before {
            bail!("MCP config changed before replacement: {}", path.display());
        }
        atomic_write(&path, after.as_bytes())?;
    }
    save_state(state_path, &next)?;
    Ok(report)
}

fn save_state(path: &Path, state: &State) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(state).into_diagnostic()?;
    if read_regular(path)?.as_deref() != Some(&bytes) {
        atomic_write(path, &bytes)?;
    }
    Ok(())
}
