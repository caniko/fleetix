use super::{Topology, ValidationReport};
use std::net::IpAddr;

pub(super) fn validate(topology: &Topology, report: &mut ValidationReport) {
    for (name, host) in &topology.hosts {
        let cfg = &host.management;
        let base = format!("hosts.{name}.management");
        if (cfg.public_address.is_some() || cfg.link.is_some())
            && host
                .host_pubkey
                .as_deref()
                .is_none_or(|key| key.trim().is_empty())
        {
            report.error(
                "management.missing_host_key",
                Some(format!("hosts.{name}.hostPubkey")),
                None,
                "explicit management routes require an enrolled runtime SSH host key",
            );
        }
        if cfg.ssh_port == Some(0) {
            report.error(
                "management.invalid_port",
                Some(format!("{base}.sshPort")),
                Some("0".into()),
                "management SSH port must be non-zero",
            );
        }
        if let Some(address) = &cfg.public_address {
            let valid = address.parse::<IpAddr>().is_ok_and(|ip| {
                !ip.is_loopback()
                    && !ip.is_unspecified()
                    && !ip.is_multicast()
                    && match ip {
                        IpAddr::V4(ip) => !ip.is_link_local() && !ip.is_broadcast(),
                        IpAddr::V6(ip) => !ip.is_unicast_link_local(),
                    }
            });
            if !valid {
                report.error(
                    "management.invalid_public_address",
                    Some(format!("{base}.publicAddress")),
                    Some(address.clone()),
                    "public recovery address must be a literal unicast IP address",
                );
            }
        }
        if let Some(link) = &cfg.link {
            if !topology.links.contains_key(link) {
                report.error(
                    "management.unknown_link",
                    Some(format!("{base}.link")),
                    Some(link.clone()),
                    "management link must be declared in topology.links",
                );
            } else if !host.links.contains_key(link) {
                report.error(
                    "management.missing_link_binding",
                    Some(format!("{base}.link")),
                    Some(link.clone()),
                    "managed host must have an address binding on its management link",
                );
            }
        }
    }
}
