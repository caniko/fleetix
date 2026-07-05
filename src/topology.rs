use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
#[serde(rename_all = "camelCase")]
pub struct Topology {
    pub links: IndexMap<String, Link>,
    pub hosts: IndexMap<String, Host>,
    pub domains: Domains,
    pub services: Services,
}

#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
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

#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
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

#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    PartialEq,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(derive(Debug), compare(PartialEq))]
pub enum LinkRole {
    #[serde(rename = "server")]
    Server,
    #[serde(rename = "client")]
    Client,
    #[serde(rename = "peer")]
    Peer,
}

#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
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

#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
pub enum DeviceType {
    #[serde(rename = "server")]
    Server,
    #[serde(rename = "desktop")]
    Desktop,
    #[serde(rename = "laptop")]
    Laptop,
}

#[derive(
    Debug, Default, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
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

#[derive(
    Debug, Default, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
#[serde(rename_all = "camelCase")]
pub struct Rebuild {
    #[serde(default)]
    pub build_host: Option<String>,
    #[serde(default)]
    pub use_substitutes: bool,
    #[serde(default)]
    pub build_cache: BuildCache,
}

#[derive(
    Debug, Default, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
#[serde(rename_all = "camelCase")]
pub struct BuildCache {
    #[serde(default)]
    pub enable: bool,
    #[serde(default)]
    pub package_attr_names: Vec<String>,
    #[serde(default)]
    pub key_prefix: Option<String>,
}

#[derive(
    Debug, Default, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
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

#[derive(
    Debug, Default, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
#[serde(rename_all = "camelCase")]
pub struct Gpu {
    #[serde(default)]
    pub igpu: Option<String>,
    #[serde(default)]
    pub dgpu: Option<String>,
}

#[derive(
    Debug, Default, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
#[serde(rename_all = "camelCase")]
pub struct Storage {
    #[serde(default)]
    pub data_root: Option<String>,
}

#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
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

#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
#[serde(rename_all = "camelCase")]
pub struct DynamicHost {
    pub fqdn: String,
    #[serde(default)]
    pub proxied: bool,
    #[serde(default)]
    pub zone: Option<String>,
}

#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
#[serde(rename_all = "camelCase")]
pub struct CodebergPagesSite {
    pub subdomain: String,
    pub target_repo: String,
}

#[derive(
    Debug, Default, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
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

#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
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

#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
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

#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
#[serde(rename_all = "camelCase")]
pub struct InternalService {
    pub name: String,
    pub port: u16,
    #[serde(default)]
    pub target_host: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(
    Debug, Default, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
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

/// Load a Topology from an rkyv archive (zero-copy access).
pub fn load_topology_from_rkyv(
    bytes: &[u8],
) -> Result<&rkyv::Archived<Topology>, rkyv::rancor::Error> {
    rkyv::access::<rkyv::Archived<Topology>, rkyv::rancor::Error>(bytes)
}

/// Evaluate a .pkl topology file and produce a typed Topology value.
pub async fn load_topology(path: &Path) -> miette::Result<Topology> {
    crate::pkl::load(path).await
}

/// Evaluate a .pkl topology file with custom evaluator options.
pub async fn load_topology_with_options(
    path: &Path,
    options: pklx::pklr::EvalOptions,
) -> miette::Result<Topology> {
    crate::pkl::load_with_options(path, options).await
}

/// Render a modular topology entrypoint into a self-contained compatibility
/// Pkl file for consumers whose evaluator cannot yet resolve local imports.
pub fn flatten_modular_topology(path: &Path) -> miette::Result<String> {
    let src_dir = path
        .parent()
        .ok_or_else(|| miette::miette!("topology path has no parent: {}", path.display()))?;
    let entrypoint = std::fs::read_to_string(path)
        .map_err(|e| miette::miette!("read topology entrypoint {}: {e}", path.display()))?;

    let mut out = String::new();
    out.push_str(
        "// Generated compatibility topology. Edit the modular topology source instead.\n\n",
    );
    out.push_str(&strip_imports(&read_to_string(src_dir.join("Schema.pkl"))?));
    out.push_str("\n\nlinks = new {\n");
    for rel in imported_paths(&entrypoint, "links/") {
        out.push_str(&unwrap_section(
            &strip_imports(&read_to_string(src_dir.join(&rel))?).replace("new S.", "new "),
            "links",
        ));
    }
    out.push_str("}\n\nhosts = new {\n");
    for rel in imported_paths(&entrypoint, "hosts/") {
        out.push_str(&unwrap_section(
            &strip_imports(&read_to_string(src_dir.join(&rel))?).replace("new S.", "new "),
            "hosts",
        ));
    }
    out.push_str("}\n\n");
    out.push_str(
        &strip_imports(&read_to_string(src_dir.join("Domains.pkl"))?).replace("new S.", "new "),
    );
    out.push_str("\n\n");
    out.push_str(
        &strip_imports(&read_to_string(src_dir.join("Services.pkl"))?).replace("new S.", "new "),
    );
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

fn read_to_string(path: PathBuf) -> miette::Result<String> {
    std::fs::read_to_string(&path).map_err(|e| miette::miette!("read {}: {e}", path.display()))
}

fn strip_imports(input: &str) -> String {
    input
        .lines()
        .filter(|line| !line.trim_start().starts_with("import "))
        .collect::<Vec<_>>()
        .join("\n")
}

fn imported_paths(input: &str, prefix: &str) -> Vec<String> {
    input
        .lines()
        .filter_map(|line| {
            let start = line.find("import(\"")? + "import(\"".len();
            let rest = &line[start..];
            let end = rest.find("\")")?;
            let rel = &rest[..end];
            rel.starts_with(prefix).then(|| rel.to_string())
        })
        .collect()
}

fn unwrap_section(input: &str, section: &str) -> String {
    let header = format!("{section} = new {{");
    let mut lines = Vec::new();
    for line in input.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&header) {
            let rest = rest.trim().strip_suffix('}').unwrap_or(rest.trim()).trim();
            if !rest.is_empty() {
                lines.push(rest.to_string());
            }
        } else {
            lines.push(line.to_string());
        }
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    if lines.last().is_some_and(|line| line.trim() == "}") {
        lines.pop();
    }
    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod flatten_tests {
    use super::*;

    #[test]
    fn imported_paths_keep_entrypoint_order() {
        let input = r#"
links = new {
  ["wg-home"] = (import("links/WgHome.pkl")).links["wg-home"]
  ["lan"] = (import("links/Lan.pkl")).links["lan"]
}
"#;
        assert_eq!(
            imported_paths(input, "links/"),
            vec!["links/WgHome.pkl", "links/Lan.pkl"]
        );
    }

    #[test]
    fn unwrap_section_removes_only_outer_binding() {
        let input = "links = new {\n  [\"wg\"] = new Link {}\n}\n";
        assert_eq!(unwrap_section(input, "links"), "  [\"wg\"] = new Link {}\n");
    }

    #[test]
    fn unwrap_section_handles_comments_and_single_line_bindings() {
        let input =
            "// comment\nlinks = new { [\"lan\"] = new Link { subnet = \"192.0.2.0/24\" } }\n";
        assert_eq!(
            unwrap_section(input, "links"),
            "// comment\n[\"lan\"] = new Link { subnet = \"192.0.2.0/24\" }\n"
        );
    }
}
