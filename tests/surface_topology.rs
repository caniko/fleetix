use fleetix::topology::{Host, ServiceSurfaces, Topology};
use fleetix::validate::validate;
use serde_json::{Value, json};

fn surfaces(services: &[&str], dev: &str, prod: &str) -> Value {
    json!({
        "services": services,
        "devContainer": dev,
        "prodContainer": prod,
    })
}

fn topology(host_surfaces: Value, catalog: Value) -> Topology {
    serde_json::from_value(json!({
        "schemaVersion": 2,
        "links": {},
        "hosts": {"hub": {"system": "x86_64-linux", "serviceSurfaces": host_surfaces}},
        "domains": {},
        "services": {"catalog": catalog},
    }))
    .unwrap()
}

fn catalog(lifecycle: &str) -> Value {
    json!({
        "media": {
            "displayName": "Media", "category": "Media",
            "lifecycle": lifecycle, "exclusionReason": "test fixture",
        },
        "search": {
            "displayName": "Search", "category": "Search",
            "lifecycle": "on-demand", "exclusionReason": "test fixture",
        },
    })
}

fn codes(topology: &Topology) -> Vec<String> {
    validate(topology)
        .errors()
        .map(|issue| issue.code.clone())
        .collect()
}

#[test]
fn service_surfaces_round_trip_preserves_static_policy() {
    let host: Host = serde_json::from_value(json!({
        "system": "x86_64-linux",
        "serviceSurfaces": surfaces(&["media", "search"], "fleetix-dev", "fleetix-prod"),
    }))
    .unwrap();
    assert_eq!(
        host.service_surfaces,
        Some(ServiceSurfaces {
            services: vec!["media".into(), "search".into()],
            dev_container: "fleetix-dev".into(),
            prod_container: "fleetix-prod".into(),
        })
    );
    let output = serde_json::to_value(&host).unwrap();
    assert_eq!(
        output["serviceSurfaces"],
        json!({
            "services": ["media", "search"],
            "devContainer": "fleetix-dev",
            "prodContainer": "fleetix-prod",
        })
    );
}

#[test]
fn service_surfaces_use_paired_container_defaults() {
    let host: Host = serde_json::from_value(json!({
        "system": "x86_64-linux",
        "serviceSurfaces": {"services": ["media"]},
    }))
    .unwrap();
    let surfaces = host.service_surfaces.unwrap();
    assert_eq!(surfaces.dev_container, "fleetix-dev");
    assert_eq!(surfaces.prod_container, "fleetix-prod");
    assert!(ServiceSurfaces::valid_container_name(
        &surfaces.dev_container
    ));
    assert!(ServiceSurfaces::valid_container_name(
        &surfaces.prod_container
    ));

    let topology = topology(json!({"services": ["media"]}), catalog("active"));
    assert!(
        validate(&topology).is_ok(),
        "{:?}",
        validate(&topology).issues
    );
}

#[test]
fn service_surfaces_reject_unknown_fields() {
    let error = serde_json::from_value::<ServiceSurfaces>(json!({
        "services": ["media"],
        "runtimeState": "deployed",
    }))
    .unwrap_err();
    assert!(error.to_string().contains("runtimeState"), "{error}");
}

#[test]
fn host_fixture_without_service_surfaces_is_unchanged() {
    let fixture = json!({
        "system": "x86_64-linux",
        "tags": ["weak"],
        "network": {"lanIp": "192.0.2.10"},
    });
    let host: Host = serde_json::from_value(fixture.clone()).unwrap();
    assert_eq!(host.service_surfaces, None);
    let output = serde_json::to_value(&host).unwrap();
    assert!(output.get("serviceSurfaces").is_none());
    assert_eq!(output["tags"], fixture["tags"]);

    let topology: Topology = serde_json::from_value(json!({
        "schemaVersion": 2,
        "links": {},
        "hosts": {"hub": fixture},
        "domains": {},
        "services": {},
    }))
    .unwrap();
    assert!(
        validate(&topology).is_ok(),
        "{:?}",
        validate(&topology).issues
    );
}

#[test]
fn accepts_active_and_on_demand_services() {
    let topology = topology(
        surfaces(&["media", "search"], "fleetix-dev", "fleetix-prod"),
        catalog("active"),
    );
    assert!(
        validate(&topology).is_ok(),
        "{:?}",
        validate(&topology).issues
    );
}

#[test]
fn rejects_unknown_service() {
    let topology = topology(
        surfaces(&["ghost"], "fleetix-dev", "fleetix-prod"),
        catalog("active"),
    );
    assert!(codes(&topology).contains(&"host.unknown_service_surface".to_string()));
}

#[test]
fn rejects_planned_and_retired_services() {
    for lifecycle in ["planned", "retired"] {
        let topology = topology(
            surfaces(&["media"], "fleetix-dev", "fleetix-prod"),
            catalog(lifecycle),
        );
        assert!(
            codes(&topology).contains(&"host.inactive_service_surface".to_string()),
            "{lifecycle}: {:?}",
            codes(&topology)
        );
    }
}

#[test]
fn rejects_empty_and_duplicate_service_lists() {
    let empty = topology(
        surfaces(&[], "fleetix-dev", "fleetix-prod"),
        catalog("active"),
    );
    assert!(codes(&empty).contains(&"host.empty_service_surfaces".to_string()));

    let duplicate = topology(
        surfaces(&["media", "media"], "fleetix-dev", "fleetix-prod"),
        catalog("active"),
    );
    assert!(codes(&duplicate).contains(&"host.duplicate_service_surface".to_string()));

    let empty_id = topology(
        surfaces(&[" "], "fleetix-dev", "fleetix-prod"),
        catalog("active"),
    );
    assert!(codes(&empty_id).contains(&"host.empty_service_surface_id".to_string()));
}

#[test]
fn rejects_shared_or_unsafe_container_names() {
    let shared = topology(
        surfaces(&["media"], "fleetix-dev", "fleetix-dev"),
        catalog("active"),
    );
    assert!(codes(&shared).contains(&"host.same_service_containers".to_string()));

    for bad in [
        "../escape",
        "fleetix/dev",
        "Fleetix",
        "1fleetix",
        "fleetix_dev",
    ] {
        let topology = topology(surfaces(&["media"], bad, "fleetix-prod"), catalog("active"));
        assert!(
            codes(&topology).contains(&"host.invalid_service_container".to_string()),
            "{bad}: {:?}",
            codes(&topology)
        );
    }
}

#[test]
fn container_name_bounds_match_ve_interface_limit() {
    for valid in [
        "a",
        "fleetix-dev",
        "fleetix-prod",
        "a1-b2-c3",
        &"a".repeat(12),
    ] {
        assert!(ServiceSurfaces::valid_container_name(valid), "{valid}");
    }
    for invalid in [
        "",
        "-fleetix",
        "Fleetix",
        "1fleetix",
        "fleetix_dev",
        "../escape",
        "fleetix/dev",
        &"a".repeat(13),
    ] {
        assert!(!ServiceSurfaces::valid_container_name(invalid), "{invalid}");
    }
}
