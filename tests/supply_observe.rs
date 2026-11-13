use fleetix::supply::{self, CONTRACT, FactsDocument, ObservedRepo};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/supply")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn git(directory: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .env("GIT_AUTHOR_NAME", "fleetix test")
        .env("GIT_AUTHOR_EMAIL", "fleetix-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "fleetix test")
        .env("GIT_COMMITTER_EMAIL", "fleetix-test@example.invalid")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap_or_else(|error| panic!("git {arguments:?}: {error}"));
    assert!(status.success(), "git {arguments:?}: {status}");
}

/// A throwaway repository with one commit. The identity env belongs to this
/// test process only; it never touches a real repository's configuration.
fn init_repo(root: &Path, relative: &str, files: &[(&str, &str)]) -> PathBuf {
    let directory = root.join(relative);
    std::fs::create_dir_all(&directory).unwrap();
    git(&directory, &["init", "-q", "-b", "main"]);
    for (name, contents) in files {
        std::fs::write(directory.join(name), contents).unwrap();
    }
    git(&directory, &["add", "-A"]);
    git(
        &directory,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "fixture",
        ],
    );
    directory
}

fn repo(coordinate: &str, relative_path: &str) -> ObservedRepo {
    ObservedRepo {
        coordinate: coordinate.into(),
        relative_path: PathBuf::from(relative_path),
        default_branch: None,
    }
}

fn document(value: Value) -> FactsDocument {
    let mut value = value;
    value["contract"] = json!(CONTRACT);
    value["vocabulary"] = json!("test.v1");
    value["producer"] = json!({"name": "test", "version": "0"});
    value["observedAt"] = json!("2026-11-09T00:00:00Z");
    serde_json::from_value(value).unwrap()
}

fn usage_node<'a>(doc: &'a FactsDocument, id: &str) -> &'a supply::Node {
    doc.nodes
        .iter()
        .find(|node| node.id == id)
        .unwrap_or_else(|| panic!("no node {id}"))
}

fn text<'a>(attrs: &'a supply::Attrs, key: &str) -> Option<&'a str> {
    attrs.get(key).and_then(Value::as_str)
}

fn bool(attrs: &supply::Attrs, key: &str) -> Option<bool> {
    attrs.get(key).and_then(Value::as_bool)
}

fn flake_lock() -> String {
    json!({
        "nodes": {
            "root": {"inputs": {
                "harbor-rs": "harbor-rs",
                "harbor-py": "harbor-py_2",
                "harbor-meta": ["scp"],
                "follows": ["harbor-rs", "nixpkgs"],
                "path-input": "path-input"
            }},
            "harbor-rs": {
                "locked": {"type": "github", "owner": "caniko", "repo": "harbor-rs", "rev": "aaaa"},
                "original": {"type": "github", "owner": "caniko", "repo": "harbor-rs"}
            },
            "harbor-rs_2": {
                "locked": {"type": "github", "owner": "caniko", "repo": "harbor-rs", "rev": "bbbb"},
                "original": {"type": "github", "owner": "caniko", "repo": "harbor-rs"}
            },
            "harbor-py_2": {
                "locked": {"type": "git", "url": "https://github.com/caniko/harbor-py.git", "rev": "cccc"},
                "original": {"type": "git", "url": "https://github.com/caniko/harbor-py.git?ref=trunk"}
            },
            "harbor-py": {
                "locked": {"type": "git", "url": "https://github.com/caniko/harbor-py.git", "rev": "ffff"},
                "original": {"type": "git", "url": "https://github.com/caniko/harbor-py.git"}
            },
            "codeberg": {
                "locked": {"type": "git", "url": "ssh://git@codeberg.org/caniko/rs-harbor.git", "rev": "dddd"},
                "original": {"type": "git", "url": "git+ssh://git@codeberg.org/caniko/rs-harbor"}
            },
            "scp": {
                "locked": {"type": "git", "url": "git@codeberg.org:caniko/harbor-meta.git", "rev": "eeee"},
                "original": {"type": "git", "url": "git@codeberg.org:caniko/harbor-meta.git"}
            },
            "path-input": {
                "locked": {"type": "path", "path": "./local"},
                "original": {"type": "path", "path": "./local"}
            }
        },
        "root": "root",
        "version": 7
    })
    .to_string()
}

fn cargo_lock() -> String {
    r#"version = 4

[[package]]
name = "app"
version = "0.1.0"

[[package]]
name = "harbor-rs"
version = "0.1.0"
source = "git+https://github.com/caniko/harbor-rs.git?rev=aaaa#aaaa"

[[package]]
name = "harbor-rs"
version = "0.2.0"
source = "git+ssh://git@codeberg.org/caniko/rs-harbor.git?branch=main#bbbb"

[[package]]
name = "registry"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[patch.unused]]
name = "unused-patch"
version = "0.1.0"
source = "git+https://github.com/caniko/unused-patch.git#ffff"
"#
    .to_string()
}

#[test]
fn observe_reads_https_ssh_scp_and_github_locks_from_the_committed_tip() {
    let root = tempfile::tempdir().unwrap();
    init_repo(
        root.path(),
        "app",
        &[("flake.lock", &flake_lock()), ("Cargo.lock", &cargo_lock())],
    );

    let observed = supply::observe(root.path(), &[repo("github.com/caniko/app", "app")]);
    assert!(observed.errors.is_empty(), "{:?}", observed.errors);
    assert_eq!(observed.producer.name, "fleetix");
    assert_eq!(observed.vocabulary, "fleetix.lock-facts.v1");
    assert_eq!(observed.usage_kinds, ["flake-input", "cargo-git"]);

    let repository = observed
        .nodes
        .iter()
        .find(|node| node.id == "repo:github.com/caniko/app")
        .unwrap();
    assert_eq!(
        text(&repository.attrs, "relativePath"),
        Some("app"),
        "the repository records its relative path"
    );

    let direct = usage_node(
        &observed,
        "usage:github.com/caniko/app#flake-input:harbor-rs",
    );
    assert_eq!(text(&direct.attrs, "usageKind"), Some("flake-input"));
    assert_eq!(text(&direct.attrs, "path"), Some("flake.lock"));
    assert_eq!(text(&direct.attrs, "input"), Some("harbor-rs"));
    assert_eq!(bool(&direct.attrs, "direct"), Some(true));
    assert_eq!(text(&direct.attrs, "lockedRev"), Some("aaaa"));
    assert_eq!(text(&direct.attrs, "state"), Some("unknown"));

    // Transitive pins are keyed by producer: lock node keys renumber.
    let transitive = usage_node(
        &observed,
        "usage:github.com/caniko/app#flake-input:transitive:github.com/caniko/harbor-rs",
    );
    assert_eq!(text(&transitive.attrs, "input"), Some("harbor-rs"));
    assert_eq!(bool(&transitive.attrs, "direct"), Some(false));

    let github_url = usage_node(
        &observed,
        "usage:github.com/caniko/app#flake-input:harbor-py",
    );
    assert_eq!(
        text(&github_url.attrs, "input"),
        Some("harbor-py"),
        "a direct node is named by its root input, not its lock key"
    );
    assert_eq!(bool(&github_url.attrs, "direct"), Some(true));
    assert_eq!(
        text(&github_url.attrs, "lockedRev"),
        Some("cccc"),
        "a transitive node named like a root input keeps its own usage"
    );
    let pins_of = |id: &str| -> Vec<String> {
        observed
            .edges
            .iter()
            .filter(|edge| edge.from == id && edge.kind == "pins")
            .map(|edge| edge.to.clone())
            .collect()
    };
    assert_eq!(
        pins_of(&github_url.id),
        ["rev:github.com/caniko/harbor-py@cccc"]
    );
    assert_eq!(
        pins_of("usage:github.com/caniko/app#flake-input:transitive:github.com/caniko/harbor-py"),
        ["rev:github.com/caniko/harbor-py@ffff"]
    );

    assert!(observed.nodes.iter().any(|node| {
        node.id
            == "usage:github.com/caniko/app#flake-input:transitive:codeberg.org/caniko/rs-harbor"
    }));
    let one_element = usage_node(
        &observed,
        "usage:github.com/caniko/app#flake-input:harbor-meta",
    );
    assert_eq!(
        bool(&one_element.attrs, "direct"),
        Some(true),
        "a one-element root input array is still a direct input"
    );
    assert!(
        !observed
            .nodes
            .iter()
            .any(|node| node.id.contains("path-input")),
        "a path input is never a producer"
    );
    assert!(
        !observed
            .nodes
            .iter()
            .any(|node| node.id.contains("#flake-input:follows")),
        "a follows entry is not a lock node"
    );

    let pins: Vec<_> = observed
        .edges
        .iter()
        .filter(|edge| edge.kind == "pins")
        .map(|edge| edge.to.as_str())
        .collect();
    assert!(
        pins.contains(&"rev:codeberg.org/caniko/rs-harbor@dddd"),
        "{pins:?}"
    );
    assert!(
        pins.contains(&"rev:codeberg.org/caniko/harbor-meta@eeee"),
        "{pins:?}"
    );
    assert!(
        pins.contains(&"rev:github.com/caniko/harbor-py@cccc"),
        "{pins:?}"
    );

    let cargo = usage_node(&observed, "usage:github.com/caniko/app#cargo-git:harbor-rs");
    assert_eq!(text(&cargo.attrs, "usageKind"), Some("cargo-git"));
    assert_eq!(text(&cargo.attrs, "path"), Some("Cargo.lock"));
    assert_eq!(
        cargo.attrs.get("lockedRev"),
        Some(&Value::Null),
        "two revisions cannot be one attribute"
    );
    let cargo_pins: Vec<_> = observed
        .edges
        .iter()
        .filter(|edge| edge.from == cargo.id && edge.kind == "pins")
        .map(|edge| edge.to.as_str())
        .collect();
    assert_eq!(
        cargo_pins,
        [
            "rev:codeberg.org/caniko/rs-harbor@bbbb",
            "rev:github.com/caniko/harbor-rs@aaaa"
        ]
    );
    assert!(
        !observed
            .nodes
            .iter()
            .any(|node| node.id.contains("cargo-git:registry")
                || node.id.contains("cargo-git:unused-patch")),
        "registry and unused patch sources are not consumed git producers"
    );

    // The merge keeps one usage per lock locator and both revision pins.
    let graph = supply::build(&[observed], None);
    assert!(graph.nodes.contains_key("repo:github.com/caniko/app"));
    assert!(
        graph
            .edges
            .iter()
            .any(|edge| edge.to == "rev:github.com/caniko/harbor-rs@aaaa"),
        "the direct and duplicate lock nodes keep distinct revisions"
    );
    assert!(supply::walk(&graph, "rev:github.com/caniko/harbor-rs@aaaa", true, 1).is_ok());
}

#[test]
fn aliases_fold_renamed_repositories_and_mirrors_into_one_producer() {
    let root = tempfile::tempdir().unwrap();
    init_repo(
        root.path(),
        "app",
        &[(
            "flake.lock",
            &json!({
                "nodes": {
                    "root": {"inputs": {"harbor-rs": "harbor-rs"}},
                    "harbor-rs": {
                        "locked": {"type": "github", "owner": "caniko", "repo": "harbor-rs", "rev": "aaaa"},
                        "original": {"type": "github", "owner": "caniko", "repo": "harbor-rs"}
                    }
                },
                "root": "root",
                "version": 7
            })
            .to_string(),
        )],
    );
    init_repo(
        root.path(),
        "mirror-app",
        &[(
            "flake.lock",
            &json!({
                "nodes": {
                    "root": {"inputs": {"harbor-rs": "harbor-rs"}},
                    "harbor-rs": {
                        "locked": {"type": "git", "url": "ssh://git@codeberg.org/caniko/rs-harbor.git", "rev": "bbbb"},
                        "original": {"type": "git", "url": "ssh://git@codeberg.org/caniko/rs-harbor.git"}
                    }
                },
                "root": "root",
                "version": 7
            })
            .to_string(),
        )],
    );
    let observed = supply::observe(
        root.path(),
        &[
            repo("github.com/caniko/app", "app"),
            repo("codeberg.org/caniko/mirror-app", "mirror-app"),
        ],
    );
    let producer = supply::parse(
        &json!({
            "contract": CONTRACT,
            "vocabulary": "harbor.facts.v1",
            "producer": {"name": "harbor", "version": "1"},
            "observedAt": "2026-11-09T00:00:00Z",
            "nodes": [{
                "id": "producer:github.com/caniko/harbor-rs",
                "kind": "producer",
                "attrs": {
                    "aliases": ["codeberg.org/caniko/rs-harbor", "github.com/caniko/rs-harbor"],
                    "defaultBranch": "trunk"
                }
            }]
        })
        .to_string(),
    )
    .unwrap();

    let graph = supply::build(&[observed, producer[0].clone()], None);
    let endpoints = graph
        .edges
        .iter()
        .flat_map(|edge| [edge.from.as_str(), edge.to.as_str()]);
    for revision in [
        "rev:github.com/caniko/harbor-rs@aaaa",
        "rev:github.com/caniko/harbor-rs@bbbb",
    ] {
        assert!(
            endpoints.clone().any(|endpoint| endpoint == revision),
            "{revision}"
        );
    }
    assert!(
        !graph
            .nodes
            .keys()
            .map(String::as_str)
            .chain(endpoints)
            .any(|id| id.contains("codeberg.org/caniko/rs-harbor")),
        "aliases are folded before merging: {:?}",
        graph.nodes.keys()
    );
    assert!(graph.conflicts.is_empty(), "{:?}", graph.conflicts);

    // A query by either alias resolves to the canonical producer.
    for alias in [
        "github.com/caniko/harbor-rs",
        "codeberg.org/caniko/rs-harbor",
        "github.com/caniko/rs-harbor",
    ] {
        let report = supply::consumers(&graph, alias, Some("cccc"));
        assert_eq!(report.producer, "github.com/caniko/harbor-rs");
        assert_eq!(report.items.len(), 2, "{alias}: {:?}", report.items);
        assert!(report.items.iter().all(|item| item.behind), "{alias}");
    }
}

#[test]
fn consumers_report_relative_paths_revisions_and_behind() {
    let graph = supply::build(
        &[document(json!({
            "nodes": [
                {"id": "repo:github.com/caniko/app", "kind": "repo", "attrs": {"relativePath": "projects/app"}},
                {"id": "repo:github.com/caniko/behind", "kind": "repo", "attrs": {"relativePath": "projects/behind"}},
                {"id": "usage:github.com/caniko/app#flake-input:harbor-rs", "kind": "usage", "attrs": {"usageKind": "flake-input", "input": "harbor-rs", "direct": true}},
                {"id": "usage:github.com/caniko/behind#flake-input:harbor-rs_2", "kind": "usage", "attrs": {"usageKind": "flake-input", "input": "harbor-rs_2", "direct": false}},
                {"id": "producer:github.com/caniko/harbor-rs", "kind": "producer", "attrs": {"aliases": ["codeberg.org/caniko/rs-harbor"]}}
            ],
            "edges": [
                {"from": "usage:github.com/caniko/app#flake-input:harbor-rs", "to": "rev:github.com/caniko/harbor-rs@aaaa", "kind": "pins"},
                {"from": "usage:github.com/caniko/behind#flake-input:harbor-rs_2", "to": "rev:github.com/caniko/harbor-rs@bbbb", "kind": "pins"}
            ]
        }))],
        None,
    );
    let report = supply::consumers(&graph, "github.com/caniko/harbor-rs", Some("aaaa"));
    assert_eq!(report.contract, supply::CONSUMERS_CONTRACT);
    assert_eq!(report.items.len(), 2);
    let app = report
        .items
        .iter()
        .find(|item| item.coordinate == "github.com/caniko/app")
        .unwrap();
    assert_eq!(app.relative_path.as_deref(), Some("projects/app"));
    assert_eq!(app.input.as_deref(), Some("harbor-rs"));
    assert_eq!(app.direct, Some(true));
    assert_eq!(app.locked_revs, ["aaaa"]);
    assert_eq!(app.target.as_deref(), Some("aaaa"));
    assert!(!app.behind);

    let behind = report
        .items
        .iter()
        .find(|item| item.coordinate == "github.com/caniko/behind")
        .unwrap();
    assert!(behind.behind);
    assert_eq!(behind.direct, Some(false));

    assert!(
        supply::consumers(&graph, "github.com/caniko/harbor-rs", None)
            .items
            .iter()
            .all(|item| !item.behind),
        "without a target nothing is behind"
    );
    assert!(
        supply::consumers(&graph, "codeberg.org/caniko/rs-harbor", None)
            .items
            .iter()
            .all(|item| !item.behind)
    );
}

#[test]
fn diff_reports_added_removed_and_revision_changes_per_producer() {
    fn usage_entry(
        repository: &str,
        pins: &[&str],
        nodes: &mut Vec<Value>,
        edges: &mut Vec<Value>,
    ) {
        let id = format!("usage:github.com/caniko/{repository}#flake-input:harbor-rs");
        nodes.push(json!({"id": id, "kind": "usage", "attrs": {"usageKind": "flake-input"}}));
        for revision in pins {
            edges.push(json!({
                "from": id,
                "to": format!("rev:github.com/caniko/harbor-rs@{revision}"),
                "kind": "pins"
            }));
        }
    }

    let mut previous_nodes = Vec::new();
    let mut previous_edges = Vec::new();
    let mut next_nodes = Vec::new();
    let mut next_edges = Vec::new();
    // removed, revision changed (two revisions), unchanged.
    usage_entry(
        "removed",
        &["aaaa"],
        &mut previous_nodes,
        &mut previous_edges,
    );
    usage_entry(
        "changed",
        &["aaaa", "bbbb"],
        &mut previous_nodes,
        &mut previous_edges,
    );
    usage_entry(
        "changed",
        &["bbbb", "cccc"],
        &mut next_nodes,
        &mut next_edges,
    );
    usage_entry("same", &["dddd"], &mut previous_nodes, &mut previous_edges);
    usage_entry("same", &["dddd"], &mut next_nodes, &mut next_edges);
    // added.
    usage_entry("added", &["eeee"], &mut next_nodes, &mut next_edges);

    let previous = supply::build(
        &[document(
            json!({"nodes": previous_nodes, "edges": previous_edges}),
        )],
        None,
    );
    let next = supply::build(
        &[document(json!({"nodes": next_nodes, "edges": next_edges}))],
        None,
    );
    let report = supply::diff(&previous, &next);
    assert_eq!(report.contract, supply::DIFF_CONTRACT);
    assert_eq!(report.producers.len(), 1);
    let producer = &report.producers[0];
    assert_eq!(producer.coordinate, "github.com/caniko/harbor-rs");
    assert_eq!(
        producer.added.keys().collect::<Vec<_>>(),
        ["usage:github.com/caniko/added#flake-input:harbor-rs"],
        "a usage that did not exist before is added"
    );
    assert_eq!(
        producer.added["usage:github.com/caniko/added#flake-input:harbor-rs"],
        ["eeee"]
    );
    assert_eq!(
        producer.removed.keys().collect::<Vec<_>>(),
        ["usage:github.com/caniko/removed#flake-input:harbor-rs"],
        "a usage that disappeared is removed"
    );
    let change =
        &producer.revision_changed["usage:github.com/caniko/changed#flake-input:harbor-rs"];
    assert_eq!(change.from, ["aaaa", "bbbb"]);
    assert_eq!(change.to, ["bbbb", "cccc"]);
    assert!(
        !producer
            .revision_changed
            .contains_key("usage:github.com/caniko/same#flake-input:harbor-rs"),
        "unchanged revisions are not reported"
    );

    // The same graph compared with itself is empty.
    assert!(supply::diff(&next, &next).producers.is_empty());
}

#[test]
fn graph_json_round_trips_with_its_contract_and_rebuilds_adjacency() {
    let mut docs = supply::parse(&fixture("simit-usage-facts.v1.json")).unwrap();
    docs.extend(supply::parse(&fixture("canix-overlay.v1.json")).unwrap());
    let graph = supply::build(&docs, None);
    let encoded = serde_json::to_string_pretty(&graph).unwrap();
    let value: Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(value["contract"], supply::GRAPH_CONTRACT);
    assert!(value.get("outgoing").is_none(), "adjacency is not state");

    let decoded: supply::Graph = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
        serde_json::to_string_pretty(&decoded).unwrap(),
        encoded,
        "a snapshot round-trips byte for byte"
    );
    let walked = supply::walk(&decoded, "github.com/example/drifted", false, 1).unwrap();
    assert!(
        !walked.nodes.is_empty(),
        "adjacency survives deserialization"
    );

    // A document from another contract is not a graph.
    assert!(serde_json::from_str::<supply::Graph>(&fixture("expected-gaps.json")).is_err());
}

#[test]
fn vocabulary_scoping_rejects_capability_rows_outside_its_usage_kinds() {
    let mut value = json!({
        "contract": CONTRACT,
        "vocabulary": "simit.usage-facts.v1",
        "producer": {"name": "simit", "version": "1"},
        "observedAt": "2026-11-09T00:00:00Z",
        "usageKinds": ["flake-input"],
        "capabilities": [{
            "usageKind": "flake-input", "path": "p", "via": "v",
            "lands": true, "status": "implemented", "reason": "r"
        }]
    });
    assert!(supply::parse(&value.to_string()).is_ok());

    value["capabilities"][0]["usageKind"] = json!("ci.primary");
    let error = supply::parse(&value.to_string()).unwrap_err().to_string();
    assert!(error.contains("ci.primary"), "{error}");
    assert!(
        error.contains("outside usageKinds [flake-input]"),
        "{error}"
    );

    // Without usageKinds the document keeps the legacy behaviour.
    value.as_object_mut().unwrap().remove("usageKinds");
    assert!(supply::parse(&value.to_string()).is_ok());

    // The envelope still rejects unknown fields.
    value["usageKind"] = json!("flake-input");
    assert!(supply::parse(&value.to_string()).is_err());
}

#[test]
fn snapshot_store_is_atomic_and_keyed_by_the_root() {
    let state = tempfile::tempdir().unwrap();
    let previous = std::env::var_os("XDG_STATE_HOME");
    std::env::set_var("XDG_STATE_HOME", state.path());
    let root = state.path().join("workspace");
    let path = supply::store_path(&root);
    let digest_directory = path
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert_eq!(digest_directory.len(), 16);
    assert!(
        digest_directory
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );
    assert_eq!(path.file_name().unwrap(), "graph.json");
    assert!(supply::load_snapshot(&root).unwrap().is_none());

    let graph = supply::build(
        &supply::parse(&fixture("simit-usage-facts.v1.json")).unwrap(),
        None,
    );
    supply::save_snapshot(&root, &graph).unwrap();
    let loaded = supply::load_snapshot(&root).unwrap().unwrap();
    assert_eq!(
        serde_json::to_string_pretty(&loaded).unwrap(),
        serde_json::to_string_pretty(&graph).unwrap()
    );

    let other = state.path().join("other-workspace");
    assert_ne!(supply::store_path(&root), supply::store_path(&other));

    match previous {
        Some(value) => std::env::set_var("XDG_STATE_HOME", value),
        None => std::env::remove_var("XDG_STATE_HOME"),
    }
}

#[test]
fn missing_lock_files_are_empty_and_git_or_parse_failures_become_fact_errors() {
    let root = tempfile::tempdir().unwrap();
    init_repo(root.path(), "empty", &[("README.md", "no locks")]);
    init_repo(root.path(), "broken", &[("flake.lock", "{not json")]);

    let observed = supply::observe(
        root.path(),
        &[
            repo("github.com/caniko/empty", "empty"),
            repo("github.com/caniko/broken", "broken"),
            repo("github.com/caniko/missing", "missing"),
        ],
    );
    assert!(
        observed
            .nodes
            .iter()
            .any(|node| node.id == "repo:github.com/caniko/empty")
    );
    assert!(
        !observed
            .nodes
            .iter()
            .any(|node| node.id.starts_with("usage:")),
        "{:?}",
        observed
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(observed.errors.len(), 2, "{:?}", observed.errors);
    assert!(
        observed
            .errors
            .iter()
            .any(|error| error.subject == "github.com/caniko/broken"
                && error.code == "parse-failure")
    );
    assert!(
        observed.errors.iter().any(
            |error| error.subject == "github.com/caniko/missing" && error.code == "git-failure"
        )
    );
}

#[test]
fn renumbered_lock_node_keys_do_not_change_the_snapshot() {
    // Nix renumbers `_N` node keys whenever a lock changes; the same pins
    // under other keys must diff as nothing.
    let lock = |tool: &str, harbor: &str| {
        json!({
            "nodes": {
                "root": {"inputs": {"tool": tool}},
                tool: {
                    "locked": {"type": "github", "owner": "caniko", "repo": "tool", "rev": "aaaa"},
                    "original": {"type": "github", "owner": "caniko", "repo": "tool"},
                    "inputs": {"harbor-rs": harbor}
                },
                harbor: {
                    "locked": {"type": "github", "owner": "caniko", "repo": "harbor-rs", "rev": "bbbb"},
                    "original": {"type": "github", "owner": "caniko", "repo": "harbor-rs"}
                }
            },
            "root": "root",
            "version": 7
        })
        .to_string()
    };
    let graph = |text: String| {
        let root = tempfile::tempdir().unwrap();
        init_repo(root.path(), "app", &[("flake.lock", &text)]);
        supply::build(
            &[supply::observe(
                root.path(),
                &[repo("github.com/caniko/app", "app")],
            )],
            None,
        )
    };
    let before = graph(lock("tool", "harbor-rs_2"));
    let after = graph(lock("tool_4", "harbor-rs_7"));
    let report = supply::diff(&before, &after);
    assert!(report.producers.is_empty(), "{report:?}");
}
