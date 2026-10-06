use fleetix::{Topology, validate};
use serde_json::{Value, json};

fn topology(gpu: Value) -> Topology {
    serde_json::from_value(json!({
        "schemaVersion": 2,
        "hosts": {"desktop": {"system": "x86_64-linux", "gpu": gpu}},
        "links": {}, "domains": {}, "services": {}
    }))
    .unwrap()
}

#[test]
fn unknown_inventory_vendors_are_rejected_without_a_compute_route() {
    for field in ["igpu", "dgpu"] {
        let report = validate(&topology(json!({field: "unknown"})));
        assert!(
            report
                .errors()
                .any(|issue| issue.code == "host.invalid_gpu_inventory_vendor"),
            "invalid {field} inventory must not require a compute route to be detected"
        );
    }
}

#[test]
fn media_routes_require_an_inventoried_vendor() {
    let report = validate(&topology(json!({
        "dgpu": "amd",
        "media": {
            "vendor": "intel",
            "renderNode": "/dev/dri/by-path/pci-0000:65:00.0-render",
            "libvaDriver": "iHD"
        }
    })));
    assert!(
        report
            .errors()
            .any(|issue| issue.code == "host.uninventoried_gpu_media_vendor")
    );
}

#[test]
fn media_and_render_routes_can_select_different_inventoried_devices() {
    let report = validate(&topology(json!({
        "igpu": "intel",
        "dgpu": "amd",
        "render": {"renderNode": "/dev/dri/by-path/pci-0000:03:00.0-render"},
        "media": {
            "vendor": "intel",
            "renderNode": "/dev/dri/by-path/pci-0000:65:00.0-render",
            "libvaDriver": "iHD"
        },
        "compute": {"backend": "rocm"}
    })));
    assert!(report.is_ok(), "{report:?}");
}
