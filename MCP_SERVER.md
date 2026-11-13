# Composable MCP server

The optional `mcp-server` feature provides a Rust MCP server for Fleetix and
downstream applications. It is separate from the `mcp` feature, which reconciles
client registrations. Neither feature enables the other.

The server uses the official Rust MCP SDK (`rmcp`), not a second JSON-RPC
implementation. The SDK is re-exported at `fleetix::mcp_server::rmcp` so consumers
can construct routes against the same protocol types.

## Composition and authorization

Create one `fleetix::mcp_server::Server`, then pass `&mut Server` to each
consumer's registration function. Each tool registers an explicit `Effect` and
an SDK `ToolRoute<()>`. Handlers capture their own typed dependencies; no
upstream crate imports or invokes a downstream executable. Registration rejects
duplicate names rather than replacing an earlier consumer's handler.

`Server::new` denies all invocations until the embedding application installs an
authorizer with `with_authorizer`. The authorizer is called on every invocation,
including repeated calls after policy changes. Its `Invocation` contains the
registered tool name and effect, plus untrusted request arguments. Tool
annotations, arguments named `approved`, and names such as `show` or `apply`
are not authorization. A consumer must separately validate target/root access
and any operation-specific approval. Do not install an unconditional allow
policy on a server containing privileged tools.

An extension has this shape:

```rust,ignore
use fleetix::mcp_server::{Effect, Server};
use fleetix::mcp_server::rmcp::handler::server::router::tool::ToolRoute;

fn register_extension(server: &mut Server, route: ToolRoute<()>)
    -> Result<(), fleetix::mcp_server::rmcp::ErrorData>
{
    server.register(Effect::Read, route)
}
```

The SDK's `Parameters<T>` extractor checks serde types. It does **not** enforce
arbitrary JSON Schema constraints. Handlers must validate ranges, path
containment, cross-field rules, and other domain constraints before side
effects. Derive schemas from the same parameter types and use
`#[serde(deny_unknown_fields)]` where unknown fields should be rejected.

## Execution contract

- Register all tools before serving. The catalog is static and deterministic;
  live registration and list-change notifications are not advertised.
- `serve_stdio` bounds incoming message lines and writes only protocol messages
  to stdout. An oversized frame closes the connection so a caller is not left
  waiting for a dropped request. Handlers must return values rather than print.
  Diagnostics belong on stderr and must not include secrets.
- The SDK request context reaches each handler, including cancellation and
  progress metadata. A handler owns cancellation safety: stopping a request is
  not proof that a deployment was rolled back. Use existing durable operation
  identities, checkpoints and locks rather than inventing MCP-specific ones.
- A denied invocation returns a tool error without running its handler.
- Expected validation and lookup failures should return
  `CallToolResult::error` or `CallToolResult::structured_error`. A bare
  `ErrorData` from a dynamic route is a protocol error, not a user-visible tool
  error. Reserve it for protocol/routing faults and internal failures.
- There is no shell runner, HTTP listener, credential store, sampling, or
  prompt execution in this provider.

Authorization is scoped to the embedding application and its stdio process.
The authorizer does not receive an authenticated per-client identity; HTTP or
multi-principal authorization is outside this provider slice.

## Standalone topology server

Build or run the opt-in CLI surface:

```sh
cargo run --features mcp-server -- mcp-serve examples/Topology.pkl
cargo run --features mcp-server -- mcp-serve examples/Topology.pkl \
  --supply-root /workspace --supply-inventory /workspace/inventory.json \
  --supply-facts harbor-facts.json
cargo run --features mcp-server -- mcp-serve examples/Topology.pkl \
  --supply-root /workspace --supply-inventory /workspace/inventory.json \
  --supply-land /usr/local/bin/land-pins --supply-land-arg --quiet
```

The operator supplies the topology path at startup. It is evaluated once; tool
arguments cannot select another file or trigger fresh Pkl evaluation. Restart
the server to load a new snapshot. This grants connected clients read access to
the selected topology, including private network metadata. Do not supply
credential-bearing material.

The server registers six typed read tools by default:

| Tool | Arguments | Result |
| --- | --- | --- |
| `fleetix_show` | Optional `host`, optional `fields` | Topology or selected host, optionally only the named top-level keys |
| `fleetix_links` | Optional `name` | Link definitions and derived addresses |
| `fleetix_validate` | Empty object | Cross-reference validation report |
| `fleetix_host_metrics` | Empty object | Live memory, swap, PSI, load, uptime and VRAM of the serving host |
| `fleetix_host_cgroups` | Optional `depth`, `min_bytes`, `limit` | Live cgroup v2 memory accounting, largest first |
| `fleetix_host_events` | Optional `since` (journalctl syntax) | Kernel OOM kills with cgroup, victim and killed-process counts |

`--supply-root` and `--supply-inventory` register two more read tools. Both
flags are required together; `--supply-facts` is repeatable and merges producer
documents. These paths are startup-only: tool arguments cannot choose a root,
inventory or facts document. Without the flags the tools are absent from the
catalog.

| Tool | Arguments | Result |
| --- | --- | --- |
| `fleetix_supply_consumers` | `producer`, optional `target` revision | `fleetix.supply-consumers.v1`: repositories that pin the producer, their relative paths, locked revisions and whether each is behind the target |
| `fleetix_supply_refresh` | Empty object | Observes the configured workspace, stores the snapshot, and returns its `fleetix.supply-diff.v1` change set |

`fleetix_supply_consumers` reads the stored snapshot and performs one
observation first when none exists. `fleetix_supply_refresh` always observes
and replaces the snapshot under
`$XDG_STATE_HOME/fleetix/supply/<root-digest>/graph.json`. Observation reads
committed lock files through `git`, never the network or a Nix evaluation. A
repository's git or parse failure is returned in `errors` and does not fail the
refresh.

### Landing behind consumers

`--supply-land PROGRAM` adds one Write tool, `fleetix_supply_land`. The flag
requires `--supply-root` and `--supply-inventory`; without it the tool is absent
from the catalog and the authorizer keeps denying every Write call. A repeatable
`--supply-land-arg ARG` is passed before the work-item file. The server runs
`PROGRAM ARG... <work-item-file>` directly, without a shell, inheriting its own
environment; `PROGRAM` is startup configuration, so tool arguments can never
choose what runs. Destructive effects stay denied.

| Tool | Arguments | Result |
| --- | --- | --- |
| `fleetix_supply_land` | `producer`, `target` (40-hex revision), optional `coordinates` | `workItems` path, the work items, `exitCode`, `success` and the command's `stdout` (parsed JSON when it parses) and `stderr`, each capped at 64 KiB |

The handler observes the workspace and stores the snapshot first, then keeps
direct consumers that are behind the target, one item per coordinate and input,
optionally restricted to the given consumer coordinates (an unknown coordinate
is an error naming it). With nothing left it returns
`{"items": [], "landed": false, "reason": "nothing-behind"}` and runs nothing.
Otherwise it atomically writes a mode-0600
`fleetix.supply-work-items.v1` file next to the snapshot as
`work-items.json`:

```json
{"contract":"fleetix.supply-work-items.v1","producer":"github.com/caniko/harbor-rs","target":"<rev>","items":[{"coordinate":"github.com/caniko/app","relativePath":"projects/app","input":"harbor-rs","target":"<rev>"}]}
```

A non-zero command exit is a structured result with `success: false`, not a
protocol error. Fleetix does not interpret the landing command's output; the
operator's program owns what the work items mean.

With `fleetix.homeModules.mcp`, register this server for every enabled harness
by naming the snapshot; the module supplies the packaged binary, `pkl` and
`journalctl`:

```nix
fleetix.mcp.topologyServer = {
  enable = true;
  file = "${inputs.self}/lib/topology/Topology.pkl";
};
```

The `fleetix_host_*` tools read the host running the server, not the topology:
`/proc`, `/sys/fs/cgroup`, DRM sysfs and the kernel journal, with the server
process's permissions. Unreadable sources are omitted; `null` limits mean an
explicit unlimited `max`.

Validation failures are structured tool errors, not server termination.
Unknown selections and malformed arguments do not terminate the server.
The existing `fleetix mcp --manifest ... --state ...` registration writer and
its flags remain unchanged.

## Downstream boundary

Fleetix owns the protocol and composition API; consumers own their handlers,
credentials and authorization policy. Do not run an entire CLI dispatcher inside
this server, and never let name-derived safety metadata authorize an operation.

## Validation

```sh
cargo test --no-default-features --features mcp-server --test mcp_server
cargo test --features mcp-server
cargo check --no-default-features --lib
cargo clippy --all-targets --features mcp-server -- -D warnings
```

Use the approved project environment and treefmt for formatting. The SDK's
published Rust requirement is 1.88, matching Fleetix; qualify the complete
resolved dependency graph on that toolchain before release.

Protocol reference: <https://modelcontextprotocol.io/specification/2026-07-28>.
SDK source: <https://github.com/modelcontextprotocol/rust-sdk/tree/rmcp-v3.9.0>.

The lockfile retains `uuid` 1.26.1 for Rust 1.88 compatibility; the SDK's broader
version range otherwise selects releases requiring Rust 1.89.
A downstream application does not inherit this workspace lockfile. For Rust
1.88, use MSRV-aware dependency resolution or select `uuid` 1.26.1 with
`cargo update -p uuid --precise 1.26.1`, then verify its resolved graph on 1.88.
