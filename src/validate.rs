use crate::topology::{
    DnsPublication, EndpointBind, HttpAccess, HttpAction, IngressScope, LinkRole, PathMatch,
    Topology, VpnConnection, VpnPortForwarding,
};
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ValidationSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidationIssue {
    pub severity: ValidationSeverity,
    pub code: String,
    pub path: Option<String>,
    pub value: Option<String>,
    pub message: String,
}

#[derive(Debug, Default, Serialize)]
pub struct ValidationReport {
    pub issues: Vec<ValidationIssue>,
}

impl ValidationReport {
    pub fn is_ok(&self) -> bool {
        self.issues
            .iter()
            .all(|issue| issue.severity != ValidationSeverity::Error)
    }

    pub fn issues(&self) -> impl Iterator<Item = &ValidationIssue> {
        self.issues.iter()
    }

    pub fn errors(&self) -> impl Iterator<Item = &ValidationIssue> {
        self.issues
            .iter()
            .filter(|issue| issue.severity == ValidationSeverity::Error)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &ValidationIssue> {
        self.issues
            .iter()
            .filter(|issue| issue.severity == ValidationSeverity::Warning)
    }

    fn error(
        &mut self,
        code: impl Into<String>,
        path: Option<String>,
        value: Option<String>,
        message: impl Into<String>,
    ) {
        self.issues.push(ValidationIssue {
            severity: ValidationSeverity::Error,
            code: code.into(),
            path,
            value,
            message: message.into(),
        });
    }

    fn warning(
        &mut self,
        code: impl Into<String>,
        path: Option<String>,
        value: Option<String>,
        message: impl Into<String>,
    ) {
        self.issues.push(ValidationIssue {
            severity: ValidationSeverity::Warning,
            code: code.into(),
            path,
            value,
            message: message.into(),
        });
    }
}

/// Cross-reference validation of a Topology.
pub fn validate(topology: &Topology) -> ValidationReport {
    let mut report = ValidationReport::default();

    if topology.schema_version != 2 {
        report.error(
            "topology.unsupported_schema_version",
            Some("schemaVersion".into()),
            Some(topology.schema_version.to_string()),
            format!(
                "topology schemaVersion must be 2, got {}",
                topology.schema_version
            ),
        );
    }

    validate_identifiers(topology, &mut report);
    validate_domains(topology, &mut report);
    validate_host_hardware(topology, &mut report);
    validate_vpn_profiles(topology, &mut report);
    validate_deployment(topology, &mut report);
    validate_services(topology, &mut report);

    // 1. Every link has at most one server
    for (link_name, link) in &topology.links {
        if link.port == 0 {
            report.error(
                "link.invalid_port",
                Some(format!("links.{link_name}.port")),
                Some("0".into()),
                format!("link '{link_name}' must use a non-zero port"),
            );
        }
        if parse_cidr(&link.subnet).is_none() {
            report.error(
                "link.invalid_subnet",
                Some(format!("links.{link_name}.subnet")),
                Some(link.subnet.clone()),
                format!("link '{link_name}' has invalid subnet '{}'", link.subnet),
            );
        }
        let servers: Vec<_> = topology
            .hosts
            .iter()
            .filter(|(_, host)| {
                host.links
                    .get(link_name)
                    .is_some_and(|b| b.role == crate::topology::LinkRole::Server)
            })
            .collect();

        if servers.is_empty() {
            report.warning(
                "link.no_server",
                Some(format!("links.{link_name}")),
                None,
                format!(
                    "link '{}' has no server — this is fine for peer-to-peer links",
                    link_name
                ),
            );
        } else if servers.len() > 1 {
            report.error(
                "link.multiple_servers",
                Some(format!("links.{link_name}")),
                Some(servers.len().to_string()),
                format!(
                    "link '{}' has {} servers — at most 1 allowed",
                    link_name,
                    servers.len()
                ),
            );
        }

        let mut addresses = HashSet::new();
        for (host_name, host) in &topology.hosts {
            let Some(binding) = host.links.get(link_name) else {
                continue;
            };
            if !addresses.insert(binding.address.as_str()) {
                report.error(
                    "link.duplicate_address",
                    Some(format!("hosts.{host_name}.links.{link_name}.address")),
                    Some(binding.address.clone()),
                    format!(
                        "link '{link_name}' has duplicate address '{}' (host '{host_name}')",
                        binding.address
                    ),
                );
            }
            if let Some((network, prefix)) = parse_cidr(&link.subnet) {
                match binding.address.parse::<IpAddr>() {
                    Ok(address) if address_in_subnet(address, network, prefix) => {}
                    Ok(_) => report.error(
                        "link.address_outside_subnet",
                        Some(format!("hosts.{host_name}.links.{link_name}.address")),
                        Some(binding.address.clone()),
                        format!(
                            "host '{host_name}' address '{}' is outside link '{link_name}' subnet '{}'",
                            binding.address, link.subnet
                        ),
                    ),
                    Err(_) => report.error(
                        "link.invalid_address",
                        Some(format!("hosts.{host_name}.links.{link_name}.address")),
                        Some(binding.address.clone()),
                        format!(
                            "host '{host_name}' on link '{link_name}' has invalid address '{}'",
                            binding.address
                        ),
                    ),
                }
            }
            if binding.role == LinkRole::Server && binding.public_key.is_none() {
                report.error(
                    "link.server_missing_public_key",
                    Some(format!("hosts.{host_name}.links.{link_name}.publicKey")),
                    None,
                    format!("server '{host_name}' on link '{link_name}' must declare publicKey"),
                );
            }
        }
    }

    // Bindings must not introduce undeclared links.
    for (host_name, host) in &topology.hosts {
        for peer in &host.network.direct_link_peers {
            if !topology.hosts.contains_key(peer) {
                report.error(
                    "host.invalid_direct_peer",
                    Some(format!("hosts.{host_name}.network.directLinkPeers")),
                    Some(peer.clone()),
                    format!("host '{host_name}' references unknown direct-link peer '{peer}'"),
                );
            }
        }
        for link_name in host.links.keys() {
            if !topology.links.contains_key(link_name) {
                report.error(
                    "host.undeclared_link",
                    Some(format!("hosts.{host_name}.links.{link_name}")),
                    Some(link_name.clone()),
                    format!("host '{host_name}' has binding for undeclared link '{link_name}'"),
                );
            }
        }
    }

    // 2. Every rebuild.build_host references an existing host
    for (host_name, host) in &topology.hosts {
        if let Some(ref build_host) = host.rebuild.build_host {
            if !topology.hosts.contains_key(build_host) {
                report.error(
                    "host.invalid_build_host",
                    Some(format!("hosts.{host_name}.rebuild.buildHost")),
                    Some(build_host.clone()),
                    format!(
                        "host '{host_name}' has rebuild.buildHost '{build_host}' which does not exist in hosts"
                    ),
                );
            }
        }
    }

    // 3. Every dynamic host FQDN is in a declared managed zone. An empty
    // managed-zone list means the general zone list is authoritative.
    let validation_zones = if topology.domains.managed_zones.is_empty() {
        &topology.domains.zones
    } else {
        &topology.domains.managed_zones
    };
    for dynamic_host in &topology.domains.dynamic_hosts {
        if !valid_hostname(&dynamic_host.fqdn) {
            report.error(
                "dns.invalid_hostname",
                Some(format!("domains.dynamicHosts.{}.fqdn", dynamic_host.fqdn)),
                Some(dynamic_host.fqdn.clone()),
                format!(
                    "dynamic host '{}' is not a valid hostname",
                    dynamic_host.fqdn
                ),
            );
        }
        let in_managed = validation_zones
            .iter()
            .any(|zone| Topology::host_in_zone(&dynamic_host.fqdn, zone));
        if !in_managed && !validation_zones.is_empty() {
            report.warning(
                "dns.dynamic_host_outside_managed_zone",
                Some(format!("domains.dynamicHosts.{}.fqdn", dynamic_host.fqdn)),
                Some(dynamic_host.fqdn.clone()),
                format!(
                    "dynamic host '{}' is not in any managed zone",
                    dynamic_host.fqdn
                ),
            );
        }
        if let Some(explicit_zone) = &dynamic_host.zone {
            if !Topology::host_in_zone(&dynamic_host.fqdn, explicit_zone) {
                report.error(
                    "dns.dynamic_host_zone_mismatch",
                    Some(format!("domains.dynamicHosts.{}.zone", dynamic_host.fqdn)),
                    Some(explicit_zone.clone()),
                    format!(
                        "dynamic host '{}' declares zone '{}' but is not inside that zone",
                        dynamic_host.fqdn, explicit_zone
                    ),
                );
            }
            if !validation_zones.contains(explicit_zone) {
                report.error(
                    "dns.dynamic_host_undeclared_zone",
                    Some(format!("domains.dynamicHosts.{}.zone", dynamic_host.fqdn)),
                    Some(explicit_zone.clone()),
                    format!(
                        "dynamic host '{}' declares undeclared zone '{}'",
                        dynamic_host.fqdn, explicit_zone
                    ),
                );
            }
        }
    }

    for redirect in &topology.domains.redirects {
        if !valid_hostname(&redirect.from) {
            report.error(
                "redirect.invalid_source",
                Some(format!("domains.redirects.{}.from", redirect.from)),
                Some(redirect.from.clone()),
                format!(
                    "redirect source '{}' is not a valid hostname",
                    redirect.from
                ),
            );
        }
        if redirect.to.trim().is_empty() {
            report.error(
                "redirect.invalid_target",
                Some(format!("domains.redirects.{}.to", redirect.from)),
                Some(redirect.to.clone()),
                format!("redirect '{}' has an empty target", redirect.from),
            );
        }
        if !(300..=399).contains(&redirect.status) {
            report.error(
                "redirect.invalid_status",
                Some(format!("domains.redirects.{}.status", redirect.from)),
                Some(redirect.status.to_string()),
                format!(
                    "redirect '{}' has invalid status {} (expected 300..=399)",
                    redirect.from, redirect.status
                ),
            );
        }
    }

    // 4. Every link client has a public_key if it's not the only link participant
    for (link_name, _link) in &topology.links {
        let participant_count = topology
            .hosts
            .values()
            .filter(|h| h.links.contains_key(link_name))
            .count();
        if participant_count > 1 {
            for (host_name, host) in &topology.hosts {
                if let Some(binding) = host.links.get(link_name) {
                    if binding.role != LinkRole::Server && binding.public_key.is_none() {
                        report.warning(
                            "link.client_missing_public_key",
                            Some(format!("hosts.{host_name}.links.{link_name}.publicKey")),
                            None,
                            format!(
                                "host '{}' on link '{}' has no publicKey — WireGuard will not work",
                                host_name, link_name
                            ),
                        );
                    }
                }
            }
        }
    }

    report
}

fn validate_services(topology: &Topology, report: &mut ValidationReport) {
    for (name, endpoint) in &topology.services.endpoints {
        let base = format!("services.endpoints.{name}");
        if !topology.hosts.contains_key(&endpoint.target_host) {
            report.error(
                "endpoint.unknown_target_host",
                Some(format!("{base}.targetHost")),
                Some(endpoint.target_host.clone()),
                format!(
                    "endpoint '{name}' references unknown host '{}'",
                    endpoint.target_host
                ),
            );
        }
        if endpoint.port == 0 {
            report.error(
                "endpoint.invalid_port",
                Some(format!("{base}.port")),
                Some("0".into()),
                format!("endpoint '{name}' must use a non-zero port"),
            );
        }
        if endpoint.tls_server_name.is_some()
            && endpoint.transport != crate::topology::EndpointTransport::Https
        {
            report.error(
                "endpoint.sni_without_https",
                Some(format!("{base}.tlsServerName")),
                endpoint.tls_server_name.clone(),
                format!("endpoint '{name}' may set tlsServerName only for https transport"),
            );
        }
        if let Some(remote_name) = &endpoint.remote_via {
            if remote_name == name {
                report.error(
                    "endpoint.remote_via_self",
                    Some(format!("{base}.remoteVia")),
                    Some(remote_name.clone()),
                    format!("endpoint '{name}' cannot use itself as remoteVia"),
                );
            } else if let Some(remote) = topology.services.endpoints.get(remote_name) {
                if remote.bind != EndpointBind::Lan || !remote.transport.is_http() {
                    report.error(
                        "endpoint.invalid_remote_via",
                        Some(format!("{base}.remoteVia")),
                        Some(remote_name.clone()),
                        format!(
                            "endpoint '{name}' remoteVia '{remote_name}' must bind to lan with HTTP-capable transport"
                        ),
                    );
                }
            } else {
                report.error(
                    "endpoint.unknown_remote_via",
                    Some(format!("{base}.remoteVia")),
                    Some(remote_name.clone()),
                    format!("endpoint '{name}' references unknown remoteVia '{remote_name}'"),
                );
            }
        }
    }

    let mut hostnames = HashSet::new();
    for (name, site) in &topology.services.http_sites {
        let base = format!("services.httpSites.{name}");
        if !valid_hostname(&site.hostname) {
            report.error(
                "site.invalid_hostname",
                Some(format!("{base}.hostname")),
                Some(site.hostname.clone()),
                format!(
                    "HTTP site '{name}' has invalid hostname '{}'",
                    site.hostname
                ),
            );
        }
        if !hostnames.insert(site.hostname.as_str()) {
            report.error(
                "site.duplicate_hostname",
                Some(format!("{base}.hostname")),
                Some(site.hostname.clone()),
                format!(
                    "HTTP site hostname '{}' is declared more than once",
                    site.hostname
                ),
            );
        }
        if site.dns_publication == DnsPublication::Managed
            && topology.zone_for_host(&site.hostname).is_none()
        {
            report.error(
                "site.managed_hostname_outside_zone",
                Some(format!("{base}.hostname")),
                Some(site.hostname.clone()),
                format!(
                    "managed HTTP site '{}' is outside declared DNS zones",
                    site.hostname
                ),
            );
        }

        let ingress = topology.deployment.ingress_groups.get(&site.ingress);
        if let Some(ingress) = ingress {
            let expected = match site.access {
                HttpAccess::Vpn => IngressScope::Vpn,
                HttpAccess::Cloudflare | HttpAccess::Direct => IngressScope::Public,
            };
            if ingress.scope != expected {
                report.error(
                    "site.ingress_scope_mismatch",
                    Some(format!("{base}.ingress")),
                    Some(site.ingress.clone()),
                    format!("HTTP site '{name}' access does not match ingress scope"),
                );
            }
        } else {
            report.error(
                "site.unknown_ingress",
                Some(format!("{base}.ingress")),
                Some(site.ingress.clone()),
                format!(
                    "HTTP site '{name}' references unknown ingress '{}'",
                    site.ingress
                ),
            );
        }
        if site.access == HttpAccess::Vpn && site.dns_publication == DnsPublication::Managed {
            report.error(
                "site.vpn_managed_dns",
                Some(format!("{base}.dnsPublication")),
                Some("managed".into()),
                format!("VPN HTTP site '{name}' cannot use managed public DNS"),
            );
        }

        validate_http_routes(topology, name, site, ingress, report);
    }
}

fn validate_http_routes(
    topology: &Topology,
    site_name: &str,
    site: &crate::topology::HttpSite,
    ingress: Option<&crate::topology::IngressGroup>,
    report: &mut ValidationReport,
) {
    let base = format!("services.httpSites.{site_name}.routes");
    if site.routes.is_empty() {
        report.error(
            "site.routes_empty",
            Some(base),
            None,
            format!("HTTP site '{site_name}' must declare at least one route"),
        );
        return;
    }

    let mut signatures = HashSet::new();
    let mut fallbacks = Vec::new();
    for (index, route) in site.routes.iter().enumerate() {
        let path = format!("{base}.{index}");
        if route.matcher.paths.is_empty() && route.matcher.absent_query_params.is_empty() {
            fallbacks.push(index);
        }

        let mut path_signature: Vec<_> = route
            .matcher
            .paths
            .iter()
            .map(|matcher| match matcher {
                PathMatch::Exact { value } => (0, value.clone()),
                PathMatch::Prefix { value } => (1, value.clone()),
            })
            .collect();
        path_signature.sort();
        let mut query_signature = route.matcher.absent_query_params.clone();
        query_signature.sort();
        if !signatures.insert((path_signature, query_signature)) {
            report.error(
                "site.route_duplicate_match",
                Some(format!("{path}.match")),
                None,
                format!("HTTP site '{site_name}' has duplicate route match signatures"),
            );
        }

        for (matcher_index, matcher) in route.matcher.paths.iter().enumerate() {
            if !matcher.value().starts_with('/') {
                report.error(
                    "site.route_path_not_absolute",
                    Some(format!("{path}.match.paths.{matcher_index}.value")),
                    Some(matcher.value().to_string()),
                    format!("HTTP site '{site_name}' route paths must be absolute"),
                );
            }
        }
        for (query_index, query) in route.matcher.absent_query_params.iter().enumerate() {
            if query.trim().is_empty() {
                report.error(
                    "site.route_empty_absent_query_param",
                    Some(format!("{path}.match.absentQueryParams.{query_index}")),
                    Some(query.clone()),
                    format!("HTTP site '{site_name}' absent query parameters must not be empty"),
                );
            }
        }
        if route
            .auth_policy
            .as_deref()
            .is_some_and(|policy| policy.trim().is_empty())
        {
            report.error(
                "site.route_empty_auth_policy",
                Some(format!("{path}.authPolicy")),
                route.auth_policy.clone(),
                format!("HTTP site '{site_name}' authPolicy must not be empty"),
            );
        }
        for header in route.response_headers.keys() {
            if header.trim().is_empty() {
                report.error(
                    "site.route_empty_response_header",
                    Some(format!("{path}.responseHeaders")),
                    Some(header.clone()),
                    format!("HTTP site '{site_name}' response header names must not be empty"),
                );
            }
        }

        match &route.action {
            HttpAction::Proxy {
                endpoint,
                strip_prefix,
            } => {
                if let Some(strip_prefix) = strip_prefix {
                    if !strip_prefix.starts_with('/') {
                        report.error(
                            "site.route_strip_prefix_not_absolute",
                            Some(format!("{path}.action.stripPrefix")),
                            Some(strip_prefix.clone()),
                            format!("HTTP site '{site_name}' stripPrefix must be absolute"),
                        );
                    }
                }
                if let Some(target) = topology.services.endpoints.get(endpoint) {
                    if !target.transport.is_http() {
                        report.error(
                            "site.route_non_http_endpoint",
                            Some(format!("{path}.action.endpoint")),
                            Some(endpoint.clone()),
                            format!(
                                "HTTP site '{site_name}' cannot proxy to TCP endpoint '{endpoint}'"
                            ),
                        );
                    }
                    if target.bind == EndpointBind::Loopback {
                        if let Some(ingress) = ingress {
                            for ingress_host in &ingress.hosts {
                                if ingress_host != &target.target_host
                                    && target.remote_via.is_none()
                                {
                                    report.error(
                                        "site.route_unreachable_endpoint",
                                        Some(format!("{path}.action.endpoint")),
                                        Some(endpoint.clone()),
                                        format!(
                                            "HTTP site '{site_name}' ingress host '{ingress_host}' cannot reach loopback endpoint '{endpoint}' without remoteVia"
                                        ),
                                    );
                                }
                            }
                        }
                    }
                } else {
                    report.error(
                        "site.route_unknown_endpoint",
                        Some(format!("{path}.action.endpoint")),
                        Some(endpoint.clone()),
                        format!("HTTP site '{site_name}' references unknown endpoint '{endpoint}'"),
                    );
                }
            }
            HttpAction::Files { root_ref, .. } if root_ref.trim().is_empty() => report.error(
                "site.route_empty_root_ref",
                Some(format!("{path}.action.rootRef")),
                Some(root_ref.clone()),
                format!("HTTP site '{site_name}' files rootRef must not be empty"),
            ),
            HttpAction::Redirect { to, status, .. } => {
                if to.trim().is_empty() {
                    report.error(
                        "site.route_empty_redirect",
                        Some(format!("{path}.action.to")),
                        Some(to.clone()),
                        format!("HTTP site '{site_name}' redirect target must not be empty"),
                    );
                }
                if !(300..=399).contains(status) {
                    report.error(
                        "site.route_invalid_redirect_status",
                        Some(format!("{path}.action.status")),
                        Some(status.to_string()),
                        format!("HTTP site '{site_name}' redirect status must be 300..=399"),
                    );
                }
            }
            HttpAction::Respond { status, .. } if !(100..=599).contains(status) => report.error(
                "site.route_invalid_response_status",
                Some(format!("{path}.action.status")),
                Some(status.to_string()),
                format!("HTTP site '{site_name}' response status must be 100..=599"),
            ),
            _ => {}
        }
    }

    if fallbacks.len() != 1 {
        report.error(
            "site.route_fallback_count",
            Some(base.clone()),
            Some(fallbacks.len().to_string()),
            format!("HTTP site '{site_name}' must declare exactly one fallback route"),
        );
    } else if fallbacks[0] + 1 != site.routes.len() {
        report.error(
            "site.route_fallback_not_last",
            Some(format!("{base}.{}", fallbacks[0])),
            None,
            format!("HTTP site '{site_name}' fallback route must be last"),
        );
    }
}

fn validate_deployment(topology: &Topology, report: &mut ValidationReport) {
    const AVAILABILITY_CLASSES: &[&str] = &["unknown", "always-on", "intermittent", "maintenance"];

    for (host_name, host) in &topology.hosts {
        if !host.availability_class.is_empty()
            && !AVAILABILITY_CLASSES.contains(&host.availability_class.as_str())
        {
            report.error(
                "host.invalid_availability_class",
                Some(format!("hosts.{host_name}.availabilityClass")),
                Some(host.availability_class.clone()),
                format!(
                    "host '{host_name}' uses unsupported availability class '{}'",
                    host.availability_class
                ),
            );
        }
    }

    for (name, group) in &topology.deployment.ingress_groups {
        if group.hosts.is_empty() {
            report.error(
                "ingress.empty_hosts",
                Some(format!("deployment.ingressGroups.{name}.hosts")),
                None,
                format!("ingress group '{name}' must contain at least one host"),
            );
        }
        for host in &group.hosts {
            if !topology.hosts.contains_key(host) {
                report.error(
                    "ingress.unknown_host",
                    Some(format!("deployment.ingressGroups.{name}.hosts")),
                    Some(host.clone()),
                    format!("ingress group '{name}' references unknown host '{host}'"),
                );
            }
        }
    }

    let declared_services: HashSet<&str> = topology
        .services
        .endpoints
        .keys()
        .chain(topology.services.http_sites.keys())
        .map(String::as_str)
        .collect();

    let intents = &topology.deployment.service_intents;
    let mut intent_indices = HashMap::new();
    for (index, intent) in intents.iter().enumerate() {
        if intent.name.trim().is_empty() {
            report.error(
                "deployment.empty_intent_name",
                Some(format!("deployment.serviceIntents[{index}].name")),
                Some(intent.name.clone()),
                "service intent names must not be empty",
            );
        }
        if intent_indices.insert(intent.name.clone(), index).is_some() {
            report.error(
                "deployment.duplicate_intent",
                Some(format!("deployment.serviceIntents[{index}].name")),
                Some(intent.name.clone()),
                format!(
                    "service intent '{}' is declared more than once",
                    intent.name
                ),
            );
        }
        if let Some(service_name) = &intent.service_name {
            if !declared_services.contains(service_name.as_str()) {
                report.error(
                    "deployment.unknown_service",
                    Some(format!("deployment.serviceIntents[{index}].serviceName")),
                    Some(service_name.clone()),
                    format!(
                        "service intent '{}' references unknown service '{}',",
                        intent.name, service_name
                    ),
                );
            }
        }
        if let Some(availability) = &intent.required_availability {
            if !AVAILABILITY_CLASSES.contains(&availability.as_str()) {
                report.error(
                    "deployment.invalid_required_availability",
                    Some(format!(
                        "deployment.serviceIntents[{index}].requiredAvailability"
                    )),
                    Some(availability.clone()),
                    format!(
                        "service intent '{}' uses unsupported required availability '{}'",
                        intent.name, availability
                    ),
                );
            }
        }
        for (field, hosts) in [
            ("requiredHosts", &intent.required_hosts),
            ("preferredHosts", &intent.preferred_hosts),
        ] {
            for host_name in hosts {
                if !topology.hosts.contains_key(host_name) {
                    report.error(
                        "deployment.unknown_host",
                        Some(format!("deployment.serviceIntents[{index}].{field}")),
                        Some(host_name.clone()),
                        format!(
                            "service intent '{}' references unknown host '{}'",
                            intent.name, host_name
                        ),
                    );
                }
            }
        }
        if intent.health.required && intent.health.endpoint.as_deref().is_none_or(str::is_empty) {
            report.error(
                "deployment.health_endpoint_required",
                Some(format!(
                    "deployment.serviceIntents[{index}].health.endpoint"
                )),
                None,
                format!(
                    "service intent '{}' requires a health endpoint when health.required is true",
                    intent.name
                ),
            );
        }
    }

    let mut dependents = vec![Vec::new(); intents.len()];
    let mut indegree = vec![0usize; intents.len()];
    for (index, intent) in intents.iter().enumerate() {
        for dependency in &intent.depends_on {
            let Some(&dependency_index) = intent_indices.get(dependency) else {
                report.error(
                    "deployment.unknown_dependency",
                    Some(format!("deployment.serviceIntents[{index}].dependsOn")),
                    Some(dependency.clone()),
                    format!(
                        "service intent '{}' depends on unknown intent '{}'",
                        intent.name, dependency
                    ),
                );
                continue;
            };
            if dependency_index == index {
                report.error(
                    "deployment.self_dependency",
                    Some(format!("deployment.serviceIntents[{index}].dependsOn")),
                    Some(dependency.clone()),
                    format!("service intent '{}' cannot depend on itself", intent.name),
                );
                continue;
            }
            dependents[dependency_index].push(index);
            indegree[index] += 1;
        }
    }

    let mut ready = VecDeque::new();
    for (index, degree) in indegree.iter().enumerate() {
        if *degree == 0 {
            ready.push_back(index);
        }
    }
    let mut processed = 0;
    while let Some(index) = ready.pop_front() {
        processed += 1;
        for dependent in &dependents[index] {
            indegree[*dependent] -= 1;
            if indegree[*dependent] == 0 {
                ready.push_back(*dependent);
            }
        }
    }
    if processed != intents.len() {
        report.error(
            "deployment.dependency_cycle",
            Some("deployment.serviceIntents".to_string()),
            None,
            "service intent dependencies must form an acyclic graph",
        );
    }
}

fn validate_identifiers(topology: &Topology, report: &mut ValidationReport) {
    let mut aliases = HashSet::new();
    for (host_name, host) in &topology.hosts {
        if host_name.trim().is_empty() || host_name.contains('.') || host_name.contains('/') {
            report.error(
                "host.invalid_name",
                Some(format!("hosts.{host_name}")),
                Some(host_name.clone()),
                format!("host identifier '{host_name}' is invalid"),
            );
        }
        if host.system.trim().is_empty() {
            report.error(
                "host.invalid_system",
                Some(format!("hosts.{host_name}.system")),
                None,
                format!("host '{host_name}' must declare system"),
            );
        }
        for alias in &host.host_names {
            if !valid_hostname(alias) && alias != host_name {
                report.error(
                    "host.invalid_alias",
                    Some(format!("hosts.{host_name}.hostNames")),
                    Some(alias.clone()),
                    format!("host '{host_name}' has invalid alias '{alias}'"),
                );
            }
            if !aliases.insert(alias) {
                report.error(
                    "host.duplicate_alias",
                    Some(format!("hosts.{host_name}.hostNames")),
                    Some(alias.clone()),
                    format!("host alias '{alias}' is declared more than once"),
                );
            }
        }
    }
    let mut zones = HashSet::new();
    for zone in topology
        .domains
        .zones
        .iter()
        .chain(topology.domains.managed_zones.iter())
    {
        if !valid_hostname(zone) {
            report.error(
                "dns.invalid_zone",
                Some("domains.zones".into()),
                Some(zone.clone()),
                format!("zone '{zone}' is not a valid hostname"),
            );
        }
        if !zones.insert(zone) {
            report.error(
                "dns.duplicate_zone",
                Some("domains.zones".into()),
                Some(zone.clone()),
                format!("zone '{zone}' is declared more than once"),
            );
        }
    }
}

fn validate_domains(topology: &Topology, report: &mut ValidationReport) {
    for zone in &topology.domains.managed_zones {
        if !topology.domains.zones.contains(zone) {
            report.error(
                "dns.managed_zone_undeclared",
                Some("domains.managedZones".into()),
                Some(zone.clone()),
                format!("managed zone '{zone}' is not in domains.zones"),
            );
        }
    }
    let mut fqdns = HashSet::new();
    for host in &topology.domains.dynamic_hosts {
        if !fqdns.insert(&host.fqdn) {
            report.error(
                "dns.duplicate_dynamic_host",
                Some("domains.dynamicHosts".into()),
                Some(host.fqdn.clone()),
                format!("dynamic host '{}' is declared more than once", host.fqdn),
            );
        }
    }
    for site in &topology.domains.pages_sites {
        if !valid_hostname(&site.subdomain) {
            report.error(
                "pages.invalid_subdomain",
                Some("domains.pagesSites".into()),
                Some(site.subdomain.clone()),
                format!("Pages relative hostname '{}' is invalid", site.subdomain),
            );
        }
        if !valid_hostname(&site.cname_target) {
            report.error(
                "pages.invalid_cname_target",
                Some("domains.pagesSites".into()),
                Some(site.cname_target.clone()),
                format!("Pages CNAME target '{}' is invalid", site.cname_target),
            );
        }
        if !site.repository.contains('/') {
            report.error(
                "pages.invalid_repository",
                Some("domains.pagesSites".into()),
                Some(site.repository.clone()),
                format!(
                    "Pages repository '{}' must be owner/repository",
                    site.repository
                ),
            );
        }
    }
}

fn validate_host_hardware(topology: &Topology, report: &mut ValidationReport) {
    for (host_name, host) in &topology.hosts {
        for (field, value) in [
            ("dataRoot", host.storage.data_root.as_deref()),
            (
                "projectStateRoot",
                host.storage.project_state_root.as_deref(),
            ),
            ("flakeRoot", host.storage.flake_root.as_deref()),
        ] {
            if let Some(value) = value {
                if !Path::new(value).is_absolute() {
                    report.error(
                        "host.relative_storage_path",
                        Some(format!("hosts.{host_name}.storage.{field}")),
                        Some(value.to_string()),
                        format!("host '{host_name}' storage.{field} must be an absolute path"),
                    );
                }
            }
        }

        if let Some(media) = &host.gpu.media {
            if !matches!(media.vendor.as_str(), "amd" | "intel" | "nvidia") {
                report.error(
                    "host.invalid_gpu_media_vendor",
                    Some(format!("hosts.{host_name}.gpu.media.vendor")),
                    Some(media.vendor.clone()),
                    format!(
                        "host '{host_name}' GPU media vendor '{}' is unsupported",
                        media.vendor
                    ),
                );
            }
            if !media.render_node.starts_with("/dev/dri/") {
                report.error(
                    "host.invalid_gpu_media_render_node",
                    Some(format!("hosts.{host_name}.gpu.media.renderNode")),
                    Some(media.render_node.clone()),
                    format!("host '{host_name}' GPU media renderNode must be under /dev/dri/"),
                );
            }
            if media.libva_driver.trim().is_empty() {
                report.error(
                    "host.empty_gpu_media_driver",
                    Some(format!("hosts.{host_name}.gpu.media.libvaDriver")),
                    Some(media.libva_driver.clone()),
                    format!("host '{host_name}' GPU media libvaDriver must not be empty"),
                );
            }
        }
    }
}

fn validate_vpn_profiles(topology: &Topology, report: &mut ValidationReport) {
    for (host_name, host) in &topology.hosts {
        for (profile_name, profile) in &host.vpn_profiles {
            let base = format!("hosts.{host_name}.vpnProfiles.{profile_name}");
            if profile.provider.trim().is_empty() {
                report.error(
                    "vpn.empty_provider",
                    Some(format!("{base}.provider")),
                    Some(profile.provider.clone()),
                    format!(
                        "VPN profile '{profile_name}' on host '{host_name}' must declare provider"
                    ),
                );
            }
            if let Some(owner) = &profile.owner {
                if !host.users.contains_key(owner) {
                    report.error(
                        "vpn.unknown_owner",
                        Some(format!("{base}.owner")),
                        Some(owner.clone()),
                        format!(
                            "VPN profile '{profile_name}' on host '{host_name}' references unknown user '{owner}'"
                        ),
                    );
                }
            }
            for (index, server) in profile.dns_servers.iter().enumerate() {
                if server.parse::<IpAddr>().is_err() {
                    report.error(
                        "vpn.invalid_dns_server",
                        Some(format!("{base}.dnsServers.{index}")),
                        Some(server.clone()),
                        format!("VPN profile '{profile_name}' has invalid DNS server '{server}'"),
                    );
                }
            }
            if profile.dns_servers.is_empty() {
                report.error(
                    "vpn.empty_dns_servers",
                    Some(format!("{base}.dnsServers")),
                    None,
                    format!("VPN profile '{profile_name}' must declare at least one DNS server"),
                );
            }

            match &profile.connection {
                VpnConnection::WireGuard(connection) => {
                    if connection.addresses.is_empty() {
                        report.error(
                            "vpn.empty_addresses",
                            Some(format!("{base}.connection.addresses")),
                            None,
                            format!(
                                "VPN profile '{profile_name}' must declare at least one address"
                            ),
                        );
                    }
                    for (index, address) in connection.addresses.iter().enumerate() {
                        if parse_cidr(address).is_none() {
                            report.error(
                                "vpn.invalid_address",
                                Some(format!("{base}.connection.addresses.{index}")),
                                Some(address.clone()),
                                format!(
                                    "VPN profile '{profile_name}' has invalid address '{address}'"
                                ),
                            );
                        }
                    }
                    if connection.private_key_ref.trim().is_empty() {
                        report.error(
                            "vpn.empty_private_key_ref",
                            Some(format!("{base}.connection.privateKeyRef")),
                            Some(connection.private_key_ref.clone()),
                            format!("VPN profile '{profile_name}' privateKeyRef must not be empty"),
                        );
                    }
                    if connection.peers.is_empty() {
                        report.error(
                            "vpn.empty_peers",
                            Some(format!("{base}.connection.peers")),
                            None,
                            format!("VPN profile '{profile_name}' must declare at least one peer"),
                        );
                    }
                    for (index, peer) in connection.peers.iter().enumerate() {
                        let peer_base = format!("{base}.connection.peers.{index}");
                        for (field, value) in [
                            ("publicKey", &peer.public_key),
                            ("endpoint", &peer.endpoint),
                        ] {
                            if value.trim().is_empty() {
                                report.error(
                                    "vpn.empty_peer_field",
                                    Some(format!("{peer_base}.{field}")),
                                    Some(value.clone()),
                                    format!("VPN profile '{profile_name}' peer {field} must not be empty"),
                                );
                            }
                        }
                        if peer.allowed_ips.is_empty() {
                            report.error(
                                "vpn.empty_allowed_ips",
                                Some(format!("{peer_base}.allowedIps")),
                                None,
                                format!(
                                    "VPN profile '{profile_name}' peer must declare allowedIps"
                                ),
                            );
                        }
                        for (allowed_index, allowed) in peer.allowed_ips.iter().enumerate() {
                            if parse_cidr(allowed).is_none() {
                                report.error(
                                    "vpn.invalid_allowed_ip",
                                    Some(format!("{peer_base}.allowedIps.{allowed_index}")),
                                    Some(allowed.clone()),
                                    format!(
                                        "VPN profile '{profile_name}' peer has invalid allowed IP '{allowed}'"
                                    ),
                                );
                            }
                        }
                    }
                }
            }

            if let Some(VpnPortForwarding::NatPmp(forwarding)) = &profile.port_forwarding {
                if forwarding.gateway.parse::<IpAddr>().is_err() {
                    report.error(
                        "vpn.invalid_port_forwarding_gateway",
                        Some(format!("{base}.portForwarding.gateway")),
                        Some(forwarding.gateway.clone()),
                        format!(
                            "VPN profile '{profile_name}' has invalid port-forwarding gateway '{}'",
                            forwarding.gateway
                        ),
                    );
                }
            }
        }
    }
}

fn valid_hostname(value: &str) -> bool {
    if value.is_empty() || value.len() > 253 || value.starts_with('.') || value.ends_with('.') {
        return false;
    }
    value.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    })
}

fn parse_cidr(cidr: &str) -> Option<(IpAddr, u8)> {
    let (address, prefix) = cidr.split_once('/')?;
    let address = address.parse::<IpAddr>().ok()?;
    let prefix = prefix.parse::<u8>().ok()?;
    let max = match address {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    };
    (prefix <= max).then_some((address, prefix))
}

fn address_in_subnet(address: IpAddr, network: IpAddr, prefix: u8) -> bool {
    match (address, network) {
        (IpAddr::V4(address), IpAddr::V4(network)) => {
            let address = u32::from(address);
            let network = u32::from(network);
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            address & mask == network & mask
        }
        (IpAddr::V6(address), IpAddr::V6(network)) => {
            let address = u128::from(address);
            let network = u128::from(network);
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            address & mask == network & mask
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::{
        DnsPublication, Domains, DynamicHost, Endpoint, EndpointBind, EndpointTransport,
        HealthIntent, Host, HttpAccess, HttpAction, HttpMatch, HttpRoute, HttpSite, IngressGroup,
        PagesSite, ServiceIntent, Services,
    };
    use indexmap::IndexMap;
    use serde_json::json;

    #[test]
    fn validates_dynamic_hosts_against_label_boundaries() {
        let topology = Topology {
            schema_version: 2,
            links: IndexMap::new(),
            hosts: IndexMap::new(),
            domains: Domains {
                zones: vec!["example.test".to_string()],
                mail_subdomain: None,
                vpn_subdomain: None,
                managed_zones: vec![],
                dynamic_hosts: vec![
                    DynamicHost {
                        fqdn: "api.example.test".to_string(),
                        proxied: false,
                        zone: None,
                    },
                    DynamicHost {
                        fqdn: "example.test.evil".to_string(),
                        proxied: false,
                        zone: None,
                    },
                ],
                dns_zones: vec![],
                pages_sites: vec![PagesSite {
                    subdomain: "apt.modde".to_string(),
                    repository: "caniko/apt-modde".to_string(),
                    cname_target: "caniko.github.io".to_string(),
                }],
                redirects: vec![],
            },
            services: Services::default(),
            deployment: Default::default(),
            trust: Default::default(),
        };

        let report = validate(&topology);
        assert!(report.warnings().any(|warning| {
            warning.message.contains("example.test.evil")
                && warning.message.contains("not in any managed zone")
        }));
        assert!(report.issues.iter().any(|issue| {
            issue.code == "dns.dynamic_host_outside_managed_zone"
                && issue.value.as_deref() == Some("example.test.evil")
        }));
        assert!(!report
            .warnings()
            .any(|warning| warning.message.contains("api.example.test")));
    }

    #[test]
    fn validates_service_intent_placement_and_health() {
        let mut topology = v2_topology();
        topology.hosts.insert(
            "atlas".to_string(),
            Host {
                system: "x86_64-linux".to_string(),
                availability_class: "always-on".to_string(),
                ..Default::default()
            },
        );
        topology.services.endpoints.insert(
            "pink-raven".to_string(),
            endpoint("atlas", EndpointBind::Loopback),
        );
        topology.deployment.service_intents.push(ServiceIntent {
            name: "pink-raven-runtime".to_string(),
            service_name: Some("pink-raven".to_string()),
            required_hosts: vec!["atlas".to_string()],
            required_availability: Some("always-on".to_string()),
            health: HealthIntent {
                required: true,
                endpoint: Some("/healthz".to_string()),
            },
            ..Default::default()
        });

        assert!(validate(&topology).is_ok());
    }

    #[test]
    fn rejects_service_intent_cycles_and_unknown_hosts() {
        let mut topology = v2_topology();
        topology.deployment.service_intents = vec![
            ServiceIntent {
                name: "first".to_string(),
                preferred_hosts: vec!["missing".to_string()],
                depends_on: vec!["second".to_string()],
                ..Default::default()
            },
            ServiceIntent {
                name: "second".to_string(),
                depends_on: vec!["first".to_string()],
                ..Default::default()
            },
        ];

        let report = validate(&topology);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "deployment.unknown_host"));
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "deployment.dependency_cycle"));
    }

    #[test]
    fn accepts_v2_sites_with_ordered_routes_and_last_fallback() {
        let mut topology = v2_topology();
        topology.hosts.insert(
            "atlas".to_string(),
            Host {
                system: "x86_64-linux".to_string(),
                ..Default::default()
            },
        );
        topology.deployment.ingress_groups.insert(
            "public".to_string(),
            IngressGroup {
                scope: IngressScope::Public,
                hosts: vec!["atlas".to_string()],
            },
        );
        topology.services.endpoints.insert(
            "foundry".to_string(),
            endpoint("atlas", EndpointBind::Loopback),
        );
        topology.services.http_sites.insert(
            "foundry".to_string(),
            HttpSite {
                hostname: "vtt.example.test".to_string(),
                ingress: "public".to_string(),
                access: HttpAccess::Direct,
                dns_publication: DnsPublication::None,
                routes: vec![
                    route(
                        HttpMatch {
                            paths: vec![PathMatch::Prefix {
                                value: "/api".to_string(),
                            }],
                            absent_query_params: vec![],
                        },
                        HttpAction::Proxy {
                            endpoint: "foundry".to_string(),
                            strip_prefix: None,
                        },
                    ),
                    route(
                        HttpMatch::default(),
                        HttpAction::Respond {
                            status: 404,
                            body: None,
                        },
                    ),
                ],
            },
        );

        assert!(validate(&topology).is_ok());
    }

    #[test]
    fn rejects_invalid_route_refs_and_reachability() {
        let mut topology = v2_topology();
        topology.hosts.insert(
            "ingress".to_string(),
            Host {
                system: "x86_64-linux".to_string(),
                ..Default::default()
            },
        );
        topology.hosts.insert(
            "target".to_string(),
            Host {
                system: "x86_64-linux".to_string(),
                ..Default::default()
            },
        );
        topology.deployment.ingress_groups.insert(
            "public".to_string(),
            IngressGroup {
                scope: IngressScope::Public,
                hosts: vec!["ingress".to_string()],
            },
        );
        topology.services.endpoints.insert(
            "local".to_string(),
            endpoint("target", EndpointBind::Loopback),
        );
        topology.services.endpoints.insert(
            "bad-remote".to_string(),
            Endpoint {
                remote_via: Some("missing-remote".to_string()),
                ..endpoint("target", EndpointBind::Loopback)
            },
        );
        topology
            .services
            .endpoints
            .insert("vpn".to_string(), endpoint("target", EndpointBind::Vpn));
        topology.services.endpoints.insert(
            "bad-vpn-remote".to_string(),
            Endpoint {
                remote_via: Some("vpn".to_string()),
                ..endpoint("target", EndpointBind::Loopback)
            },
        );
        topology.services.http_sites.insert(
            "broken".to_string(),
            HttpSite {
                hostname: "broken.example.test".to_string(),
                ingress: "public".to_string(),
                access: HttpAccess::Direct,
                dns_publication: DnsPublication::None,
                routes: vec![
                    route(
                        HttpMatch::default(),
                        HttpAction::Proxy {
                            endpoint: "local".to_string(),
                            strip_prefix: None,
                        },
                    ),
                    route(
                        HttpMatch {
                            paths: vec![PathMatch::Exact {
                                value: "api".to_string(),
                            }],
                            absent_query_params: vec![String::new()],
                        },
                        HttpAction::Proxy {
                            endpoint: "missing".to_string(),
                            strip_prefix: Some("api".to_string()),
                        },
                    ),
                    route(
                        HttpMatch {
                            paths: vec![PathMatch::Exact {
                                value: "api".to_string(),
                            }],
                            absent_query_params: vec![String::new()],
                        },
                        HttpAction::Respond {
                            status: 700,
                            body: None,
                        },
                    ),
                ],
            },
        );

        let report = validate(&topology);
        for code in [
            "site.route_fallback_not_last",
            "site.route_path_not_absolute",
            "site.route_unknown_endpoint",
            "site.route_strip_prefix_not_absolute",
            "site.route_duplicate_match",
            "site.route_empty_absent_query_param",
            "site.route_unreachable_endpoint",
            "site.route_invalid_response_status",
            "endpoint.unknown_remote_via",
            "endpoint.invalid_remote_via",
        ] {
            assert!(
                report.issues.iter().any(|issue| issue.code == code),
                "missing validation issue {code}: {:?}",
                report.issues
            );
        }
    }

    #[test]
    fn validates_laptop_media_route_and_storage_paths() {
        let mut topology = v2_topology();
        topology.hosts.insert(
            "nomad".to_string(),
            Host {
                system: "x86_64-linux".to_string(),
                gpu: crate::topology::Gpu {
                    media: Some(crate::topology::GpuMedia {
                        vendor: "vulkan".to_string(),
                        render_node: "/sys/class/drm/renderD128".to_string(),
                        libva_driver: String::new(),
                    }),
                    ..Default::default()
                },
                storage: crate::topology::Storage {
                    project_state_root: Some("ProjectState".to_string()),
                    flake_root: Some("/data/can/canix".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        let report = validate(&topology);
        for code in [
            "host.invalid_gpu_media_vendor",
            "host.invalid_gpu_media_render_node",
            "host.empty_gpu_media_driver",
            "host.relative_storage_path",
        ] {
            assert!(
                report.issues.iter().any(|issue| issue.code == code),
                "missing validation issue {code}: {:?}",
                report.issues
            );
        }
    }

    #[test]
    fn validates_host_local_vpn_profiles() {
        let topology: Topology = serde_json::from_value(json!({
            "schemaVersion": 2,
            "links": {},
            "hosts": {
                "edge": {
                    "system": "x86_64-linux",
                    "users": { "alice": { "hasAccount": true } },
                    "vpnProfiles": {
                        "empty": {
                            "provider": " ",
                            "owner": "missing",
                            "dnsServers": ["not-an-ip"],
                            "connection": {
                                "type": "wireguard",
                                "addresses": ["not-a-cidr"],
                                "privateKeyRef": "",
                                "peers": [
                                    {
                                        "publicKey": "",
                                        "endpoint": " ",
                                        "allowedIps": ["not-a-cidr"]
                                    },
                                    {
                                        "publicKey": "peer-public-key",
                                        "endpoint": "vpn.example.test:51820",
                                        "allowedIps": []
                                    }
                                ]
                            },
                            "portForwarding": {
                                "type": "nat-pmp",
                                "gateway": "not-an-ip"
                            }
                        },
                        "missing-network-data": {
                            "provider": "Example VPN",
                            "connection": {
                                "type": "wireguard",
                                "addresses": ["198.51.100.2/32"],
                                "privateKeyRef": "vpn/example/private-key",
                                "peers": []
                            }
                        }
                    }
                },
                "empty-profiles": {
                    "system": "x86_64-linux",
                    "vpnProfiles": {}
                }
            },
            "domains": {},
            "services": {}
        }))
        .unwrap();

        let report = validate(&topology);
        for code in [
            "vpn.empty_provider",
            "vpn.unknown_owner",
            "vpn.invalid_dns_server",
            "vpn.empty_dns_servers",
            "vpn.invalid_address",
            "vpn.empty_private_key_ref",
            "vpn.empty_peers",
            "vpn.empty_peer_field",
            "vpn.empty_allowed_ips",
            "vpn.invalid_allowed_ip",
            "vpn.invalid_port_forwarding_gateway",
        ] {
            assert!(
                report.issues.iter().any(|issue| issue.code == code),
                "missing validation issue {code}: {:?}",
                report.issues
            );
        }
        assert!(topology.hosts["empty-profiles"].vpn_profiles.is_empty());
    }

    #[test]
    fn rejects_absent_and_old_schema_versions() {
        let absent = Topology::default();
        assert_eq!(absent.schema_version, 0);
        assert!(validate(&absent)
            .issues
            .iter()
            .any(|issue| issue.code == "topology.unsupported_schema_version"));

        let mut old = v2_topology();
        old.schema_version = 1;
        assert!(!validate(&old).is_ok());
        assert!(validate(&v2_topology()).is_ok());
    }

    fn v2_topology() -> Topology {
        Topology {
            schema_version: 2,
            ..Default::default()
        }
    }

    fn endpoint(target_host: &str, bind: EndpointBind) -> Endpoint {
        Endpoint {
            target_host: target_host.to_string(),
            port: 8080,
            transport: EndpointTransport::Http,
            bind,
            remote_via: None,
            tls_server_name: None,
            tcp_probe: true,
        }
    }

    fn route(matcher: HttpMatch, action: HttpAction) -> HttpRoute {
        HttpRoute {
            matcher,
            action,
            auth_policy: None,
            response_headers: IndexMap::new(),
        }
    }
}
