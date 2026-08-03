use crate::topology::{LinkRole, Topology};
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;

#[derive(Debug, Clone, Copy, Serialize)]
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
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub issues: Vec<ValidationIssue>,
}

impl ValidationReport {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    fn error(
        &mut self,
        code: impl Into<String>,
        path: Option<String>,
        value: Option<String>,
        message: impl Into<String>,
    ) {
        let message = message.into();
        self.errors.push(message.clone());
        self.issues.push(ValidationIssue {
            severity: ValidationSeverity::Error,
            code: code.into(),
            path,
            value,
            message,
        });
    }

    fn warning(
        &mut self,
        code: impl Into<String>,
        path: Option<String>,
        value: Option<String>,
        message: impl Into<String>,
    ) {
        let message = message.into();
        self.warnings.push(message.clone());
        self.issues.push(ValidationIssue {
            severity: ValidationSeverity::Warning,
            code: code.into(),
            path,
            value,
            message,
        });
    }
}

/// Cross-reference validation of a Topology.
pub fn validate(topology: &Topology) -> ValidationReport {
    let mut report = ValidationReport::default();

    validate_identifiers(topology, &mut report);
    validate_domains(topology, &mut report);
    validate_deployment(topology, &mut report);

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

    // 3. Every reverseProxyService.target_host references an existing host
    for svc in &topology.services.reverse_proxy_services {
        if let Some(ref target) = svc.target_host {
            if !topology.hosts.contains_key(target) {
                report.error(
                    "service.invalid_target_host",
                    Some(format!("services.reverseProxyServices.{}.targetHost", svc.name)),
                    Some(target.clone()),
                    format!(
                        "reverse proxy service '{}' has targetHost '{}' which does not exist in hosts",
                        svc.name, target
                    ),
                );
            }
        }
    }

    for svc in &topology.services.internal_services {
        if let Some(ref target) = svc.target_host {
            if !topology.hosts.contains_key(target) {
                report.error(
                    "service.invalid_target_host",
                    Some(format!("services.internalServices.{}.targetHost", svc.name)),
                    Some(target.clone()),
                    format!(
                        "internal service '{}' has targetHost '{}' which does not exist in hosts",
                        svc.name, target
                    ),
                );
            }
        }
    }

    for svc in &topology.services.reverse_proxy_services {
        if svc.port == 0 {
            report.error(
                "service.invalid_port",
                Some(format!("services.reverseProxyServices.{}.port", svc.name)),
                Some("0".into()),
                format!("service '{}' must use a non-zero port", svc.name),
            );
        }
        validate_service_hostname(topology, &mut report, &svc.name, svc.hostname.as_deref());
        if let Some(zone) = &svc.zone {
            if !topology.domains.zones.contains(zone) {
                report.error(
                    "service.undeclared_zone",
                    Some(format!("services.reverseProxyServices.{}.zone", svc.name)),
                    Some(zone.clone()),
                    format!("service '{}' references undeclared zone '{zone}'", svc.name),
                );
            }
        }
        validate_reverse_proxy_routes(topology, svc, &mut report);
    }
    for svc in &topology.services.static_file_services {
        validate_service_hostname(topology, &mut report, &svc.name, svc.hostname.as_deref());
    }
    for svc in &topology.services.internal_services {
        if svc.port == 0 {
            report.error(
                "service.invalid_port",
                Some(format!("services.internalServices.{}.port", svc.name)),
                Some("0".into()),
                format!("service '{}' must use a non-zero port", svc.name),
            );
        }
    }

    // 4. Every dynamic host FQDN is in a declared managed zone. An empty
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

    // Names are used as map keys by consumers; duplicate identities are
    // ambiguous even when their individual records are otherwise valid.
    let mut service_names = HashSet::new();
    let mut service_hostnames = HashSet::new();
    for service in topology
        .services
        .reverse_proxy_services
        .iter()
        .map(|service| (&service.name, service.hostname.as_ref()))
        .chain(
            topology
                .services
                .static_file_services
                .iter()
                .map(|service| (&service.name, service.hostname.as_ref())),
        )
        .chain(
            topology
                .services
                .internal_services
                .iter()
                .map(|service| (&service.name, None)),
        )
    {
        if !service_names.insert(service.0.as_str()) {
            report.error(
                "service.duplicate_name",
                Some(format!("services.{}.name", service.0)),
                Some(service.0.clone()),
                format!("duplicate service name '{}'", service.0),
            );
        }
        if let Some(hostname) = service.1 {
            if !service_hostnames.insert(hostname.as_str()) {
                report.error(
                    "service.duplicate_hostname",
                    Some(format!("services.hostname.{hostname}")),
                    Some(hostname.clone()),
                    format!("duplicate service hostname '{hostname}'"),
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

    // 5. Every link client has a public_key if it's not the only link participant
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

fn validate_reverse_proxy_routes(
    topology: &Topology,
    service: &crate::topology::ReverseProxyService,
    report: &mut ValidationReport,
) {
    if service.routes.is_empty() {
        return;
    }

    let base = format!("services.reverseProxyServices.{}.routes", service.name);
    let mut matchers = HashSet::new();
    let mut fallback = None;

    for (index, route) in service.routes.iter().enumerate() {
        let path = format!("{base}.{index}");
        if route.paths.is_empty() && fallback.replace(index).is_some() {
            report.error(
                "service.route_multiple_fallbacks",
                Some(path.clone()),
                None,
                format!(
                    "service '{}' declares more than one fallback route",
                    service.name
                ),
            );
        }

        for (path_index, matcher) in route.paths.iter().enumerate() {
            if !matcher.starts_with('/') {
                report.error(
                    "service.route_path_not_absolute",
                    Some(format!("{path}.paths.{path_index}")),
                    Some(matcher.clone()),
                    format!(
                        "service '{}' route path '{}' must start with '/'",
                        service.name, matcher
                    ),
                );
            }
            if !matchers.insert(matcher) {
                report.error(
                    "service.route_duplicate_matcher",
                    Some(format!("{path}.paths.{path_index}")),
                    Some(matcher.clone()),
                    format!(
                        "service '{}' repeats route matcher '{}'; order would be ambiguous",
                        service.name, matcher
                    ),
                );
            }
        }

        if let Some(port) = route.port {
            if port == 0 {
                report.error(
                    "service.route_invalid_port",
                    Some(format!("{path}.port")),
                    Some("0".into()),
                    format!("service '{}' route port must be non-zero", service.name),
                );
            }
        }

        if let Some(target) = &route.target_host {
            if !topology.hosts.contains_key(target) {
                report.error(
                    "service.route_invalid_target_host",
                    Some(format!("{path}.targetHost")),
                    Some(target.clone()),
                    format!(
                        "service '{}' route targetHost '{}' does not exist in hosts",
                        service.name, target
                    ),
                );
            }
        }

        if let Some(strip_prefix) = &route.strip_prefix {
            if !strip_prefix.starts_with('/') {
                report.error(
                    "service.route_strip_prefix_not_absolute",
                    Some(format!("{path}.stripPrefix")),
                    Some(strip_prefix.clone()),
                    format!(
                        "service '{}' route stripPrefix '{}' must start with '/'",
                        service.name, strip_prefix
                    ),
                );
            }
        }
    }

    if let Some(index) = fallback {
        if index + 1 != service.routes.len() {
            report.error(
                "service.route_fallback_not_last",
                Some(format!("{base}.{index}")),
                None,
                format!("service '{}' fallback route must be last", service.name),
            );
        }
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

    let declared_services: HashSet<&str> = topology
        .services
        .reverse_proxy_services
        .iter()
        .map(|service| service.name.as_str())
        .chain(
            topology
                .services
                .static_file_services
                .iter()
                .map(|service| service.name.as_str()),
        )
        .chain(
            topology
                .services
                .internal_services
                .iter()
                .map(|service| service.name.as_str()),
        )
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

fn validate_service_hostname(
    topology: &Topology,
    report: &mut ValidationReport,
    name: &str,
    hostname: Option<&str>,
) {
    let Some(hostname) = hostname else {
        return;
    };
    if !valid_hostname(hostname) {
        report.error(
            "service.invalid_hostname",
            Some(format!("services.{name}.hostname")),
            Some(hostname.into()),
            format!("service '{name}' has invalid hostname '{hostname}'"),
        );
    } else if !topology.domains.zones.is_empty()
        && !topology
            .domains
            .zones
            .iter()
            .any(|zone| Topology::host_in_zone(hostname, zone))
    {
        report.error(
            "service.hostname_outside_zone",
            Some(format!("services.{name}.hostname")),
            Some(hostname.into()),
            format!("service hostname '{hostname}' is outside declared zones"),
        );
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
        Domains, DynamicHost, HealthIntent, Host, InternalService, PagesSite, ReverseProxyRoute,
        ReverseProxyService, ServiceIntent, Services,
    };
    use indexmap::IndexMap;

    #[test]
    fn validates_dynamic_hosts_against_label_boundaries() {
        let topology = Topology {
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
        assert!(report.warnings.iter().any(|warning| {
            warning.contains("example.test.evil") && warning.contains("not in any managed zone")
        }));
        assert!(report.issues.iter().any(|issue| {
            issue.code == "dns.dynamic_host_outside_managed_zone"
                && issue.value.as_deref() == Some("example.test.evil")
        }));
        assert!(!report
            .warnings
            .iter()
            .any(|warning| warning.contains("api.example.test")));
    }

    #[test]
    fn validates_service_intent_placement_and_health() {
        let mut topology = Topology::default();
        topology.hosts.insert(
            "atlas".to_string(),
            Host {
                system: "x86_64-linux".to_string(),
                availability_class: "always-on".to_string(),
                ..Default::default()
            },
        );
        topology.services.internal_services.push(InternalService {
            name: "pink-raven".to_string(),
            port: 3000,
            target_host: Some("atlas".to_string()),
            description: None,
        });
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
        let mut topology = Topology::default();
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
    fn accepts_ordered_routes_with_a_last_fallback() {
        let mut topology = Topology::default();
        topology.hosts.insert(
            "atlas".to_string(),
            Host {
                system: "x86_64-linux".to_string(),
                ..Default::default()
            },
        );
        topology
            .services
            .reverse_proxy_services
            .push(ReverseProxyService {
                name: "foundry".to_string(),
                hostname: Some("vtt.example.test".to_string()),
                port: 8030,
                target_host: Some("atlas".to_string()),
                routes: vec![
                    ReverseProxyRoute {
                        paths: vec!["/api".to_string(), "/api/*".to_string()],
                        target_host: Some("atlas".to_string()),
                        port: Some(8032),
                        ..Default::default()
                    },
                    ReverseProxyRoute::default(),
                ],
                ..Default::default()
            });

        assert!(validate(&topology).is_ok());
    }

    #[test]
    fn rejects_ambiguous_and_malformed_routes() {
        let mut topology = Topology::default();
        topology.hosts.insert(
            "atlas".to_string(),
            Host {
                system: "x86_64-linux".to_string(),
                ..Default::default()
            },
        );
        topology
            .services
            .reverse_proxy_services
            .push(ReverseProxyService {
                name: "foundry".to_string(),
                hostname: Some("vtt.example.test".to_string()),
                port: 8030,
                target_host: Some("atlas".to_string()),
                routes: vec![
                    ReverseProxyRoute::default(),
                    ReverseProxyRoute {
                        paths: vec!["api".to_string()],
                        target_host: Some("missing".to_string()),
                        port: Some(0),
                        strip_prefix: Some("api".to_string()),
                        ..Default::default()
                    },
                    ReverseProxyRoute {
                        paths: vec!["api".to_string()],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            });

        let report = validate(&topology);
        for code in [
            "service.route_fallback_not_last",
            "service.route_path_not_absolute",
            "service.route_invalid_target_host",
            "service.route_invalid_port",
            "service.route_strip_prefix_not_absolute",
            "service.route_duplicate_matcher",
        ] {
            assert!(
                report.issues.iter().any(|issue| issue.code == code),
                "missing validation issue {code}: {:?}",
                report.issues
            );
        }
    }
}
