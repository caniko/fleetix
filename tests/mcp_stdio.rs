#![cfg(all(feature = "cli", feature = "mcp-server"))]

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

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
                assert_eq!(listed["tools"].as_array().unwrap().len(), 3);
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
