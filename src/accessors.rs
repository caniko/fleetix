use crate::topology::{Link, LinkBinding, LinkRole, Topology};

/// Accessor methods on Topology — derived from the link schema.
impl Topology {
    /// Get a link by name.
    pub fn link(&self, name: &str) -> Option<&Link> {
        self.links.get(name)
    }

    /// The CIDR of a link (its subnet).
    pub fn link_cidr(&self, name: &str) -> Option<&str> {
        self.links.get(name).map(|l| l.subnet.as_str())
    }

    /// The server binding for a link (the binding with role == Server).
    /// Returns None if no server or multiple servers (caller should `validate`).
    pub fn link_server(&self, name: &str) -> Option<&LinkBinding> {
        self.links.get(name).and_then(|_| {
            self.hosts
                .values()
                .filter_map(|h| h.links.get(name))
                .find(|b| b.role == LinkRole::Server)
        })
    }

    /// The server hostname for a link.
    pub fn link_server_host(&self, name: &str) -> Option<&str> {
        let server_binding = self.link_server(name)?;
        self.hosts.iter().find_map(|(hname, host)| {
            host.links
                .get(name)
                .filter(|b| b.address == server_binding.address)
                .map(|_| hname.as_str())
        })
    }

    /// The server address for a link (e.g., "10.123.0.1").
    pub fn link_server_address(&self, name: &str) -> Option<&str> {
        self.link_server(name).map(|b| b.address.as_str())
    }

    /// Dial string for a given link name: "server_address:port".
    pub fn link_dial(&self, name: &str) -> Option<String> {
        let link = self.links.get(name)?;
        let server_addr = self.link_server_address(name)?;
        Some(format!("{}:{}", server_addr, link.port))
    }

    /// Listen address for a given host on a given link (address/subnet).
    pub fn listen_address(&self, host_name: &str, link_name: &str) -> Option<String> {
        let host = self.hosts.get(host_name)?;
        let link = self.links.get(link_name)?;
        let binding = host.links.get(link_name)?;
        let prefix = cidr_prefix_len(&link.subnet).unwrap_or(24);
        Some(format!("{}/{}", binding.address, prefix))
    }

    /// Allowed IPs for a host on a given link ("address/32").
    pub fn allowed_ips(&self, host_name: &str, link_name: &str) -> Option<Vec<String>> {
        let host = self.hosts.get(host_name)?;
        let binding = host.links.get(link_name)?;
        Some(vec![format!("{}/32", binding.address)])
    }

    /// Client bindings for a link (all non-server bindings).
    pub fn link_clients(&self, name: &str) -> Vec<(&str, &LinkBinding)> {
        self.hosts
            .iter()
            .filter_map(|(hname, host)| {
                host.links
                    .get(name)
                    .filter(|b| b.role == LinkRole::Client)
                    .map(|b| (hname.as_str(), b))
            })
            .collect()
    }

    /// Server's peer list for a link (every client binding as a WireGuard peer).
    pub fn link_peers(&self, name: &str) -> Vec<PeerEntry<'_>> {
        let server_host = self.link_server_host(name);
        self.hosts
            .iter()
            .filter(|(hname, _)| Some(hname.as_str()) != server_host)
            .filter_map(|(hname, host)| {
                let binding = host.links.get(name)?;
                let address = binding.address.clone();
                Some(PeerEntry {
                    hostname: hname,
                    public_key: binding.public_key.as_deref().unwrap_or_default(),
                    allowed_ips: vec![format!("{}/32", address)],
                    address,
                })
            })
            .collect()
    }

    /// Best SSH address for a host (prefer LAN, then WG, then direct-link).
    pub fn best_ssh_address(&self, host_name: &str) -> Option<&str> {
        let host = self.hosts.get(host_name)?;
        host.network
            .lan_ip
            .as_deref()
            .or(host.links.get("wg-home").map(|b| b.address.as_str()))
            .or(host.network.direct_link_ip.as_deref())
    }
}

#[derive(Debug, Clone)]
pub struct PeerEntry<'a> {
    pub hostname: &'a str,
    pub public_key: &'a str,
    pub allowed_ips: Vec<String>,
    pub address: String,
}

fn cidr_prefix_len(subnet: &str) -> Option<u8> {
    subnet.split('/').nth(1)?.parse().ok()
}
