use fleetix::{Topology, validate};
use serde_json::{Value, json};

fn topology(management: Value, key: Value) -> Topology {
    serde_json::from_value(json!({
        "schemaVersion": 2,
        "links": {"wg-edge": {"subnet": "10.90.0.0/24", "port": 51820}},
        "hosts": {
            "builder": {"system": "x86_64-linux", "links": {
                "wg-edge": {"address": "10.90.0.2", "publicKey": "builder-key"}
            }},
            "edge": {"system": "x86_64-linux", "hostPubkey": key,
                "management": management,
                "links": {"wg-edge": {"address": "10.90.0.1", "publicKey": "edge-key"}}
            }
        },
        "domains": {}, "services": {}
    }))
    .unwrap()
}

#[test]
fn cloud_management_requires_runtime_identity() {
    let t = topology(
        json!({"publicAddress": "192.0.2.10", "sshPort": 1337}),
        Value::Null,
    );
    assert!(
        validate(&t)
            .errors()
            .any(|e| e.code == "management.missing_host_key")
    );
}

#[test]
fn cloud_management_rejects_invalid_addresses_ports_and_links() {
    for (management, code) in [
        (
            json!({"publicAddress": "-oProxyCommand=bad"}),
            "management.invalid_public_address",
        ),
        (
            json!({"publicAddress": "127.0.0.1"}),
            "management.invalid_public_address",
        ),
        (
            json!({"publicAddress": "0.0.0.0"}),
            "management.invalid_public_address",
        ),
        (
            json!({"publicAddress": "224.0.0.1"}),
            "management.invalid_public_address",
        ),
        (
            json!({"publicAddress": "192.0.2.10", "sshPort": 0}),
            "management.invalid_port",
        ),
        (json!({"link": "absent"}), "management.unknown_link"),
    ] {
        let t = topology(management, json!("ssh-ed25519 fixture-key"));
        assert!(
            validate(&t).errors().any(|e| e.code == code),
            "missing {code}"
        );
    }
    let mut t = topology(json!({"link": "wg-edge"}), json!("ssh-ed25519 fixture-key"));
    t.hosts["edge"].links.clear();
    assert!(
        validate(&t)
            .errors()
            .any(|e| e.code == "management.missing_link_binding")
    );
}

#[test]
fn cloud_management_accepts_distinct_recovery_and_tunnel_routes_without_lan() {
    let t = topology(
        json!({"publicAddress": "192.0.2.10", "sshPort": 1337, "link": "wg-edge"}),
        json!("ssh-ed25519 fixture-key"),
    );
    assert!(validate(&t).is_ok());
    assert!(t.hosts["edge"].network.lan_ip.is_none());
    let declared = serde_json::to_value(&t.hosts["edge"]).unwrap();
    assert_eq!(declared["management"]["sshPort"], 1337);
    assert_eq!(declared["management"]["publicAddress"], "192.0.2.10");
    let routes = t.management_routes("builder", "edge", 22).unwrap();
    assert_eq!(
        routes
            .iter()
            .map(|r| r.address.as_str())
            .collect::<Vec<_>>(),
        ["10.90.0.1", "192.0.2.10"]
    );
    assert!(
        routes
            .iter()
            .all(|r| r.port == 1337 && r.host_key == "ssh-ed25519 fixture-key")
    );
    assert!(t.management_routes("edge", "edge", 22).unwrap().is_empty());
}

#[test]
fn public_recovery_survives_absent_source_tunnel_binding() {
    let mut t = topology(
        json!({"publicAddress": "192.0.2.10", "link": "wg-edge"}),
        json!("ssh-ed25519 fixture-key"),
    );
    t.hosts["builder"].links.clear();
    let routes = t.management_routes("builder", "edge", 1337).unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(
        routes[0].kind,
        fleetix::management::ManagementRouteKind::Public
    );
    assert_eq!(routes[0].port, 1337);
    assert!(t.management_routes("missing", "edge", 1337).is_err());
    assert!(t.management_routes("builder", "missing", 1337).is_err());
    assert!(t.management_routes("builder", "edge", 0).is_err());
}

#[test]
fn recovery_identity_is_part_of_declared_trust() {
    let t = topology(
        json!({"publicAddress": "192.0.2.10"}),
        json!("ssh-ed25519 fixture-key"),
    );
    let trust = fleetix::trust::DeclaredTrust::from_topology(&t);
    assert_eq!(
        trust.keys_for("[192.0.2.10]:1337"),
        ["ssh-ed25519 fixture-key"]
    );
}
