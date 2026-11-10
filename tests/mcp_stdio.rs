#![cfg(all(feature = "cli", feature = "mcp-server"))]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

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
        .unwrap();
    assert!(status.success(), "git {arguments:?}");
}

/// A committed consumer repository for the startup-configured supply tools.
fn supply_workspace(directory: &Path) -> (String, String) {
    let root = directory.join("workspace");
    let consumer = root.join("consumer");
    std::fs::create_dir_all(&consumer).unwrap();
    git(&consumer, &["init", "-q", "-b", "main"]);
    std::fs::write(
        consumer.join("flake.lock"),
        json!({
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
    )
    .unwrap();
    git(&consumer, &["add", "-A"]);
    git(
        &consumer,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "fixture",
        ],
    );
    let inventory = directory.join("inventory.json");
    std::fs::write(
        &inventory,
        json!([{"coordinate": "github.com/caniko/consumer", "relativePath": "consumer"}])
            .to_string(),
    )
    .unwrap();
    (root.display().to_string(), inventory.display().to_string())
}

#[test]
fn supply_tools_exist_only_with_startup_flags_and_refresh_the_snapshot() {
    let state = tempfile::tempdir().unwrap();
    let (root, inventory) = supply_workspace(state.path());
    let mut child = Command::new(env!("CARGO_BIN_EXE_fleetix"))
        .args(["mcp-serve", "examples/Topology.pkl", "--supply-root"])
        .arg(&root)
        .arg("--supply-inventory")
        .arg(&inventory)
        .env("XDG_STATE_HOME", state.path().join("state"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let (send, receive) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            if send.send(line).is_err() {
                break;
            }
        }
    });

    let mut request = |message: Value, expect_reply: bool| {
        writeln!(input, "{message}").unwrap();
        input.flush().unwrap();
        if !expect_reply {
            return Value::Null;
        }
        let line = receive
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .unwrap();
        let response: Value = serde_json::from_str(&line).expect("stdout must contain only MCP");
        assert_eq!(response["id"], message["id"]);
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    };
    request(
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": {"name": "fleetix-stdio-test", "version": "1"}}
        }),
        true,
    );
    request(
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        false,
    );
    let listed = request(
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        true,
    );
    let names: Vec<_> = listed["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "fleetix_host_cgroups",
            "fleetix_host_events",
            "fleetix_host_metrics",
            "fleetix_links",
            "fleetix_show",
            "fleetix_supply_consumers",
            "fleetix_supply_refresh",
            "fleetix_validate"
        ]
    );

    let refreshed = request(
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "fleetix_supply_refresh", "arguments": {}}}),
        true,
    );
    assert_eq!(refreshed["isError"], false, "{refreshed}");
    assert_eq!(
        refreshed["structuredContent"]["diff"]["producers"][0]["coordinate"],
        "github.com/caniko/harbor-rs"
    );
    assert_eq!(
        refreshed["structuredContent"]["diff"]["producers"][0]["added"]["usage:github.com/caniko/consumer#flake-input:harbor-rs"],
        json!(["aaaa"])
    );
    assert!(
        refreshed["structuredContent"]["store"]
            .as_str()
            .unwrap()
            .ends_with("graph.json")
    );

    let current = request(
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "fleetix_supply_consumers",
                "arguments": {"producer": "github.com/caniko/harbor-rs", "target": "aaaa"}}}),
        true,
    );
    assert_eq!(current["isError"], false, "{current}");
    assert_eq!(
        current["structuredContent"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(current["structuredContent"]["items"][0]["behind"], false);
    assert_eq!(
        current["structuredContent"]["items"][0]["relativePath"],
        "consumer"
    );

    let behind = request(
        json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": {"name": "fleetix_supply_consumers",
                "arguments": {"producer": "github.com/caniko/harbor-rs", "target": "bbbb"}}}),
        true,
    );
    assert_eq!(behind["structuredContent"]["items"][0]["behind"], true);

    drop(input);
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("MCP server did not close after stdin EOF");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    reader.join().unwrap();
    assert!(status.success(), "{status}");
}

#[test]
fn standalone_stdio_keeps_protocol_output_clean_and_closes_on_eof() {
    for oversized in [false, true] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_fleetix"))
            .args(["mcp-serve", "examples/Topology.pkl"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (send, receive) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            {
                let mut request = |message: Value, expect_reply: bool| {
                    writeln!(input, "{message}").unwrap();
                    input.flush().unwrap();
                    if !expect_reply {
                        return Value::Null;
                    }
                    let line = receive
                        .recv_timeout(Duration::from_secs(20))
                        .unwrap()
                        .unwrap();
                    let response: Value =
                        serde_json::from_str(&line).expect("stdout must contain only MCP");
                    assert_eq!(response["id"], message["id"]);
                    assert!(response.get("error").is_none(), "{response}");
                    response["result"].clone()
                };
                let initialized = request(
                    json!({
                        "jsonrpc": "2.0", "id": 1, "method": "initialize",
                        // Deliberately negotiate the prior protocol to guard SDK compatibility.
                        "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                            "clientInfo": {"name": "fleetix-stdio-test", "version": "1"}}
                    }),
                    true,
                );
                assert_eq!(initialized["serverInfo"]["name"], "fleetix");
                // The initialization notification must precede tool requests.
                request(
                    json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                    false,
                );
                let listed = request(
                    json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
                    true,
                );
                assert_eq!(listed["tools"].as_array().unwrap().len(), 6);
                let bad = request(
                    json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "fleetix_show", "arguments": {"path": "/not-authorized"}}}),
                    true,
                );
                assert_eq!(bad["isError"], true);
                let missing = request(
                    json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "fleetix_show", "arguments": {"host": "missing"}}}),
                    true,
                );
                assert_eq!(missing["isError"], true);
                let missing = request(
                    json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": {"name": "fleetix_links", "arguments": {"name": "missing"}}}),
                    true,
                );
                assert_eq!(missing["isError"], true);
                let shown = request(
                    json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": {"name": "fleetix_show", "arguments": {"host": "edge"}}}),
                    true,
                );
                assert_eq!(shown["isError"], false);
                assert_eq!(shown["structuredContent"]["system"], "x86_64-linux");
            }
            if oversized {
                let frame = json!({"jsonrpc": "2.0", "id": 99, "method": "tools/call",
                "params": {"name": "fleetix_show", "arguments": {
                    "host": "x".repeat(fleetix::mcp_server::MAX_STDIO_LINE_LENGTH)
                }}});
                if let Err(error) = writeln!(input, "{frame}") {
                    assert!(matches!(
                        error.kind(),
                        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                    ));
                }
                assert!(
                    matches!(
                        receive.recv_timeout(Duration::from_secs(10)),
                        Err(mpsc::RecvTimeoutError::Disconnected)
                    ),
                    "oversized requests must close the connection instead of leaving a caller waiting"
                );
            }
        }));
        drop(input);
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("MCP server did not close after stdin EOF");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        reader.join().unwrap();
        if let Err(error) = result {
            std::panic::resume_unwind(error);
        }
        assert_eq!(receive.try_iter().count(), 0, "unexpected trailing stdout");
        assert!(status.success(), "{status}");
    }
}
