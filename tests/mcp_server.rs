#![cfg(feature = "mcp-server")]

//! Protocol-level tests for the opt-in MCP server adapter.

use std::borrow::Cow;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use fleetix::mcp_server::{Effect, Invocation, Server};
use rmcp::ClientLifecycleMode;
use rmcp::handler::server::common::schema_for_input;
use rmcp::handler::server::router::tool::ToolRoute;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, JsonObject, ProtocolVersion, Tool,
    ToolAnnotations,
};
use rmcp::schemars;
use rmcp::schemars::JsonSchema;
use rmcp::service::{RoleClient, RunningService, serve_client_with_lifecycle, serve_server};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Notify;

struct TestClient;

impl rmcp::ClientHandler for TestClient {}

async fn connect(server: Server) -> RunningService<RoleClient, TestClient> {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let running = serve_server(server, server_io)
            .await
            .expect("server initializes");
        let _ = running.waiting().await;
    });
    serve_client_with_lifecycle(
        TestClient,
        client_io,
        ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        },
    )
    .await
    .expect("client initializes")
}

fn args(value: serde_json::Value) -> JsonObject {
    serde_json::from_value(value).expect("object arguments")
}

#[derive(Deserialize, JsonSchema)]
struct Echo {
    text: String,
}

fn echo_tool(name: impl Into<Cow<'static, str>>, description: &'static str) -> Tool {
    Tool::new(
        name,
        description,
        schema_for_input::<Echo>().expect("schema"),
    )
}

fn echo_route(
    name: impl Into<Cow<'static, str>>,
    description: &'static str,
    calls: Arc<AtomicUsize>,
) -> ToolRoute<()> {
    ToolRoute::new(
        echo_tool(name, description),
        move |Parameters(Echo { text }): Parameters<Echo>| {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                CallToolResult::success(vec![ContentBlock::text(text)])
            }
        },
    )
}

async fn call(
    client: &RunningService<RoleClient, TestClient>,
    name: &str,
    arguments: serde_json::Value,
) -> CallToolResult {
    client
        .call_tool(CallToolRequestParams::new(name.to_owned()).with_arguments(args(arguments)))
        .await
        .expect("tool call round-trips")
}

fn text(result: &CallToolResult) -> &str {
    match result.content.first() {
        Some(ContentBlock::Text(text)) => &text.text,
        other => panic!("expected text content, got {other:?}"),
    }
}

#[test]
fn sdk_is_reexported() {
    let implementation = fleetix::mcp_server::rmcp::model::Implementation::new("reexport", "0");
    assert_eq!(implementation.name, "reexport");
}

#[tokio::test]
async fn protocol_smoke_discovers_sorted_tools_and_recovers_after_an_error() {
    let zeta_calls = Arc::new(AtomicUsize::new(0));
    let alpha_calls = Arc::new(AtomicUsize::new(0));
    let authorizations = Arc::new(AtomicUsize::new(0));
    let observed = authorizations.clone();
    let mut server =
        Server::new("fleetix-test", "0.0.0").with_authorizer(move |_: &Invocation<'_>| {
            observed.fetch_add(1, Ordering::SeqCst);
            true
        });
    server
        .register(Effect::Read, echo_route("zeta", "last", zeta_calls.clone()))
        .unwrap();
    server
        .register(Effect::Read, echo_route("alpha", "first", alpha_calls))
        .unwrap();

    let client = connect(server).await;
    let info = client.peer_info().expect("handshake info");
    assert_eq!(
        info.server_info.as_ref().map(|server| server.name.as_str()),
        Some("fleetix-test")
    );
    assert!(info.capabilities.tools.is_some());
    assert!(info.capabilities.resources.is_none());
    assert!(info.capabilities.prompts.is_none());
    assert_eq!(
        info.capabilities
            .tools
            .as_ref()
            .expect("tools capability")
            .list_changed,
        None
    );

    let names: Vec<_> = client
        .list_tools(None)
        .await
        .unwrap()
        .tools
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect();
    assert_eq!(names, ["alpha", "zeta"]);

    assert_eq!(
        text(&call(&client, "alpha", json!({"text": "one"})).await),
        "one"
    );
    assert!(
        client
            .call_tool(CallToolRequestParams::new("missing"))
            .await
            .is_err()
    );
    assert_eq!(
        authorizations.load(Ordering::SeqCst),
        1,
        "unknown tools must not reach policy"
    );
    assert_eq!(
        text(&call(&client, "zeta", json!({"text": "two"})).await),
        "two"
    );
    assert_eq!(zeta_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn duplicate_registration_is_refused_without_overwriting() {
    let first = Arc::new(AtomicUsize::new(0));
    let second = Arc::new(AtomicUsize::new(0));
    let mut server = Server::new("duplicate", "0.0.0").with_authorizer(|_: &Invocation<'_>| true);
    server
        .register(Effect::Read, echo_route("same", "first", first.clone()))
        .unwrap();
    let error = server
        .register(
            Effect::Destructive,
            echo_route("same", "second", second.clone()),
        )
        .unwrap_err();
    assert!(error.message.contains("already registered"), "{error}");

    let client = connect(server).await;
    let tools = client.list_tools(None).await.unwrap().tools;
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].description.as_deref(), Some("first"));
    assert_eq!(
        text(&call(&client, "same", json!({"text": "x"})).await),
        "x"
    );
    assert_eq!(first.load(Ordering::SeqCst), 1);
    assert_eq!(second.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn invalid_names_are_refused_without_mutating_the_catalog() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut server = Server::new("names", "0.0.0").with_authorizer(|_: &Invocation<'_>| true);
    server
        .register(Effect::Read, echo_route("valid", "kept", calls))
        .unwrap();

    let too_long = "a".repeat(129);
    for name in [
        "",
        "has space",
        "bad@char",
        "trailing, comma",
        too_long.as_str(),
    ] {
        let error = server
            .register(
                Effect::Read,
                echo_route(name.to_owned(), "rejected", Arc::new(AtomicUsize::new(0))),
            )
            .unwrap_err();
        assert!(
            error.message.contains("invalid tool name"),
            "{name:?}: {error}"
        );
    }

    let client = connect(server).await;
    let names: Vec<_> = client
        .list_tools(None)
        .await
        .unwrap()
        .tools
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect();
    assert_eq!(names, ["valid"]);
}

#[tokio::test]
async fn default_authorizer_denies_read_even_when_annotations_claim_safe() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = calls.clone();
    let mut server = Server::new("deny", "0.0.0");
    let mut tool = echo_tool("inspect", "claims read-only");
    tool.annotations = Some(ToolAnnotations::default().read_only(true));
    server
        .register(
            Effect::Read,
            ToolRoute::new(tool, move |Parameters(Echo { text }): Parameters<Echo>| {
                let calls = handler_calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    CallToolResult::success(vec![ContentBlock::text(text)])
                }
            }),
        )
        .unwrap();

    let client = connect(server).await;
    let result = call(&client, "inspect", json!({"text": "x"})).await;
    assert_eq!(result.is_error, Some(true));
    assert!(text(&result).contains("denied"), "{}", text(&result));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn effect_comes_from_registration_not_annotations_or_arguments() {
    let observed = Arc::new(Mutex::new(None));
    let observed_authorizer = observed.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = calls.clone();
    let mut server =
        Server::new("effects", "0.0.0").with_authorizer(move |inv: &Invocation<'_>| {
            *observed_authorizer.lock().unwrap() = Some((inv.name.to_owned(), inv.effect));
            inv.effect == Effect::Read
        });
    let mut tool = echo_tool("inspect", "annotations lie");
    tool.annotations = Some(ToolAnnotations::default().destructive(true));
    server
        .register(
            Effect::Read,
            ToolRoute::new(tool, move |Parameters(Echo { text }): Parameters<Echo>| {
                let calls = handler_calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    CallToolResult::success(vec![ContentBlock::text(text)])
                }
            }),
        )
        .unwrap();

    let client = connect(server).await;
    let result = call(
        &client,
        "inspect",
        json!({"text": "x", "effect": "write", "safe": true}),
    )
    .await;
    assert_eq!(result.is_error, Some(false));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *observed.lock().unwrap(),
        Some(("inspect".to_owned(), Effect::Read))
    );
}

#[tokio::test]
async fn authorization_is_checked_on_every_call() {
    let permitted = Arc::new(AtomicBool::new(true));
    let authority = permitted.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut server = Server::new("revoke", "0.0.0").with_authorizer(move |inv: &Invocation<'_>| {
        authority.load(Ordering::SeqCst) && inv.effect == Effect::Write
    });
    server
        .register(Effect::Write, echo_route("mutate", "writes", calls.clone()))
        .unwrap();

    let client = connect(server).await;
    assert_eq!(
        text(&call(&client, "mutate", json!({"text": "first"})).await),
        "first"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    permitted.store(false, Ordering::SeqCst);
    let denied = call(&client, "mutate", json!({"text": "second"})).await;
    assert_eq!(denied.is_error, Some(true));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn malformed_typed_arguments_are_rejected_without_running_the_handler() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut server = Server::new("typed", "0.0.0").with_authorizer(|_: &Invocation<'_>| true);
    server
        .register(Effect::Read, echo_route("echo", "typed", calls.clone()))
        .unwrap();

    let client = connect(server).await;
    let result = call(&client, "echo", json!({"text": 7})).await;
    assert_eq!(result.is_error, Some(true));
    assert!(
        text(&result).contains("failed to deserialize parameters"),
        "{}",
        text(&result)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn arguments_are_data_and_never_reach_a_shell() {
    let temporary = tempfile::tempdir().unwrap();
    let sentinel = temporary.path().join("must-not-exist");
    let calls = Arc::new(AtomicUsize::new(0));
    let mut server = Server::new("data", "0.0.0").with_authorizer(|_: &Invocation<'_>| true);
    server
        .register(Effect::Read, echo_route("echo", "echoes", calls))
        .unwrap();

    let client = connect(server).await;
    let payload = format!("x; touch {}", sentinel.display());
    let result = call(&client, "echo", json!({"text": payload})).await;
    assert_eq!(text(&result), payload);
    assert!(!sentinel.exists());
}

mod consumer {
    use super::*;

    /// One downstream consumer contributes a typed route.
    pub fn install_reads(server: &mut Server, calls: Arc<AtomicUsize>) {
        server
            .register(Effect::Read, echo_route("alpha-read", "consumer a", calls))
            .unwrap();
    }

    /// Another consumer contributes a dynamic route to the same server.
    pub fn install_dynamic(server: &mut Server) {
        let route = ToolRoute::new_dyn(echo_tool("beta-run", "consumer b"), |context| {
            Box::pin(async move {
                let name = context.name.to_string();
                let text = context
                    .arguments
                    .as_ref()
                    .and_then(|arguments| arguments.get("text"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("missing");
                Ok(
                    CallToolResult::success(vec![ContentBlock::text(format!("{name}:{text}"))])
                        .into(),
                )
            })
        });
        server.register(Effect::Write, route).unwrap();
    }
}

#[tokio::test]
async fn downstream_consumers_compose_on_one_server() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut server = Server::new("compose", "0.0.0").with_authorizer(|_: &Invocation<'_>| true);
    consumer::install_reads(&mut server, calls.clone());
    consumer::install_dynamic(&mut server);

    let client = connect(server).await;
    let names: Vec<_> = client
        .list_tools(None)
        .await
        .unwrap()
        .tools
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect();
    assert_eq!(names, ["alpha-read", "beta-run"]);
    assert_eq!(
        text(&call(&client, "beta-run", json!({"text": "ok"})).await),
        "beta-run:ok"
    );
    assert_eq!(
        text(&call(&client, "alpha-read", json!({"text": "a"})).await),
        "a"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancellation_reaches_the_handler_and_the_connection_survives() {
    let started = Arc::new(Notify::new());
    let cancelled = Arc::new(AtomicBool::new(false));
    let route = {
        let started = started.clone();
        let cancelled = cancelled.clone();
        ToolRoute::new_dyn(
            echo_tool("hang", "waits for cancellation"),
            move |context| {
                let started = started.clone();
                let cancelled = cancelled.clone();
                Box::pin(async move {
                    started.notify_one();
                    context.request_context.ct.cancelled().await;
                    cancelled.store(true, Ordering::SeqCst);
                    Ok(CallToolResult::success(vec![ContentBlock::text("cancelled")]).into())
                })
            },
        )
    };

    let mut server = Server::new("cancel", "0.0.0").with_authorizer(|_: &Invocation<'_>| true);
    server.register(Effect::Read, route).unwrap();
    server
        .register(
            Effect::Read,
            echo_route("echo", "still alive", Arc::new(AtomicUsize::new(0))),
        )
        .unwrap();

    let client = connect(server).await;
    let mut pending = Box::pin(client.call_tool_once(
        CallToolRequestParams::new("hang").with_arguments(args(json!({"text": "x"}))),
    ));
    tokio::select! {
        result = &mut pending => panic!("call completed before cancellation: {result:?}"),
        started = tokio::time::timeout(Duration::from_secs(5), started.notified()) => {
            started.expect("handler did not start");
        }
    }
    drop(pending);

    tokio::time::timeout(Duration::from_secs(5), async {
        while !cancelled.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("handler observed cancellation");

    assert_eq!(
        text(&call(&client, "echo", json!({"text": "alive"})).await),
        "alive"
    );
}
