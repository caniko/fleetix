use crate::topology::{
    DnsPublication, DynamicHost, Endpoint, EndpointBind, HttpAccess, HttpSite, Link, LinkBinding,
    LinkRole, Topology, VpnProfile,
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

    /// The server address for a link (e.g., "198.51.100.1").
    pub fn link_server_address(&self, name: &str) -> Option<&str> {
        self.link_server(name).map(|b| b.address.as_str())
    }

    /// Dial string for a given link name: "server_address:port".
    pub fn link_dial(&self, name: &str) -> Option<String> {
        let link = self.links.get(name)?;
        let server_addr = self.link_server_address(name)?;
        Some(format!("{}:{}", server_addr, link.port))
    }

    /// Listen address for a host on a link (address/subnet).
    pub fn listen_address(&self, host_name: &str, link_name: &str) -> Option<String> {
        let host = self.hosts.get(host_name)?;
        let link = self.links.get(link_name)?;
        let binding = host.links.get(link_name)?;
        Some(format!(
            "{}/{}",
            binding.address,
            crate::validate::parse_cidr(&link.subnet)?.1
        ))
    }

    /// Allowed IPs for a host on a link.
    pub fn allowed_ips(&self, host_name: &str, link_name: &str) -> Option<Vec<String>> {
        let binding = self.hosts.get(host_name)?.links.get(link_name)?;
        Some(vec![format!(
            "{}/{}",
            binding.address,
            address_prefix_len(&binding.address)?
        )])
    }

    /// Client bindings for a link.
    pub fn link_clients(&self, name: &str) -> Vec<(&str, &LinkBinding)> {
        self.hosts
            .iter()
            .filter_map(|(host_name, host)| {
                host.links
                    .get(name)
                    .filter(|binding| binding.role == LinkRole::Client)
                    .map(|binding| (host_name.as_str(), binding))
            })
            .collect()
    }

    /// Server peer entries for a link.
    pub fn link_peers(&self, name: &str) -> Vec<PeerEntry<'_>> {
        let server_host = self.link_server_host(name);
        self.hosts
            .iter()
            .filter(|(host_name, _)| Some(host_name.as_str()) != server_host)
            .filter_map(|(host_name, host)| {
                let binding = host.links.get(name)?;
                let address = binding.address.clone();
                Some(PeerEntry {
                    hostname: host_name,
                    public_key: binding.public_key.as_deref().unwrap_or_default(),
                    allowed_ips: vec![format!("{address}/{}", address_prefix_len(&address)?)],
                    address,
                })
            })
            .collect()
    }

    /// Best SSH address for a host: LAN, WireGuard, then direct-link.
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

    /// Best SSH address for a host (prefer LAN, then WG, then direct-link).
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

    /// Get an endpoint by name.
    pub fn endpoint(&self, name: &str) -> Option<&Endpoint> {
        self.services.endpoints.get(name)
    }

    /// Get an HTTP site by name.
    pub fn http_site(&self, name: &str) -> Option<&HttpSite> {
        self.services.http_sites.get(name)
    }

    /// Get a host-local VPN profile by host and profile name.
    pub fn vpn_profile(&self, host_name: &str, profile_name: &str) -> Option<&VpnProfile> {
        self.hosts.get(host_name)?.vpn_profiles.get(profile_name)
    }

    /// Endpoints targeting a host, preserving declaration order.
    pub fn endpoints_for_host(&self, host_name: &str) -> Vec<(&str, &Endpoint)> {
        self.services
            .endpoints
            .iter()
            .filter(|(_, endpoint)| endpoint.target_host == host_name)
            .map(|(name, endpoint)| (name.as_str(), endpoint))
            .collect()
    }

    /// Resolve the endpoint an ingress host should dial.
    pub fn endpoint_for_ingress<'a>(
        &'a self,
        endpoint_name: &'a str,
        ingress_host: &str,
    ) -> Option<(&'a str, &'a Endpoint)> {
        let endpoint = self.services.endpoints.get(endpoint_name)?;
        if endpoint.bind == EndpointBind::Loopback && endpoint.target_host != ingress_host {
            let remote_name = endpoint.remote_via.as_deref()?;
            Some((remote_name, self.services.endpoints.get(remote_name)?))
        } else {
            Some((endpoint_name, endpoint))
        }
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

    /// HTTP site hostnames keyed by site name.
    pub fn service_hosts(&self) -> indexmap::IndexMap<&str, &str> {
        self.services
            .http_sites
            .iter()
            .map(|(name, site)| (name.as_str(), site.hostname.as_str()))
            .collect()
    }

    /// Generic CNAME intents for public service hostnames.
    pub fn service_cname_intents(&self) -> Vec<CnameIntent> {
        self.services
            .http_sites
            .iter()
            .filter_map(|(name, site)| {
                if site.dns_publication != DnsPublication::Managed || site.access == HttpAccess::Vpn
                {
                    return None;
                }
                let zone = self.zone_for_host(&site.hostname)?;
                Some(CnameIntent {
                    name: name.clone(),
                    hostname: site.hostname.clone(),
                    zone: zone.to_string(),
                    relative_name: Self::relative_name(&site.hostname, zone)
                        .unwrap_or(&site.hostname)
                        .to_string(),
                    target: zone.to_string(),
                    proxied: site.access == HttpAccess::Cloudflare,
                    comment: None,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressKind {
    Lan,
    DirectLink,
    Link(String),
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

fn address_prefix_len(address: &str) -> Option<u8> {
    Some(match address.parse::<IpAddr>().ok()? {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::{
        DnsPublication, Domains, DynamicHost, Endpoint, EndpointBind, EndpointTransport, Host,
        HttpAccess, HttpAction, HttpMatch, HttpRoute, HttpSite, Link, Network, PagesSite, Redirect,
        Services,
    };
    use indexmap::IndexMap;

    fn test_topology() -> Topology {
        let mut hub_links = IndexMap::new();
        hub_links.insert(
            "wg-home".to_string(),
            LinkBinding {
                address: "198.51.100.5".to_string(),
                public_key: None,
                role: LinkRole::Client,
                external_interface: None,
                mac_address: None,
            },
        );
        hub_links.insert(
            "direct-link".to_string(),
            LinkBinding {
                address: "203.0.113.1".to_string(),
                public_key: None,
                role: LinkRole::Client,
                external_interface: None,
                mac_address: None,
            },
        );

        let mut hosts = IndexMap::new();
        hosts.insert(
            "hub".to_string(),
            Host {
                system: "x86_64-linux".to_string(),
                network: Network {
                    lan_ip: Some("192.0.2.10".to_string()),
                    direct_link_ip: Some("203.0.113.1".to_string()),
                    ..Default::default()
                },
                links: hub_links,
                ..empty_host()
            },
        );

        let mut spoke_links = IndexMap::new();
        spoke_links.insert(
            "direct-link".to_string(),
            LinkBinding {
                address: "203.0.113.2".to_string(),
                public_key: None,
                role: LinkRole::Client,
                external_interface: None,
                mac_address: None,
            },
        );
        hosts.insert(
            "spoke".to_string(),
            Host {
                system: "x86_64-linux".to_string(),
                links: spoke_links,
                ..empty_host()
            },
        );

        Topology {
            schema_version: 2,
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
                dns_zones: vec![],
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
                endpoints: IndexMap::from([
                    (
                        "photos".to_string(),
                        Endpoint {
                            target_host: "hub".to_string(),
                            port: 2283,
                            transport: EndpointTransport::Http,
                            bind: EndpointBind::Loopback,
                            remote_via: Some("photos-lan".to_string()),
                            tls_server_name: None,
                            tcp_probe: true,
                        },
                    ),
                    (
                        "photos-lan".to_string(),
                        Endpoint {
                            target_host: "hub".to_string(),
                            port: 2283,
                            transport: EndpointTransport::Http,
                            bind: EndpointBind::Lan,
                            remote_via: None,
                            tls_server_name: None,
                            tcp_probe: true,
                        },
                    ),
                ]),
                http_sites: IndexMap::from([
                    (
                        "photos".to_string(),
                        HttpSite {
                            hostname: "photos.example.test".to_string(),
                            ingress: "public".to_string(),
                            access: HttpAccess::Cloudflare,
                            dns_publication: DnsPublication::Managed,
                            routes: vec![HttpRoute {
                                matcher: HttpMatch::default(),
                                action: HttpAction::Proxy {
                                    endpoint: "photos".to_string(),
                                    strip_prefix: None,
                                },
                                auth_policy: None,
                                response_headers: IndexMap::new(),
                            }],
                        },
                    ),
                    (
                        "models".to_string(),
                        HttpSite {
                            hostname: "models.internal.example.test".to_string(),
                            ingress: "vpn".to_string(),
                            access: HttpAccess::Vpn,
                            dns_publication: DnsPublication::None,
                            routes: vec![HttpRoute {
                                matcher: HttpMatch::default(),
                                action: HttpAction::Respond {
                                    status: 404,
                                    body: None,
                                },
                                auth_policy: None,
                                response_headers: IndexMap::new(),
                            }],
                        },
                    ),
                ]),
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
            vpn_profiles: IndexMap::new(),
            gpu: Default::default(),
            storage: Default::default(),
        }
    }

    #[test]
    fn resolves_host_addresses_by_explicit_policy() {
        let topo = test_topology();
        assert_eq!(
            topo.resolve_host_address("hub", &[AddressKind::Lan]),
            Some("192.0.2.10")
        );
        assert_eq!(
            topo.resolve_host_address("spoke", &[AddressKind::DirectLink]),
            Some("203.0.113.2")
        );
        assert_eq!(
            topo.resolve_host_address("hub", &[AddressKind::Link("wg-home".to_string())]),
            Some("198.51.100.5")
        );
    }

    #[test]
    fn resolves_endpoints_for_ingress_hosts() {
        let topo = test_topology();
        assert_eq!(
            topo.endpoint_for_ingress("photos", "hub").unwrap().0,
            "photos"
        );
        assert_eq!(
            topo.endpoint_for_ingress("photos", "spoke").unwrap().0,
            "photos-lan"
        );
    }

    #[test]
    fn filters_endpoints_for_host() {
        let topo = test_topology();
        assert_eq!(topo.endpoints_for_host("hub").len(), 2);
        assert!(topo.endpoints_for_host("spoke").is_empty());
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
        assert_eq!(topo.service_hosts()["photos"], "photos.example.test");

        let service_intents = topo.service_cname_intents();
        assert!(service_intents.iter().any(|intent| {
            intent.name == "photos"
                && intent.relative_name == "photos"
                && intent.target == "example.test"
                && intent.proxied
        }));
        assert!(!service_intents.iter().any(|intent| intent.name == "models"));

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
                subnet: "198.51.100.0/24".to_string(),
                port: 54321,
                endpoint_subdomain: None,
                exempt_from_proxy: false,
            },
        );
        topo.hosts
            .get_mut("hub")
            .expect("hub fixture")
            .links
            .get_mut("wg-home")
            .expect("hub link fixture")
            .role = LinkRole::Server;

        assert_eq!(topo.link_server_host("wg-home"), Some("hub"));
        assert_eq!(topo.link_server_address("wg-home"), Some("198.51.100.5"));

        topo.hosts
            .get_mut("spoke")
            .expect("spoke fixture")
            .links
            .insert(
                "wg-home".to_string(),
                LinkBinding {
                    address: "198.51.100.6".to_string(),
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
