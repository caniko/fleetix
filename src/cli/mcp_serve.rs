//! Read-only tools over one operator-selected, immutable topology snapshot.

use std::{future::ready, sync::Arc};

use crate::Topology;
use crate::mcp_server::{Effect, Server, rmcp};
use rmcp::handler::server::{
    common::schema_for_input, router::tool::ToolRoute, wrapper::Parameters,
};
use rmcp::model::{CallToolResult, Tool, ToolAnnotations};
use rmcp::{ErrorData, schemars};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct HostSelection {
    /// Return a single host instead of the entire topology.
    host: Option<String>,
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

pub(super) fn server(topology: Topology) -> Result<Server, ErrorData> {
    let topology = Arc::new(topology);
    let mut server = Server::new("fleetix", env!("CARGO_PKG_VERSION"))
        .with_authorizer(|call| call.effect == Effect::Read);

    let snapshot = Arc::clone(&topology);
    server.register(
        Effect::Read,
        ToolRoute::new(
            tool::<HostSelection>(
                "fleetix_show",
                "Inspect the selected topology or a single host",
            )?,
            move |Parameters(args): Parameters<HostSelection>| {
                ready(show(&snapshot, args.host.as_deref()))
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
    Ok(server)
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

fn encoded(value: &impl Serialize) -> Result<CallToolResult, ErrorData> {
    serde_json::to_value(value)
        .map(CallToolResult::structured)
        .map_err(|_| ErrorData::internal_error("could not serialize topology result", None))
}

fn show(topology: &Topology, host: Option<&str>) -> Result<CallToolResult, ErrorData> {
    match host {
        Some(name) => match topology.hosts.get(name) {
            Some(host) => encoded(host),
            None => Ok(CallToolResult::structured_error(
                serde_json::json!({"code": "host_not_found"}),
            )),
        },
        None => encoded(topology),
    }
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
            let serving =
                tokio::spawn(
                    async move { server(topology).unwrap().serve(server_io).await.unwrap() },
                );
            let client = ().serve(client_io).await.unwrap();
            let running = serving.await.unwrap();
            let names: Vec<_> = client
                .list_all_tools()
                .await
                .unwrap()
                .into_iter()
                .map(|tool| tool.name.into_owned())
                .collect();
            assert_eq!(names, ["fleetix_links", "fleetix_show", "fleetix_validate"]);

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
    fn topology_tools_filter_and_report_validation_without_side_effects() {
        let topology: crate::Topology = serde_json::from_value(json!({
            "schemaVersion": 2,
            "links": {"mesh": {"subnet": "10.44.0.0/24", "port": 51820}},
            "hosts": {"edge": {"system": "x86_64-linux"}},
            "domains": {}, "services": {}
        }))
        .unwrap();
        let all = show(&topology, None).unwrap();
        assert_eq!(
            all.structured_content.unwrap()["hosts"]["edge"]["system"],
            "x86_64-linux"
        );
        let host = show(&topology, Some("edge")).unwrap();
        assert_eq!(host.structured_content.unwrap()["system"], "x86_64-linux");
        assert_eq!(
            show(&topology, Some("missing")).unwrap().is_error,
            Some(true)
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
