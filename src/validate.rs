use crate::topology::{LinkRole, Topology};
use serde::Serialize;
use std::collections::HashSet;
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

    // 1. Every link has at most one server
    for (link_name, link) in &topology.links {
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
        }
    }

    // Bindings must not introduce undeclared links.
    for (host_name, host) in &topology.hosts {
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

    // 4. Every dynamic host FQDN is in a declared managed zone. An empty
    // managed-zone list means the general zone list is authoritative.
    let validation_zones = if topology.domains.managed_zones.is_empty() {
        &topology.domains.zones
    } else {
        &topology.domains.managed_zones
    };
    for dynamic_host in &topology.domains.dynamic_hosts {
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
    use crate::topology::{Domains, DynamicHost, Services};
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
                codeberg_pages_sites: vec![],
                redirects: vec![],
            },
            services: Services::default(),
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
}
