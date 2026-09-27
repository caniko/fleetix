//! Generic Pkl-to-Nix sidecar generation, shared by Fleetix and its consumers.

use crate::{fsutil::atomic_write, topology};
use miette::{IntoDiagnostic, Result};
pub use pklx::pklr::EvalOptions;
use std::path::{Path, PathBuf};

mod cache;

/// Configure HTTP access for Pkl imports. Non-default HTTP settings bypass
/// the persistent cache because their response content may change remotely.
pub fn options_with_http(
    http_rewrites: Vec<String>,
    http_proxy: Option<&str>,
) -> Result<EvalOptions> {
    let mut options = EvalOptions {
        http_rewrites,
        ..EvalOptions::default()
    };
    if let Some(proxy_url) = http_proxy {
        let proxy = pklx::pklr::reqwest::Proxy::all(proxy_url)
            .map_err(|error| miette::miette!("invalid proxy URL '{proxy_url}': {error}"))?;
        options.client = Some(
            pklx::pklr::reqwest::Client::builder()
                .proxy(proxy)
                .build()
                .map_err(|error| miette::miette!("failed to build HTTP client: {error}"))?,
        );
    }
    Ok(options)
}

/// Cache policy for a sidecar write. Cache failures fall back to evaluation.
#[derive(Default)]
pub struct CacheOptions {
    /// Bypass the persistent cache when false (for example in validation checks).
    pub enabled: bool,
    /// Override the default `$XDG_CACHE_HOME/fleetix/pkl-to-nix` directory.
    pub directory: Option<PathBuf>,
}

impl CacheOptions {
    pub fn persistent() -> Self {
        Self {
            enabled: true,
            directory: None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct WriteOutcome {
    pub cached: bool,
    pub changed: bool,
}

/// Render a Pkl file as an importable Nix expression.
pub async fn render(path: &Path, options: EvalOptions) -> Result<String> {
    if path.file_name().and_then(|name| name.to_str()) == Some("Topology.aggregated.pkl") {
        let temporary = topology::flattened_tempfile(path)?;
        let nix = pklx::eval_pkl(temporary.path(), options).await?;
        return Ok(wrap_topology_nix(nix));
    }

    pklx::eval_pkl(path, options).await
}

/// Render and write a sidecar atomically. An unchanged output is left untouched.
pub async fn write(path: &Path, output: &Path, options: EvalOptions) -> Result<WriteOutcome> {
    write_with_cache(path, output, options, CacheOptions::persistent()).await
}

pub async fn write_with_cache(
    path: &Path,
    output: &Path,
    options: EvalOptions,
    cache_options: CacheOptions,
) -> Result<WriteOutcome> {
    let flattened = (path.file_name().and_then(|name| name.to_str())
        == Some("Topology.aggregated.pkl"))
    .then(|| topology::flatten_modular_topology(path))
    .transpose()?;
    let flattened_hash = flattened
        .as_ref()
        .map(|source| cache::digest(source.as_bytes()));
    let cache_location =
        if cache_options.enabled && options.client.is_none() && options.http_rewrites.is_empty() {
            cache_options
                .directory
                .or_else(cache::default_dir)
                .and_then(|dir| cache::key(path).ok().map(|key| (dir, key)))
        } else {
            None
        };
    if let Some((dir, key)) = &cache_location {
        if let Some(nix) = cache::get(dir, key, flattened_hash.as_deref()).await {
            return Ok(WriteOutcome {
                cached: true,
                changed: write_rendered(output, &nix)?,
            });
        }
    }

    let temporary = flattened
        .as_deref()
        .map(|source| topology::tempfile_from_flattened(path, source))
        .transpose()?;
    let eval_path = temporary.as_ref().map_or(path, |file| file.path());
    let (nix, mut snapshot) = cache::evaluate(eval_path, options).await?;
    if let Some(temporary) = &temporary {
        snapshot.omit_temporary(temporary.path());
    }
    let nix = if temporary.is_some() {
        wrap_topology_nix(nix)
    } else {
        nix
    };
    // Never publish a cache entry for a failed sidecar write.
    let changed = write_rendered(output, &nix)?;
    if let Some((dir, key)) = &cache_location {
        cache::put(dir, key, flattened_hash, snapshot, &nix);
    }
    Ok(WriteOutcome {
        cached: false,
        changed,
    })
}

/// Synchronous entry point for embedding in command-line tools.
pub fn write_sync(path: &Path, output: &Path, options: EvalOptions) -> Result<WriteOutcome> {
    write_with_cache_sync(path, output, options, CacheOptions::persistent())
}

/// Synchronous sidecar generation with explicit cache policy.
pub fn write_with_cache_sync(
    path: &Path,
    output: &Path,
    options: EvalOptions,
    cache_options: CacheOptions,
) -> Result<WriteOutcome> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(miette::miette!(
            "synchronous Pkl generation cannot run inside a Tokio runtime; use write_with_cache(...).await"
        ));
    }
    let runtime = tokio::runtime::Runtime::new().into_diagnostic()?;
    runtime.block_on(write_with_cache(path, output, options, cache_options))
}

fn write_rendered(output: &Path, nix: &str) -> Result<bool> {
    let contents = format!("# Generated by fleetix; do not edit by hand.\n{nix}");
    if std::fs::read(output).is_ok_and(|current| current == contents.as_bytes()) {
        return Ok(false);
    }
    atomic_write(output, contents.as_bytes())?;
    Ok(true)
}

fn wrap_topology_nix(nix: String) -> String {
    format!(
        "let\n  scrub = value:\n    if builtins.isAttrs value then\n      builtins.listToAttrs (\n        builtins.filter (entry: entry.value != null) (\n          builtins.map (name: {{ inherit name; value = scrub value.${{name}}; }})\n            (builtins.attrNames (builtins.removeAttrs value [\"__pkl_class\"]))\n        )\n      )\n    else if builtins.isList value then builtins.map scrub value\n    else value;\nin\n  scrub (\n{nix}\n  )\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn cached(dir: &Path) -> CacheOptions {
        CacheOptions {
            enabled: true,
            directory: Some(dir.to_path_buf()),
        }
    }

    #[tokio::test]
    async fn local_imports_invalidate_persistent_cache() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("Main.pkl");
        let dependency = dir.path().join("Value.pkl");
        let output = dir.path().join("generated.nix");
        let cache_dir = dir.path().join("cache");
        fs::write(&source, "import \"Value.pkl\" as V\nvalue = V.value\n").unwrap();
        fs::write(&dependency, "value = 1\n").unwrap();

        let generate =
            || write_with_cache(&source, &output, EvalOptions::default(), cached(&cache_dir));
        assert_eq!(
            generate().await.unwrap(),
            WriteOutcome {
                cached: false,
                changed: true
            }
        );
        assert_eq!(
            generate().await.unwrap(),
            WriteOutcome {
                cached: true,
                changed: false
            }
        );

        fs::write(&dependency, "value = 2\n").unwrap();
        assert_eq!(
            generate().await.unwrap(),
            WriteOutcome {
                cached: false,
                changed: true
            }
        );
        assert!(fs::read_to_string(&output).unwrap().contains("value = 2"));
        assert_eq!(
            generate().await.unwrap(),
            WriteOutcome {
                cached: true,
                changed: false
            }
        );

        fs::write(&output, "corrupted").unwrap();
        assert_eq!(
            generate().await.unwrap(),
            WriteOutcome {
                cached: true,
                changed: true
            }
        );
        assert!(fs::read_to_string(&output).unwrap().contains("value = 2"));

        assert_eq!(
            write_with_cache(
                &source,
                &output,
                EvalOptions::default(),
                CacheOptions::default()
            )
            .await
            .unwrap(),
            WriteOutcome {
                cached: false,
                changed: false
            }
        );
    }

    #[tokio::test]
    async fn aggregate_cache_tracks_flattened_imports() {
        let dir = tempfile::tempdir().unwrap();
        let topology = dir.path().join("topology");
        fs::create_dir_all(topology.join("hosts")).unwrap();
        fs::create_dir_all(dir.path().join("shared")).unwrap();
        fs::write(
            topology.join("Schema.pkl"),
            "class Host { system: String }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("shared/Names.pkl"),
            "names = new { site = \"first\" }\n",
        )
        .unwrap();
        fs::write(topology.join("hosts/Hub.pkl"), "import \"../Schema.pkl\" as S\nhosts = new { hub = new S.Host { system = \"x86_64-linux\" } }\n").unwrap();
        fs::write(
            topology.join("Domains.pkl"),
            "import \"../shared/Names.pkl\" as N\ndomains = new { site = N.names.site }\n",
        )
        .unwrap();
        fs::write(topology.join("Services.pkl"), "services = new {}\n").unwrap();
        let source = topology.join("Topology.aggregated.pkl");
        fs::write(
            &source,
            "schemaVersion = 2\nhosts = new { hub = (import(\"hosts/Hub.pkl\")).hosts.hub }\n",
        )
        .unwrap();
        let output = dir.path().join("topology.nix");
        let cache_dir = dir.path().join("cache");

        let generate =
            || write_with_cache(&source, &output, EvalOptions::default(), cached(&cache_dir));
        assert_eq!(
            generate().await.unwrap(),
            WriteOutcome {
                cached: false,
                changed: true
            }
        );
        assert_eq!(
            generate().await.unwrap(),
            WriteOutcome {
                cached: true,
                changed: false
            }
        );
        fs::write(
            dir.path().join("shared/Names.pkl"),
            "names = new { site = \"second\" }\n",
        )
        .unwrap();
        assert_eq!(
            generate().await.unwrap(),
            WriteOutcome {
                cached: false,
                changed: true
            }
        );
        assert!(fs::read_to_string(&output).unwrap().contains("second"));
    }

    #[tokio::test]
    async fn environment_reads_are_never_cached() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("Env.pkl");
        let output = dir.path().join("env.nix");
        fs::write(&source, "home = read(\"env:HOME\")\n").unwrap();
        for _ in 0..2 {
            assert!(
                !write_with_cache(
                    &source,
                    &output,
                    EvalOptions::default(),
                    cached(&dir.path().join("cache")),
                )
                .await
                .unwrap()
                .cached
            );
        }
    }
}
