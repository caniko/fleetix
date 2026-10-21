#![cfg(feature = "pkl")]

use fleetix::pkl_to_nix::{CacheOptions, EvalOptions, WriteOutcome, write_with_cache_sync};

/// Exercise the synchronous API used by Canix, including the evaluator option
/// type shared with Fleetix's Pkl loader, without enabling either frontend.
#[test]
fn embedded_consumer_keeps_the_fleetix_api() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Policy.pkl");
    let output = dir.path().join("policy.nix");
    std::fs::write(&source, "value = 42\n").unwrap();
    let generate = || {
        write_with_cache_sync(
            &source,
            &output,
            EvalOptions::default(),
            CacheOptions::default(),
        )
    };
    assert_eq!(
        generate().unwrap(),
        WriteOutcome {
            cached: false,
            changed: true
        }
    );
    assert_eq!(
        generate().unwrap(),
        WriteOutcome {
            cached: false,
            changed: false
        }
    );
    assert!(
        std::fs::read_to_string(output)
            .unwrap()
            .contains("value = 42")
    );
}

#[test]
fn example_topology_renders_with_its_schema() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("examples")).unwrap();
    std::fs::create_dir_all(dir.path().join("lib/topology")).unwrap();
    std::fs::write(
        dir.path().join("examples/Topology.pkl"),
        include_str!("../examples/Topology.pkl"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("lib/topology/Schema.pkl"),
        include_str!("../lib/topology/Schema.pkl"),
    )
    .unwrap();
    let output = dir.path().join("topology.nix");
    write_with_cache_sync(
        &dir.path().join("examples/Topology.pkl"),
        &output,
        EvalOptions::default(),
        CacheOptions::default(),
    )
    .unwrap_or_else(|error| panic!("{error:#}"));
    let rendered = std::fs::read_to_string(output).unwrap();
    assert!(rendered.contains("schemaVersion = 2"));
    assert!(rendered.contains("git.example.test"));
}
