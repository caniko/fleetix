//! Static address projections for explicit service publication destinations.
use crate::{DnsPublication, HttpAccess, Topology};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationAddressIntent {
    pub hostname: String,
    pub zone: String,
    pub relative_name: String,
    pub ipv4: String,
    pub ipv6: Option<String>,
}

impl Topology {
    /// Address owners include destination names and directly published apexes.
    /// Consumers must validate the topology before rendering or applying DNS.
    pub fn publication_address_intents(&self) -> Vec<PublicationAddressIntent> {
        let mut owners: Vec<_> = self
            .domains
            .publication_targets
            .values()
            .map(|target| (target.hostname.as_str(), target))
            .collect();
        owners.extend(self.services.http_sites.values().filter_map(|site| {
            if site.dns_publication != DnsPublication::Managed
                || site.access != HttpAccess::Direct
                || self.zone_for_host(&site.hostname) != Some(site.hostname.as_str())
            {
                return None;
            }
            Some((
                site.hostname.as_str(),
                self.domains
                    .publication_targets
                    .get(site.publication_target.as_deref()?)?,
            ))
        }));
        let mut seen = std::collections::HashSet::new();
        owners
            .into_iter()
            .filter_map(|(hostname, target)| {
                let zone = self.zone_for_host(hostname)?;
                if !seen.insert(hostname.to_ascii_lowercase()) {
                    return None;
                }
                Some(PublicationAddressIntent {
                    hostname: hostname.into(),
                    zone: zone.into(),
                    relative_name: Self::relative_name(hostname, zone)?.into(),
                    ipv4: target.ipv4.clone(),
                    ipv6: target.ipv6.clone(),
                })
            })
            .collect()
    }
}
