#![cfg(feature = "pkl")]

use fleetix::{gpu, pkl};

#[test]
fn generated_gpu_contract_matches_its_pkl_producer() {
    let source =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("lib/topology/GpuContract.pkl");
    let generated: serde_json::Value = serde_json::from_str(gpu::CONTRACT_JSON).unwrap();
    let evaluated: serde_json::Value = pkl::load_sync(&source).unwrap();
    assert_eq!(
        evaluated, generated,
        "regenerate with pkl eval --format json lib/topology/GpuContract.pkl -o lib/generated/gpu-contract.json"
    );
}

#[test]
fn pkl_and_rust_accept_the_same_stable_node_identities() {
    let schema = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("lib/topology/Schema.pkl");
    let contract: serde_json::Value = serde_json::from_str(gpu::CONTRACT_JSON).unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("Node.pkl");
    for case in contract["cases"].as_array().unwrap() {
        let node = case["node"].as_str().unwrap();
        std::fs::write(
            &path,
            format!(
                "import {} as S\nvalue: S.StableRenderNode = {}\n",
                pkl::string_literal(&schema.to_string_lossy()),
                pkl::string_literal(node)
            ),
        )
        .unwrap();
        // Embedded evaluation supplies data; official Pkl enforces constraints.
        let evaluated = std::process::Command::new("pkl")
            .args(["eval", "--format", "json"])
            .arg(&path)
            .output()
            .expect("official pkl must be on PATH for GPU schema qualification");
        assert_eq!(
            evaluated.status.success(),
            gpu::pci_selector(node).is_some(),
            "{node}: {}",
            String::from_utf8_lossy(&evaluated.stderr)
        );
    }
}
