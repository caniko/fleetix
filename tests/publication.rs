use fleetix::{Topology, validate};
use serde_json::{Value, json};

fn fixture() -> Value {
    json!({
        "schemaVersion": 2, "links": {},
        "hosts": {"edge": {"system": "x86_64-linux"}},
        "domains": {"zones": ["example.com"],
            "publicationTargets": {"edge": {
                "hostname": "edge.example.com", "targetHost": "edge", "ipv4": "192.0.2.10"
            }},
            "pagesSites": [{"subdomain": "docs", "repository": "owner/docs", "cnameTarget": "docs.codeberg.page"}]
        },
        "services": {"httpSites": {"media": {
            "hostname": "media.example.com", "ingress": "public", "access": "direct",
            "dnsPublication": "managed", "publicationTarget": "edge",
            "routes": [{"match": {}, "action": {"type": "respond", "status": 200}}]
        }}},
        "deployment": {"ingressGroups": {"public": {"scope": "public", "hosts": ["edge"]}}}
    })
}

fn parse(value: Value) -> Topology {
    serde_json::from_value(value).unwrap()
}

#[test]
fn managed_cname_uses_explicit_destination_and_preserves_pages() {
    let t = parse(fixture());
    assert!(validate(&t).is_ok());
    assert_eq!(t.service_cname_intents()[0].target, "edge.example.com");
    assert!(!t.service_cname_intents()[0].proxied);
    assert_eq!(t.pages_cname_intents(None)[0].target, "docs.codeberg.page");
    let addresses = t.publication_address_intents();
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].hostname, "edge.example.com");
    assert_eq!(addresses[0].ipv4, "192.0.2.10");
    assert!(addresses[0].ipv6.is_none());
}

#[test]
fn apex_and_destination_sites_emit_addresses_without_self_cnames() {
    for hostname in ["example.com", "edge.example.com"] {
        let mut t = fixture();
        t["services"]["httpSites"]["media"]["hostname"] = json!(hostname);
        let t = parse(t);
        assert!(validate(&t).is_ok());
        assert!(t.service_cname_intents().is_empty());
        assert!(
            t.publication_address_intents()
                .iter()
                .any(|intent| intent.hostname == hostname && intent.ipv4 == "192.0.2.10")
        );
    }
}

#[test]
fn unselected_legacy_sites_keep_the_zone_apex_and_private_names_stay_unpublished() {
    let mut value = fixture();
    value["services"]["httpSites"]["media"]["publicationTarget"] = Value::Null;
    value["services"]["httpSites"]["media"]["access"] = json!("cloudflare");
    let t = parse(value.clone());
    assert!(validate(&t).is_ok());
    assert_eq!(t.service_cname_intents()[0].target, "example.com");
    assert!(t.service_cname_intents()[0].proxied);
    value["services"]["httpSites"]["media"]["access"] = json!("vpn");
    value["services"]["httpSites"]["media"]["dnsPublication"] = json!("none");
    assert!(parse(value).service_cname_intents().is_empty());
}

#[test]
fn publication_rejects_unknown_targets_and_non_public_sites() {
    for (field, value, code) in [
        (
            "publicationTarget",
            json!("missing"),
            "publication.unknown_target",
        ),
        ("access", json!("vpn"), "publication.invalid_site_policy"),
        (
            "access",
            json!("cloudflare"),
            "publication.invalid_site_policy",
        ),
        (
            "dnsPublication",
            json!("external"),
            "publication.invalid_site_policy",
        ),
    ] {
        let mut t = fixture();
        t["services"]["httpSites"]["media"][field] = value;
        assert!(
            validate(&parse(t)).errors().any(|e| e.code == code),
            "missing {code}"
        );
    }
}

#[test]
fn publication_rejects_competing_ddns_and_excluded_address_writers() {
    for field in ["dynamicHosts", "dnsZones"] {
        let mut t = fixture();
        t["domains"][field] = if field == "dynamicHosts" {
            json!([{"fqdn": "edge.example.com"}])
        } else {
            json!([{"name": "example.com", "exclude": [{"name": "edge", "type": "AAAA"}]}])
        };
        assert!(
            validate(&parse(t))
                .errors()
                .any(|e| e.code == "publication.competing_dns_writer")
        );
    }
}

#[test]
fn publication_validates_host_addresses_and_explicit_pages_ownership() {
    for (field, value, code) in [
        ("targetHost", json!("absent"), "publication.unknown_host"),
        ("ipv4", json!("::1"), "publication.invalid_ipv4"),
        ("ipv4", json!("127.0.0.1"), "publication.invalid_ipv4"),
        ("ipv6", json!("192.0.2.1"), "publication.invalid_ipv6"),
        (
            "hostname",
            json!("edge.invalid"),
            "publication.invalid_hostname",
        ),
    ] {
        let mut t = fixture();
        t["domains"]["publicationTargets"]["edge"][field] = value;
        assert!(
            validate(&parse(t)).errors().any(|e| e.code == code),
            "missing {code}"
        );
    }
    for hostname in ["edge.example.com", "media.example.com"] {
        let mut t = fixture();
        let owner = hostname.strip_suffix(".example.com").unwrap();
        t["domains"]["dnsZones"] = json!([{"name": "example.com", "records": [{"name": owner, "type": "A", "data": "198.51.100.1"}]}]);
        assert!(
            validate(&parse(t))
                .errors()
                .any(|e| e.code == "publication.competing_dns_writer")
        );
    }
    let mut t = fixture();
    t["domains"]["publicationTargets"]["edge"]["hostname"] = json!("docs.example.com");
    assert!(
        validate(&parse(t))
            .errors()
            .any(|e| e.code == "publication.competing_dns_writer")
    );
}

#[test]
fn dns_writer_conflicts_are_case_insensitive() {
    let mut t = fixture();
    t["domains"]["dynamicHosts"] = json!([{"fqdn": "EDGE.example.com"}]);
    assert!(
        validate(&parse(t))
            .errors()
            .any(|e| e.code == "publication.competing_dns_writer")
    );
    let mut t = fixture();
    t["domains"]["dnsZones"] =
        json!([{"name": "example.com", "exclude": [{"name": "EDGE", "type": "AAAA"}]}]);
    assert!(
        validate(&parse(t))
            .errors()
            .any(|e| e.code == "publication.competing_dns_writer")
    );
}
