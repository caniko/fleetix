use miette::{IntoDiagnostic, Result};
use serde::de::DeserializeOwned;
use std::path::Path;

use pklx::pklr::EvalOptions;

/// Evaluate a Pkl file and deserialize it into a serde data model.
pub async fn load<T>(path: &Path) -> Result<T>
where
    T: DeserializeOwned,
{
    load_with_options(path, EvalOptions::default()).await
}

/// Evaluate a Pkl file with custom evaluator options and deserialize into a serde data model.
pub async fn load_with_options<T>(path: &Path, options: EvalOptions) -> Result<T>
where
    T: DeserializeOwned,
{
    pklx::eval_to_typed(path, options).await
}

/// Synchronous wrapper around [`load`] for command-line and legacy callers.
///
/// This function intentionally refuses to create a nested runtime. Callers
/// already inside Tokio must use [`load`] instead; returning an error is safer
/// than panicking in `Runtime::block_on`.
pub fn load_sync<T>(path: &Path) -> Result<T>
where
    T: DeserializeOwned,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(miette::miette!(
            "load_sync cannot run inside a Tokio runtime; use load(path).await"
        ));
    }
    let rt = tokio::runtime::Runtime::new().into_diagnostic()?;
    rt.block_on(load(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Fixture {
        name: String,
        items: Vec<String>,
        nested: Nested,
    }

    #[derive(Debug, Deserialize)]
    struct Nested {
        enabled: bool,
    }

    #[test]
    fn loads_generic_pkl_into_serde_model() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Fixture.pkl");
        std::fs::write(
            &path,
            r#"
name = "demo"
items = new Listing { "one"; "two" }
nested = new {
  enabled = true
}
"#,
        )
        .unwrap();

        let fixture: Fixture = load_sync(&path).unwrap();

        assert_eq!(fixture.name, "demo");
        assert_eq!(fixture.items, ["one", "two"]);
        assert!(fixture.nested.enabled);
    }

    #[tokio::test]
    async fn topology_loading_uses_shared_loader() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Topology.pkl");
        std::fs::write(
            &path,
            r#"
links = new {
  ["wg-mesh"] = new {
    subnet = "10.44.0.0/24"
    port = 51820
  }
}

hosts = new {
  ["edge-a"] = new {
    system = "x86_64-linux"
  }
}

domains = new {}
services = new {}
"#,
        )
        .unwrap();

        let topology = topology::load_topology(&path).await.unwrap();

        assert!(topology.hosts.contains_key("edge-a"));
        assert!(topology.links.contains_key("wg-mesh"));
    }
}
