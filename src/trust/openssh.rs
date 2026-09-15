// OpenSSH known_hosts observation.
//
// Only plain host-key lines can ever be declared: hashed hostnames (`|1|...`)
// and `@revoked` / `@cert-authority` markers are reported as skipped. A line
// whose hostnames are already declared with a different key is a conflict —
// security-relevant and reported, but never offered for integration.

use super::{DeclaredTrust, Observation};
use miette::{miette, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

/// The OpenSSH known_hosts store.
pub struct OpenSshKnownHosts;

/// A single host-key line from known_hosts, ready for proposal or declaration.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Content hash identifying this exact hostnames+key pair.
    pub id: String,
    /// Hostnames as written in the store (patterns like `[host]:port` kept).
    pub host_names: Vec<String>,
    /// Key algorithm (e.g. `ssh-ed25519`).
    pub key_type: String,
    /// Base64 key body.
    pub key: String,
    /// Full `keytype base64` text, used verbatim in declarations.
    pub key_text: String,
    /// Optional trailing comment from the store line.
    pub comment: Option<String>,
    /// Fingerprint when `ssh-keygen` could validate the key.
    pub fingerprint: Option<String>,
    /// 1-based line number in the store file.
    pub line: usize,
}

/// Deterministic proposal id: sorted unique hostnames + key text.
pub fn entry_id(host_names: &[String], key_text: &str) -> String {
    let mut names: Vec<&str> = host_names.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    let mut hasher = Sha256::new();
    for name in &names {
        hasher.update(name);
        hasher.update(b"\n");
    }
    hasher.update(key_text.as_bytes());
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

impl OpenSshKnownHosts {
    /// Observe a known_hosts store against the declared trust.
    pub fn observe(&self, path: &Path, declared: &DeclaredTrust) -> Result<Observation> {
        let (entries, skipped) = parse_entries(path)?;
        let mut proposals = Vec::new();
        let mut conflicts = Vec::new();
        for entry in entries {
            let mut conflict = false;
            let mut all_declared = true;
            for hostname in &entry.host_names {
                let declared_keys = declared.keys_for(hostname);
                if declared_keys.contains(&entry.key_text) {
                    continue;
                }
                all_declared = false;
                if !declared_keys.is_empty() {
                    conflict = true;
                    break;
                }
            }
            if conflict {
                conflicts.push(entry);
            } else if !all_declared {
                proposals.push(entry);
            }
        }
        Ok(Observation {
            proposals,
            conflicts,
            skipped,
        })
    }
}

/// Parse a known_hosts store into entries and skip reasons. Used by the
/// observer and by `integrate`/`ignore`, which re-read the store.
pub fn parse_entries(path: &Path) -> Result<(Vec<Entry>, Vec<String>)> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| miette!("read known_hosts {}: {error}", path.display()))?;
    let mut entries = Vec::new();
    let mut skipped = Vec::new();
    for (index, raw) in content.lines().enumerate() {
        let line_number = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match parse_line(line, line_number) {
            Ok(Some(entry)) => entries.push(entry),
            Ok(None) => skipped.push(format!(
                "{}:{}: {}",
                path.display(),
                line_number,
                marker_reason(line)
            )),
            Err(reason) => skipped.push(format!("{}:{line_number}: {reason}", path.display())),
        }
    }
    validate_fingerprints(&mut entries);
    Ok((entries, skipped))
}

/// Parse one known_hosts line. `Ok(None)` marks lines that are structurally
/// valid but can never be declared (hashed/marker lines).
fn parse_line(line: &str, line_number: usize) -> Result<Option<Entry>> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 3 {
        return Err(miette!("malformed line: expected 'hostnames keytype key'"));
    }
    let host_field = fields[0];
    if host_field.starts_with('|') {
        return Ok(None);
    }
    if host_field.starts_with('@') {
        return Ok(None);
    }
    let key_type = fields[1];
    if !key_type
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    {
        return Err(miette!(
            "invalid key type '{}'",
            key_type.chars().take(24).collect::<String>()
        ));
    }
    let key = fields[2];
    if key.is_empty()
        || !key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '='))
    {
        return Err(miette!("invalid base64 key body"));
    }
    let mut host_names: Vec<String> = host_field
        .split(',')
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect();
    if host_names.is_empty() {
        return Err(miette!("empty hostname list"));
    }
    host_names.sort();
    host_names.dedup();
    let key_text = format!("{key_type} {key}");
    let comment = fields.get(3).map(|part| (*part).to_string());
    Ok(Some(Entry {
        id: entry_id(&host_names, &key_text),
        host_names,
        key_type: key_type.to_string(),
        key: key.to_string(),
        key_text,
        comment,
        fingerprint: None,
        line: line_number,
    }))
}

fn marker_reason(line: &str) -> String {
    let field = line.split_whitespace().next().unwrap_or_default();
    if field.starts_with('|') {
        "hashed hostname line cannot be declared; ssh-keyscan can produce plain hostnames"
            .to_string()
    } else {
        format!("marker line ('{field}') cannot be declared")
    }
}

/// Fill fingerprints via `ssh-keygen -lf` on each entry in isolation. Missing
/// or failing ssh-keygen leaves the fingerprint `None`; the shape check above
/// still guards against garbage keys.
fn validate_fingerprints(entries: &mut Vec<Entry>) {
    let mut kept = Vec::with_capacity(entries.len());
    for mut entry in std::mem::take(entries) {
        if let Some(fingerprint) = fingerprint(&entry.key_text) {
            entry.fingerprint = Some(fingerprint);
        }
        kept.push(entry);
    }
    *entries = kept;
}

/// `ssh-keygen -lf` on a one-entry store. `None` when ssh-keygen is missing
/// or fails; a malformed key then still passes the shape check above.
fn fingerprint(key_text: &str) -> Option<String> {
    let directory = tempfile::tempdir().ok()?;
    let store = directory.path().join("known_hosts");
    std::fs::write(&store, format!("placeholder {key_text}\n")).ok()?;
    let output = std::process::Command::new("ssh-keygen")
        .args(["-l", "-f"])
        .arg(&store)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.split_whitespace().find_map(|part| {
        part.strip_prefix("SHA256:")
            .map(|value| format!("SHA256:{value}"))
    })
}

#[cfg(test)]
mod tests {
    use super::OpenSshKnownHosts;
    use super::*;

    fn declared() -> DeclaredTrust {
        // Two declared fleet hosts + one declared trust entry.
        let mut topology = crate::topology::Topology::default();
        topology.hosts.insert(
            "hub".to_string(),
            crate::topology::Host {
                system: "x86_64-linux".to_string(),
                host_pubkey: Some("ssh-ed25519 AAAHubKey".to_string()),
                host_names: vec!["hub.local".to_string()],
                ..Default::default()
            },
        );
        topology
            .trust
            .ssh_known_hosts
            .push(crate::topology::SshKnownHost {
                host_names: vec!["git.example.test".to_string()],
                public_keys: vec!["ssh-ed25519 AAAADeclaredGitKey".to_string()],
                provenance: Some("observed-known-hosts".to_string()),
            });
        DeclaredTrust::from_topology(&topology)
    }

    #[test]
    fn proposal_for_undeclared_entry() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("known_hosts");
        std::fs::write(&path, "new.example.test ssh-ed25519 AAAANewKey\n").unwrap();
        let observation = OpenSshKnownHosts.observe(&path, &declared()).unwrap();
        assert_eq!(observation.proposals.len(), 1);
        assert_eq!(observation.conflicts.len(), 0);
        assert_eq!(observation.proposals[0].host_names, ["new.example.test"]);
        assert_eq!(observation.proposals[0].id.len(), 64);
    }

    #[test]
    fn declared_entry_is_not_a_proposal() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("known_hosts");
        std::fs::write(
            &path,
            "git.example.test ssh-ed25519 AAAADeclaredGitKey\nhub ssh-ed25519 AAAHubKey\n",
        )
        .unwrap();
        let observation = OpenSshKnownHosts.observe(&path, &declared()).unwrap();
        assert!(observation.proposals.is_empty());
        assert!(observation.conflicts.is_empty());
    }

    #[test]
    fn ip_aliases_and_bracket_port_forms_match_declared_hosts() {
        let mut topology = crate::topology::Topology::default();
        topology.hosts.insert(
            "hub".to_string(),
            crate::topology::Host {
                system: "x86_64-linux".to_string(),
                host_pubkey: Some("ssh-ed25519 AAAHubKey".to_string()),
                network: crate::topology::Network {
                    lan_ip: Some("192.0.2.10".to_string()),
                    direct_link_ip: Some("203.0.113.1".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let declared = DeclaredTrust::from_topology(&topology);
        assert!(declared
            .keys_for("192.0.2.10")
            .contains(&"ssh-ed25519 AAAHubKey".to_string()));
        assert!(declared
            .keys_for("[203.0.113.1]:1337")
            .contains(&"ssh-ed25519 AAAHubKey".to_string()));
    }

    #[test]
    fn different_key_for_declared_hostname_is_a_conflict() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("known_hosts");
        std::fs::write(&path, "hub ssh-ed25519 AAAAEvilKey\n").unwrap();
        let observation = OpenSshKnownHosts.observe(&path, &declared()).unwrap();
        assert!(observation.proposals.is_empty());
        assert_eq!(observation.conflicts.len(), 1);
        assert_eq!(observation.conflicts[0].host_names, ["hub"]);
    }

    #[test]
    fn hashed_and_marker_lines_are_skipped() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("known_hosts");
        std::fs::write(
            &path,
            "|1|abc|def ssh-ed25519 AAAAHashed\n@revoked example.test ssh-ed25519 AAAARevoked\n",
        )
        .unwrap();
        let observation = OpenSshKnownHosts.observe(&path, &declared()).unwrap();
        assert!(observation.proposals.is_empty());
        assert!(observation.conflicts.is_empty());
        assert_eq!(observation.skipped.len(), 2);
        assert!(observation.skipped[0].contains("hashed"));
    }

    #[test]
    fn malformed_lines_are_reported_skipped() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("known_hosts");
        std::fs::write(
            &path,
            "just-two-fields\nok.example.test ssh-ed25519 notbase64!!!\n",
        )
        .unwrap();
        let observation = OpenSshKnownHosts.observe(&path, &declared()).unwrap();
        assert!(observation.proposals.is_empty());
        assert_eq!(observation.skipped.len(), 2);
    }

    #[test]
    fn entry_ids_are_stable_and_order_independent() {
        let a = entry_id(
            &["b.example".to_string(), "a.example".to_string()],
            "ssh-ed25519 KEY",
        );
        let b = entry_id(
            &["a.example".to_string(), "b.example".to_string()],
            "ssh-ed25519 KEY",
        );
        assert_eq!(a, b);
        let c = entry_id(&["a.example".to_string()], "ssh-ed25519 OTHER");
        assert_ne!(a, c);
    }
}
