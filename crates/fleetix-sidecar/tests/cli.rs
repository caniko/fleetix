#![cfg(feature = "cli")]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn run(action: &str, source: &Path, output: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fleetix-sidecar"));
    command.arg(action).arg(source).arg(output);
    if action == "generate" {
        command.arg("--no-cache");
    }
    command
        // Generation and checking must work without Nix, Pkl, or a frontend on PATH.
        .env("PATH", "")
        .env("XDG_CACHE_HOME", source.parent().unwrap().join("cache"))
        .output()
        .unwrap()
}

#[test]
fn standalone_generation_and_read_only_check() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Source.pkl");
    let dependency = dir.path().join("Value.pkl");
    let output = dir.path().join("generated/source.nix");
    fs::write(&source, "import \"Value.pkl\" as V\nvalue = V.value\n").unwrap();
    fs::write(&dependency, "value = 42\n").unwrap();

    let missing = run("check", &source, &output);
    assert_eq!(missing.status.code(), Some(2));
    assert!(!output.parent().unwrap().exists());
    let generated = run("generate", &source, &output);
    assert!(generated.status.success(), "{:?}", generated);
    let contents = fs::read(&output).unwrap();
    assert!(String::from_utf8_lossy(&contents).contains("value = 42"));
    assert!(run("check", &source, &output).status.success());

    fs::write(&dependency, "value = 43\n").unwrap();
    assert_eq!(run("check", &source, &output).status.code(), Some(2));
    assert_eq!(fs::read(&output).unwrap(), contents);

    fs::write(&source, "value = missingProperty\n").unwrap();
    assert_eq!(run("generate", &source, &output).status.code(), Some(1));
    assert_eq!(fs::read(&output).unwrap(), contents);
    assert!(!dir.path().join("cache").exists());
}
