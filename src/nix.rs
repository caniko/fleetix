//! Serialize fleet topology to a Nix expression (for `.fleetix-topology.nix` sidecar).

use crate::topology::*;
use indexmap::IndexMap;
use std::fmt::Write;

/// Serialize the entire topology to a Nix attrset expression.
pub fn topology_to_nix(topo: &Topology) -> String {
    let mut out = String::new();
    out.push_str("{\n");

    // links
    out.push_str("  links = {\n");
    for (name, link) in &topo.links {
        out.push_str(&format!("    {} = {};\n", nix_key(name), link_to_nix(link)));
    }
    out.push_str("  };\n");

    // hosts
    out.push_str("  hosts = {\n");
    for (name, host) in &topo.hosts {
        out.push_str(&format!("    {} = {};\n", nix_key(name), host_to_nix(host)));
    }
    out.push_str("  };\n");

    // domains
    out.push_str(&format!("  domains = {};\n", domains_to_nix(&topo.domains)));

    // services
    out.push_str(&format!(
        "  services = {};\n",
        services_to_nix(&topo.services)
    ));

    out.push('}');
    out
}

fn link_to_nix(link: &Link) -> String {
    let mut out = "{ ".to_string();
    out.push_str(&format!("__pkl_class = \"Link\"; "));
    out.push_str(&format!("subnet = {}; ", nix_str(&link.subnet)));
    out.push_str(&format!("port = {}; ", link.port));
    if let Some(es) = &link.endpoint_subdomain {
        out.push_str(&format!("endpointSubdomain = {}; ", nix_str(es)));
    } else {
        out.push_str("endpointSubdomain = null; ");
    }
    out.push_str(&format!(
        "exemptFromProxy = {}; ",
        nix_bool(link.exempt_from_proxy)
    ));
    out.push('}');
    out
}

fn host_to_nix(host: &Host) -> String {
    let mut out = "{ ".to_string();
    out.push_str(&format!("__pkl_class = \"Host\"; "));
    out.push_str(&format!("system = {}; ", nix_str(&host.system)));
    if let Some(dt) = &host.device_type {
        out.push_str(&format!(
            "deviceType = {}; ",
            nix_str(&format!("{dt:?}").to_lowercase())
        ));
    } else {
        out.push_str("deviceType = null; ");
    }
    if let Some(pk) = &host.host_pubkey {
        out.push_str(&format!("hostPubkey = {}; ", nix_str(pk)));
    } else {
        out.push_str("hostPubkey = null; ");
    }
    out.push_str(&format!("hostNames = {}; ", nix_str_list(&host.host_names)));
    out.push_str(&format!("network = {}; ", network_to_nix(&host.network)));
    out.push_str(&format!("rebuild = {}; ", rebuild_to_nix(&host.rebuild)));
    out.push_str(&format!("links = {}; ", link_bindings_to_nix(&host.links)));
    out.push_str(&format!("users = {}; ", users_to_nix(&host.users)));
    out.push_str(&format!("gpu = {}; ", gpu_to_nix(&host.gpu)));
    out.push_str(&format!("storage = {}; ", storage_to_nix(&host.storage)));
    out.push('}');
    out
}

fn network_to_nix(net: &Network) -> String {
    let mut out = "{ __pkl_class = \"Network\"; ".to_string();
    field_opt(&mut out, "lanIp", &net.lan_ip);
    field_opt(&mut out, "lanBroadcast", &net.lan_broadcast);
    field_opt(&mut out, "macAddress", &net.mac_address);
    field_opt(&mut out, "lanInterface", &net.lan_interface);
    field_opt(&mut out, "directLinkIp", &net.direct_link_ip);
    field_opt(&mut out, "directLinkMac", &net.direct_link_mac);
    field_opt(&mut out, "directLinkInterface", &net.direct_link_interface);
    out.push_str(&format!(
        "directLinkPeers = [{}]; ",
        net.direct_link_peers
            .iter()
            .map(|p| nix_str(p))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    field_opt(&mut out, "wakeOnLanInterface", &net.wake_on_lan_interface);
    out.push('}');
    out
}

fn rebuild_to_nix(rb: &Rebuild) -> String {
    let mut out = "{ __pkl_class = \"Rebuild\"; ".to_string();
    field_opt(&mut out, "buildHost", &rb.build_host);
    out.push_str(&format!(
        "useSubstitutes = {}; ",
        nix_bool(rb.use_substitutes)
    ));
    out.push('}');
    out
}

fn link_bindings_to_nix(lbs: &IndexMap<String, LinkBinding>) -> String {
    let mut out = "{ ".to_string();
    for (name, lb) in lbs {
        out.push_str(&format!(
            "{} = {}; ",
            nix_key(name),
            link_binding_to_nix(lb)
        ));
    }
    out.push('}');
    out
}

fn link_binding_to_nix(lb: &LinkBinding) -> String {
    let mut out = "{ __pkl_class = \"LinkBinding\"; ".to_string();
    out.push_str(&format!("address = {}; ", nix_str(&lb.address)));
    field_opt(&mut out, "publicKey", &lb.public_key);
    out.push_str(&format!(
        "role = {}; ",
        nix_str(&format!("{:?}", lb.role).to_lowercase())
    ));
    field_opt(&mut out, "externalInterface", &lb.external_interface);
    field_opt(&mut out, "macAddress", &lb.mac_address);
    out.push('}');
    out
}

fn users_to_nix(users: &IndexMap<String, User>) -> String {
    let mut out = "{ ".to_string();
    for (name, user) in users {
        out.push_str(&format!("{} = {}; ", nix_key(name), user_to_nix(user)));
    }
    out.push('}');
    out
}

fn user_to_nix(user: &User) -> String {
    let mut out = "{ __pkl_class = \"User\"; ".to_string();
    out.push_str(&format!("hasAccount = {}; ", nix_bool(user.has_account)));
    out.push_str(&format!("personalPc = {}; ", nix_bool(user.personal_pc)));
    field_opt(&mut out, "gpg", &user.gpg);
    field_opt(&mut out, "signingKey", &user.signing_key);
    out.push('}');
    out
}

fn gpu_to_nix(gpu: &Gpu) -> String {
    let mut out = "{ __pkl_class = \"Gpu\"; ".to_string();
    field_opt(&mut out, "igpu", &gpu.igpu);
    field_opt(&mut out, "dgpu", &gpu.dgpu);
    out.push('}');
    out
}

fn storage_to_nix(st: &Storage) -> String {
    let mut out = "{ __pkl_class = \"Storage\"; ".to_string();
    field_opt(&mut out, "dataRoot", &st.data_root);
    out.push('}');
    out
}

fn domains_to_nix(d: &Domains) -> String {
    let mut out = "{ __pkl_class = \"Domains\"; ".to_string();
    out.push_str(&format!(
        "zones = [{}]; ",
        d.zones
            .iter()
            .map(|s| nix_str(s))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    field_opt(&mut out, "mailSubdomain", &d.mail_subdomain);
    field_opt(&mut out, "vpnSubdomain", &d.vpn_subdomain);
    out.push_str(&format!(
        "managedZones = [{}]; ",
        d.managed_zones
            .iter()
            .map(|s| nix_str(s))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    out.push_str(&format!(
        "dynamicHosts = [{}]; ",
        d.dynamic_hosts
            .iter()
            .map(dynamic_host_to_nix)
            .collect::<Vec<_>>()
            .join(" ")
    ));
    out.push_str(&format!(
        "codebergPagesSites = [{}]; ",
        d.codeberg_pages_sites
            .iter()
            .map(codeberg_pages_site_to_nix)
            .collect::<Vec<_>>()
            .join(" ")
    ));
    out.push('}');
    out
}

fn dynamic_host_to_nix(dh: &DynamicHost) -> String {
    let mut out = "{ __pkl_class = \"DynamicHost\"; ".to_string();
    out.push_str(&format!("fqdn = {}; ", nix_str(&dh.fqdn)));
    out.push_str(&format!("proxied = {}; ", nix_bool(dh.proxied)));
    field_opt(&mut out, "zone", &dh.zone);
    out.push('}');
    out
}

fn codeberg_pages_site_to_nix(site: &CodebergPagesSite) -> String {
    let mut out = "{ __pkl_class = \"CodebergPagesSite\"; ".to_string();
    out.push_str(&format!("subdomain = {}; ", nix_str(&site.subdomain)));
    out.push_str(&format!("targetRepo = {}; ", nix_str(&site.target_repo)));
    out.push('}');
    out
}

fn services_to_nix(s: &Services) -> String {
    let mut out = "{ __pkl_class = \"Services\"; ".to_string();
    out.push_str(&format!("sshPort = {}; ", s.ssh_port));
    field_opt(&mut out, "hostSshKeyPath", &s.host_ssh_key_path);
    field_opt(&mut out, "hostSshPubKeyPath", &s.host_ssh_pub_key_path);
    out.push_str(&format!(
        "reverseProxyServices = [{}]; ",
        s.reverse_proxy_services
            .iter()
            .map(rps_to_nix)
            .collect::<Vec<_>>()
            .join(" ")
    ));
    out.push_str(&format!(
        "staticFileServices = [{}]; ",
        s.static_file_services
            .iter()
            .map(sfs_to_nix)
            .collect::<Vec<_>>()
            .join(" ")
    ));
    out.push_str(&format!(
        "internalServices = [{}]; ",
        s.internal_services
            .iter()
            .map(internal_service_to_nix)
            .collect::<Vec<_>>()
            .join(" ")
    ));
    out.push_str(&format!(
        "emailIdentities = {}; ",
        email_identities_to_nix(&s.email_identities)
    ));
    out.push('}');
    out
}

fn rps_to_nix(rps: &ReverseProxyService) -> String {
    let mut out = "{ __pkl_class = \"ReverseProxyService\"; ".to_string();
    out.push_str(&format!("name = {}; ", nix_str(&rps.name)));
    field_opt(&mut out, "hostname", &rps.hostname);
    out.push_str(&format!("port = {}; ", rps.port));
    field_opt(&mut out, "targetHost", &rps.target_host);
    out.push_str(&format!("proxied = {}; ", nix_bool(rps.proxied)));
    out.push_str(&format!(
        "cloudflareProxied = {}; ",
        nix_bool(rps.cloudflare_proxied)
    ));
    out.push_str(&format!("publishCname = {}; ", nix_bool(rps.publish_cname)));
    out.push_str(&format!("vpnOnly = {}; ", nix_bool(rps.vpn_only)));
    field_opt(&mut out, "upstreamScheme", &rps.upstream_scheme);
    field_opt(&mut out, "tlsServerName", &rps.tls_server_name);
    field_opt(&mut out, "serviceHost", &rps.service_host);
    field_opt(&mut out, "zone", &rps.zone);
    out.push('}');
    out
}

fn sfs_to_nix(sfs: &StaticFileService) -> String {
    let mut out = "{ __pkl_class = \"StaticFileService\"; ".to_string();
    out.push_str(&format!("name = {}; ", nix_str(&sfs.name)));
    field_opt(&mut out, "hostname", &sfs.hostname);
    field_opt(&mut out, "kind", &sfs.kind);
    out.push_str(&format!(
        "cloudflareProxied = {}; ",
        nix_bool(sfs.cloudflare_proxied)
    ));
    field_opt(&mut out, "dnsComment", &sfs.dns_comment);
    out.push('}');
    out
}

fn internal_service_to_nix(svc: &InternalService) -> String {
    let mut out = "{ __pkl_class = \"InternalService\"; ".to_string();
    out.push_str(&format!("name = {}; ", nix_str(&svc.name)));
    out.push_str(&format!("port = {}; ", svc.port));
    field_opt(&mut out, "targetHost", &svc.target_host);
    field_opt(&mut out, "description", &svc.description);
    out.push('}');
    out
}

fn email_identities_to_nix(ids: &EmailIdentities) -> String {
    let mut out = "{ __pkl_class = \"EmailIdentities\"; ".to_string();
    field_opt(&mut out, "adminEmail", &ids.admin_email);
    field_opt(&mut out, "noreplyEmail", &ids.noreply_email);
    field_opt(
        &mut out,
        "cloudflareContactEmail",
        &ids.cloudflare_contact_email,
    );
    field_opt(&mut out, "brevoLogin", &ids.brevo_login);
    field_opt(&mut out, "postmasterEmail", &ids.postmaster_email);
    out.push('}');
    out
}

// ── helpers ────────────────────────────────────────────────────────

fn nix_str(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!("\"{escaped}\"")
}

fn nix_key(s: &str) -> String {
    // Nix allows bare identifiers that match [a-zA-Z_][a-zA-Z0-9_'-]*
    if s.contains(|c: char| !c.is_alphanumeric() && c != '_' && c != '-') {
        nix_str(s)
    } else {
        s.to_string()
    }
}

fn nix_bool(b: bool) -> &'static str {
    if b {
        "true"
    } else {
        "false"
    }
}

fn nix_str_list(v: &[String]) -> String {
    if v.is_empty() {
        "[ ]".to_string()
    } else {
        format!(
            "[{}]",
            v.iter()
                .map(|s| format!(" {}", nix_str(s)))
                .collect::<Vec<_>>()
                .join("")
        )
    }
}

fn field_opt(out: &mut String, name: &str, val: &Option<String>) {
    if let Some(v) = val {
        let _ = write!(out, "{name} = {}; ", nix_str(v));
    } else {
        let _ = write!(out, "{name} = null; ");
    }
}
