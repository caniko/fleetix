//! Opt-in MCP server adapter built on the official `rmcp` SDK.
//!
//! The adapter owns tool registration and authorization; it does not own your
//! domain operations. Downstream crates compose by calling [`Server::register`]
//! with an SDK [`ToolRoute`].
//!
//! # Authorization
//!
//! [`Server::new`] installs a default-deny authorizer, so a server with no
//! explicit policy serves discovery but refuses every call. [`Effect`] is taken
//! from registration only: client arguments and SDK tool annotations
//! are never consulted. The authorizer is evaluated on every call, including
//! repeats, and a denial returns a tool-level `isError` result without running
//! the handler.
//!
//! # Handler contract
//!
//! The SDK only deserializes typed [`Parameters`](rmcp::handler::server::wrapper::Parameters)
//! values; it does not validate arbitrary JSON Schema constraints such as
//! ranges, formats, or string patterns. Handlers must validate their own input
//! and treat every argument as untrusted data. Mutating handlers must observe the
//! [`RequestContext::ct`](rmcp::service::RequestContext) cancellation token at
//! operation-defined safe points. Cancellation does not undo completed effects;
//! this adapter does not cancel or drop handlers automatically.
//!
//! # Discovery
//!
//! [`Server`] advertises the tools capability only, with deterministic,
//! sorted, static discovery and no `list_changed` notification.
//!
//! # Stdio
//!
//! [`Server::serve_stdio`] speaks newline-delimited JSON-RPC on stdin/stdout,
//! caps a single inbound line at [`MAX_STDIO_LINE_LENGTH`], and treats EOF as a
//! clean shutdown. The library never writes to stdout outside that transport.

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::StreamExt;
use rmcp::handler::server::router::tool::{ToolRoute, ToolRouter};
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::{ErrorData, ServerHandler};
use tokio_util::codec::{FramedRead, LinesCodec};
use tokio_util::io::StreamReader;

pub use rmcp;

/// Maximum size of one inbound JSON-RPC line on the stdio transport.
pub const MAX_STDIO_LINE_LENGTH: usize = 1024 * 1024;

/// The registered authority a tool requires.
///
/// The effect is fixed at registration time. It is never inferred from client
/// arguments or the tool's SDK annotations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Effect {
    /// Observes state without changing it.
    Read,
    /// Changes managed state. This implies neither idempotence nor retry safety.
    Write,
    /// Changes state irreversibly or with side effects beyond the managed system.
    Destructive,
}

/// What an authorizer sees for one call.
pub struct Invocation<'a> {
    /// The registered tool name.
    pub name: &'a str,
    /// The effect declared when the tool was registered.
    pub effect: Effect,
    /// Raw client arguments. Treat them as untrusted input.
    pub arguments: Option<&'a JsonObject>,
}

type Authorizer = dyn Fn(&Invocation<'_>) -> bool + Send + Sync;

/// An MCP server with a tool catalog and a call-time authorizer.
pub struct Server {
    name: String,
    version: String,
    authorizer: Arc<Authorizer>,
    router: ToolRouter<()>,
    effects: HashMap<String, Effect>,
}

impl Server {
    /// Create a server that advertises its tools but denies every call until an
    /// authorizer is installed with [`Server::with_authorizer`].
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            authorizer: Arc::new(|_: &Invocation<'_>| false),
            router: ToolRouter::new(),
            effects: HashMap::new(),
        }
    }

    /// Replace the default-deny authorizer. The closure runs on every call.
    pub fn with_authorizer<F>(mut self, authorizer: F) -> Self
    where
        F: Fn(&Invocation<'_>) -> bool + Send + Sync + 'static,
    {
        self.authorizer = Arc::new(authorizer);
        self
    }

    /// Register one tool route.
    ///
    /// Duplicate names and names outside the MCP tool-name character set
    /// (1-128 ASCII alphanumerics, `_`, `-`, `.`) are refused without changing
    /// the catalog; the existing route is never overwritten.
    pub fn register(&mut self, effect: Effect, route: ToolRoute<()>) -> Result<(), ErrorData> {
        let name = route.name().to_owned();
        if !valid_tool_name(&name) {
            return Err(ErrorData::invalid_params(
                format!(
                    "invalid tool name {name:?}: expected 1-128 ASCII alphanumerics, '_', '-', or '.'"
                ),
                None,
            ));
        }
        if self.effects.contains_key(&name) || self.router.has_route(&name) {
            return Err(ErrorData::invalid_params(
                format!("tool {name:?} is already registered"),
                None,
            ));
        }
        self.effects.insert(name, effect);
        self.router.add_route(route);
        Ok(())
    }

    /// Serve this server over stdin/stdout until EOF.
    ///
    /// Protocol messages are the only output written to stdout. An oversized
    /// inbound line (over [`MAX_STDIO_LINE_LENGTH`]) closes the connection rather
    /// than leaving its caller waiting for a silently discarded request.
    pub async fn serve_stdio(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // The SDK drops oversized lines but keeps the connection open. A bounded
        // codec turns that framing failure into an I/O error; the SDK still owns
        // all JSON-RPC parsing and responses for otherwise well-framed messages.
        let lines = FramedRead::new(
            tokio::io::stdin(),
            LinesCodec::new_with_max_length(MAX_STDIO_LINE_LENGTH - 1),
        )
        .map(|line| {
            line.map(|line| std::io::Cursor::new(format!("{line}\n").into_bytes()))
                .map_err(std::io::Error::other)
        });
        let transport = AsyncRwTransport::new_server(StreamReader::new(lines), tokio::io::stdout())
            .with_max_line_length(MAX_STDIO_LINE_LENGTH);
        let running = match rmcp::serve_server(self, transport).await {
            Ok(running) => running,
            Err(rmcp::service::ServerInitializeError::ConnectionClosed(_)) => return Ok(()),
            Err(error) => return Err(Box::new(error)),
        };
        running.waiting().await?;
        Ok(())
    }
}

fn valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build());
        info.server_info = Implementation::new(self.name.clone(), self.version.clone());
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.router.list_all()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let effect = self
            .effects
            .get(request.name.as_ref())
            .copied()
            .ok_or_else(|| ErrorData::invalid_params("tool not found", None))?;
        let allowed = {
            let invocation = Invocation {
                name: &request.name,
                effect,
                arguments: request.arguments.as_ref(),
            };
            (self.authorizer)(&invocation)
        };
        if !allowed {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "tool {:?} denied by authorizer",
                request.name
            ))])
            .into());
        }
        let service = ();
        self.router
            .call(ToolCallContext::new(&service, request, context))
            .await
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.router.get(name).cloned()
    }
}
