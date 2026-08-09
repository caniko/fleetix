// Trust observation framework.
//
// The OpenSSH known_hosts observer classifies store entries against the
// declared fleet trust and surfaces proposals that can be integrated into the
// consumer-owned `Trust.pkl` topology source. The CLI and the Home Manager
// observer drive the shared scan/integrate/ignore pipeline.

pub mod notify;
pub mod openssh;
pub mod patch;
pub mod state;

use crate::topology::Topology;
use openssh::Entry;
use std::collections::HashMap;

/// Index of declared host keys: hostname (lowercased) -> key texts.
pub struct DeclaredTrust {
    host_keys: HashMap<String, Vec<String>>,
}

impl DeclaredTrust {
    /// Build the declared index from fleet hosts and the `Trust` section.
    ///
    /// A host's key is indexed under its name, host names, LAN and direct-link
    /// IPs, and every link binding address — the aliases the OpenSSH
    /// knownHosts generation writes. Lookups normalize `[host]:port` forms.
    pub fn from_topology(topology: &Topology) -> Self {
        let mut host_keys: HashMap<String, Vec<String>> = HashMap::new();
        for (name, host) in &topology.hosts {
            if let Some(host_pubkey) = &host.host_pubkey {
                let mut aliases: Vec<String> = vec![name.clone()];
                aliases.extend(host.host_names.iter().cloned());
                if let Some(ip) = &host.network.lan_ip {
                    aliases.push(ip.clone());
                }
                if let Some(ip) = &host.network.direct_link_ip {
                    aliases.push(ip.clone());
                }
                aliases.extend(host.links.values().map(|binding| binding.address.clone()));
                for alias in aliases {
                    host_keys
                        .entry(alias.to_ascii_lowercase())
                        .or_default()
                        .push(host_pubkey.clone());
                }
            }
        }
        for entry in &topology.trust.ssh_known_hosts {
            for hostname in &entry.host_names {
                host_keys
                    .entry(hostname.to_ascii_lowercase())
                    .or_default()
                    .extend(entry.public_keys.iter().cloned());
            }
        }
        Self { host_keys }
    }

    /// Keys declared for a hostname, if any. `[host]:port` forms resolve to
    /// the bracketed host.
    pub fn keys_for(&self, hostname: &str) -> &[String] {
        let normalized = hostname
            .strip_prefix('[')
            .and_then(|rest| rest.split_once(']'))
            .map_or(hostname.to_string(), |(host, _)| host.to_string());
        self.host_keys
            .get(&normalized.to_ascii_lowercase())
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

/// One observation of a trust store.
pub struct Observation {
    /// Undeclared entries that may be integrated.
    pub proposals: Vec<Entry>,
    /// Entries whose hostname is declared with a *different* key. Security
    /// relevant: reported but never offered for integration.
    pub conflicts: Vec<Entry>,
    /// Lines that can never be declared (hashed hostnames, markers, garbage).
    pub skipped: Vec<String>,
}
