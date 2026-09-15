use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Topology {
    #[serde(default)]
    pub schema_version: u16,
    pub links: IndexMap<String, Link>,
    pub hosts: IndexMap<String, Host>,
    pub domains: Domains,
    pub services: Services,
    #[serde(default)]
    pub deployment: Deployment,
    #[serde(default)]
    pub trust: Trust,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LinkRole {
    #[serde(rename = "server")]
    Server,
    #[serde(rename = "client")]
    Client,
    #[serde(rename = "peer")]
    Peer,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    pub system: String,
    #[serde(default)]
    pub device_type: Option<DeviceType>,
    #[serde(default = "default_availability_class")]
    pub availability_class: String,
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
    pub vpn_profiles: IndexMap<String, VpnProfile>,
    #[serde(default)]
    pub gpu: Gpu,
    #[serde(default)]
    pub storage: Storage,
}

fn default_availability_class() -> String {
    "unknown".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]

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
    #[serde(default)]
    pub build_cache: BuildCache,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildCache {
    #[serde(default)]
    pub enable: bool,
    #[serde(default)]
    pub package_attr_names: Vec<String>,
    #[serde(default)]
    pub key_prefix: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VpnProfile {
    pub provider: String,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub dns_servers: Vec<String>,
    pub connection: VpnConnection,
    #[serde(default)]
    pub port_forwarding: Option<VpnPortForwarding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum VpnConnection {
    #[serde(rename = "wireguard")]
    WireGuard(WireGuardVpnConnection),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireGuardVpnConnection {
    pub addresses: Vec<String>,
    pub private_key_ref: String,
    pub peers: Vec<WireGuardPeer>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireGuardPeer {
    pub public_key: String,
    pub endpoint: String,
    pub allowed_ips: Vec<String>,
    #[serde(default)]
    pub persistent_keepalive_seconds: Option<u16>,
    #[serde(default)]
    pub dynamic_endpoint_refresh_seconds: Option<u16>,
    #[serde(default)]
    pub dynamic_endpoint_refresh_restart_seconds: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum VpnPortForwarding {
    #[serde(rename = "nat-pmp")]
    NatPmp(NatPmpPortForwarding),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NatPmpPortForwarding {
    pub gateway: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Gpu {
    #[serde(default)]
    pub igpu: Option<String>,
    #[serde(default)]
    pub dgpu: Option<String>,
    #[serde(default)]
    pub media: Option<GpuMedia>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuMedia {
    pub vendor: String,
    pub render_node: String,
    pub libva_driver: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Storage {
    #[serde(default)]
    pub data_root: Option<String>,
    #[serde(default)]
    pub project_state_root: Option<String>,
    #[serde(default)]
    pub flake_root: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
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
    pub dns_zones: Vec<DnsZone>,
    #[serde(default)]
    pub pages_sites: Vec<PagesSite>,
    #[serde(default)]
    pub redirects: Vec<Redirect>,
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
pub struct DnsRecord {
    pub name: String,
    #[serde(rename = "type")]
    pub record_type: String,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub preference: Option<i64>,
    #[serde(default)]
    pub proxied: bool,
    #[serde(default = "default_true")]
    pub ttl_auto: bool,
    #[serde(default)]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsExclude {
    pub name: String,
    #[serde(rename = "type")]
    pub record_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DnsZone {
    pub name: String,
    #[serde(default = "default_dns_ttl")]
    pub default_ttl: i64,
    #[serde(default = "default_dns_mode")]
    pub mode: String,
    #[serde(default)]
    pub records: Vec<DnsRecord>,
    #[serde(default)]
    pub exclude: Vec<DnsExclude>,
}

fn default_true() -> bool {
    true
}

fn default_dns_ttl() -> i64 {
    300
}

fn default_dns_mode() -> String {
    "lenient".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PagesSite {
    pub subdomain: String,
    pub repository: String,
    pub cname_target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Services {
    #[serde(default)]
    pub endpoints: IndexMap<String, Endpoint>,
    #[serde(default)]
    pub http_sites: IndexMap<String, HttpSite>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    pub target_host: String,
    pub port: u16,
    pub transport: EndpointTransport,
    pub bind: EndpointBind,
    #[serde(default)]
    pub remote_via: Option<String>,
    #[serde(default)]
    pub tls_server_name: Option<String>,
    #[serde(default = "default_true")]
    pub tcp_probe: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EndpointTransport {
    Tcp,
    Http,
    Https,
    H2c,
}

impl EndpointTransport {
    pub fn is_http(self) -> bool {
        matches!(self, Self::Http | Self::Https | Self::H2c)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EndpointBind {
    Loopback,
    Lan,
    Vpn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpSite {
    pub hostname: String,
    pub ingress: String,
    pub access: HttpAccess,
    pub dns_publication: DnsPublication,
    pub routes: Vec<HttpRoute>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HttpAccess {
    Cloudflare,
    Direct,
    Vpn,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DnsPublication {
    Managed,
    External,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpRoute {
    #[serde(rename = "match")]
    pub matcher: HttpMatch,
    pub action: HttpAction,
    #[serde(default)]
    pub auth_policy: Option<String>,
    #[serde(default)]
    pub response_headers: IndexMap<String, Vec<String>>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpMatch {
    #[serde(default)]
    pub paths: Vec<PathMatch>,
    #[serde(default)]
    pub absent_query_params: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum PathMatch {
    Exact { value: String },
    Prefix { value: String },
}

impl PathMatch {
    pub fn value(&self) -> &str {
        match self {
            Self::Exact { value } | Self::Prefix { value } => value,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum HttpAction {
    Proxy {
        endpoint: String,
        #[serde(default, rename = "stripPrefix")]
        strip_prefix: Option<String>,
    },
    Files {
        #[serde(rename = "rootRef")]
        root_ref: String,
        #[serde(default = "default_index_names", rename = "indexNames")]
        index_names: Vec<String>,
    },
    Redirect {
        to: String,
        #[serde(default = "default_redirect_status")]
        status: u16,
        #[serde(default = "default_true", rename = "preserveUri")]
        preserve_uri: bool,
    },
    Respond {
        status: u16,
        #[serde(default)]
        body: Option<String>,
    },
}

fn default_index_names() -> Vec<String> {
    vec!["index.html".to_string()]
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthIntent {
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub endpoint: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceIntent {
    pub name: String,
    #[serde(default)]
    pub service_name: Option<String>,
    #[serde(default)]
    pub required_hosts: Vec<String>,
    #[serde(default)]
    pub preferred_hosts: Vec<String>,
    #[serde(default)]
    pub required_availability: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub health: HealthIntent,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Deployment {
    #[serde(default)]
    pub service_intents: Vec<ServiceIntent>,
    #[serde(default)]
    pub ingress_groups: IndexMap<String, IngressGroup>,
    #[serde(default)]
    pub local_access: IndexMap<String, LocalAccessPolicy>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalAccessPolicy {
    #[serde(default)]
    pub clients: Vec<String>,
    #[serde(default)]
    pub target_host: String,
    #[serde(default = "default_local_access_preferred_link")]
    pub preferred_link: String,
    #[serde(default = "default_local_access_fallback_link")]
    pub fallback_link: String,
    #[serde(default)]
    pub destination: Option<String>,
    #[serde(default)]
    pub tcp_ports: Vec<u16>,
    #[serde(default)]
    pub udp_ports: Vec<u16>,
}

fn default_local_access_preferred_link() -> String {
    "lan".to_string()
}

fn default_local_access_fallback_link() -> String {
    "wg-home".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngressGroup {
    pub scope: IngressScope,
    pub hosts: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IngressScope {
    Public,
    Vpn,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Trust {
    #[serde(default)]
    pub ssh_known_hosts: Vec<SshKnownHost>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshKnownHost {
    pub host_names: Vec<String>,
    pub public_keys: Vec<String>,
    #[serde(default)]
    pub provenance: Option<String>,
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
    let schema_version = entrypoint
        .lines()
        .find(|line| line.trim_start().starts_with("schemaVersion"))
        .map(str::trim)
        .unwrap_or("schemaVersion = 0");
    out.push_str("\n\n");
    out.push_str(schema_version);
    let shared_names = strip_imports(&read_to_string(src_dir.join("../shared/Names.pkl"))?);

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
    out.push_str(&shared_names);
    out.push_str("\n\n");
    out.push_str(
        &strip_imports(&read_to_string(src_dir.join("Domains.pkl"))?)
            .replace("new S.", "new ")
            .replace("N.names.", "names."),
    );
    out.push_str("\n\n");
    out.push_str(
        &strip_imports(&read_to_string(src_dir.join("Services.pkl"))?)
            .replace("new S.", "new ")
            .replace("N.names.", "names."),
    );
    let deployment_path = src_dir.join("Deployment.pkl");
    if deployment_path.exists() {
        out.push_str("\n\n");
        out.push_str(
            &strip_imports(&read_to_string(deployment_path)?)
                .replace("new S.", "new ")
                .replace("N.names.", "names."),
        );
    } else {
        out.push_str("\n\ndeployment = new Deployment {}\n");
    }
    let trust_path = src_dir.join("Trust.pkl");
    if trust_path.exists() {
        out.push_str("\n\n");
        out.push_str(
            &strip_imports(&read_to_string(trust_path)?)
                .replace("new S.", "new ")
                .replace("<S.", "<")
                .replace("N.names.", "names."),
        );
    } else {
        out.push_str("\n\ntrust = new Trust {}\n");
    }
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

pub(crate) fn matching_brace(input: &str, open_index: usize) -> Option<usize> {
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

/// Flatten a modular topology entrypoint into a temporary file the Pkl
/// evaluator can read.
pub(crate) fn flattened_tempfile(path: &Path) -> miette::Result<tempfile::NamedTempFile> {
    let flattened = flatten_modular_topology(path)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| miette::miette!("create temporary topology: {error}"))?;
    temporary
        .write_all(flattened.as_bytes())
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|error| miette::miette!("write temporary topology: {error}"))?;
    Ok(temporary)
}

/// Evaluate a .pkl topology file and produce a typed Topology value.
pub async fn load_topology(path: &Path) -> miette::Result<Topology> {
    load_topology_with_options(path, pklx::pklr::EvalOptions::default()).await
}

/// Evaluate a .pkl topology file with custom evaluator options.
pub async fn load_topology_with_options(
    path: &Path,
    options: pklx::pklr::EvalOptions,
) -> miette::Result<Topology> {
    let topology = evaluate_topology_with_options(path, options).await?;

    let report = crate::validate::validate(&topology);
    if report.is_ok() {
        Ok(topology)
    } else {
        let errors = report
            .errors()
            .map(|issue| format!("{}: {}", issue.code, issue.message))
            .collect::<Vec<_>>()
            .join("\n");
        Err(miette::miette!("invalid topology:\n{errors}"))
    }
}

pub(crate) async fn evaluate_topology_with_options(
    path: &Path,
    options: pklx::pklr::EvalOptions,
) -> miette::Result<Topology> {
    let topology =
        if path.file_name().and_then(|name| name.to_str()) == Some("Topology.aggregated.pkl") {
            let temporary = flattened_tempfile(path)?;
            crate::pkl::load_with_options(temporary.path(), options).await?
        } else {
            crate::pkl::load_with_options(path, options).await?
        };
    Ok(topology)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn imported_paths_keep_entrypoint_order() {
        let input = r#"
import "hosts/Hub.pkl"
import "links/WgHome.pkl"
import "hosts/Nomad.pkl"
"#;

        assert_eq!(
            imported_paths(input, "hosts/"),
            vec!["hosts/Hub.pkl", "hosts/Nomad.pkl"]
        );
    }

    #[test]
    fn unwrap_section_removes_only_outer_binding() {
        let input = r#"
links = new {
  wg-home = new Link {
    subnet = "198.51.100.0/24"
  }
}
"#;

        let body = unwrap_section(input, "links");
        assert!(body.contains("wg-home = new Link"));
        assert!(!body.contains("links = new"));
        assert!(body.contains("subnet = \"198.51.100.0/24\""));
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

    #[tokio::test]
    async fn flatten_modular_topology_mirrors_aggregate_import_shape() -> miette::Result<()> {
        let temp = tempfile::tempdir().map_err(|e| miette::miette!("create tempdir: {e}"))?;
        let root = temp.path().join("topology");
        fs::create_dir_all(&root).map_err(|e| miette::miette!("create topology dir: {e}"))?;
        fs::create_dir_all(root.join("links"))
            .map_err(|e| miette::miette!("create links dir: {e}"))?;
        fs::create_dir_all(root.join("hosts"))
            .map_err(|e| miette::miette!("create hosts dir: {e}"))?;
        fs::create_dir_all(root.join("../shared"))
            .map_err(|e| miette::miette!("create shared dir: {e}"))?;

        fs::write(
            root.join("Schema.pkl"),
            r#"
class Link {
  subnet: String
  port: UInt16 = 51820
}

class Host {
  system: String
}

class SshKnownHost {
  hostNames: Listing<String>
  publicKeys: Listing<String>
}

class Trust {
  sshKnownHosts: Listing<SshKnownHost> = new Listing {}
}

class Deployment {}
"#,
        )
        .map_err(|e| miette::miette!("write Schema.pkl: {e}"))?;
        fs::write(
            root.join("links/WgHome.pkl"),
            r#"
import "../Schema.pkl" as S

links = new {
  ["wg-home"] = new S.Link {
    subnet = "198.51.100.0/24"
  }
}
"#,
        )
        .map_err(|e| miette::miette!("write link fixture: {e}"))?;
        fs::write(
            root.join("hosts/Hub.pkl"),
            r#"
import "../Schema.pkl" as S

hosts = new {
  ["hub"] = new S.Host {
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
import "../shared/Names.pkl" as N

domains = new {
  zones = new Listing<String> {
    N.names.zone
  }
}
"#,
        )
        .map_err(|e| miette::miette!("write Domains.pkl: {e}"))?;
        fs::write(
            root.join("../shared/Names.pkl"),
            r#"
names = new {
  zone = "example.test"
}
"#,
        )
        .map_err(|e| miette::miette!("write Names.pkl: {e}"))?;
        fs::write(
            root.join("Services.pkl"),
            r#"
import "Schema.pkl" as S

services = new {
  endpoints = new {}
  httpSites = new {}
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
  hub = (import("hosts/Hub.pkl")).hosts["hub"]
}

domains = (import("Domains.pkl")).domains
services = (import("Services.pkl")).services
schemaVersion: UInt16 = 2
trust = (import("Trust.pkl")).trust
"#,
        )
        .map_err(|e| miette::miette!("write aggregate fixture: {e}"))?;
        fs::write(
            root.join("Trust.pkl"),
            r#"
import "Schema.pkl" as S

trust = new S.Trust {
  sshKnownHosts = new Listing<S.SshKnownHost> {
    new S.SshKnownHost {
      hostNames = new Listing { "git.example.test" }
      publicKeys = new Listing { "ssh-ed25519 AAAA" }
    }
  }
}
"#,
        )
        .map_err(|e| miette::miette!("write Trust fixture: {e}"))?;

        let flattened = flatten_modular_topology(&root.join("Topology.aggregated.pkl"))?;
        assert!(flattened.contains("[\"wg-home\"] = new Link"));
        assert!(flattened.contains("[\"hub\"] = new Host"));
        assert!(flattened.contains("names = new"));
        assert!(flattened.contains("names.zone"));
        assert!(!flattened.contains("N.names"));
        assert!(flattened.contains("domains = new"));
        assert!(flattened.contains("services = new"));
        assert!(flattened.contains("sshKnownHosts = new Listing<SshKnownHost>"));
        assert!(!flattened.contains("import "));

        let loaded = load_topology(&root.join("Topology.aggregated.pkl")).await?;
        assert!(loaded.hosts.contains_key("hub"));
        assert!(loaded.links.contains_key("wg-home"));
        assert_eq!(loaded.trust.ssh_known_hosts.len(), 1);
        assert_eq!(
            loaded.trust.ssh_known_hosts[0].host_names,
            vec!["git.example.test"]
        );

        Ok(())
    }
    #[tokio::test]
    async fn example_deserializes_tagged_variants() -> miette::Result<()> {
        let temp = tempfile::tempdir().map_err(|error| miette::miette!("tempdir: {error}"))?;
        let path = temp.path().join("Topology.pkl");
        std::fs::write(
            &path,
            r#"
schemaVersion = 2
links = new {}
hosts = new {
  ["edge"] = new {
    system = "x86_64-linux"
    users = new { ["alice"] = new { hasAccount = true } }
    vpnProfiles = new {
      ["example"] = new {
        provider = "Example VPN"
        owner = "alice"
        dnsServers = new Listing { "192.0.2.53" }
        connection = new {
          type = "wireguard"
          addresses = new Listing { "198.51.100.2/32" }
          privateKeyRef = "vpn/example/private-key"
          peers = new Listing {
            new {
              publicKey = "peer-public-key"
              endpoint = "192.0.2.2:51820"
              allowedIps = new Listing { "0.0.0.0/0" }
            }
          }
        }
        portForwarding = new { type = "nat-pmp"; gateway = "192.0.2.1" }
      }
    }
  }
  ["target"] = new { system = "x86_64-linux" }
}
domains = new {}
services = new {
  endpoints = new {
    ["dashboard"] = new {
      targetHost = "target"
      port = 8080
      transport = "http"
      bind = "vpn"
    }
  }
  httpSites = new {
    ["dashboard"] = new {
      hostname = "dashboard.example.test"
      ingress = "public"
      access = "direct"
      dnsPublication = "none"
      routes = new Listing {
        new {
          match = new {}
          action = new { type = "proxy"; endpoint = "dashboard" }
        }
      }
    }
  }
}
deployment = new {
  ingressGroups = new {
    ["public"] = new { scope = "public"; hosts = new Listing { "edge" } }
  }
}
"#,
        )
        .map_err(|error| miette::miette!("write topology: {error}"))?;
        let topology = load_topology(&path).await?;
        let endpoint = &topology.services.endpoints["dashboard"];
        assert_eq!(endpoint.bind, EndpointBind::Vpn);
        assert_eq!(serde_json::to_string(&endpoint.bind).unwrap(), r#""vpn""#);
        assert_eq!(
            topology
                .endpoint_for_ingress("dashboard", "edge")
                .unwrap()
                .0,
            "dashboard"
        );
        let dashboard = &topology.services.http_sites["dashboard"];
        assert!(matches!(
            dashboard.routes[0].action,
            HttpAction::Proxy { ref endpoint, .. } if endpoint == "dashboard"
        ));
        assert_eq!(topology.schema_version, 2);
        let profile = topology.vpn_profile("edge", "example").unwrap();
        assert!(matches!(profile.connection, VpnConnection::WireGuard(_)));
        assert!(matches!(
            profile.port_forwarding,
            Some(VpnPortForwarding::NatPmp(_))
        ));
        let encoded = serde_json::to_value(profile).unwrap();
        let decoded: VpnProfile = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), encoded);
        Ok(())
    }

    #[tokio::test]
    async fn load_rejects_absent_and_wrong_schema_versions() -> miette::Result<()> {
        let temp = tempfile::tempdir().map_err(|error| miette::miette!("tempdir: {error}"))?;
        for (name, schema) in [("absent", ""), ("wrong", "schemaVersion = 1")] {
            let path = temp.path().join(format!("{name}.pkl"));
            fs::write(
                &path,
                format!(
                    "{schema}\nlinks = new {{}}\nhosts = new {{}}\ndomains = new {{}}\nservices = new {{}}\n"
                ),
            )
            .map_err(|error| miette::miette!("write topology: {error}"))?;

            let error = load_topology(&path).await.expect_err("invalid schema");
            assert!(
                error
                    .to_string()
                    .contains("topology.unsupported_schema_version"),
                "unexpected error: {error:?}"
            );
        }
        Ok(())
    }
}
