//! Observe flake.lock and Cargo.lock pins from a repository's committed tip.
//!
//! No network access and no Nix evaluation: every read is `git show` against
//! the repository's default-branch tip, so an uncommitted or stashed lock file
//! never enters the facts.

use super::{
    Attrs, CONTRACT, Edge, FactError, FactsDocument, Node, Producer, normalize_coordinate,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

/// One repository to observe: its coordinate and location relative to the
/// workspace root supplied at query time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObservedRepo {
    pub coordinate: String,
    pub relative_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
}

/// Observe `repos` under `root` and return one facts document.
///
/// A repository whose lock files are missing has no usages. A git or parse
/// failure becomes a [`FactError`] for that repository and never aborts the
/// observation of the others.
pub fn observe(root: &Path, repos: &[ObservedRepo]) -> FactsDocument {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut errors = Vec::new();
    for repo in repos {
        let coordinate = normalize_coordinate(&repo.coordinate);
        nodes.push(Node {
            id: format!("repo:{coordinate}"),
            kind: "repo".into(),
            attrs: BTreeMap::from([(
                "relativePath".to_string(),
                Value::String(repo.relative_path.display().to_string()),
            )]),
            expects: Vec::new(),
            blockers: Vec::new(),
        });

        let directory = root.join(&repo.relative_path);
        let reference = match resolve_ref(&directory, repo.default_branch.as_deref()) {
            Ok(reference) => reference,
            Err(message) => {
                errors.push(fact_error(&coordinate, "git-failure", message));
                continue;
            }
        };

        // A missing lock file is normal; a git failure is not.
        for (file, parse) in [
            (
                "flake.lock",
                flake_usages as fn(&str, &str) -> Result<Usages, String>,
            ),
            ("Cargo.lock", cargo_usages),
        ] {
            match show_file(&directory, &reference, file) {
                Ok(None) => {}
                Ok(Some(text)) => match parse(&coordinate, &text) {
                    Ok(mut usages) => {
                        nodes.append(&mut usages.nodes);
                        edges.append(&mut usages.edges);
                    }
                    Err(message) => errors.push(fact_error(&coordinate, "parse-failure", message)),
                },
                Err(message) => errors.push(fact_error(&coordinate, "git-failure", message)),
            }
        }
    }

    FactsDocument {
        contract: CONTRACT.into(),
        vocabulary: "fleetix.lock-facts.v1".into(),
        producer: Producer {
            name: "fleetix".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        observed_at: now_rfc3339(),
        usage_kinds: vec!["flake-input".into(), "cargo-git".into()],
        nodes,
        edges,
        capabilities: Vec::new(),
        errors,
    }
}

#[derive(Default)]
struct Usages {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl Usages {
    /// Add one usage, folding lock entries that share an input name into one
    /// node with a set of revisions. Two lock nodes can carry the same name
    /// (a direct root input and a transitive node key); emitting both would
    /// be a contract conflict over `direct` and `lockedRev`.
    fn push_usage(&mut self, repository: &str, node: Node, pins: &[(String, String)]) {
        let id = node.id.clone();
        if let Some(known) = self.nodes.iter_mut().find(|known| known.id == id) {
            let direct = known
                .attrs
                .get("direct")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || node
                    .attrs
                    .get("direct")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            known.attrs.insert("direct".into(), Value::Bool(direct));
            let known_rev = known.attrs.get("lockedRev").and_then(Value::as_str);
            let new_rev = node.attrs.get("lockedRev").and_then(Value::as_str);
            let merged = match (known_rev, new_rev) {
                (Some(left), Some(right)) if left == right => Value::String(left.into()),
                _ => Value::Null,
            };
            known.attrs.insert("lockedRev".into(), merged);
        } else {
            self.edges.push(uses(repository, id.clone()));
            self.nodes.push(node);
        }
        for (producer, revision) in pins {
            self.edges.push(pin_edge(&id, producer, revision));
        }
    }
}

fn fact_error(subject: &str, code: &str, message: String) -> FactError {
    FactError {
        subject: subject.into(),
        code: code.into(),
        message,
    }
}

/// The default-branch tip: `origin/HEAD`, the declared default branch, then
/// `HEAD`. The returned name is usable as `git show <ref>:<file>`.
fn resolve_ref(directory: &Path, default_branch: Option<&str>) -> Result<String, String> {
    let mut candidates = vec!["origin/HEAD".to_string()];
    if let Some(branch) = default_branch {
        candidates.push(format!("origin/{branch}"));
        candidates.push(branch.to_string());
    }
    candidates.push("HEAD".into());
    for candidate in candidates {
        let spec = format!("{candidate}^{{commit}}");
        let output = git(directory, &["rev-parse", "--verify", "--quiet", &spec])?;
        if output.status.success() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "git -C {}: no origin/HEAD, default branch or HEAD",
        directory.display()
    ))
}

/// The committed file at `reference`, or `None` when the tree has no such path.
fn show_file(directory: &Path, reference: &str, file: &str) -> Result<Option<String>, String> {
    let spec = format!("{reference}:{file}");
    if !git(directory, &["cat-file", "-e", &spec])?.status.success() {
        return Ok(None);
    }
    let output = git(directory, &["show", &spec])?;
    if !output.status.success() {
        return Err(format!("git show {spec}: {}", stderr(&output)));
    }
    String::from_utf8(output.stdout)
        .map(Some)
        .map_err(|error| format!("git show {spec}: {error}"))
}

fn git(directory: &Path, arguments: &[&str]) -> Result<Output, String> {
    Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .output()
        .map_err(|error| format!("git -C {}: {error}", directory.display()))
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

fn usage(coordinate: &str, locator: String, attrs: Attrs) -> Node {
    let mut attrs = attrs;
    attrs.insert("state".into(), Value::String("unknown".into()));
    Node {
        id: format!("usage:{coordinate}#{locator}"),
        kind: "usage".into(),
        attrs,
        expects: Vec::new(),
        blockers: Vec::new(),
    }
}

fn uses(from: &str, to: String) -> Edge {
    Edge {
        from: from.into(),
        to,
        kind: "uses".into(),
        attrs: BTreeMap::new(),
    }
}

fn pin_edge(from: &str, producer: &str, revision: &str) -> Edge {
    Edge {
        from: from.into(),
        to: format!("rev:{producer}@{revision}"),
        kind: "pins".into(),
        attrs: BTreeMap::new(),
    }
}

fn flake_usages(coordinate: &str, text: &str) -> Result<Usages, String> {
    let lock: Value = serde_json::from_str(text).map_err(|error| format!("flake.lock: {error}"))?;
    let nodes = lock
        .get("nodes")
        .and_then(Value::as_object)
        .ok_or_else(|| "flake.lock: no nodes object".to_string())?;
    let root_inputs = nodes
        .get("root")
        .and_then(|root| root.get("inputs"))
        .and_then(Value::as_object);

    let mut usages = Usages::default();
    let repository = format!("repo:{coordinate}");
    for (key, node) in nodes {
        if key == "root" {
            continue;
        }
        let Some(producer) = lock_coordinate(node) else {
            continue;
        };
        let revision = node
            .get("locked")
            .and_then(|locked| locked.get("rev"))
            .and_then(Value::as_str);
        let (input, direct) = root_input_name(root_inputs, key);
        // Lock node keys (`nixpkgs_10`) renumber whenever a lock changes, so a
        // transitive pin is keyed by its producer: one usage per repository
        // and producer. Its input and revision stay unknown (null) so aliased
        // producers fold in `build`; the pins edges carry every revision.
        let (locator, input, locked) = if direct {
            let locked = revision.map_or(Value::Null, |revision| Value::String(revision.into()));
            (input.clone(), Value::String(input), locked)
        } else {
            (format!("transitive:{producer}"), Value::Null, Value::Null)
        };
        let node = usage(
            coordinate,
            format!("flake-input:{locator}"),
            Attrs::from([
                ("usageKind".to_string(), Value::String("flake-input".into())),
                ("path".to_string(), Value::String("flake.lock".into())),
                ("input".to_string(), input),
                ("direct".to_string(), Value::Bool(direct)),
                ("lockedRev".to_string(), locked),
            ]),
        );
        let pins: Vec<_> = revision
            .map(|revision| (producer, revision.to_string()))
            .into_iter()
            .collect();
        usages.push_usage(&repository, node, &pins);
    }
    Ok(usages)
}

/// The root input name that names `key` directly, or the lock node key.
fn root_input_name(
    root_inputs: Option<&serde_json::Map<String, Value>>,
    key: &str,
) -> (String, bool) {
    let Some(root_inputs) = root_inputs else {
        return (key.to_string(), false);
    };
    for (name, value) in root_inputs {
        let names_key = value.as_str() == Some(key)
            || value
                .as_array()
                .is_some_and(|value| value.len() == 1 && value[0].as_str() == Some(key));
        if names_key {
            return (name.clone(), true);
        }
    }
    (key.to_string(), false)
}

/// The producer coordinate of a lock node, from `original`, falling back to
/// `locked`. `path` inputs are local and never producers.
fn lock_coordinate(node: &Value) -> Option<String> {
    let original = node.get("original").filter(|value| value.is_object());
    if original
        .and_then(|original| original.get("type"))
        .and_then(Value::as_str)
        == Some("path")
    {
        return None;
    }
    let locked = node.get("locked").and_then(lock_spec_coordinate);
    original.and_then(lock_spec_coordinate).or(locked)
}

fn lock_spec_coordinate(spec: &Value) -> Option<String> {
    match spec.get("type").and_then(Value::as_str)? {
        "github" => {
            let owner = spec.get("owner").and_then(Value::as_str)?;
            let repo = spec.get("repo").and_then(Value::as_str)?;
            Some(format!("github.com/{owner}/{repo}"))
        }
        "git" => coordinate_from_url(spec.get("url").and_then(Value::as_str)?),
        _ => None,
    }
}

/// `host/owner/name` from an https, ssh, `git+…` or scp-style git URL,
/// without query or fragment.
fn coordinate_from_url(raw: &str) -> Option<String> {
    let url = raw.strip_prefix("git+").unwrap_or(raw);
    let url = url.split(['?', '#']).next()?;
    let (authority, path) = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?,
        None => url.split_once(':')?,
    };
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
        .split(':')
        .next()?;
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let segments: Vec<_> = path.split('/').filter(|part| !part.is_empty()).collect();
    if host.is_empty() || segments.len() < 2 {
        return None;
    }
    Some(format!(
        "{}/{}/{}",
        host.to_ascii_lowercase(),
        segments[0],
        segments[segments.len() - 1]
    ))
}

/// Git sources in a Cargo.lock, grouped by package name. A package pinned at
/// several revisions lists them all.
fn cargo_usages(coordinate: &str, text: &str) -> Result<Usages, String> {
    let mut packages: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
    let mut current: Option<(Option<String>, Option<String>)> = None;
    for line in text.lines() {
        let line = line.trim();
        // A new table ends the previous package; `[[patch.unused]]` and any
        // other table must not contribute its names or sources.
        if line.starts_with('[') {
            finish_package(&mut current, &mut packages);
            current = (line == "[[package]]").then_some((None, None));
            continue;
        }
        let Some((name, source)) = current.as_mut() else {
            continue;
        };
        if let Some(value) = toml_string(line, "name") {
            *name = Some(value);
        } else if let Some(value) = toml_string(line, "source") {
            *source = Some(value);
        }
    }
    finish_package(&mut current, &mut packages);

    let mut usages = Usages::default();
    let repository = format!("repo:{coordinate}");
    for (package, sources) in packages {
        let mut revisions: BTreeSet<&str> = BTreeSet::new();
        for (_, revision) in &sources {
            revisions.insert(revision);
        }
        let locked_rev = match revisions.len() {
            1 => Value::String(revisions.into_iter().next().unwrap_or_default().into()),
            _ => Value::Null,
        };
        let node = usage(
            coordinate,
            format!("cargo-git:{package}"),
            Attrs::from([
                ("usageKind".to_string(), Value::String("cargo-git".into())),
                ("path".to_string(), Value::String("Cargo.lock".into())),
                ("input".to_string(), Value::String(package)),
                ("lockedRev".to_string(), locked_rev),
            ]),
        );
        let pins: Vec<_> = sources.into_iter().collect();
        usages.push_usage(&repository, node, &pins);
    }
    Ok(usages)
}

fn finish_package(
    current: &mut Option<(Option<String>, Option<String>)>,
    packages: &mut BTreeMap<String, BTreeSet<(String, String)>>,
) {
    let Some((Some(name), Some(source))) = current.take() else {
        return;
    };
    let Some((url, revision)) = source.strip_prefix("git+").and_then(|s| s.rsplit_once('#')) else {
        return;
    };
    let Some(producer) = coordinate_from_url(url) else {
        return;
    };
    packages
        .entry(name)
        .or_default()
        .insert((producer, revision.to_string()));
}

/// `key = "value"` on one Cargo.lock line; other lines are ignored.
fn toml_string(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.trim_start();
    let value = rest.strip_prefix('=')?.trim();
    let value = value.strip_prefix('"')?.strip_suffix('"')?;
    Some(value.to_string())
}

fn now_rfc3339() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let clock = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        clock / 3600,
        clock % 3600 / 60,
        clock % 60
    )
}

/// Howard Hinnant's `civil_from_days`: the inverse of the day count in
/// [`super::age_days`].
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (
        if month <= 2 { year + 1 } else { year },
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_urls_reduce_to_host_owner_name() {
        for (url, expected) in [
            (
                "https://github.com/caniko/harbor-rs.git",
                "github.com/caniko/harbor-rs",
            ),
            (
                "git+ssh://git@codeberg.org/caniko/rs-harbor",
                "codeberg.org/caniko/rs-harbor",
            ),
            (
                "ssh://git@codeberg.org:2222/caniko/rs-harbor.git",
                "codeberg.org/caniko/rs-harbor",
            ),
            (
                "git@codeberg.org:caniko/harbor-meta.git",
                "codeberg.org/caniko/harbor-meta",
            ),
            (
                "https://github.com/caniko/harbor-py.git?ref=trunk",
                "github.com/caniko/harbor-py",
            ),
        ] {
            assert_eq!(coordinate_from_url(url).as_deref(), Some(expected), "{url}");
        }
        assert_eq!(coordinate_from_url("file:///tmp/repo"), None);
        assert_eq!(coordinate_from_url("not a url"), None);
    }

    #[test]
    fn civil_dates_round_trip_the_age_rule() {
        for days in [0, 1, 11_017, 20_000] {
            let (year, month, day) = civil_from_days(days);
            let stamp = format!("{year:04}-{month:02}-{day:02}T00:00:00Z");
            assert_eq!(
                super::super::age_days(&stamp, u64::try_from(days).unwrap_or(0) * 86_400),
                Some(0),
                "{days} -> {stamp}"
            );
        }
    }
}
