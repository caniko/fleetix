use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(
    Debug, Default, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
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
    Eq,
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
    Debug, Default, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
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
}

#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
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
    #[serde(default)]
    pub redirects: Vec<Redirect>,
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
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
#[serde(rename_all = "camelCase")]
pub struct Redirect {
    pub from: String,
    pub to: String,
    #[serde(default = "default_redirect_status")]
    pub status: u16,
    #[serde(default = "default_redirect_preserve_path")]
    pub preserve_path: bool,
}

fn default_redirect_status() -> u16 {
    301
}

fn default_redirect_preserve_path() -> bool {
    true
}

fn default_publish_cname() -> bool {
    true
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
    #[serde(default = "default_publish_cname")]
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

/// Load a legacy unversioned Topology rkyv archive (zero-copy access).
///
/// Prefer [`load_topology_archive_from_rkyv`] for persisted or cross-release
/// bytes; this function remains for callers of the original raw format.
pub fn load_topology_from_rkyv(
    bytes: &[u8],
) -> Result<&rkyv::Archived<Topology>, rkyv::rancor::Error> {
    rkyv::access::<rkyv::Archived<Topology>, rkyv::rancor::Error>(bytes)
}

/// Version marker for the self-describing archive envelope.
pub const TOPOLOGY_ARCHIVE_VERSION: u16 = 1;

/// Versioned archive payload for consumers that need to persist topology
/// archives across process or release boundaries.
#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
#[serde(rename_all = "camelCase")]
pub struct TopologyArchive {
    pub schema_version: u16,
    pub topology: Topology,
}

/// Serialize a topology with an explicit archive schema version.
pub fn archive_topology(topology: &Topology) -> Result<Vec<u8>, rkyv::rancor::Error> {
    rkyv::to_bytes::<rkyv::rancor::Error>(&TopologyArchive {
        schema_version: TOPOLOGY_ARCHIVE_VERSION,
        topology: topology.clone(),
    })
    .map(|bytes| bytes.to_vec())
}

#[derive(Debug)]
pub enum TopologyArchiveError {
    Archive(rkyv::rancor::Error),
    UnsupportedVersion(u16),
}

impl std::fmt::Display for TopologyArchiveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Archive(error) => write!(formatter, "archive decode failed: {error}"),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported topology archive schema version {version}"
                )
            }
        }
    }
}

impl std::error::Error for TopologyArchiveError {}

/// Load a versioned topology archive and reject versions this library cannot
/// interpret.
pub fn load_topology_archive_from_rkyv(
    bytes: &[u8],
) -> Result<&rkyv::Archived<TopologyArchive>, TopologyArchiveError> {
    let archive = rkyv::access::<rkyv::Archived<TopologyArchive>, rkyv::rancor::Error>(bytes)
        .map_err(TopologyArchiveError::Archive)?;
    if archive.schema_version != TOPOLOGY_ARCHIVE_VERSION {
        return Err(TopologyArchiveError::UnsupportedVersion(
            archive.schema_version.into(),
        ));
    }
    Ok(archive)
}

/// Render a modular topology entrypoint into a self-contained compatibility
/// Pkl file for consumers whose evaluator cannot resolve local aggregate
/// imports.
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
    let mut paths: Vec<String> = input
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("import \"")?;
            let (path, _) = rest.split_once('"')?;
            path.starts_with(prefix).then(|| path.to_string())
        })
        .collect();

    let mut rest = input;
    while let Some(index) = rest.find("import(\"") {
        let after_import = &rest[index + "import(\"".len()..];
        let Some((path, after_path)) = after_import.split_once('"') else {
            break;
        };
        if path.starts_with(prefix) {
            paths.push(path.to_string());
        }
        rest = after_path;
    }

    paths
}

fn unwrap_section(input: &str, section: &str) -> String {
    let needle = format!("{section} = new");
    let Some(start) = input.find(&needle) else {
        return input.to_string();
    };
    let Some(open_rel) = input[start..].find('{') else {
        return input.to_string();
    };
    let body_start = start + open_rel + 1;
    let Some(body_end) = matching_brace(input, body_start - 1) else {
        return input.to_string();
    };

    input[body_start..body_end].to_string()
}

fn matching_brace(input: &str, open_index: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut escaped = false;
    let mut in_string = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;

    for (offset, ch) in input[open_index..].char_indices() {
        let absolute = open_index + offset;
        if in_line_comment {
            if ch == '\n' {
                in_line_comment = false;
            }
            continue;
        }
        if in_block_comment {
            if ch == '*' && input[absolute..].starts_with("*/") {
                in_block_comment = false;
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
        } else if ch == '/' && input[absolute..].starts_with("//") {
            in_line_comment = true;
        } else if ch == '/' && input[absolute..].starts_with("/*") {
            in_block_comment = true;
        } else if ch == '{' {
            depth += 1;
        } else if ch == '}' {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(absolute);
            }
        }
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use std::fs;

    #[test]
    fn imported_paths_keep_entrypoint_order() {
        let input = r#"
import "hosts/Atlas.pkl"
import "links/WgHome.pkl"
import "hosts/Nomad.pkl"
"#;

        assert_eq!(
            imported_paths(input, "hosts/"),
            vec!["hosts/Atlas.pkl", "hosts/Nomad.pkl"]
        );
    }

    #[test]
    fn unwrap_section_removes_only_outer_binding() {
        let input = r#"
links = new {
  wg-home = new Link {
    subnet = "10.123.0.0/24"
  }
}
"#;

        let body = unwrap_section(input, "links");
        assert!(body.contains("wg-home = new Link"));
        assert!(!body.contains("links = new"));
        assert!(body.contains("subnet = \"10.123.0.0/24\""));
    }

    #[test]
    fn unwrap_section_ignores_braces_in_strings_and_comments() {
        let input = r#"
links = new {
  wg-home = new Link {
    description = "literal { brace }"
    // comment with a closing brace }
    nested = new { value = "still { text }" }
  }
}
"#;

        let body = unwrap_section(input, "links");
        assert!(body.contains("description = \"literal { brace }\""));
        assert!(body.contains("nested = new { value = \"still { text }\" }"));
    }

    #[test]
    fn flatten_modular_topology_mirrors_aggregate_import_shape() -> miette::Result<()> {
        let temp = tempfile::tempdir().map_err(|e| miette::miette!("create tempdir: {e}"))?;
        let root = temp.path();
        fs::create_dir_all(root.join("links"))
            .map_err(|e| miette::miette!("create links dir: {e}"))?;
        fs::create_dir_all(root.join("hosts"))
            .map_err(|e| miette::miette!("create hosts dir: {e}"))?;

        fs::write(
            root.join("Schema.pkl"),
            r#"
class Link {
  subnet: String
}

class Host {
  system: String
}
"#,
        )
        .map_err(|e| miette::miette!("write Schema.pkl: {e}"))?;
        fs::write(
            root.join("links/WgHome.pkl"),
            r#"
import "../Schema.pkl" as S

links = new {
  ["wg-home"] = new S.Link {
    subnet = "10.123.0.0/24"
  }
}
"#,
        )
        .map_err(|e| miette::miette!("write link fixture: {e}"))?;
        fs::write(
            root.join("hosts/Atlas.pkl"),
            r#"
import "../Schema.pkl" as S

hosts = new {
  ["atlas"] = new S.Host {
    system = "x86_64-linux"
  }
}
"#,
        )
        .map_err(|e| miette::miette!("write host fixture: {e}"))?;
        fs::write(
            root.join("Domains.pkl"),
            r#"
import "Schema.pkl" as S

domains = new {
  zones = new Listing<String> {
    "example.test"
  }
}
"#,
        )
        .map_err(|e| miette::miette!("write Domains.pkl: {e}"))?;
        fs::write(
            root.join("Services.pkl"),
            r#"
import "Schema.pkl" as S

services = new {
  reverseProxyServices = new Listing {}
}
"#,
        )
        .map_err(|e| miette::miette!("write Services.pkl: {e}"))?;
        fs::write(
            root.join("Topology.aggregated.pkl"),
            r#"
links = new {
  ["wg-home"] = (import("links/WgHome.pkl")).links["wg-home"]
}

hosts = new {
  atlas = (import("hosts/Atlas.pkl")).hosts["atlas"]
}

domains = (import("Domains.pkl")).domains
services = (import("Services.pkl")).services
"#,
        )
        .map_err(|e| miette::miette!("write aggregate fixture: {e}"))?;

        let flattened = flatten_modular_topology(&root.join("Topology.aggregated.pkl"))?;
        assert!(flattened.contains("[\"wg-home\"] = new Link"));
        assert!(flattened.contains("[\"atlas\"] = new Host"));
        assert!(flattened.contains("domains = new"));
        assert!(flattened.contains("services = new"));
        assert!(!flattened.contains("import "));

        Ok(())
    }

    #[test]
    fn versioned_archive_round_trips_with_schema_marker() {
        let topology = Topology {
            links: IndexMap::new(),
            hosts: IndexMap::new(),
            domains: Domains {
                zones: vec!["example.test".to_string()],
                mail_subdomain: None,
                vpn_subdomain: None,
                managed_zones: vec![],
                dynamic_hosts: vec![],
                codeberg_pages_sites: vec![],
                redirects: vec![],
            },
            services: Services::default(),
        };

        let bytes = archive_topology(&topology).expect("archive topology");
        let archived = load_topology_archive_from_rkyv(&bytes).expect("load archive");
        assert_eq!(archived.schema_version, TOPOLOGY_ARCHIVE_VERSION);
        assert_eq!(archived.topology.links.len(), 0);
        assert_eq!(archived.topology.hosts.len(), 0);
    }
}
