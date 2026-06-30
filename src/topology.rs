use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Topology {
    pub links: IndexMap<String, Link>,
    pub hosts: IndexMap<String, Host>,
    pub domains: Domains,
    pub services: Services,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub subnet: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub endpoint_subdomain: Option<String>,
    #[serde(default)]
    pub exempt_from_proxy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkBinding {
    pub address: String,
    #[serde(default)]
    pub public_key: Option<String>,
    #[serde(default = "default_link_role")]
    pub role: LinkRole,
    #[serde(default)]
    pub external_interface: Option<String>,
    #[serde(default)]
    pub mac_address: Option<String>,
}

fn default_link_role() -> LinkRole {
    LinkRole::Client
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LinkRole {
    #[serde(rename = "server")]
    Server,
    #[serde(rename = "client")]
    Client,
    #[serde(rename = "peer")]
    Peer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    pub system: String,
    #[serde(default)]
    pub device_type: Option<DeviceType>,
    #[serde(default)]
    pub host_pubkey: Option<String>,
    #[serde(default)]
    pub host_names: Vec<String>,
    #[serde(default)]
    pub network: Network,
    #[serde(default)]
    pub rebuild: Rebuild,
    #[serde(default)]
    pub links: IndexMap<String, LinkBinding>,
    #[serde(default)]
    pub users: IndexMap<String, User>,
    #[serde(default)]
    pub gpu: Gpu,
    #[serde(default)]
    pub storage: Storage,
    #[serde(default)]
    pub data_root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeviceType {
    #[serde(rename = "server")]
    Server,
    #[serde(rename = "desktop")]
    Desktop,
    #[serde(rename = "laptop")]
    Laptop,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Network {
    #[serde(default)]
    pub lan_ip: Option<String>,
    #[serde(default)]
    pub lan_broadcast: Option<String>,
    #[serde(default)]
    pub mac_address: Option<String>,
    #[serde(default)]
    pub lan_interface: Option<String>,
    #[serde(default)]
    pub direct_link_ip: Option<String>,
    #[serde(default)]
    pub direct_link_mac: Option<String>,
    #[serde(default)]
    pub direct_link_interface: Option<String>,
    #[serde(default)]
    pub direct_link_peers: Vec<String>,
    #[serde(default)]
    pub wake_on_lan_interface: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rebuild {
    #[serde(default)]
    pub build_host: Option<String>,
    #[serde(default)]
    pub use_substitutes: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    #[serde(default)]
    pub has_account: bool,
    #[serde(default)]
    pub personal_pc: bool,
    #[serde(default)]
    pub gpg: Option<String>,
    #[serde(default)]
    pub signing_key: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Gpu {
    #[serde(default)]
    pub igpu: Option<String>,
    #[serde(default)]
    pub dgpu: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Storage {
    #[serde(default)]
    pub data_root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Domains {
    #[serde(default)]
    pub zones: Vec<String>,
    #[serde(default)]
    pub mail_subdomain: Option<String>,
    #[serde(default)]
    pub vpn_subdomain: Option<String>,
    #[serde(default)]
    pub managed_zones: Vec<String>,
    #[serde(default)]
    pub dynamic_hosts: Vec<DynamicHost>,
    #[serde(default)]
    pub codeberg_pages_sites: Vec<CodebergPagesSite>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicHost {
    pub fqdn: String,
    #[serde(default)]
    pub proxied: bool,
    #[serde(default)]
    pub zone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodebergPagesSite {
    pub subdomain: String,
    pub target_repo: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Services {
    #[serde(default = "default_ssh_port")]
    pub ssh_port: u16,
    #[serde(default)]
    pub host_ssh_key_path: Option<String>,
    #[serde(default)]
    pub host_ssh_pub_key_path: Option<String>,
    #[serde(default)]
    pub reverse_proxy_services: Vec<ReverseProxyService>,
    #[serde(default)]
    pub static_file_services: Vec<StaticFileService>,
    #[serde(default)]
    pub internal_services: Vec<InternalService>,
    #[serde(default)]
    pub email_identities: EmailIdentities,
}

fn default_ssh_port() -> u16 {
    1337
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReverseProxyService {
    pub name: String,
    #[serde(default)]
    pub hostname: Option<String>,
    pub port: u16,
    #[serde(default)]
    pub target_host: Option<String>,
    #[serde(default)]
    pub proxied: bool,
    #[serde(default)]
    pub cloudflare_proxied: bool,
    #[serde(default)]
    pub publish_cname: bool,
    #[serde(default)]
    pub vpn_only: bool,
    #[serde(default)]
    pub lan_exposed: bool,
    #[serde(default)]
    pub upstream_scheme: Option<String>,
    #[serde(default)]
    pub tls_server_name: Option<String>,
    #[serde(default)]
    pub service_host: Option<String>,
    #[serde(default)]
    pub zone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticFileService {
    pub name: String,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub cloudflare_proxied: bool,
    #[serde(default)]
    pub dns_comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InternalService {
    pub name: String,
    pub port: u16,
    #[serde(default)]
    pub target_host: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailIdentities {
    #[serde(default)]
    pub admin_email: Option<String>,
    #[serde(default)]
    pub noreply_email: Option<String>,
    #[serde(default)]
    pub cloudflare_contact_email: Option<String>,
    #[serde(default)]
    pub brevo_login: Option<String>,
    #[serde(default)]
    pub postmaster_email: Option<String>,
}

/// Evaluate a .pkl topology file and produce a typed Topology value.
pub async fn load_topology(path: &Path) -> miette::Result<Topology> {
    if !path.exists() {
        return Err(miette::miette!("File not found: {}", path.display()));
    }

    let mut evaluator = pklr::Evaluator::new();
    evaluator.set_base_path(path.parent().unwrap_or_else(|| Path::new(".")));

    let value = evaluator
        .eval_file_pub(path)
        .await
        .map_err(|e| miette::miette!("Failed to evaluate '{}': {e}", path.display()))?;

    let json_str = pklr_value_to_json(&value);
    let topo: Topology = serde_json::from_str(&json_str)
        .map_err(|e| miette::miette!("Failed to deserialize topology from JSON produced by pklr: {e}\n\nJSON was:\n{json_str}"))?;

    Ok(topo)
}

/// Convert a pklr::Value to a JSON string for serde consumption.
fn pklr_value_to_json(value: &pklr::Value) -> String {
    match value {
        pklr::Value::Null => "null".to_string(),
        pklr::Value::Bool(b) => b.to_string(),
        pklr::Value::Int(n) => n.to_string(),
        pklr::Value::Float(f) => {
            let s = format!("{}", f);
            if !s.contains('.') && !s.contains('e') && !s.contains('E') {
                format!("{}.0", s)
            } else {
                s
            }
        }
        pklr::Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| format!("\"{}\"", s)),
        pklr::Value::Object(map, _source) => {
            let mut out = "{".to_string();
            let mut first = true;
            for (k, v) in map.iter() {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&format!("\"{}\":{}", k, pklr_value_to_json(v)));
            }
            out.push('}');
            out
        }
        pklr::Value::List(items) => {
            let mut out = "[".to_string();
            let mut first = true;
            for item in items {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&pklr_value_to_json(item));
            }
            out.push(']');
            out
        }
        pklr::Value::Lambda(..) => "\"<lambda>\"".to_string(),
    }
}
