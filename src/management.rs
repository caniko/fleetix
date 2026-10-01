//! Provider-neutral installed-system management candidates. Callers choose where
//! these fit in their existing route ladder and probe reachability themselves.
use crate::Topology;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagementRouteKind {
    Link(String),
    Public,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagementRoute {
    pub kind: ManagementRouteKind,
    pub address: String,
    pub port: u16,
    pub host_key: String,
}

impl Topology {
    /// Prefer a shared declared management link, followed by independent public
    /// recovery. A public route never requires the source to join that link.
    pub fn management_routes(
        &self,
        from_host: &str,
        target_host: &str,
        default_port: u16,
    ) -> miette::Result<Vec<ManagementRoute>> {
        let report = crate::validate(self);
        if !report.is_ok() {
            return Err(miette::miette!(
                "invalid topology: {}",
                report
                    .errors()
                    .map(|issue| format!("{}: {}", issue.code, issue.message))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        let source = self
            .hosts
            .get(from_host)
            .ok_or_else(|| miette::miette!("unknown source host {from_host}"))?;
        let target = self
            .hosts
            .get(target_host)
            .ok_or_else(|| miette::miette!("unknown target host {target_host}"))?;
        if default_port == 0 {
            return Err(miette::miette!("default SSH port must be non-zero"));
        }
        if from_host == target_host {
            return Ok(vec![]);
        }
        let port = target.management.ssh_port.unwrap_or(default_port);
        let mut routes = Vec::new();
        let Some(key) = target.host_pubkey.as_deref() else {
            return Ok(routes);
        };
        if let Some(link) = &target.management.link {
            if source.links.contains_key(link) {
                if let Some(binding) = target.links.get(link) {
                    routes.push(ManagementRoute {
                        kind: ManagementRouteKind::Link(link.clone()),
                        address: binding.address.clone(),
                        port,
                        host_key: key.into(),
                    });
                }
            }
        }
        if let Some(address) = &target.management.public_address {
            routes.push(ManagementRoute {
                kind: ManagementRouteKind::Public,
                address: address.clone(),
                port,
                host_key: key.into(),
            });
        }
        Ok(routes)
    }
}
