use crate::topology::{Link, LinkBinding, LinkRole, ReverseProxyService, Topology};

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
        self.resolve_host_address(
            host_name,
            &[
                AddressKind::Lan,
                AddressKind::Link("wg-home".to_string()),
                AddressKind::DirectLink,
            ],
        )
    }

    /// Resolve a host address using an explicit ordered address policy.
    pub fn resolve_host_address<'a>(
        &'a self,
        host_name: &str,
        policy: &[AddressKind],
    ) -> Option<&'a str> {
        let host = self.hosts.get(host_name)?;
        policy.iter().find_map(|kind| match kind {
            AddressKind::Lan => host.network.lan_ip.as_deref(),
            AddressKind::DirectLink => host
                .network
                .direct_link_ip
                .as_deref()
                .or_else(|| host.links.get("direct-link").map(|b| b.address.as_str())),
            AddressKind::Link(link_name) => host.links.get(link_name).map(|b| b.address.as_str()),
        })
    }

    /// Build a service endpoint for a reverse proxy service target.
    pub fn service_endpoint(
        &self,
        service_name: &str,
        policy: &[AddressKind],
        scheme: Option<&str>,
    ) -> Option<ServiceEndpoint<'_>> {
        let service = self
            .services
            .reverse_proxy_services
            .iter()
            .find(|svc| svc.name == service_name)?;
        let target_host = service.target_host.as_deref()?;
        let address = self.resolve_host_address(target_host, policy)?;
        let scheme = scheme
            .or(service.upstream_scheme.as_deref())
            .unwrap_or("http")
            .to_string();
        let url = format!("{scheme}://{address}:{}", service.port);

        Some(ServiceEndpoint {
            service: service.name.as_str(),
            target_host,
            address,
            port: service.port,
            scheme,
            url,
        })
    }

    /// Reverse proxy services targeting a host.
    pub fn reverse_proxy_services_for_host(
        &self,
        host_name: &str,
        include_vpn_only: bool,
    ) -> Vec<&ReverseProxyService> {
        self.services
            .reverse_proxy_services
            .iter()
            .filter(|svc| svc.target_host.as_deref() == Some(host_name))
            .filter(|svc| include_vpn_only || !svc.vpn_only)
            .collect()
    }

    /// LAN-exposed reverse proxy ports targeting a host.
    pub fn lan_exposed_ports(&self, host_name: &str) -> Vec<u16> {
        self.reverse_proxy_services_for_host(host_name, false)
            .into_iter()
            .filter(|svc| svc.lan_exposed)
            .map(|svc| svc.port)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressKind {
    Lan,
    DirectLink,
    Link(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEndpoint<'a> {
    pub service: &'a str,
    pub target_host: &'a str,
    pub address: &'a str,
    pub port: u16,
    pub scheme: String,
    pub url: String,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::{Host, Network, ReverseProxyService, Services};
    use indexmap::IndexMap;

    fn test_topology() -> Topology {
        let mut atlas_links = IndexMap::new();
        atlas_links.insert(
            "wg-home".to_string(),
            LinkBinding {
                address: "10.123.0.5".to_string(),
                public_key: None,
                role: LinkRole::Client,
                external_interface: None,
                mac_address: None,
            },
        );
        atlas_links.insert(
            "direct-link".to_string(),
            LinkBinding {
                address: "10.10.0.1".to_string(),
                public_key: None,
                role: LinkRole::Client,
                external_interface: None,
                mac_address: None,
            },
        );

        let mut hosts = IndexMap::new();
        hosts.insert(
            "atlas".to_string(),
            Host {
                system: "x86_64-linux".to_string(),
                network: Network {
                    lan_ip: Some("192.168.178.88".to_string()),
                    direct_link_ip: Some("10.10.0.1".to_string()),
                    ..Default::default()
                },
                links: atlas_links,
                ..empty_host()
            },
        );

        let mut nomad_links = IndexMap::new();
        nomad_links.insert(
            "direct-link".to_string(),
            LinkBinding {
                address: "10.10.0.2".to_string(),
                public_key: None,
                role: LinkRole::Client,
                external_interface: None,
                mac_address: None,
            },
        );
        hosts.insert(
            "nomad".to_string(),
            Host {
                system: "x86_64-linux".to_string(),
                links: nomad_links,
                ..empty_host()
            },
        );

        Topology {
            links: IndexMap::new(),
            hosts,
            domains: crate::topology::Domains {
                zones: vec![],
                mail_subdomain: None,
                vpn_subdomain: None,
                managed_zones: vec![],
                dynamic_hosts: vec![],
                codeberg_pages_sites: vec![],
            },
            services: Services {
                reverse_proxy_services: vec![
                    ReverseProxyService {
                        name: "immich".to_string(),
                        hostname: Some("immich.example.test".to_string()),
                        port: 2283,
                        target_host: Some("atlas".to_string()),
                        lan_exposed: true,
                        ..empty_reverse_proxy_service()
                    },
                    ReverseProxyService {
                        name: "ollama".to_string(),
                        hostname: Some("ollama.example.test".to_string()),
                        port: 11434,
                        target_host: Some("atlas".to_string()),
                        vpn_only: true,
                        ..empty_reverse_proxy_service()
                    },
                    ReverseProxyService {
                        name: "secure".to_string(),
                        hostname: Some("secure.example.test".to_string()),
                        port: 8443,
                        target_host: Some("atlas".to_string()),
                        upstream_scheme: Some("https".to_string()),
                        ..empty_reverse_proxy_service()
                    },
                ],
                ..Default::default()
            },
        }
    }

    fn empty_host() -> Host {
        Host {
            system: String::new(),
            device_type: None,
            host_pubkey: None,
            host_names: vec![],
            network: Network::default(),
            rebuild: Default::default(),
            links: IndexMap::new(),
            users: IndexMap::new(),
            gpu: Default::default(),
            storage: Default::default(),
            data_root: None,
        }
    }

    fn empty_reverse_proxy_service() -> ReverseProxyService {
        ReverseProxyService {
            name: String::new(),
            hostname: None,
            port: 0,
            target_host: None,
            proxied: false,
            cloudflare_proxied: false,
            publish_cname: false,
            vpn_only: false,
            lan_exposed: false,
            upstream_scheme: None,
            tls_server_name: None,
            service_host: None,
            zone: None,
        }
    }

    #[test]
    fn resolves_host_addresses_by_explicit_policy() {
        let topo = test_topology();
        assert_eq!(
            topo.resolve_host_address("atlas", &[AddressKind::Lan]),
            Some("192.168.178.88")
        );
        assert_eq!(
            topo.resolve_host_address("nomad", &[AddressKind::DirectLink]),
            Some("10.10.0.2")
        );
        assert_eq!(
            topo.resolve_host_address("atlas", &[AddressKind::Link("wg-home".to_string())]),
            Some("10.123.0.5")
        );
    }

    #[test]
    fn builds_service_endpoints_from_resolved_targets() {
        let topo = test_topology();
        let endpoint = topo
            .service_endpoint("secure", &[AddressKind::Lan], None)
            .expect("secure endpoint");
        assert_eq!(endpoint.scheme, "https");
        assert_eq!(endpoint.url, "https://192.168.178.88:8443");

        let endpoint = topo
            .service_endpoint("immich", &[AddressKind::DirectLink], Some("http"))
            .expect("immich endpoint");
        assert_eq!(endpoint.url, "http://10.10.0.1:2283");
    }

    #[test]
    fn filters_host_services_and_lan_exposed_ports() {
        let topo = test_topology();
        let public_services = topo.reverse_proxy_services_for_host("atlas", false);
        assert_eq!(public_services.len(), 2);
        assert!(public_services.iter().all(|svc| !svc.vpn_only));

        let all_services = topo.reverse_proxy_services_for_host("atlas", true);
        assert_eq!(all_services.len(), 3);
        assert_eq!(topo.lan_exposed_ports("atlas"), vec![2283]);
    }
}
