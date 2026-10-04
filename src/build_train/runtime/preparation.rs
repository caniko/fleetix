//! Exact-identity, process-shared preparation receipts. Waits own no evaluation
//! lease; a caller acquires the phase-local lease only inside the supplied work.
use super::private_dir;
use fs2::FileExt;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

/// Key components are evidence identities, never human package labels.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Key {
    Evaluation {
        source: String,
        inputs: String,
        attribute: String,
        evaluator: String,
        settings: BTreeMap<String, String>,
    },
    Cargo {
        source: String,
        lockfile: String,
        toolchain: String,
        target: String,
        features: Vec<String>,
        command: Vec<String>,
        environment: String,
    },
}

/// Only successful checks/evaluations can be reused. Skips have no pass receipt.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Evidence<T> {
    Passed(T),
    Skipped(String),
}

/// Serialize one exact preparation task, and atomically retain only a passed result.
pub fn run<T: Serialize + DeserializeOwned>(
    directory: &Path,
    key: &Key,
    wait: Duration,
    work: impl FnOnce() -> Result<Evidence<T>, String>,
) -> Result<Evidence<T>, String> {
    run_checked(directory, key, wait, |_| true, work)
}

/// Revalidate cached artifacts under the task lease before reusing their receipt.
pub fn run_checked<T: Serialize + DeserializeOwned>(
    directory: &Path,
    key: &Key,
    wait: Duration,
    valid: impl Fn(&T) -> bool,
    work: impl FnOnce() -> Result<Evidence<T>, String>,
) -> Result<Evidence<T>, String> {
    private_dir(directory)?;
    let identity = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(key).map_err(|e| e.to_string())?)
    );
    let anchor = directory.join(format!("{identity}.lock"));
    let lease = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(anchor)
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + wait;
    loop {
        match lease.try_lock_exclusive() {
            Ok(()) => break,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(format!("preparation lease unavailable: {error}")),
        }
    }
    let receipt = directory.join(format!("{identity}.json"));
    match File::open(&receipt) {
        Ok(file) => {
            let cached: Evidence<T> = serde_json::from_reader(file)
                .map_err(|e| format!("invalid preparation receipt: {e}"))?;
            match cached {
                Evidence::Passed(value) if valid(&value) => return Ok(Evidence::Passed(value)),
                Evidence::Passed(_) => {}
                Evidence::Skipped(_) => return Err("invalid non-pass preparation receipt".into()),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    let result = work()?;
    if matches!(result, Evidence::Passed(_)) {
        crate::fsutil::atomic_write(
            &receipt,
            &serde_json::to_vec(&result).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(result)
}
