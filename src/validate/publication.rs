use super::{Topology, ValidationReport, valid_hostname};
use crate::{DnsPublication, HttpAccess};
use std::{
    collections::{BTreeSet, HashSet},
    net::{Ipv4Addr, Ipv6Addr},
};

pub(super) fn validate(topology: &Topology, report: &mut ValidationReport) {
    let mut destinations = HashSet::new();
    for (name, target) in &topology.domains.publication_targets {
        let base = format!("domains.publicationTargets.{name}");
        if !valid_hostname(&target.hostname) || topology.zone_for_host(&target.hostname).is_none() {
            report.error(
                "publication.invalid_hostname",
                Some(format!("{base}.hostname")),
                Some(target.hostname.clone()),
                "publication hostname must belong to a managed zone",
            );
        }
        if !destinations.insert(target.hostname.to_ascii_lowercase()) {
            report.error(
                "publication.duplicate_hostname",
                Some(format!("{base}.hostname")),
                Some(target.hostname.clone()),
                "publication destinations must have unique hostnames",
            );
        }
        if !topology.hosts.contains_key(&target.target_host) {
            report.error(
                "publication.unknown_host",
                Some(format!("{base}.targetHost")),
                Some(target.target_host.clone()),
                "publication destination must reference a declared fleet host",
            );
        }
        if !target.ipv4.parse::<Ipv4Addr>().is_ok_and(|ip| {
            !ip.is_unspecified()
                && !ip.is_loopback()
                && !ip.is_multicast()
                && !ip.is_link_local()
                && !ip.is_broadcast()
        }) {
            report.error(
                "publication.invalid_ipv4",
                Some(format!("{base}.ipv4")),
                Some(target.ipv4.clone()),
                "publication ipv4 must be a literal unicast IPv4 address",
            );
        }
        if target.ipv6.as_deref().is_some_and(|address| {
            !address.parse::<Ipv6Addr>().is_ok_and(|ip| {
                !ip.is_unspecified()
                    && !ip.is_loopback()
                    && !ip.is_multicast()
                    && !ip.is_unicast_link_local()
            })
        }) {
            report.error(
                "publication.invalid_ipv6",
                Some(format!("{base}.ipv6")),
                target.ipv6.clone(),
                "publication ipv6 must be a literal unicast IPv6 address",
            );
        }
    }
    for (name, site) in &topology.services.http_sites {
        if site.dns_publication == DnsPublication::Managed {
            if let Some((target_name, _)) = topology
                .domains
                .publication_targets
                .iter()
                .find(|(_, target)| target.hostname.eq_ignore_ascii_case(&site.hostname))
            {
                if site.publication_target.as_deref() != Some(target_name) {
                    report.error(
                        "publication.destination_site_conflict",
                        Some(format!("services.httpSites.{name}.publicationTarget")),
                        Some(site.hostname.clone()),
                        "a destination hostname cannot also publish a CNAME to a different target",
                    );
                }
            }
        }
        let Some(destination) = &site.publication_target else {
            continue;
        };
        let path = format!("services.httpSites.{name}.publicationTarget");
        if !topology
            .domains
            .publication_targets
            .contains_key(destination)
        {
            report.error(
                "publication.unknown_target",
                Some(path.clone()),
                Some(destination.clone()),
                "HTTP publication target must be declared",
            );
        }
        if site.access != HttpAccess::Direct || site.dns_publication != DnsPublication::Managed {
            report.error(
                "publication.invalid_site_policy",
                Some(path.clone()),
                Some(destination.clone()),
                "explicit publication requires managed DNS-only public access",
            );
        }
        if let Some(target) = topology.domains.publication_targets.get(destination) {
            if !topology
                .deployment
                .ingress_groups
                .get(&site.ingress)
                .is_some_and(|group| group.hosts.contains(&target.target_host))
            {
                report.error(
                    "publication.host_not_in_ingress",
                    Some(path),
                    Some(target.target_host.clone()),
                    "publication host must serve the site's ingress group",
                );
            }
        }
    }

    // Every statically published name takes ownership of both address families,
    // including absent AAAA, so an IPv4-only cutover can remove stale home IPv6.
    let static_names: BTreeSet<String> = topology
        .domains
        .publication_targets
        .values()
        .map(|target| target.hostname.to_ascii_lowercase())
        .chain(
            topology
                .services
                .http_sites
                .values()
                .filter(|site| site.publication_target.is_some())
                .map(|site| site.hostname.to_ascii_lowercase()),
        )
        .collect();
    for hostname in static_names {
        let competing_ddns = topology
            .domains
            .dynamic_hosts
            .iter()
            .any(|host| host.fqdn.eq_ignore_ascii_case(&hostname));
        let competing_zone = topology.domains.dns_zones.iter().any(|zone| {
            if topology.zone_for_host(&hostname) != Some(zone.name.as_str()) {
                return false;
            }
            let owner = Topology::relative_name(&hostname, &zone.name);
            let conflict = |name: &str, kind: &str| {
                owner.is_some_and(|owner| {
                    owner.eq_ignore_ascii_case(if name.is_empty() { "@" } else { name })
                }) && matches!(kind.to_ascii_uppercase().as_str(), "A" | "AAAA" | "CNAME")
            };
            zone.records
                .iter()
                .any(|record| conflict(&record.name, &record.record_type))
                || zone
                    .exclude
                    .iter()
                    .any(|record| conflict(&record.name, &record.record_type))
        });
        let competing_pages = topology.domains.pages_sites.iter().any(|site| {
            topology.domains.zones.first().is_some_and(|zone| {
                format!("{}.{}", site.subdomain, zone).eq_ignore_ascii_case(&hostname)
            })
        });
        if competing_ddns || competing_zone || competing_pages {
            report.error("publication.competing_dns_writer", Some("domains.publicationTargets".into()), Some(hostname),
                "static publication conflicts with DDNS, explicit/excluded address records or Pages ownership; complete the writer handoff first");
        }
    }
}
