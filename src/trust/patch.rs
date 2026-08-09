// Atomic patching of the consumer-owned Trust.pkl.
//
// Entries are inserted into the `sshKnownHosts` listing by brace matching,
// preserving comments and formatting around the edit. The caller owns
// rollback (restore the original bytes) when a later topology eval fails.

use super::openssh::Entry;
use crate::fsutil::atomic_write;
use crate::pkl::string_literal;
use crate::topology::matching_brace;
use miette::{miette, Result};
use std::path::Path;

const DEFAULT_TRUST_PKL: &str = r#"// Fleet-level trust declarations.
// Entries are typically proposed by `fleetix trust scan` and integrated
// through the desktop observer; edit by hand when needed.
import "Schema.pkl" as S

trust = new S.Trust {
  sshKnownHosts = new Listing<S.SshKnownHost> {
  }
}
"#;

/// Render an entry as a Pkl object literal for the `sshKnownHosts` listing.
pub fn entry_literal(entry: &Entry) -> String {
    let mut out = String::new();
    out.push_str("    new S.SshKnownHost {\n");
    out.push_str("      hostNames = new Listing {\n");
    for name in &entry.host_names {
        out.push_str(&format!("        {}\n", string_literal(name)));
    }
    out.push_str("      }\n");
    out.push_str("      publicKeys = new Listing {\n");
    out.push_str(&format!("        {}\n", string_literal(&entry.key_text)));
    out.push_str("      }\n");
    out.push_str("      provenance = \"observed-known-hosts\"\n");
    out.push_str("    }");
    out
}

/// Insert `entry` into the `sshKnownHosts` listing of `content`, atomically
/// replacing `path`. Creates a default Trust.pkl when the file is missing.
/// Returns the previous file contents so callers can roll back.
pub fn patch_trust_pkl(path: &Path, entry: &Entry) -> Result<Option<Vec<u8>>> {
    let previous = std::fs::read(path).ok();
    let content = previous
        .as_deref()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_else(|| DEFAULT_TRUST_PKL.to_string());
    let patched = insert_entry(&content, entry)?;
    atomic_write(path, patched.as_bytes())?;
    Ok(previous)
}

/// Restore a rolled-back Trust.pkl from the bytes captured before patching.
/// When the file did not exist before the patch, it is removed.
pub fn restore_trust_pkl(path: &Path, previous: Option<&[u8]>) -> Result<()> {
    let Some(previous) = previous else {
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(miette!("remove {}: {error}", path.display())),
        }
    };
    atomic_write(path, previous)
}

fn insert_entry(content: &str, entry: &Entry) -> Result<String> {
    let Some(listing) = content.find("sshKnownHosts") else {
        return Err(miette!(
            "Trust.pkl has no sshKnownHosts listing; add one or integrate manually"
        ));
    };
    let after = &content[listing + "sshKnownHosts".len()..];
    let Some(open_rel) = after.find('{') else {
        return Err(miette!("sshKnownHosts has no opening brace"));
    };
    let open = listing + "sshKnownHosts".len() + open_rel;
    if matching_brace(content, open).is_none() {
        return Err(miette!("sshKnownHosts listing is not brace-balanced"));
    }
    let mut patched = String::with_capacity(content.len() + 256);
    patched.push_str(&content[..open + 1]);
    patched.push('\n');
    patched.push_str(&entry_literal(entry));
    patched.push_str(&content[open + 1..]);
    Ok(patched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::openssh::{entry_id, Entry};

    fn entry(hostname: &str, key: &str) -> Entry {
        let host_names = vec![hostname.to_string()];
        let key_text = format!("ssh-ed25519 {key}");
        Entry {
            id: entry_id(&host_names, &key_text),
            host_names,
            key_type: "ssh-ed25519".to_string(),
            key: key.to_string(),
            key_text,
            comment: None,
            fingerprint: None,
            line: 1,
        }
    }

    #[test]
    fn inserts_into_empty_listing_and_keeps_comments() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Trust.pkl");
        std::fs::write(&path, DEFAULT_TRUST_PKL).unwrap();
        let entry = entry("git.example.test", "AAAA");
        let previous = patch_trust_pkl(&path, &entry).unwrap().unwrap();
        let patched = std::fs::read_to_string(&path).unwrap();
        assert!(patched.contains("// Fleet-level trust declarations."));
        assert!(patched.contains("new S.SshKnownHost {"));
        assert!(patched.contains("\"git.example.test\""));
        assert!(patched.contains("\"ssh-ed25519 AAAA\""));
        assert!(patched.contains("provenance = \"observed-known-hosts\""));
        assert!(patched.contains("}\n}\n"));
        assert_eq!(String::from_utf8(previous).unwrap(), DEFAULT_TRUST_PKL);
    }

    #[test]
    fn creates_default_file_when_missing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Trust.pkl");
        let previous = patch_trust_pkl(&path, &entry("host.example", "BBBB")).unwrap();
        assert!(previous.is_none());
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("\"ssh-ed25519 BBBB\""));
    }

    #[test]
    fn escapes_pkl_strings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Trust.pkl");
        let mut entry = entry("quoted\"name", "AAAA");
        entry.key = "a\"b\\c".to_string();
        entry.key_text = "ssh-ed25519 a\"b\\c".to_string();
        patch_trust_pkl(&path, &entry).unwrap();
        let patched = std::fs::read_to_string(&path).unwrap();
        assert!(patched.contains("\"quoted\\\"name\""));
    }

    #[test]
    fn missing_listing_is_rejected_without_touching_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Trust.pkl");
        std::fs::write(&path, "trust = new {}\n").unwrap();
        assert!(patch_trust_pkl(&path, &entry("host.example", "AAAA")).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "trust = new {}\n");
    }

    #[test]
    fn nested_listing_brace_matching_survives_strings_and_comments() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Trust.pkl");
        std::fs::write(
            &path,
            r#"import "Schema.pkl" as S

trust = new S.Trust {
  // comment with a closing brace }
  sshKnownHosts = new Listing<S.SshKnownHost> {
    new S.SshKnownHost {
      hostNames = new Listing { "already.example" }
      publicKeys = new Listing { "ssh-ed25519 AAAAOld" }
    }
  }
}
"#,
        )
        .unwrap();
        patch_trust_pkl(&path, &entry("new.example", "CCCC")).unwrap();
        let patched = std::fs::read_to_string(&path).unwrap();
        assert!(patched.contains("\"already.example\""));
        assert!(patched.contains("\"new.example\""));
        assert!(patched.trim_end().ends_with('}'));
    }

    #[test]
    fn restore_returns_file_to_previous_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Trust.pkl");
        let original = r#"trust = new S.Trust {
  sshKnownHosts = new Listing<S.SshKnownHost> {}
}
"#
        .to_string();
        std::fs::write(&path, &original).unwrap();
        let previous = patch_trust_pkl(&path, &entry("host.example", "AAAA")).unwrap();
        restore_trust_pkl(&path, previous.as_deref()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn restore_removes_file_created_by_patch() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Trust.pkl");
        let previous = patch_trust_pkl(&path, &entry("host.example", "AAAA")).unwrap();
        assert!(previous.is_none());
        assert!(path.exists());
        restore_trust_pkl(&path, None).unwrap();
        assert!(!path.exists());
    }
}
