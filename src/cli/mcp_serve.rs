//! Read-only tools over one operator-selected, immutable topology snapshot,
//! plus live memory views of the host running the server.

mod host;

use std::{future::ready, path::Path, path::PathBuf, sync::Arc};

use crate::Topology;
use crate::mcp_server::{Effect, Server, rmcp};
use crate::supply;
use rmcp::handler::server::{
    common::schema_for_input, router::tool::ToolRoute, wrapper::Parameters,
};
use rmcp::model::{CallToolResult, Tool, ToolAnnotations};
use rmcp::{ErrorData, schemars};
use serde::{Deserialize, Serialize};

/// Startup-selected workspace for the supply tools. Tool arguments can never
/// choose another root, inventory or facts set.
#[derive(Debug, Clone)]
pub(super) struct SupplyConfig {
    pub root: PathBuf,
    pub inventory: PathBuf,
    pub facts: Vec<PathBuf>,
    /// Operator-configured landing command. Without it the land tool is absent
    /// from the catalog and Write effects stay denied.
    pub land: Option<LandConfig>,
}

/// The landing command `fleetix_supply_land` runs: `PROGRAM ARG... <work-items>`.
#[derive(Debug, Clone)]
pub(super) struct LandConfig {
    pub program: PathBuf,
    pub args: Vec<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct HostSelection {
    /// Return a single host instead of the entire topology.
    host: Option<String>,
    /// Keep only these top-level keys of the result, e.g. ["hosts"] or ["gpu", "network"].
    fields: Option<Vec<String>>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CgroupSelection {
    /// Levels below /sys/fs/cgroup to descend (default 4, at most 8).
    depth: Option<usize>,
    /// Skip cgroups whose memory.current is below this (default 256 MiB).
    min_bytes: Option<u64>,
    /// Return at most this many cgroups, largest first (default 40).
    limit: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct EventSelection {
    /// journalctl --since value (default "48 hours ago").
    since: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct LinkSelection {
    /// Return a single link instead of all links.
    name: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SupplyConsumersSelection {
    /// Producer coordinate, renamed repository or mirror alias.
    producer: String,
    /// Target revision; consumers that do not pin it are reported `behind`.
    target: Option<String>,
    /// Keep only direct inputs; transitive pins can be several times as many.
    #[serde(default, rename = "directOnly")]
    direct_only: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SupplyLandSelection {
    /// Producer coordinate, renamed repository or mirror alias.
    producer: String,
    /// Target revision every landed consumer must pin (40 hex characters).
    target: String,
    /// Restrict to these consumer coordinates; each must consume the producer.
    coordinates: Option<Vec<String>>,
}

pub(super) fn server(
    topology: Topology,
    supply_config: Option<SupplyConfig>,
) -> Result<Server, ErrorData> {
    let topology = Arc::new(topology);
    // Landing is opt-in at startup: the configured command is the only Write
    // authority this server ever grants, and Destructive stays denied.
    let landing = supply_config
        .as_ref()
        .is_some_and(|config| config.land.is_some());
    let mut server =
        Server::new("fleetix", env!("CARGO_PKG_VERSION")).with_authorizer(move |call| {
            match call.effect {
                Effect::Read => true,
                Effect::Write => landing,
                Effect::Destructive => false,
            }
        });

    let snapshot = Arc::clone(&topology);
    server.register(
        Effect::Read,
        ToolRoute::new(
            tool::<HostSelection>(
                "fleetix_show",
                "Inspect the selected topology or a single host",
            )?,
            move |Parameters(args): Parameters<HostSelection>| {
                ready(show(
                    &snapshot,
                    args.host.as_deref(),
                    args.fields.as_deref(),
                ))
            },
        ),
    )?;

    server.register(
        Effect::Read,
        ToolRoute::new(
            tool::<Empty>(
                "fleetix_host_metrics",
                "Live memory, swap, PSI, load and VRAM of the host running this server",
            )?,
            |Parameters(_): Parameters<Empty>| {
                ready(encoded(&host::metrics(
                    Path::new("/proc"),
                    Path::new("/sys"),
                )))
            },
        ),
    )?;

    server.register(
        Effect::Read,
        ToolRoute::new(
            tool::<CgroupSelection>(
                "fleetix_host_cgroups",
                "Live cgroup v2 memory (current/anon/file/peak/max/swap, OOM events), largest first",
            )?,
            |Parameters(args): Parameters<CgroupSelection>| {
                ready(encoded(&host::cgroups(
                    Path::new("/sys/fs/cgroup"),
                    args.depth.unwrap_or(4).min(8),
                    args.min_bytes.unwrap_or(256 << 20),
                    args.limit.unwrap_or(40),
                )))
            },
        ),
    )?;

    server.register(
        Effect::Read,
        ToolRoute::new(
            tool::<EventSelection>(
                "fleetix_host_events",
                "Kernel OOM kills on this host with cgroup, victim and killed-process counts",
            )?,
            |Parameters(args): Parameters<EventSelection>| async move {
                let since = args.since.unwrap_or_else(|| "48 hours ago".to_owned());
                match tokio::task::spawn_blocking(move || host::oom_events(&since)).await {
                    Ok(Ok(events)) => encoded(&events),
                    Ok(Err(message)) => Ok(CallToolResult::structured_error(
                        serde_json::json!({"code": "journal_unavailable", "message": message}),
                    )),
                    Err(_) => Err(ErrorData::internal_error("journal reader failed", None)),
                }
            },
        ),
    )?;

    let snapshot = Arc::clone(&topology);
    server.register(
        Effect::Read,
        ToolRoute::new(
            tool::<LinkSelection>("fleetix_links", "Inspect links and their derived addresses")?,
            move |Parameters(args): Parameters<LinkSelection>| {
                ready(links(&snapshot, args.name.as_deref()))
            },
        ),
    )?;

    server.register(
        Effect::Read,
        ToolRoute::new(
            tool::<Empty>(
                "fleetix_validate",
                "Validate cross-references in the selected topology",
            )?,
            move |Parameters(_): Parameters<Empty>| ready(validation(&topology)),
        ),
    )?;

    if let Some(config) = supply_config {
        let config = Arc::new(config);
        let consumers_config = Arc::clone(&config);
        server.register(
            Effect::Read,
            ToolRoute::new(
                tool::<SupplyConsumersSelection>(
                    "fleetix_supply_consumers",
                    "Repositories that pin a producer, optionally behind a target revision",
                )?,
                move |Parameters(args): Parameters<SupplyConsumersSelection>| {
                    let config = Arc::clone(&consumers_config);
                    async move {
                        let producer = args.producer;
                        let target = args.target;
                        let direct_only = args.direct_only;
                        blocking(move || {
                            let graph = load_graph(&config)?;
                            serde_json::to_value(consumers_report(
                                &graph,
                                &producer,
                                target.as_deref(),
                                direct_only,
                            ))
                            .map_err(|error| miette::miette!("serialize consumers: {error}"))
                        })
                        .await
                    }
                },
            ),
        )?;
        let refresh_config = Arc::clone(&config);
        server.register(
            Effect::Read,
            ToolRoute::new(
                tool::<Empty>(
                    "fleetix_supply_refresh",
                    "Observe the configured workspace, store the snapshot and return its diff",
                )?,
                move |Parameters(_): Parameters<Empty>| {
                    let config = Arc::clone(&refresh_config);
                    async move { blocking(move || refresh(&config)).await }
                },
            ),
        )?;
        if let Some(lander) = config.land.clone() {
            let lander = Arc::new(lander);
            let land_config = Arc::clone(&config);
            server.register(
                Effect::Write,
                ToolRoute::new(
                    mutation_tool::<SupplyLandSelection>(
                        "fleetix_supply_land",
                        "Run the configured landing command with the work items of behind direct consumers",
                    )?,
                    move |Parameters(args): Parameters<SupplyLandSelection>,
                          context: rmcp::service::RequestContext<rmcp::RoleServer>| {
                        let config = Arc::clone(&land_config);
                        let cancelled = context.ct.clone();
                        let lander = Arc::clone(&lander);
                        let SupplyLandSelection {
                            producer,
                            target,
                            coordinates,
                        } = args;
                        async move {
                            blocking(move || {
                                land(
                                    &config,
                                    &lander,
                                    &producer,
                                    &target,
                                    coordinates.as_deref(),
                                    &cancelled,
                                )
                            })
                            .await
                        }
                    },
                ),
            )?;
        }
    }
    Ok(server)
}

/// Run blocking filesystem and git work off the async runtime and turn a
/// domain failure into a structured tool error.
async fn blocking(
    work: impl FnOnce() -> miette::Result<serde_json::Value> + Send + 'static,
) -> Result<CallToolResult, ErrorData> {
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(value)) => encoded(&value),
        Ok(Err(error)) => Ok(CallToolResult::structured_error(serde_json::json!({
            "code": "supply_failed",
            "message": error.to_string(),
        }))),
        Err(_) => Err(ErrorData::internal_error("supply observer failed", None)),
    }
}

/// The stored snapshot, observing once when none exists yet.
fn load_graph(config: &SupplyConfig) -> miette::Result<supply::Graph> {
    match supply::load_snapshot(&config.root)? {
        Some(graph) => Ok(graph),
        None => store_refresh(config).map(|(graph, _)| graph),
    }
}

fn observe_graph(config: &SupplyConfig) -> miette::Result<supply::Graph> {
    let text = std::fs::read_to_string(&config.inventory)
        .map_err(|error| miette::miette!("read {}: {error}", config.inventory.display()))?;
    let repos: Vec<supply::ObservedRepo> = serde_json::from_str(&text)
        .map_err(|error| miette::miette!("parse {}: {error}", config.inventory.display()))?;
    let mut docs = vec![supply::observe(&config.root, &repos)];
    for path in &config.facts {
        let text = std::fs::read_to_string(path)
            .map_err(|error| miette::miette!("read {}: {error}", path.display()))?;
        docs.extend(supply::parse(&text)?);
    }
    Ok(supply::build(&docs, None))
}

fn store_refresh(config: &SupplyConfig) -> miette::Result<(supply::Graph, supply::DiffReport)> {
    let graph = observe_graph(config)?;
    let previous = supply::load_snapshot(&config.root)?;
    supply::save_snapshot(&config.root, &graph)?;
    let diff = supply::diff(
        previous.as_ref().unwrap_or(&supply::Graph::default()),
        &graph,
    );
    Ok((graph, diff))
}

fn refresh(config: &SupplyConfig) -> miette::Result<serde_json::Value> {
    let (graph, diff) = store_refresh(config)?;
    Ok(serde_json::json!({
        "store": supply::store_path(&config.root),
        "diff": diff,
        "errors": &graph.errors,
        "conflicts": &graph.conflicts,
    }))
}

/// `supply::consumers`, optionally without transitive pins.
fn consumers_report(
    graph: &supply::Graph,
    producer: &str,
    target: Option<&str>,
    direct_only: bool,
) -> supply::ConsumersReport {
    let mut report = supply::consumers(graph, producer, target);
    if direct_only {
        report.items.retain(|item| item.direct == Some(true));
    }
    report
}

/// Capture limit for one stream of the landing command's output.
const CAPTURE_LIMIT: usize = 64 * 1024;

/// A lowercase 40-hex revision, the only target shape a landing command gets.
fn target_revision(target: &str) -> miette::Result<String> {
    if target.len() == 40 && target.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(target.to_ascii_lowercase());
    }
    Err(miette::miette!(
        "target must be a 40-character hex revision, got {target:?}"
    ))
}

/// One work item per behind direct consumer input, optionally restricted to
/// consumer coordinates that must all consume the producer.
fn work_items(
    report: &supply::ConsumersReport,
    target: &str,
    coordinates: Option<&[String]>,
) -> miette::Result<Vec<supply::WorkItem>> {
    if let Some(coordinates) = coordinates {
        let known: std::collections::BTreeSet<&str> = report
            .items
            .iter()
            .map(|item| item.coordinate.as_str())
            .collect();
        let unknown: Vec<String> = coordinates
            .iter()
            .filter(|coordinate| !known.contains(coordinate.as_str()))
            .cloned()
            .collect();
        if !unknown.is_empty() {
            return Err(miette::miette!(
                "not consumers of {}: {}",
                report.producer,
                unknown.join(", ")
            ));
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut items = Vec::new();
    for item in &report.items {
        if item.direct != Some(true) || !item.behind {
            continue;
        }
        if coordinates.is_some_and(|coordinates| !coordinates.contains(&item.coordinate)) {
            continue;
        }
        let (Some(relative_path), Some(input)) =
            (item.relative_path.as_deref(), item.input.as_deref())
        else {
            return Err(miette::miette!(
                "consumer {} has no relative path or input",
                item.coordinate
            ));
        };
        // One item per coordinate: a landing command updates a repository in
        // one transaction. A second direct input of the same producer lands
        // on a later call, once the first no longer shows as behind.
        if !seen.insert(item.coordinate.clone()) {
            continue;
        }
        items.push(supply::WorkItem {
            coordinate: item.coordinate.clone(),
            relative_path: relative_path.to_string(),
            input: input.to_string(),
            target: target.to_string(),
        });
    }
    Ok(items)
}

/// Observe fresh, then hand the behind direct consumers to the configured
/// landing command. A non-zero command exit is a result, not an error.
fn land(
    config: &SupplyConfig,
    lander: &LandConfig,
    producer: &str,
    target: &str,
    coordinates: Option<&[String]>,
    cancelled: &tokio_util::sync::CancellationToken,
) -> miette::Result<serde_json::Value> {
    let target = target_revision(target)?;
    let (graph, _) = store_refresh(config)?;
    // Safe point: a call cancelled while observing never starts the command.
    if cancelled.is_cancelled() {
        return Err(miette::miette!("cancelled before landing"));
    }
    let report = supply::consumers(&graph, producer, Some(&target));
    let items = work_items(&report, &target, coordinates)?;
    if items.is_empty() {
        return Ok(serde_json::json!({
            "items": [],
            "landed": false,
            "reason": "nothing-behind",
        }));
    }
    // One handoff per call, so concurrent calls never overwrite each other's.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let path = supply::store_path(&config.root)
        .with_file_name(format!("work-items-{stamp}-{}.json", std::process::id()));
    let work = supply::WorkItems {
        contract: supply::WORK_ITEMS_CONTRACT,
        producer: report.producer,
        target: target.clone(),
        items,
    };
    save_work_items(&path, &work)?;
    let output = std::process::Command::new(&lander.program)
        .args(&lander.args)
        .arg(&path)
        .output()
        .map_err(|error| miette::miette!("run {}: {error}", lander.program.display()))?;
    let stdout = capped(&output.stdout);
    Ok(serde_json::json!({
        "workItems": path.display().to_string(),
        "items": work.items,
        "exitCode": output.status.code(),
        "success": output.status.success(),
        "stdout": serde_json::from_str(&stdout).unwrap_or(serde_json::Value::String(stdout)),
        "stderr": capped(&output.stderr),
    }))
}

/// Atomically write the work-item handoff with a private mode.
fn save_work_items(path: &Path, work: &supply::WorkItems) -> miette::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(work)
        .map_err(|error| miette::miette!("serialize work items: {error}"))?;
    bytes.push(b'\n');
    crate::fsutil::atomic_write(path, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| miette::miette!("set permissions on {}: {error}", path.display()))?;
    }
    Ok(())
}

fn capped(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(CAPTURE_LIMIT)]).into_owned()
}

fn tool<T: schemars::JsonSchema + 'static>(
    name: &'static str,
    description: &'static str,
) -> Result<Tool, ErrorData> {
    let schema = schema_for_input::<T>().map_err(|error| ErrorData::internal_error(error, None))?;
    Ok(Tool::new(name, description, schema).with_annotations(
        ToolAnnotations::new()
            .read_only(true)
            .destructive(false)
            .idempotent(true)
            .open_world(false),
    ))
}

fn mutation_tool<T: schemars::JsonSchema + 'static>(
    name: &'static str,
    description: &'static str,
) -> Result<Tool, ErrorData> {
    let schema = schema_for_input::<T>().map_err(|error| ErrorData::internal_error(error, None))?;
    Ok(Tool::new(name, description, schema).with_annotations(
        ToolAnnotations::new()
            .read_only(false)
            .destructive(false)
            .idempotent(false)
            .open_world(true),
    ))
}

fn encoded(value: &impl Serialize) -> Result<CallToolResult, ErrorData> {
    serde_json::to_value(value)
        .map(CallToolResult::structured)
        .map_err(|_| ErrorData::internal_error("could not serialize topology result", None))
}

fn show(
    topology: &Topology,
    host: Option<&str>,
    fields: Option<&[String]>,
) -> Result<CallToolResult, ErrorData> {
    let value = match host {
        Some(name) => match topology.hosts.get(name) {
            Some(host) => serde_json::to_value(host),
            None => {
                return Ok(CallToolResult::structured_error(
                    serde_json::json!({"code": "host_not_found"}),
                ));
            }
        },
        None => serde_json::to_value(topology),
    }
    .map_err(|_| ErrorData::internal_error("could not serialize topology result", None))?;
    let (Some(fields), serde_json::Value::Object(object)) = (fields, &value) else {
        return encoded(&value);
    };
    if fields.iter().any(|field| !object.contains_key(field)) {
        return Ok(CallToolResult::structured_error(serde_json::json!({
            "code": "fields_not_found",
            "available": object.keys().collect::<Vec<_>>(),
        })));
    }
    let kept: serde_json::Map<_, _> = object
        .iter()
        .filter(|(key, _)| fields.contains(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    encoded(&kept)
}

fn links(topology: &Topology, name: Option<&str>) -> Result<CallToolResult, ErrorData> {
    if name.is_some_and(|name| !topology.links.contains_key(name)) {
        return Ok(CallToolResult::structured_error(
            serde_json::json!({"code": "link_not_found"}),
        ));
    }
    let views: Vec<super::LinkView> = topology
        .links
        .iter()
        .filter(|(key, _)| name.is_none_or(|name| key.as_str() == name))
        .map(|(name, link)| super::LinkView {
            name: name.clone(),
            subnet: link.subnet.clone(),
            port: link.port,
            cidr: topology.link_cidr(name).map(str::to_owned),
            server: topology.link_server_address(name).map(str::to_owned),
            dial: topology.link_dial(name),
        })
        .collect();
    encoded(&serde_json::json!({"links": views}))
}

fn validation(topology: &Topology) -> Result<CallToolResult, ErrorData> {
    let report = crate::validate(topology);
    let mut result = encoded(&report)?;
    result.is_error = Some(!report.is_ok());
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::{ServiceExt, model::CallToolRequestParams};
    use serde_json::json;

    #[tokio::test]
    async fn snapshot_tools_are_discoverable_and_recover_after_bad_arguments() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let topology = crate::Topology {
                schema_version: 2,
                ..Default::default()
            };
            let (server_io, client_io) = tokio::io::duplex(64 * 1024);
            let serving = tokio::spawn(async move {
                server(topology, None)
                    .unwrap()
                    .serve(server_io)
                    .await
                    .unwrap()
            });
            let client = ().serve(client_io).await.unwrap();
            let running = serving.await.unwrap();
            let names: Vec<_> = client
                .list_all_tools()
                .await
                .unwrap()
                .into_iter()
                .map(|tool| tool.name.into_owned())
                .collect();
            assert_eq!(
                names,
                [
                    "fleetix_host_cgroups",
                    "fleetix_host_events",
                    "fleetix_host_metrics",
                    "fleetix_links",
                    "fleetix_show",
                    "fleetix_validate"
                ]
            );

            let bad = client
                .call_tool(
                    CallToolRequestParams::new("fleetix_show").with_arguments(
                        json!({"path": "/not-an-authorized-input"})
                            .as_object()
                            .unwrap()
                            .clone(),
                    ),
                )
                .await
                .unwrap();
            assert_eq!(bad.is_error, Some(true));
            let shown = client
                .call_tool(CallToolRequestParams::new("fleetix_show"))
                .await
                .unwrap();
            assert_eq!(shown.is_error, Some(false));
            assert_eq!(shown.structured_content.unwrap()["schemaVersion"], 2);
            let checked = client
                .call_tool(CallToolRequestParams::new("fleetix_validate"))
                .await
                .unwrap();
            assert_eq!(checked.is_error, Some(false));
            client.cancel().await.unwrap();
            running.waiting().await.unwrap();
        })
        .await
        .unwrap();
    }

    #[test]
    fn supply_tools_are_registered_only_with_startup_flags() {
        use rmcp::ServerHandler;

        let topology = || crate::Topology {
            schema_version: 2,
            ..Default::default()
        };
        let plain = server(topology(), None).unwrap();
        assert!(plain.get_tool("fleetix_supply_refresh").is_none());

        let configured = server(
            topology(),
            Some(SupplyConfig {
                root: std::path::PathBuf::from("/workspace"),
                inventory: std::path::PathBuf::from("/workspace/inventory.json"),
                facts: Vec::new(),
                land: None,
            }),
        )
        .unwrap();
        assert!(configured.get_tool("fleetix_supply_consumers").is_some());
        assert!(configured.get_tool("fleetix_supply_refresh").is_some());
        assert!(configured.get_tool("fleetix_show").is_some());
        assert!(configured.get_tool("fleetix_supply_land").is_none());

        let landing = server(
            topology(),
            Some(SupplyConfig {
                root: std::path::PathBuf::from("/workspace"),
                inventory: std::path::PathBuf::from("/workspace/inventory.json"),
                facts: Vec::new(),
                land: Some(LandConfig {
                    program: std::path::PathBuf::from("/bin/true"),
                    args: Vec::new(),
                }),
            }),
        )
        .unwrap();
        assert!(landing.get_tool("fleetix_supply_land").is_some());
        assert!(landing.get_tool("fleetix_supply_consumers").is_some());
    }

    fn git(directory: &Path, arguments: &[&str]) {
        let status = std::process::Command::new("git")
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

    /// A throwaway repository with one commit, observed through its tip only.
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

    fn flake_lock(rev: &str) -> String {
        serde_json::json!({
            "nodes": {
                "root": {"inputs": {"harbor-rs": "harbor-rs"}},
                "harbor-rs": {
                    "locked": {
                        "type": "github", "owner": "caniko", "repo": "harbor-rs", "rev": rev
                    },
                    "original": {"type": "github", "owner": "caniko", "repo": "harbor-rs"}
                }
            },
            "root": "root",
            "version": 7
        })
        .to_string()
    }

    /// Keep the snapshot store inside the test's tempdir, restoring the
    /// process environment on drop.
    struct StateHome(Option<std::ffi::OsString>);

    impl StateHome {
        fn set(path: &Path) -> Self {
            let previous = std::env::var_os("XDG_STATE_HOME");
            std::env::set_var("XDG_STATE_HOME", path);
            Self(previous)
        }
    }

    impl Drop for StateHome {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var("XDG_STATE_HOME", value),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
        }
    }

    fn consumer_input(
        coordinate: &str,
        input: &str,
        direct: bool,
        behind: bool,
    ) -> supply::ConsumerItem {
        supply::ConsumerItem {
            coordinate: coordinate.to_string(),
            relative_path: Some(format!("projects/{coordinate}")),
            usage: format!("usage:{coordinate}#flake-input:{input}"),
            input: Some(input.to_string()),
            direct: Some(direct),
            locked_revs: Vec::new(),
            target: Some("b".repeat(40)),
            behind,
        }
    }

    #[test]
    fn target_validation_requires_a_full_hex_revision() {
        assert_eq!(target_revision(&"A".repeat(40)).unwrap(), "a".repeat(40));
        let invalid = [
            String::new(),
            "abc".to_string(),
            "a".repeat(39),
            "a".repeat(41),
            "g".repeat(40),
        ];
        for target in &invalid {
            assert!(
                target_revision(target).is_err(),
                "{target:?} must be rejected"
            );
        }

        // Rejection happens before any observation or command run.
        let config = SupplyConfig {
            root: PathBuf::from("/nonexistent"),
            inventory: PathBuf::from("/nonexistent/inventory.json"),
            facts: Vec::new(),
            land: None,
        };
        let lander = LandConfig {
            program: PathBuf::from("/bin/true"),
            args: Vec::new(),
        };
        assert!(
            land(
                &config,
                &lander,
                "github.com/caniko/harbor-rs",
                "short",
                None,
                &tokio_util::sync::CancellationToken::new(),
            )
            .is_err()
        );
    }

    #[test]
    fn work_items_keep_behind_direct_inputs_and_reject_unknown_coordinates() {
        let target = "b".repeat(40);
        let report = supply::ConsumersReport {
            contract: supply::CONSUMERS_CONTRACT,
            producer: "github.com/caniko/harbor-rs".into(),
            target: Some(target.clone()),
            items: vec![
                consumer_input("github.com/caniko/app", "harbor-rs", true, true),
                // A duplicated usage folds into one item.
                consumer_input("github.com/caniko/app", "harbor-rs", true, true),
                // A second direct input of the same coordinate waits for a later call.
                consumer_input("github.com/caniko/app", "harbor-rs_2", true, true),
                consumer_input("github.com/caniko/current", "harbor-rs", true, false),
                consumer_input("github.com/caniko/other", "harbor-rs", true, true),
                consumer_input("github.com/caniko/transitive", "harbor-rs_2", false, true),
            ],
        };
        let items = work_items(&report, &target, None).unwrap();
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(items[0].coordinate, "github.com/caniko/app");
        assert_eq!(items[0].input, "harbor-rs");
        assert_eq!(items[1].coordinate, "github.com/caniko/other");
        assert!(items.iter().all(|item| item.target == target));

        let app = ["github.com/caniko/app".to_string()];
        assert_eq!(work_items(&report, &target, Some(&app)).unwrap().len(), 1);
        let current = ["github.com/caniko/current".to_string()];
        assert!(
            work_items(&report, &target, Some(&current))
                .unwrap()
                .is_empty(),
            "a consumer already at the target has no work"
        );

        let unknown = [
            "github.com/caniko/app".to_string(),
            "github.com/caniko/missing".to_string(),
        ];
        let error = work_items(&report, &target, Some(&unknown))
            .unwrap_err()
            .to_string();
        assert!(error.contains("github.com/caniko/missing"), "{error}");
        assert!(!error.contains("caniko/app"), "{error}");
    }

    #[test]
    fn consumers_direct_only_drops_transitive_pins() {
        let workspace = tempfile::tempdir().unwrap();
        let rev = "a".repeat(40);
        let harbor = |rev: &str| {
            serde_json::json!({
                "locked": {"type": "github", "owner": "caniko", "repo": "harbor-rs", "rev": rev},
                "original": {"type": "github", "owner": "caniko", "repo": "harbor-rs"}
            })
        };
        let lock = serde_json::json!({
            "nodes": {
                "root": {"inputs": {"harbor-rs": "harbor-rs", "tool": "tool"}},
                "harbor-rs": harbor(&rev),
                "tool": {
                    "locked": {"type": "github", "owner": "caniko", "repo": "tool", "rev": rev},
                    "original": {"type": "github", "owner": "caniko", "repo": "tool"},
                    "inputs": {"harbor-rs": "harbor-rs_2"}
                },
                "harbor-rs_2": harbor(&"b".repeat(40))
            },
            "root": "root",
            "version": 7
        });
        init_repo(
            workspace.path(),
            "app",
            &[("flake.lock", &lock.to_string())],
        );
        let repos: Vec<supply::ObservedRepo> = serde_json::from_value(serde_json::json!([
            {"coordinate": "github.com/caniko/app", "relativePath": "app"}
        ]))
        .unwrap();
        let graph = supply::build(&[supply::observe(workspace.path(), &repos)], None);
        let all = consumers_report(&graph, "github.com/caniko/harbor-rs", None, false);
        assert_eq!(all.items.len(), 2, "{:?}", all.items);
        let direct = consumers_report(&graph, "github.com/caniko/harbor-rs", None, true);
        assert_eq!(direct.items.len(), 1, "{:?}", direct.items);
        assert_eq!(direct.items[0].direct, Some(true));
    }

    #[test]
    fn land_writes_work_items_runs_the_command_and_short_circuits_when_current() {
        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let _state_home = StateHome::set(state.path());
        let pinned = "a".repeat(40);
        let target = "b".repeat(40);
        init_repo(
            workspace.path(),
            "app",
            &[("flake.lock", &flake_lock(&pinned))],
        );
        let inventory = workspace.path().join("inventory.json");
        std::fs::write(
            &inventory,
            serde_json::json!([{
                "coordinate": "github.com/caniko/app",
                "relativePath": "app"
            }])
            .to_string(),
        )
        .unwrap();
        let config = SupplyConfig {
            root: workspace.path().to_path_buf(),
            inventory,
            facts: Vec::new(),
            land: None,
        };
        let copied = workspace.path().join("landed.json");
        let lander = LandConfig {
            program: PathBuf::from("/bin/sh"),
            args: vec![
                "-c".into(),
                r#"cp "$2" "$1" && printf '%s' '{"ok":true}'"#.into(),
                "fleetix-land".into(),
                copied.display().to_string(),
            ],
        };

        let result = land(
            &config,
            &lander,
            "github.com/caniko/harbor-rs",
            &target,
            None,
            &tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(result["success"], true, "{result}");
        assert_eq!(result["exitCode"], 0, "{result}");
        assert_eq!(result["stdout"], serde_json::json!({"ok": true}));
        let handoff = Path::new(result["workItems"].as_str().unwrap());
        assert_eq!(
            handoff.parent(),
            supply::store_path(&config.root).parent(),
            "{result}"
        );
        assert!(
            handoff
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("work-items-"),
            "{result}"
        );

        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&copied).unwrap()).unwrap();
        assert_eq!(written["contract"], supply::WORK_ITEMS_CONTRACT);
        assert_eq!(written["producer"], "github.com/caniko/harbor-rs");
        assert_eq!(written["target"], target);
        assert_eq!(written["items"], result["items"]);
        assert_eq!(
            written["items"][0],
            serde_json::json!({
                "coordinate": "github.com/caniko/app",
                "relativePath": "app",
                "input": "harbor-rs",
                "target": target,
            })
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(result["workItems"].as_str().unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // Nothing behind: the command must not run. A nonexistent program
        // would fail the call if the short-circuit were missing.
        let unreachable = LandConfig {
            program: PathBuf::from("/nonexistent/fleetix-lander"),
            args: Vec::new(),
        };
        // A call cancelled before the safe point never runs the command.
        let cancelled = tokio_util::sync::CancellationToken::new();
        cancelled.cancel();
        let error = land(
            &config,
            &unreachable,
            "github.com/caniko/harbor-rs",
            &target,
            None,
            &cancelled,
        )
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"), "{error}");
        let current = land(
            &config,
            &unreachable,
            "github.com/caniko/harbor-rs",
            &pinned,
            None,
            &tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(
            current,
            serde_json::json!({"items": [], "landed": false, "reason": "nothing-behind"})
        );
    }

    #[test]
    fn topology_tools_filter_and_report_validation_without_side_effects() {
        let topology: crate::Topology = serde_json::from_value(json!({
            "schemaVersion": 2,
            "links": {"mesh": {"subnet": "10.44.0.0/24", "port": 51820}},
            "hosts": {"edge": {"system": "x86_64-linux"}},
            "domains": {}, "services": {}
        }))
        .unwrap();
        let all = show(&topology, None, None).unwrap();
        assert_eq!(
            all.structured_content.unwrap()["hosts"]["edge"]["system"],
            "x86_64-linux"
        );
        let host = show(&topology, Some("edge"), None).unwrap();
        assert_eq!(host.structured_content.unwrap()["system"], "x86_64-linux");
        assert_eq!(
            show(&topology, Some("missing"), None).unwrap().is_error,
            Some(true)
        );
        let only_hosts = show(&topology, None, Some(&["hosts".to_owned()]))
            .unwrap()
            .structured_content
            .unwrap();
        assert_eq!(only_hosts["hosts"]["edge"]["system"], "x86_64-linux");
        assert!(only_hosts.get("links").is_none());
        let unknown = show(
            &topology,
            Some("edge"),
            Some(&["system".to_owned(), "nope".to_owned()]),
        )
        .unwrap();
        assert_eq!(unknown.is_error, Some(true));
        assert_eq!(
            unknown.structured_content.unwrap()["code"],
            "fields_not_found"
        );
        let link = links(&topology, Some("mesh")).unwrap();
        assert_eq!(
            link.structured_content.unwrap()["links"][0]["cidr"],
            "10.44.0.0/24"
        );
        assert_eq!(
            links(&topology, Some("missing")).unwrap().is_error,
            Some(true)
        );
        assert_eq!(validation(&topology).unwrap().is_error, Some(false));
        let mut invalid = topology;
        invalid.schema_version = 1;
        let failure = validation(&invalid).unwrap();
        assert_eq!(failure.is_error, Some(true));
        assert_eq!(
            failure.structured_content.unwrap()["issues"][0]["code"],
            "topology.unsupported_schema_version"
        );
    }
}
