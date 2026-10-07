use super::*;
use std::collections::BTreeSet;
use std::os::unix::fs::{DirBuilderExt, symlink};

fn service() -> Service {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/build-train-service.json"
    ))
    .unwrap()
}

#[test]
fn policy_matches_original_nix_contract() {
    let service = service();
    assert_eq!(
        policy_identity(&service).unwrap(),
        service.coordinator.policy
    );
}

#[test]
fn operator_bound_admission_keeps_version_two_parity_and_rejects_ownership_transfer() {
    let service: Service = serde_json::from_str(include_str!(
        "../../tests/fixtures/build-train-operator-service.json"
    ))
    .unwrap();
    assert_eq!(
        policy_identity(&service).unwrap(),
        service.coordinator.policy
    );
    let mut next = service.clone();
    next.admission_contract = serde_json::to_string(&(
        "fleetix-train-operator",
        "other",
        "qualified-resource-policy",
    ))
    .unwrap();
    next.coordinator.policy = policy_identity(&next).unwrap();
    assert_ne!(next.coordinator.policy, service.coordinator.policy);
    assert_eq!(
        rollover(service, next, "unused").unwrap_err(),
        "policy rollover cannot transfer operator ownership"
    );
}

#[test]
fn scheduling_and_execution_limits_remain_policy_bound() {
    let service = service();
    let expected = policy_identity(&service).unwrap();
    for mutate in [
        |s: &mut Service| s.coordinator.workers += 1,
        |s: &mut Service| s.coordinator.planning_workers += 1,
        |s: &mut Service| s.coordinator.queue_limit += 1,
        |s: &mut Service| s.coordinator.aging_seconds += 1,
        |s: &mut Service| s.coordinator.planning_timeout_seconds += 1,
        |s: &mut Service| s.native.timeout_seconds += 1,
        |s: &mut Service| s.native.query_timeout_seconds += 1,
        |s: &mut Service| s.memory_max.push('0'),
        |s: &mut Service| s.admission_contract.push('x'),
        |s: &mut Service| s.native.substitutes = !s.native.substitutes,
        |s: &mut Service| s.coordinator.state_dir.push("changed"),
    ] {
        let mut changed = service.clone();
        mutate(&mut changed);
        assert_ne!(policy_identity(&changed).unwrap(), expected);
    }
    let mut changed = service;
    changed.coordinator.policy = "not-part-of-itself".into();
    assert_eq!(policy_identity(&changed).unwrap(), expected);
}

#[test]
fn rollover_rejects_changed_ownership_and_invalid_contracts_before_touching_state() {
    let temp = tempfile::tempdir().unwrap();
    let mut previous = service();
    previous.coordinator.state_dir = temp.path().join("state");
    previous.coordinator.socket = temp.path().join("coordinator.sock");
    previous.coordinator.policy = policy_identity(&previous).unwrap();
    std::fs::create_dir(&previous.coordinator.state_dir).unwrap();
    let journal = previous.coordinator.state_dir.join("train.json");
    std::fs::write(&journal, "original recovery evidence").unwrap();
    for field in ["builder", "roots", "policy", "deadline"] {
        let mut next = previous.clone();
        match field {
            "builder" => next.builder = "replacement-builder".into(),
            "roots" => next.native.gc_roots = temp.path().join("foreign-roots"),
            "deadline" => next.coordinator.planning_timeout_seconds = 1,
            _ => {}
        }
        next.coordinator.policy = policy_identity(&next).unwrap();
        if field == "policy" {
            next.coordinator.policy = "unverified-policy".into();
        }
        assert!(rollover(previous.clone(), next, "token").is_err());
        assert_eq!(
            std::fs::read_to_string(&journal).unwrap(),
            "original recovery evidence"
        );
        assert!(
            !previous
                .coordinator
                .state_dir
                .join("rollover.json")
                .exists()
        );
    }
}

#[test]
fn connection_discovery_fails_closed_except_for_absent_default() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("connection.json");
    assert!(
        Connection::discover("atlas", None, &path)
            .unwrap()
            .is_none()
    );
    assert!(Connection::discover("atlas", Some(&path), &path).is_err());
    symlink(temp.path().join("missing.json"), &path).unwrap();
    assert!(Connection::discover("atlas", None, &path).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, "malformed").unwrap();
    assert!(Connection::discover("atlas", None, &path).is_err());
    let service = service();
    let connection = Connection {
        builder: service.builder,
        policy: service.coordinator.policy,
        socket: service.coordinator.socket,
        gc_roots: service.native.gc_roots,
        preparation_dir: "/var/lib/fleetix-train/preparation".into(),
    };
    std::fs::write(&path, serde_json::to_vec(&connection).unwrap()).unwrap();
    assert_eq!(
        Connection::discover("atlas", None, &path)
            .unwrap()
            .unwrap()
            .policy,
        connection.policy
    );
    assert!(Connection::discover("murph", None, &path).is_err());
}

#[test]
fn archived_ownership_excludes_unused_siblings_and_retains_build_evidence() {
    let root = Goal {
        derivation: "root.drv".into(),
        output: "out".into(),
    };
    let headers = Goal {
        derivation: "shared.drv".into(),
        output: "dev".into(),
    };
    let sibling = Goal {
        output: "out".into(),
        ..headers.clone()
    };
    let request = Request {
        attempt: "a".into(),
        target: "target".into(),
        source: "source#target".into(),
        roots: BTreeSet::from([root.clone()]),
        activates: true,
    };
    let definition = |path: &str, dependencies| Definition {
        output_path: path.into(),
        dependencies,
        operation: Operation::Restore,
    };
    let graph = Graph::from([
        (
            root,
            definition("root-out", BTreeSet::from([headers.clone()])),
        ),
        (headers, definition("shared-dev", BTreeSet::new())),
        (sibling.clone(), definition("unused-out", BTreeSet::new())),
    ]);
    let mut archived = graph.clone();
    archived.remove(&sibling);
    assert_eq!(
        request_paths(&request, &graph),
        request_paths(&request, &archived)
    );
    assert_eq!(
        request_paths(&request, &graph),
        BTreeSet::from([
            "source".into(),
            "root.drv".into(),
            "root-out".into(),
            "shared.drv".into(),
            "shared-dev".into()
        ])
    );
}

#[test]
fn invalid_root_identities_never_create_request_namespaces() {
    let temp = tempfile::tempdir().unwrap();
    let mut native = service().native;
    native.gc_roots = temp.path().join("roots");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&native.gc_roots)
        .unwrap();
    let backend = NixBackend(native);
    let valid = "/nix/store/00000000000000000000000000000000-source";
    let request = Request {
        attempt: "invalid".into(),
        target: "builder".into(),
        source: valid.into(),
        roots: BTreeSet::from([Goal {
            derivation: format!("{valid}.drv"),
            output: "out".into(),
        }]),
        activates: false,
    };
    for bad in [
        "00000000000000000000000000000000-source",
        "/checkout/source",
        "/nix/store/00000000000000000000000000000000-source/subpath",
        "/nix/store/../00000000000000000000000000000000-source",
    ] {
        let mut invalid = request.clone();
        invalid.source = format!("{bad}#target");
        assert!(backend.retain(&invalid, &Graph::new()).is_err());
        assert!(
            !backend.0.gc_roots.join("requests").exists(),
            "invalid source created namespace: {bad}"
        );
        invalid = request.clone();
        invalid.roots = BTreeSet::from([Goal {
            derivation: format!("{bad}.drv"),
            output: "out".into(),
        }]);
        assert!(backend.retain(&invalid, &Graph::new()).is_err());
        assert!(
            !backend.0.gc_roots.join("requests").exists(),
            "invalid derivation created namespace: {bad}"
        );
        let graph = Graph::from([(
            request.roots.iter().next().unwrap().clone(),
            Definition {
                output_path: bad.into(),
                dependencies: BTreeSet::new(),
                operation: Operation::Build,
            },
        )]);
        assert!(backend.retain(&request, &graph).is_err());
        assert!(
            !backend.0.gc_roots.join("requests").exists(),
            "invalid output created namespace: {bad}"
        );
    }
}

#[test]
fn malformed_native_intake_is_rejected_without_poisoning_restart() {
    use runtime::Command;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    let temp = tempfile::tempdir().unwrap();
    let mut service = service();
    service.coordinator.socket = temp.path().join("runtime/coordinator.sock");
    service.coordinator.state_dir = temp.path().join("state");
    service.native.gc_roots = temp.path().join("roots");
    let config = service.coordinator;
    let backend = Arc::new(NixBackend(service.native));
    let client = Client {
        socket: config.socket.clone(),
        policy: config.policy.clone(),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let start = || {
        let (config, backend, stop) = (config.clone(), backend.clone(), stop.clone());
        std::thread::spawn(move || runtime::serve(config, backend, stop))
    };
    let ready = |worker: &std::thread::JoinHandle<Result<(), String>>| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !worker.is_finished() {
            if client.call(Command::Inspect).is_ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    };
    let worker = start();
    assert!(ready(&worker));
    let valid = "/nix/store/00000000000000000000000000000000-source";
    let mut statuses = Vec::new();
    for (index, field) in ["source", "derivation", "output"].into_iter().enumerate() {
        let mut request = Request {
            attempt: format!("invalid-{index}"),
            target: "builder".into(),
            source: format!("{valid}#target"),
            roots: BTreeSet::from([Goal {
                derivation: format!("{valid}.drv"),
                output: "out".into(),
            }]),
            activates: false,
        };
        match field {
            "source" => request.source = "/checkout/source#target".into(),
            "derivation" => {
                request.roots = BTreeSet::from([Goal {
                    derivation: "00000000000000000000000000000000-relative.drv".into(),
                    output: "out".into(),
                }])
            }
            _ => {
                request.roots = BTreeSet::from([Goal {
                    derivation: format!("{valid}.drv"),
                    output: "bad/name".into(),
                }])
            }
        }
        let attempt = request.attempt.clone();
        let command = if index == 1 {
            Command::Submit(request)
        } else {
            Command::Register(request)
        };
        assert!(client.call(command).is_err());
        statuses.push(client.call(Command::Status(attempt)));
    }
    stop.store(true, Ordering::Relaxed);
    worker.join().unwrap().unwrap();
    stop.store(false, Ordering::Relaxed);
    let worker = start();
    let restarted = ready(&worker);
    stop.store(true, Ordering::Relaxed);
    let restart_result = worker.join().unwrap();
    assert!(
        restarted,
        "malformed intake blocked coordinator restart: {restart_result:?}"
    );
    restart_result.unwrap();
    assert!(
        statuses
            .into_iter()
            .all(|status| status.is_err_and(|error| error.contains("unknown")))
    );
    assert!(!backend.0.gc_roots.exists());
    let journal: serde_json::Value =
        serde_json::from_slice(&std::fs::read(config.state_dir.join("train.json")).unwrap())
            .unwrap();
    assert!(journal["requests"].as_object().unwrap().is_empty());
}

#[test]
fn root_release_is_request_scoped_retryable_and_rejects_foreign_entries() {
    let temp = tempfile::tempdir().unwrap();
    let mut native = service().native;
    native.gc_roots = temp.path().join("roots");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&native.gc_roots)
        .unwrap();
    let backend = NixBackend(native);
    let shared = "/nix/store/00000000000000000000000000000000-shared";
    let request = |attempt: &str| Request {
        attempt: attempt.into(),
        target: "builder".into(),
        source: shared.into(),
        roots: BTreeSet::new(),
        activates: false,
    };
    let a = request("a");
    let b = request("b");
    let root_a = backend.request_native(&a, true).unwrap().gc_roots;
    let root_b = backend.request_native(&b, true).unwrap().gc_roots;
    for directory in [&root_a, &root_b] {
        symlink(
            shared,
            directory.join(Path::new(shared).file_name().unwrap()),
        )
        .unwrap();
    }
    backend.release(&a, &Graph::new()).unwrap();
    backend.release(&a, &Graph::new()).unwrap();
    assert!(!root_a.exists());
    assert_eq!(
        std::fs::read_link(root_b.join(Path::new(shared).file_name().unwrap())).unwrap(),
        Path::new(shared)
    );
    let foreign = "/nix/store/11111111111111111111111111111111-foreign";
    let link = root_b.join(Path::new(foreign).file_name().unwrap());
    symlink(foreign, &link).unwrap();
    assert!(backend.release(&b, &Graph::new()).is_err());
    assert_eq!(std::fs::read_link(&link).unwrap(), Path::new(foreign));
    std::fs::remove_file(link).unwrap();
    std::fs::write(root_b.join("unexpected"), "foreign data").unwrap();
    assert!(backend.release(&b, &Graph::new()).is_err());
    assert!(root_b.join("unexpected").exists());
}
