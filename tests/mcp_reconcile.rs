#![cfg(feature = "mcp")]

use fleetix::mcp::{Manifest, reconcile};
use serde_json::json;
use std::fs;

fn manifest(path: &std::path::Path, servers: serde_json::Value) -> Manifest {
    serde_json::from_value(json!({
        "version": 1,
        "targets": [{"name": "codex", "path": path, "format": "toml", "root": ["mcp_servers"], "servers": servers}]
    })).unwrap()
}

#[test]
fn mcp_reconciles_only_owned_entries_and_preserves_toml_comments() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let state = dir.path().join("state.json");
    fs::write(
        &path,
        "# personal settings\nmodel = 'native'\n[mcp_servers.manual]\ncommand = '/bin/manual'\n",
    )
    .unwrap();
    let desired = manifest(&path, json!({"graph": {"url": "http://localhost/mcp"}}));
    assert_eq!(reconcile(&desired, &state, true).unwrap().changed.len(), 1);
    assert!(!state.exists());
    assert!(!fs::read_to_string(&path).unwrap().contains("localhost"));
    reconcile(&desired, &state, false).unwrap();
    let rendered = fs::read_to_string(&path).unwrap();
    assert!(rendered.contains("# personal settings"));
    assert!(rendered.contains("'/bin/manual'"));
    assert!(
        reconcile(&desired, &state, false)
            .unwrap()
            .changed
            .is_empty()
    );
    let empty: Manifest = serde_json::from_value(json!({"version": 1, "targets": []})).unwrap();
    reconcile(&empty, &state, false).unwrap();
    let rendered = fs::read_to_string(&path).unwrap();
    assert!(!rendered.contains("localhost"));
    assert!(rendered.contains("'/bin/manual'"));
}

#[test]
fn mcp_conflicts_and_malformed_documents_do_not_write_any_target() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let state = dir.path().join("state.json");
    fs::write(&path, "[mcp_servers.graph]\ncommand = '/bin/personal'\n").unwrap();
    let desired = manifest(&path, json!({"graph": {"url": "http://localhost/mcp"}}));
    let before = fs::read(&path).unwrap();
    assert!(reconcile(&desired, &state, false).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!state.exists());
    fs::write(&path, "broken = [").unwrap();
    assert!(reconcile(&desired, &state, false).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "broken = [");
}

#[test]
fn mcp_refuses_to_remove_edited_owned_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let state = dir.path().join("state.json");
    reconcile(
        &manifest(&path, json!({"graph": {"command": "/bin/managed"}})),
        &state,
        false,
    )
    .unwrap();
    fs::write(&path, "[mcp_servers.graph]\ncommand = '/bin/edited'\n").unwrap();
    assert!(reconcile(&manifest(&path, json!({})), &state, false).is_err());
    assert!(fs::read_to_string(&path).unwrap().contains("/bin/edited"));
}

#[test]
fn mcp_json_preserves_unrelated_values_and_retires_only_matching_legacy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let state = dir.path().join("state.json");
    fs::write(&path, json!({"keep": [1,2], "mcpServers": {"openpencil": {"command": "/nix/store/old/bin/openpencil-desktop", "args": ["--mcp", "agent.op"]}, "personal": {"command": "/bin/keep"}}}).to_string()).unwrap();
    let desired: Manifest = serde_json::from_value(json!({"version": 1, "targets": [{"name": "claude", "path": path, "format": "json", "root": ["mcpServers"], "servers": {"open-pencil": {"command": "/bin/new"}}, "retire": [{"key": "openpencil", "commandSuffix": "/bin/openpencil-desktop", "argsPrefix": ["--mcp"]}]}]})).unwrap();
    reconcile(&desired, &state, false).unwrap();
    let output: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(output["keep"], json!([1, 2]));
    assert!(output["mcpServers"].get("openpencil").is_none());
    assert_eq!(output["mcpServers"]["personal"]["command"], "/bin/keep");
}

#[cfg(unix)]
#[test]
fn mcp_never_replaces_a_declarative_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("managed.toml");
    let path = dir.path().join("config.toml");
    fs::write(&target, "model = 'keep'\n").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(
        reconcile(
            &manifest(&path, json!({"graph": {"command": "/bin/test"}})),
            &dir.path().join("state.json"),
            false
        )
        .is_err()
    );
    assert!(path.is_symlink());
}

#[test]
fn mcp_adopts_identical_json_without_reformatting_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let state = dir.path().join("state.json");
    let original = "{\"mcpServers\":{\"graph\":{\"url\":\"http://localhost/mcp\"}},\"keep\":true}";
    fs::write(&path, original).unwrap();
    let desired: Manifest = serde_json::from_value(json!({"version":1,"targets":[{"name":"claude","path":path,"format":"json","root":["mcpServers"],"servers":{"graph":{"url":"http://localhost/mcp"}}}]})).unwrap();
    assert!(
        reconcile(&desired, &state, false)
            .unwrap()
            .changed
            .is_empty()
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn mcp_validates_all_destinations_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.json");
    let second = dir.path().join("second.toml");
    fs::write(&second, "invalid = [").unwrap();
    let desired: Manifest = serde_json::from_value(json!({"version":1,"targets":[
        {"name":"claude","path":first,"format":"json","root":["mcpServers"],"servers":{"graph":{"command":"/bin/test"}}},
        {"name":"codex","path":second,"format":"toml","root":["mcp_servers"],"servers":{"graph":{"command":"/bin/test"}}}
    ]})).unwrap();
    assert!(reconcile(&desired, &dir.path().join("state.json"), false).is_err());
    assert!(!first.exists());
    assert_eq!(fs::read_to_string(&second).unwrap(), "invalid = [");
}

#[test]
fn mcp_moves_and_removes_owned_destinations() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old.toml");
    let new = dir.path().join("new.toml");
    let state = dir.path().join("state.json");
    let servers = json!({"graph":{"command":"/bin/test"}});
    reconcile(&manifest(&old, servers.clone()), &state, false).unwrap();
    reconcile(&manifest(&new, servers), &state, false).unwrap();
    assert!(!fs::read_to_string(&old).unwrap().contains("/bin/test"));
    assert!(fs::read_to_string(&new).unwrap().contains("/bin/test"));
}

#[test]
fn mcp_lock_contention_fails_without_mutating_config() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    let lock = fs::File::create(state.with_extension("lock")).unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let path = dir.path().join("config.toml");
    assert!(
        reconcile(
            &manifest(&path, json!({"graph":{"command":"/bin/test"}})),
            &state,
            false
        )
        .is_err()
    );
    assert!(!path.exists());
}

#[test]
fn mcp_preserves_inline_toml_roots() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "# personal\nmcp_servers = { keep = { command = '/bin/personal' } }\n",
    )
    .unwrap();
    reconcile(
        &manifest(&path, json!({"graph":{"url":"http://localhost/mcp"}})),
        &dir.path().join("state.json"),
        false,
    )
    .unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("# personal"));
    let parsed: serde_json::Value = toml_edit::de::from_str(&text).unwrap();
    assert_eq!(parsed["mcp_servers"]["keep"]["command"], "/bin/personal");
    assert_eq!(
        parsed["mcp_servers"]["graph"]["url"],
        "http://localhost/mcp"
    );
}

#[test]
fn mcp_replays_interrupted_update_and_pruning() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let state = dir.path().join("state.json");
    let old = json!({"command":"/bin/old"});
    let new = json!({"command":"/bin/new"});
    reconcile(
        &manifest(
            &path,
            json!({"graph":old,"retired":{"command":"/bin/retired"}}),
        ),
        &state,
        false,
    )
    .unwrap();
    let mut journal: serde_json::Value =
        serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    journal["targets"]["codex"]["entries"]["graph"]
        .as_array_mut()
        .unwrap()
        .push(json!(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&new).unwrap())
        )));
    fs::write(&state, journal.to_string()).unwrap();
    // Simulate a process stopping after the destination write but before finalizing state.
    fs::write(&path, "[mcp_servers.graph]\ncommand = '/bin/new'\n").unwrap();
    assert!(
        reconcile(&manifest(&path, json!({"graph":new})), &state, false)
            .unwrap()
            .changed
            .is_empty()
    );
    let final_state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    assert!(
        final_state["targets"]["codex"]["entries"]
            .get("retired")
            .is_none()
    );
    assert_eq!(
        final_state["targets"]["codex"]["entries"]["graph"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
