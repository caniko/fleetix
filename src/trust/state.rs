// Persistent observer decisions.
//
// known_hosts and Trust.pkl are the sources of truth; this file only records
// *decisions* (suppressed proposal ids) so a dismissed or ignored proposal is
// not re-prompted on every scan. The first-run baseline with
// reviewExisting = false also lands here.

use crate::fsutil::atomic_write;
use miette::{miette, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const STATE_VERSION: u16 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub version: u16,
    pub suppressed: BTreeSet<String>,
}

impl State {
    /// Load the decisions file, or `None` when no decisions exist yet.
    pub fn load(state_dir: &Path) -> Result<Option<Self>> {
        let path = state_dir.join("state.json");
        let Ok(content) = fs::read_to_string(&path) else {
            return Ok(None);
        };
        let state: State = serde_json::from_str(&content)
            .map_err(|error| miette!("parse {}: {error}", path.display()))?;
        if state.version != STATE_VERSION {
            return Err(miette!(
                "{} uses unsupported state version {} (expected {STATE_VERSION})",
                path.display(),
                state.version
            ));
        }
        Ok(Some(state))
    }

    pub fn new() -> Self {
        Self {
            version: STATE_VERSION,
            suppressed: BTreeSet::new(),
        }
    }

    pub fn is_suppressed(&self, id: &str) -> bool {
        self.suppressed.contains(id)
    }

    pub fn suppress(&mut self, id: &str) {
        self.suppressed.insert(id.to_string());
    }

    pub fn save(&self, state_dir: &Path) -> Result<()> {
        let path = state_dir.join("state.json");
        let contents = serde_json::to_string_pretty(self)
            .map_err(|error| miette!("serialize state: {error}"))?;
        atomic_write(&path, contents.as_bytes())
    }
}

/// Resolve the observer state directory: `$XDG_STATE_HOME/fleetix/trust` or
/// `$HOME/.local/state/fleetix/trust`.
pub fn default_state_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from(".local/state"));
    base.join("fleetix/trust")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_state_loads_as_none() {
        let directory = tempfile::tempdir().unwrap();
        assert!(State::load(directory.path()).unwrap().is_none());
    }

    #[test]
    fn suppress_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = State::new();
        state.suppress("abc");
        state.save(directory.path()).unwrap();
        let loaded = State::load(directory.path()).unwrap().unwrap();
        assert!(loaded.is_suppressed("abc"));
        assert!(!loaded.is_suppressed("def"));
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("state.json"),
            r#"{"version": 99, "suppressed": []}"#,
        )
        .unwrap();
        assert!(State::load(directory.path()).is_err());
    }

    #[test]
    fn save_is_atomic_and_parseable() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = State::new();
        state.suppress("one");
        state.save(directory.path()).unwrap();
        state.save(directory.path()).unwrap();
        let raw = fs::read_to_string(directory.path().join("state.json")).unwrap();
        assert!(raw.contains("one"));
    }
}
