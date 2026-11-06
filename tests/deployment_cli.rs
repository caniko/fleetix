#![cfg(all(unix, feature = "deployment-cli"))]

use std::fs;
use std::process::Command;

#[test]
fn worker_requires_explicit_policy_and_request_paths() {
    let output = Command::new(env!("CARGO_BIN_EXE_fleetix-deploy"))
        .arg("run")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--policy"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--request"));
}

#[test]
fn unknown_or_malformed_policy_never_activates_or_prompts() {
    let root = tempfile::tempdir().unwrap();
    let policy = root.path().join("policy.json");
    let request = root.path().join("request.json");
    fs::write(&request, "{}").unwrap();
    let before = fs::read_link("/run/current-system").ok();
    for document in ["{}", r#"{"version":1,"approved":true}"#, "not JSON"] {
        fs::write(&policy, document).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_fleetix-deploy"))
            .args(["run", "--policy"])
            .arg(&policy)
            .arg("--request")
            .arg(&request)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("password"));
        assert_eq!(fs::read_link("/run/current-system").ok(), before);
    }
}
