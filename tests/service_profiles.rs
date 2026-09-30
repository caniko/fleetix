use fleetix::topology::Topology;
use fleetix::validate::validate;
use serde_json::{Value, json};

fn topology(catalog: Value) -> Topology {
    serde_json::from_value(json!({
        "schemaVersion": 2,
        "hosts": {}, "links": {}, "domains": {},
        "services": {"catalog": catalog},
        "access": {"ssh": {"port": 22, "hostKeyPath": "/host.key", "hostPubKeyPath": "/host.pub"}}
    }))
    .unwrap()
}

#[test]
fn service_profiles_survive_round_trip_without_gatus_configuration() {
    let input = json!({"archive": {
        "displayName": "Archive", "category": "Storage", "visibility": "internal",
        "lifecycle": "planned", "exclusionReason": "Not deployed yet"
    }});
    let topology = topology(input);
    let output = serde_json::to_value(&topology).unwrap();
    assert_eq!(
        output["services"]["catalog"]["archive"]["displayName"],
        "Archive"
    );
    assert!(validate(&topology).is_ok());
}

#[test]
fn rejects_active_services_without_a_health_policy() {
    let topology = topology(json!({"database": {
        "displayName": "Database", "category": "Storage", "visibility": "internal"
    }}));
    let report = validate(&topology);
    assert!(
        report
            .errors()
            .any(|e| e.code == "service_profile.missing_health_policy")
    );
}

#[test]
fn rejects_unknown_probe_targets_and_publication_of_internal_services() {
    let topology = topology(json!({"database": {
        "displayName": "Database", "category": "Storage", "visibility": "internal",
        "health": {"health": {
            "visibility": "public",
            "probe": {"type": "http", "site": "missing"}
        }}
    }}));
    let report = validate(&topology);
    assert!(
        report
            .errors()
            .any(|e| e.code == "service_profile.unknown_site")
    );
    assert!(
        report
            .errors()
            .any(|e| e.code == "service_profile.visibility_escalation")
    );
}

#[test]
fn rejects_unit_globs_and_zero_freshness() {
    let topology = topology(json!({"jobs": {
        "displayName": "Jobs", "category": "Storage",
        "health": {"backup": {"probe": {
            "type": "job", "host": "hub", "unit": "backup*.service", "maxAgeSeconds": 0
        }}}
    }}));
    let report = validate(&topology);
    for code in ["invalid_unit", "invalid_freshness"] {
        assert!(
            report
                .errors()
                .any(|e| e.code == format!("service_profile.{code}"))
        );
    }
}

#[test]
fn rejects_public_checks_against_vpn_sites() {
    let mut topology = topology(json!({"web": {
        "displayName": "Website", "category": "Apps", "visibility": "public", "sites": ["web"],
        "health": {"health": {"probe": {"type": "http", "site": "web"}}}
    }}));
    topology.services.http_sites.insert("web".into(), serde_json::from_value(json!({
        "hostname": "web.example.test", "ingress": "vpn", "access": "vpn", "dnsPublication": "none", "routes": []
    })).unwrap());
    assert!(
        validate(&topology)
            .errors()
            .any(|e| e.code == "service_profile.private_probe")
    );
}
