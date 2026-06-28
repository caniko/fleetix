use crate::topology::Topology;

#[derive(Debug)]
pub struct ValidationReport {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl ValidationReport {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Cross-reference validation of a Topology.
pub fn validate(topology: &Topology) -> ValidationReport {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    // 1. Every link has at most one server
    for (link_name, _link) in &topology.links {
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
            warnings.push(format!(
                "link '{}' has no server — this is fine for peer-to-peer links",
                link_name
            ));
        } else if servers.len() > 1 {
            errors.push(format!(
                "link '{}' has {} servers — at most 1 allowed",
                link_name,
                servers.len()
            ));
        }
    }

    // 2. Every rebuild.build_host references an existing host
    for (host_name, host) in &topology.hosts {
        if let Some(ref build_host) = host.rebuild.build_host {
            if !topology.hosts.contains_key(build_host) {
                errors.push(format!(
                    "host '{host_name}' has rebuild.buildHost '{build_host}' which does not exist in hosts"
                ));
            }
        }
    }

    // 3. Every reverseProxyService.target_host references an existing host
    for svc in &topology.services.reverse_proxy_services {
        if let Some(ref target) = svc.target_host {
            if !topology.hosts.contains_key(target) {
                errors.push(format!(
                    "reverse proxy service '{}' has targetHost '{}' which does not exist in hosts",
                    svc.name, target
                ));
            }
        }
    }

    // 4. Every dynamicHosts.fqdn zone is in managedZones
    for dh in &topology.services.static_file_services {
        if let Some(ref zone) = dh.hostname {
            // Check if any managed zone is a suffix of the hostname
            let in_managed = topology
                .domains
                .managed_zones
                .iter()
                .any(|mz| zone.ends_with(mz));
            if !in_managed && !topology.domains.managed_zones.is_empty() {
                warnings.push(format!(
                    "static file service '{}' hostname '{}' is not in any managed zone",
                    dh.name, zone
                ));
            }
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
                    if binding.public_key.is_none() {
                        warnings.push(format!(
                            "host '{}' on link '{}' has no publicKey — WireGuard will not work",
                            host_name, link_name
                        ));
                    }
                }
            }
        }
    }

    ValidationReport { errors, warnings }
}
