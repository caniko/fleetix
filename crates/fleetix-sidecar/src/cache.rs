use crate::fsutil::atomic_write;
use miette::{IntoDiagnostic, WrapErr};
use pklx::pklr::{self, EvalCapabilities, NativeCapabilities};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// Bump when the receipt schema changes; the producer covers renderer upgrades.
const FORMAT: u32 = 2;

// The sidecar library is also embedded in downstream CLIs. Fingerprint the running
// producer once, so renderer/pklr upgrades invalidate receipts even when the
// crate version or source file names have not changed.
fn producer() -> Option<&'static str> {
    static PRODUCER: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    PRODUCER
        .get_or_init(|| {
            #[cfg(target_os = "linux")]
            let executable = PathBuf::from("/proc/self/exe");
            #[cfg(not(target_os = "linux"))]
            let executable = std::env::current_exe().ok()?;
            Some(digest(&std::fs::read(executable).ok()?))
        })
        .as_deref()
}

pub(super) fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(super) fn default_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .map(|path| path.join("fleetix/pkl-to-nix"))
}

pub(super) fn key(path: &Path) -> miette::Result<String> {
    let path = path
        .canonicalize()
        .into_diagnostic()
        .wrap_err_with(|| format!("resolve Pkl source {}", path.display()))?;
    Ok(digest(path.as_os_str().as_encoded_bytes()))
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

#[derive(Default, Serialize, Deserialize)]
pub(super) struct Snapshot {
    files: BTreeMap<PathBuf, String>,
    existence: BTreeMap<PathBuf, bool>,
    canonical: BTreeMap<PathBuf, PathBuf>,
    globs: Vec<Glob>,
    #[serde(skip)]
    volatile: bool,
}

impl Snapshot {
    async fn is_current(&self) -> bool {
        for (path, hash) in &self.files {
            if !std::fs::read(path).is_ok_and(|bytes| digest(&bytes) == *hash) {
                return false;
            }
        }
        for (path, was_present) in &self.existence {
            if path.exists() != *was_present {
                return false;
            }
        }
        for (path, resolved) in &self.canonical {
            if !path
                .canonicalize()
                .is_ok_and(|current| current == *resolved)
            {
                return false;
            }
        }
        let mut native = NativeCapabilities::new();
        for glob in &self.globs {
            let Ok(paths) = native.glob(&glob.base, &glob.pattern).await else {
                return false;
            };
            let mut paths: Vec<_> = paths.iter().map(|path| absolute(path)).collect();
            paths.sort();
            if paths != glob.paths {
                return false;
            }
        }
        true
    }
}

#[derive(Serialize, Deserialize)]
struct Glob {
    base: PathBuf,
    pattern: String,
    paths: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    format: u32,
    producer: String,
    flattened_hash: Option<String>,
    snapshot: Snapshot,
    nix: String,
    nix_hash: String,
}

pub(super) async fn get(dir: &Path, key: &str, flattened_hash: Option<&str>) -> Option<String> {
    let entry: Entry =
        serde_json::from_slice(&std::fs::read(dir.join(format!("{key}.json"))).ok()?).ok()?;
    if entry.format != FORMAT
        || Some(entry.producer.as_str()) != producer()
        || entry.flattened_hash.as_deref() != flattened_hash
        || entry.snapshot.volatile
        || digest(entry.nix.as_bytes()) != entry.nix_hash
        || !entry.snapshot.is_current().await
    {
        return None;
    }
    Some(entry.nix)
}

pub(super) fn put(
    dir: &Path,
    key: &str,
    flattened_hash: Option<String>,
    snapshot: Snapshot,
    nix: &str,
) {
    if snapshot.volatile || ensure_private_dir(dir).is_err() {
        return;
    }
    let Some(producer) = producer() else {
        return;
    };
    let entry = Entry {
        format: FORMAT,
        producer: producer.to_owned(),
        flattened_hash,
        snapshot,
        nix: nix.to_owned(),
        nix_hash: digest(nix.as_bytes()),
    };
    if let Ok(contents) = serde_json::to_vec(&entry) {
        let path = dir.join(format!("{key}.json"));
        if atomic_write(&path, &contents).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
}

fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    if std::fs::symlink_metadata(dir)?.file_type().is_symlink() {
        return Err(std::io::Error::other("cache directory is a symlink"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Evaluate with a capability wrapper that records *actual* reads, including
/// expression imports that pklr's static import scanner does not see.
pub(super) async fn evaluate(
    path: &Path,
    options: pklr::EvalOptions,
) -> miette::Result<(String, Snapshot)> {
    let observed = Arc::new(Mutex::new(Snapshot::default()));
    let capabilities = TracingCapabilities {
        native: NativeCapabilities::new(),
        observed: Arc::clone(&observed),
    };
    let mut evaluator = pklr::Evaluator::with_capabilities(capabilities);
    evaluator.set_base_path(path.parent().unwrap_or_else(|| Path::new(".")));
    if let Some(client) = options.client {
        evaluator.set_http_client(client);
    }
    if !options.http_rewrites.is_empty() {
        evaluator.set_http_rewrites(&options.http_rewrites);
    }
    let value = evaluator
        .eval_file_pub(path)
        .await
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to evaluate '{}'", path.display()))?;
    let snapshot = std::mem::take(&mut *observed.lock().unwrap());
    Ok((pklx::pkl_value_to_nix(&value), snapshot))
}

struct TracingCapabilities {
    native: NativeCapabilities,
    observed: Arc<Mutex<Snapshot>>,
}

impl EvalCapabilities for TracingCapabilities {
    fn read_to_string<'a>(
        &'a mut self,
        path: &'a Path,
    ) -> pklr::capabilities::BoxFuture<'a, pklr::Result<String>> {
        Box::pin(async move {
            let result = self.native.read_to_string(path).await;
            let mut snapshot = self.observed.lock().unwrap();
            match &result {
                Ok(text) => {
                    snapshot
                        .files
                        .insert(absolute(path), digest(text.as_bytes()));
                }
                Err(_) => snapshot.volatile = true,
            }
            result
        })
    }

    fn path_exists<'a>(
        &'a mut self,
        path: &'a Path,
    ) -> pklr::capabilities::BoxFuture<'a, pklr::Result<bool>> {
        Box::pin(async move {
            let result = self.native.path_exists(path).await;
            if let Ok(exists) = &result {
                self.observed
                    .lock()
                    .unwrap()
                    .existence
                    .insert(absolute(path), *exists);
            }
            result
        })
    }

    fn canonicalize<'a>(
        &'a mut self,
        path: &'a Path,
    ) -> pklr::capabilities::BoxFuture<'a, pklr::Result<PathBuf>> {
        Box::pin(async move {
            let result = self.native.canonicalize(path).await;
            let mut snapshot = self.observed.lock().unwrap();
            match &result {
                Ok(resolved) => {
                    snapshot.canonical.insert(absolute(path), resolved.clone());
                }
                Err(_) => snapshot.volatile = true,
            }
            result
        })
    }

    fn read_env<'a>(
        &'a mut self,
        name: &'a str,
    ) -> pklr::capabilities::BoxFuture<'a, pklr::Result<Option<String>>> {
        self.observed.lock().unwrap().volatile = true;
        self.native.read_env(name)
    }

    fn fetch_text<'a>(
        &'a mut self,
        url: &'a str,
    ) -> pklr::capabilities::BoxFuture<'a, pklr::Result<String>> {
        self.observed.lock().unwrap().volatile = true;
        self.native.fetch_text(url)
    }

    fn fetch_bytes<'a>(
        &'a mut self,
        url: &'a str,
    ) -> pklr::capabilities::BoxFuture<'a, pklr::Result<Vec<u8>>> {
        self.observed.lock().unwrap().volatile = true;
        self.native.fetch_bytes(url)
    }

    fn set_http_client(&mut self, client: pklr::reqwest::Client) {
        self.native.set_http_client(client);
    }

    fn temp_dir<'a>(
        &'a mut self,
        prefix: &'a str,
    ) -> pklr::capabilities::BoxFuture<'a, pklr::Result<PathBuf>> {
        self.observed.lock().unwrap().volatile = true;
        self.native.temp_dir(prefix)
    }

    fn glob<'a>(
        &'a mut self,
        base: &'a Path,
        pattern: &'a str,
    ) -> pklr::capabilities::BoxFuture<'a, pklr::Result<Vec<PathBuf>>> {
        Box::pin(async move {
            let result = self.native.glob(base, pattern).await;
            if let Ok(paths) = &result {
                let mut sorted: Vec<_> = paths.iter().map(|path| absolute(path)).collect();
                sorted.sort();
                self.observed.lock().unwrap().globs.push(Glob {
                    base: absolute(base),
                    pattern: pattern.to_owned(),
                    paths: sorted,
                });
            }
            result
        })
    }
}
