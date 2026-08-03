use crate::topology::{
    DynamicHost, Link, LinkBinding, LinkRole, NormalizedReverseProxyRoute, ReverseProxyService,
    Topology,
};
use std::net::IpAddr;

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
    ///
    /// A link must have exactly one server for this accessor to return a
    /// value. Returning `None` for an ambiguous link prevents callers from
    /// silently selecting whichever host happens to appear first in the
    /// topology map.
    pub fn link_server(&self, name: &str) -> Option<&LinkBinding> {
        self.links.get(name)?;
        let mut servers = self
            .hosts
            .values()
            .filter_map(|host| host.links.get(name))
            .filter(|binding| binding.role == LinkRole::Server);
        let server = servers.next()?;
        servers.next().is_none().then_some(server)
    }

    /// The server hostname for a link.
    pub fn link_server_host(&self, name: &str) -> Option<&str> {
        let server_binding = self.link_server(name)?;
        self.hosts.iter().find_map(|(hname, host)| {
            host.links
                .get(name)
                .filter(|binding| std::ptr::eq(*binding, server_binding))
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
        let prefix = cidr_prefix_len(&link.subnet)?;
        Some(format!("{}/{}", binding.address, prefix))
    }

    /// Allowed IPs for a host on a given link ("address/32").
    pub fn allowed_ips(&self, host_name: &str, link_name: &str) -> Option<Vec<String>> {
        let host = self.hosts.get(host_name)?;
        let binding = host.links.get(link_name)?;
        Some(vec![format!(
            "{}/{}",
            binding.address,
            address_prefix_len(&binding.address)?
        )])
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
                let prefix = address_prefix_len(&address)?;
                Some(PeerEntry {
                    hostname: hname,
                    public_key: binding.public_key.as_deref().unwrap_or_default(),
                    allowed_ips: vec![format!("{address}/{prefix}")],
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
        let url = format!(
            "{scheme}://{}:{}",
            format_endpoint_address(address),
            service.port
        );

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

    /// Return normalized routes for reverse-proxy services targeting a host.
    pub fn reverse_proxy_routes_for_host(
        &self,
        host_name: &str,
        include_vpn_only: bool,
    ) -> Vec<(&ReverseProxyService, NormalizedReverseProxyRoute)> {
        self.services
            .reverse_proxy_services
            .iter()
            .filter(|svc| include_vpn_only || !svc.vpn_only)
            .flat_map(|svc| {
                svc.normalized_routes()
                    .into_iter()
                    .map(move |route| (svc, route))
            })
            .filter(|(_, route)| route.target_host.as_deref() == Some(host_name))
            .collect()
    }

    /// LAN-exposed reverse proxy ports targeting a host.
    pub fn lan_exposed_ports(&self, host_name: &str) -> Vec<u16> {
        let mut ports = self
            .reverse_proxy_routes_for_host(host_name, false)
            .into_iter()
            .filter(|(svc, _)| svc.lan_exposed)
            .map(|(_, route)| route.port)
            .collect::<Vec<_>>();
        ports.sort_unstable();
        ports.dedup();
        ports
    }

    /// Whether an FQDN belongs to a DNS zone.
    pub fn host_in_zone(fqdn: &str, zone: &str) -> bool {
        fqdn == zone || fqdn.ends_with(&format!(".{zone}"))
    }

    /// Managed zone for an FQDN, preferring the longest matching suffix.
    pub fn zone_for_host<'a>(&'a self, fqdn: &str) -> Option<&'a str> {
        let zones = if self.domains.managed_zones.is_empty() {
            &self.domains.zones
        } else {
            &self.domains.managed_zones
        };
        zones
            .iter()
            .filter(|zone| Self::host_in_zone(fqdn, zone))
            .max_by_key(|zone| zone.split('.').count())
            .map(String::as_str)
    }

    /// Relative DNS owner name for an FQDN inside a zone.
    pub fn relative_name<'a>(fqdn: &'a str, zone: &str) -> Option<&'a str> {
        if fqdn == zone {
            Some("@")
        } else {
            fqdn.strip_suffix(&format!(".{zone}"))
        }
    }

    /// Dynamic hosts that belong to a zone.
    pub fn dynamic_hosts_for_zone<'a>(&'a self, zone: &str) -> Vec<&'a DynamicHost> {
        self.domains
            .dynamic_hosts
            .iter()
            .filter(|host| self.zone_for_host(&host.fqdn) == Some(zone))
            .collect()
    }

    /// Provider-neutral A/AAAA ownership intent for dynamic hosts in a zone.
    pub fn dynamic_host_address_excludes(&self, zone: &str) -> Vec<AddressExclude> {
        self.dynamic_hosts_for_zone(zone)
            .into_iter()
            .flat_map(|host| {
                let name = Self::relative_name(&host.fqdn, zone).unwrap_or(host.fqdn.as_str());
                [
                    AddressExclude {
                        name: name.to_string(),
                        record_type: "A",
                    },
                    AddressExclude {
                        name: name.to_string(),
                        record_type: "AAAA",
                    },
                ]
            })
            .collect()
    }

    /// Reverse-proxy service hostnames keyed by service name.
    pub fn service_hosts(&self) -> indexmap::IndexMap<&str, &str> {
        let reverse = self
            .services
            .reverse_proxy_services
            .iter()
            .map(|svc| (svc.name.as_str(), svc.hostname.as_deref().unwrap_or("")));
        let static_files = self
            .services
            .static_file_services
            .iter()
            .map(|svc| (svc.name.as_str(), svc.hostname.as_deref().unwrap_or("")));
        reverse.chain(static_files).collect()
    }

    /// Generic CNAME intents for public service hostnames.
    pub fn service_cname_intents(&self) -> Vec<CnameIntent> {
        self.services
            .reverse_proxy_services
            .iter()
            .map(ServiceRef::from)
            .chain(
                self.services
                    .static_file_services
                    .iter()
                    .map(ServiceRef::from),
            )
            .filter_map(|service| {
                let hostname = service.hostname()?;
                if service.vpn_only() || !service.publish_cname() {
                    return None;
                }
                let zone = self.zone_for_host(hostname)?;
                Some(CnameIntent {
                    name: service.name().to_string(),
                    hostname: hostname.to_string(),
                    zone: zone.to_string(),
                    relative_name: Self::relative_name(hostname, zone)
                        .unwrap_or(hostname)
                        .to_string(),
                    target: zone.to_string(),
                    proxied: service.cloudflare_proxied(),
                    comment: service.dns_comment().map(str::to_string),
                    source: "service",
                })
            })
            .collect()
    }

    /// Generic CNAME intents for Pages sites.
    pub fn pages_cname_intents(&self, base_zone: Option<&str>) -> Vec<CnameIntent> {
        let Some(default_zone) =
            base_zone.or_else(|| self.domains.zones.first().map(String::as_str))
        else {
            return vec![];
        };
        self.domains
            .pages_sites
            .iter()
            .filter_map(|site| {
                let hostname = format!("{}.{}", site.subdomain, default_zone);
                let zone = self.zone_for_host(&hostname)?;
                Some(CnameIntent {
                    name: site.subdomain.clone(),
                    hostname,
                    zone: zone.to_string(),
                    relative_name: site.subdomain.clone(),
                    target: site.cname_target.clone(),
                    proxied: false,
                    comment: None,
                    source: "pages",
                })
            })
            .collect()
    }
}

enum ServiceRef<'a> {
    Reverse(&'a ReverseProxyService),
    Static(&'a crate::topology::StaticFileService),
}

impl<'a> From<&'a crate::topology::StaticFileService> for ServiceRef<'a> {
    fn from(service: &'a crate::topology::StaticFileService) -> Self {
        Self::Static(service)
    }
}

impl<'a> ServiceRef<'a> {
    fn name(&self) -> &'a str {
        match self {
            Self::Reverse(service) => service.name.as_str(),
            Self::Static(service) => service.name.as_str(),
        }
    }

    fn hostname(&self) -> Option<&'a str> {
        match self {
            Self::Reverse(service) => service.hostname.as_deref(),
            Self::Static(service) => service.hostname.as_deref(),
        }
    }

    fn vpn_only(&self) -> bool {
        match self {
            Self::Reverse(service) => service.vpn_only,
            Self::Static(_) => false,
        }
    }

    fn publish_cname(&self) -> bool {
        match self {
            Self::Reverse(service) => service.publish_cname,
            Self::Static(_) => true,
        }
    }

    fn cloudflare_proxied(&self) -> bool {
        match self {
            Self::Reverse(service) => service.cloudflare_proxied,
            Self::Static(service) => service.cloudflare_proxied,
        }
    }

    fn dns_comment(&self) -> Option<&'a str> {
        match self {
            Self::Reverse(_) => None,
            Self::Static(service) => service.dns_comment.as_deref(),
        }
    }
}

impl<'a> From<&'a ReverseProxyService> for ServiceRef<'a> {
    fn from(service: &'a ReverseProxyService) -> Self {
        Self::Reverse(service)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressExclude {
    pub name: String,
    pub record_type: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CnameIntent {
    pub name: String,
    pub hostname: String,
    pub zone: String,
    pub relative_name: String,
    pub target: String,
    pub proxied: bool,
    pub comment: Option<String>,
    pub source: &'static str,
}

#[derive(Debug, Clone)]
pub struct PeerEntry<'a> {
    pub hostname: &'a str,
    pub public_key: &'a str,
    pub allowed_ips: Vec<String>,
    pub address: String,
}

fn cidr_prefix_len(subnet: &str) -> Option<u8> {
    let (address, prefix) = subnet.split_once('/')?;
    let ip = address.parse::<IpAddr>().ok()?;
    let prefix = prefix.parse::<u8>().ok()?;
    (prefix
        <= match ip {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        })
    .then_some(prefix)
}

fn address_prefix_len(address: &str) -> Option<u8> {
    Some(match address.parse::<IpAddr>().ok()? {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    })
}

fn format_endpoint_address(address: &str) -> String {
    if address.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{address}]")
    } else {
        address.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::{
        Domains, DynamicHost, Host, Link, Network, PagesSite, Redirect, ReverseProxyRoute,
        ReverseProxyService, Services,
    };
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
            domains: Domains {
                zones: vec![
                    "example.test".to_string(),
                    "internal.example.test".to_string(),
                ],
                mail_subdomain: None,
                vpn_subdomain: None,
                managed_zones: vec![
                    "example.test".to_string(),
                    "internal.example.test".to_string(),
                ],
                dynamic_hosts: vec![
                    DynamicHost {
                        fqdn: "example.test".to_string(),
                        proxied: true,
                        zone: None,
                    },
                    DynamicHost {
                        fqdn: "wg.example.test".to_string(),
                        proxied: false,
                        zone: None,
                    },
                    DynamicHost {
                        fqdn: "host.internal.example.test".to_string(),
                        proxied: false,
                        zone: None,
                    },
                ],
                pages_sites: vec![PagesSite {
                    subdomain: "docs".to_string(),
                    repository: "example/docs".to_string(),
                    cname_target: "example.github.io".to_string(),
                }],
                redirects: vec![Redirect {
                    from: "example.test".to_string(),
                    to: "https://dashboard.example.test".to_string(),
                    status: 301,
                    preserve_path: true,
                }],
            },
            services: Services {
                reverse_proxy_services: vec![
                    ReverseProxyService {
                        name: "immich".to_string(),
                        hostname: Some("immich.example.test".to_string()),
                        port: 2283,
                        target_host: Some("atlas".to_string()),
                        lan_exposed: true,
                        cloudflare_proxied: true,
                        publish_cname: true,
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
                    ReverseProxyService {
                        name: "multi".to_string(),
                        hostname: Some("multi.example.test".to_string()),
                        port: 8030,
                        target_host: Some("atlas".to_string()),
                        lan_exposed: true,
                        routes: vec![
                            ReverseProxyRoute {
                                paths: vec!["/api".to_string(), "/api/*".to_string()],
                                target_host: Some("atlas".to_string()),
                                port: Some(8032),
                                upstream_scheme: None,
                                tls_server_name: None,
                                strip_prefix: None,
                                monitoring_identity: Some("multi-api".to_string()),
                            },
                            ReverseProxyRoute {
                                paths: vec![],
                                target_host: None,
                                port: None,
                                upstream_scheme: None,
                                tls_server_name: None,
                                strip_prefix: None,
                                monitoring_identity: None,
                            },
                        ],
                        ..empty_reverse_proxy_service()
                    },
                ],
                ..Default::default()
            },
            deployment: Default::default(),
            trust: Default::default(),
        }
    }

    fn empty_host() -> Host {
        Host {
            system: String::new(),
            device_type: None,
            availability_class: "unknown".to_string(),
            host_pubkey: None,
            host_names: vec![],
            network: Network::default(),
            rebuild: Default::default(),
            links: IndexMap::new(),
            users: IndexMap::new(),
            gpu: Default::default(),
            storage: Default::default(),
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
            routes: vec![],
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

        let mut ipv6_topology = test_topology();
        ipv6_topology
            .hosts
            .get_mut("atlas")
            .expect("atlas fixture")
            .network
            .lan_ip = Some("2001:db8::10".to_string());
        let endpoint = ipv6_topology
            .service_endpoint("secure", &[AddressKind::Lan], None)
            .expect("IPv6 secure endpoint");
        assert_eq!(endpoint.url, "https://[2001:db8::10]:8443");
    }

    #[test]
    fn filters_host_services_and_lan_exposed_ports() {
        let topo = test_topology();
        let public_services = topo.reverse_proxy_services_for_host("atlas", false);
        assert_eq!(public_services.len(), 3);
        assert!(public_services.iter().all(|svc| !svc.vpn_only));

        let all_services = topo.reverse_proxy_services_for_host("atlas", true);
        assert_eq!(all_services.len(), 4);
        assert_eq!(topo.lan_exposed_ports("atlas"), vec![2283, 8030, 8032]);
    }

    #[test]
    fn normalizes_legacy_service_to_one_fallback_route() {
        let topo = test_topology();
        let service = topo
            .services
            .reverse_proxy_services
            .iter()
            .find(|service| service.name == "secure")
            .expect("secure service");
        let routes = service.normalized_routes();
        assert_eq!(routes.len(), 1);
        assert!(routes[0].paths.is_empty());
        assert_eq!(routes[0].port, 8443);
        assert_eq!(routes[0].upstream_scheme, "https");
        assert_eq!(routes[0].monitoring_identity, "secure");
    }

    #[test]
    fn derives_domain_zone_and_relative_names() {
        let topo = test_topology();
        assert!(Topology::host_in_zone("api.example.test", "example.test"));
        assert_eq!(
            topo.zone_for_host("host.internal.example.test"),
            Some("internal.example.test")
        );
        assert_eq!(
            Topology::relative_name("example.test", "example.test"),
            Some("@")
        );
        assert_eq!(
            Topology::relative_name("wg.example.test", "example.test"),
            Some("wg")
        );
    }

    #[test]
    fn derives_dynamic_host_excludes() {
        let topo = test_topology();
        let excludes = topo.dynamic_host_address_excludes("example.test");
        assert_eq!(
            excludes,
            vec![
                AddressExclude {
                    name: "@".to_string(),
                    record_type: "A",
                },
                AddressExclude {
                    name: "@".to_string(),
                    record_type: "AAAA",
                },
                AddressExclude {
                    name: "wg".to_string(),
                    record_type: "A",
                },
                AddressExclude {
                    name: "wg".to_string(),
                    record_type: "AAAA",
                },
            ]
        );

        let internal_excludes = topo.dynamic_host_address_excludes("internal.example.test");
        assert_eq!(
            internal_excludes,
            vec![
                AddressExclude {
                    name: "host".to_string(),
                    record_type: "A",
                },
                AddressExclude {
                    name: "host".to_string(),
                    record_type: "AAAA",
                },
            ]
        );
    }

    #[test]
    fn exposes_redirects_service_hosts_and_cname_intents() {
        let topo = test_topology();
        assert_eq!(topo.domains.redirects[0].from, "example.test");
        assert_eq!(topo.service_hosts()["immich"], "immich.example.test");

        let service_intents = topo.service_cname_intents();
        assert!(service_intents.iter().any(|intent| {
            intent.name == "immich"
                && intent.relative_name == "immich"
                && intent.target == "example.test"
                && intent.proxied
        }));
        assert!(!service_intents.iter().any(|intent| intent.name == "ollama"));

        let page_intents = topo.pages_cname_intents(None);
        assert_eq!(page_intents.len(), 1);
        assert_eq!(page_intents[0].relative_name, "docs");
        assert_eq!(page_intents[0].target, "example.github.io");

        let mut nested_topo = topo;
        nested_topo.domains.pages_sites[0].subdomain = "apt.modde".to_string();
        let nested_intents = nested_topo.pages_cname_intents(None);
        assert_eq!(nested_intents[0].hostname, "apt.modde.example.test");
        assert_eq!(nested_intents[0].relative_name, "apt.modde");
    }

    #[test]
    fn rejects_ambiguous_link_servers() {
        let mut topo = test_topology();
        topo.links.insert(
            "wg-home".to_string(),
            Link {
                subnet: "10.123.0.0/24".to_string(),
                port: 54321,
                endpoint_subdomain: None,
                exempt_from_proxy: false,
            },
        );
        topo.hosts
            .get_mut("atlas")
            .expect("atlas fixture")
            .links
            .get_mut("wg-home")
            .expect("atlas link fixture")
            .role = LinkRole::Server;

        assert_eq!(topo.link_server_host("wg-home"), Some("atlas"));
        assert_eq!(topo.link_server_address("wg-home"), Some("10.123.0.5"));

        topo.hosts
            .get_mut("nomad")
            .expect("nomad fixture")
            .links
            .insert(
                "wg-home".to_string(),
                LinkBinding {
                    address: "10.123.0.6".to_string(),
                    public_key: None,
                    role: LinkRole::Server,
                    external_interface: None,
                    mac_address: None,
                },
            );
        assert!(topo.link_server("wg-home").is_none());
        assert!(topo.link_server_host("wg-home").is_none());
    }

    #[test]
    fn falls_back_to_general_zones_when_managed_zones_are_empty() {
        let mut topo = test_topology();
        topo.domains.managed_zones.clear();
        assert_eq!(
            topo.zone_for_host("host.internal.example.test"),
            Some("internal.example.test")
        );
        assert!(!Topology::host_in_zone("notexample.test", "example.test"));
    }
}
