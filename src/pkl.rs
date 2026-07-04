use miette::{IntoDiagnostic, Result};
use serde::de::DeserializeOwned;
use std::path::Path;

/// Evaluate a Pkl file and deserialize it into a serde data model.
pub async fn load<T>(path: &Path) -> Result<T>
where
    T: DeserializeOwned,
{
    if !path.exists() {
        return Err(miette::miette!("File not found: {}", path.display()));
    }

    let mut evaluator = pklr::Evaluator::new();
    evaluator.set_base_path(path.parent().unwrap_or_else(|| Path::new(".")));

    let value = evaluator
        .eval_file_pub(path)
        .await
        .map_err(|err| miette::miette!("Failed to evaluate '{}': {err}", path.display()))?;
    let json = value_to_json(&value);
    serde_json::from_str(&json).map_err(|err| {
        miette::miette!(
            "Failed to deserialize Pkl output from {}: {err}\n\nJSON was:\n{json}",
            path.display()
        )
    })
}

/// Synchronous wrapper around [`load`] for command-line and legacy callers.
pub fn load_sync<T>(path: &Path) -> Result<T>
where
    T: DeserializeOwned,
{
    let rt = tokio::runtime::Runtime::new().into_diagnostic()?;
    rt.block_on(load(path))
}

/// Render a string as a safe Pkl string literal.
pub fn string_literal(value: &str) -> Result<String> {
    serde_json::to_string(value).into_diagnostic()
}

/// Convert a pklr value to JSON so serde data models can be reused.
pub fn value_to_json(value: &pklr::Value) -> String {
    match value {
        pklr::Value::Null => "null".to_string(),
        pklr::Value::Bool(value) => value.to_string(),
        pklr::Value::Int(value) => value.to_string(),
        pklr::Value::Float(value) => {
            let rendered = format!("{value}");
            if !rendered.contains('.') && !rendered.contains('e') && !rendered.contains('E') {
                format!("{rendered}.0")
            } else {
                rendered
            }
        }
        pklr::Value::String(value) => {
            serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""))
        }
        pklr::Value::Object(map, _source) => {
            let mut out = "{".to_string();
            let mut first = true;
            for (key, value) in map.iter() {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&serde_json::to_string(key).unwrap_or_else(|_| format!("\"{key}\"")));
                out.push(':');
                out.push_str(&value_to_json(value));
            }
            out.push('}');
            out
        }
        pklr::Value::List(items) => {
            let mut out = "[".to_string();
            let mut first = true;
            for item in items {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&value_to_json(item));
            }
            out.push(']');
            out
        }
        pklr::Value::Lambda(..) => "\"<lambda>\"".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology;
    use serde::Deserialize;
    use std::fs;

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
        fs::write(
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

    #[test]
    fn string_literal_escapes_pkl_strings() {
        assert_eq!(
            string_literal("quote: \" newline:\n slash: \\").unwrap(),
            "\"quote: \\\" newline:\\n slash: \\\\\""
        );
    }

    #[tokio::test]
    async fn topology_loading_uses_shared_loader() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Topology.pkl");
        fs::write(
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
