#![cfg(all(unix, feature = "build-train-runtime"))]
use fleetix::build_train::{runtime::*, *};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

fn goal(name: &str) -> Goal {
    Goal {
        derivation: name.into(),
        output: "out".into(),
    }
}
fn request(id: &str, target: &str) -> Request {
    Request {
        attempt: id.into(),
        target: target.into(),
        source: format!("frozen-{id}"),
        roots: BTreeSet::from([goal(target)]),
        activates: true,
    }
}

#[derive(Default)]
struct TestBackend {
    valid: Mutex<BTreeSet<Goal>>,
    started: Mutex<Vec<String>>,
    released: Mutex<bool>,
    gate: Condvar,
}
impl Backend for TestBackend {
    fn plan(&self, request: &Request) -> Result<Graph, String> {
        if request.target == "multi" {
            let mut dev = goal("multi");
            dev.output = "dev".into();
            return Ok([goal("multi"), dev]
                .into_iter()
                .map(|goal| {
                    let path = format!("/store/multi-{}", goal.output);
                    (
                        goal,
                        Definition {
                            output_path: path,
                            dependencies: BTreeSet::new(),
                            operation: Operation::Build,
                        },
                    )
                })
                .collect());
        }
        let entries: Vec<(&str, Vec<&str>)> = match request.target.as_str() {
            "atlas" => vec![
                ("atlas", vec!["busy", "exclusive", "shared"]),
                ("busy", vec![]),
                ("exclusive", vec![]),
                ("shared", vec![]),
            ],
            "murph" => vec![("murph", vec!["shared"]), ("shared", vec![])],
            _ => return Err("unsupported test target".into()),
        };
        Ok(entries
            .into_iter()
            .map(|(name, deps)| {
                (
                    goal(name),
                    Definition {
                        output_path: format!("/store/{name}"),
                        dependencies: deps.into_iter().map(goal).collect(),
                        operation: Operation::Build,
                    },
                )
            })
            .collect())
    }
    fn retain(&self, _: &Request, _: &Graph) -> Result<(), String> {
        Ok(())
    }
    fn valid(&self, goal: &Goal, _: &Definition) -> Result<bool, String> {
        Ok(self.valid.lock().unwrap().contains(goal))
    }
    fn realise(&self, dispatch: &Dispatch) -> Result<(), String> {
        self.started
            .lock()
            .unwrap()
            .push(dispatch.goal.derivation.clone());
        if dispatch.goal == goal("busy") {
            let mut released = self.released.lock().unwrap();
            while !*released {
                released = self.gate.wait(released).unwrap();
            }
        }
        self.valid.lock().unwrap().insert(dispatch.goal.clone());
        if dispatch.goal.derivation == "multi" {
            let mut other = dispatch.goal.clone();
            other.output = if other.output == "out" { "dev" } else { "out" }.into();
            self.valid.lock().unwrap().insert(other);
        }
        Ok(())
    }
}

fn eventually(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "coordinator did not reach expected state"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct Server {
    config: Config,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<Result<(), String>>>,
    backend: Arc<TestBackend>,
    _temp: tempfile::TempDir,
}
impl Server {
    fn start() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            socket: temp.path().join("runtime/coordinator.sock"),
            state_dir: temp.path().join("state"),
            policy: "exact-policy".into(),
            workers: 1,
            queue_limit: 8,
            aging_seconds: 60,
        };
        let backend = Arc::new(TestBackend::default());
        let mut server = Self {
            config,
            stop: Arc::new(AtomicBool::new(false)),
            worker: None,
            backend,
            _temp: temp,
        };
        server.restart();
        server
    }
    fn client(&self) -> Client {
        Client {
            socket: self.config.socket.clone(),
            policy: self.config.policy.clone(),
        }
    }
    fn restart(&mut self) {
        self.stop.store(false, Ordering::Relaxed);
        let config = self.config.clone();
        let backend = self.backend.clone();
        let stop = self.stop.clone();
        self.worker = Some(std::thread::spawn(move || serve(config, backend, stop)));
        eventually(|| self.client().call(Command::Inspect).is_ok());
    }
    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        *self.backend.released.lock().unwrap() = true;
        self.backend.gate.notify_all();
        self.worker.take().unwrap().join().unwrap().unwrap();
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if self.worker.is_some() {
            self.shutdown();
        }
    }
}

#[test]
fn disconnected_request_continues_and_late_join_changes_actual_worker_order() {
    let server = Server::start();
    server
        .client()
        .call(Command::Submit(request("a", "atlas")))
        .unwrap();
    eventually(|| server.backend.started.lock().unwrap().as_slice() == ["busy"]);
    let client = server.client();
    // Submitting from another connection attaches to the existing coordinator.
    client.call(Command::Submit(request("m", "murph"))).unwrap();
    *server.backend.released.lock().unwrap() = true;
    server.backend.gate.notify_all();
    eventually(|| {
        client.call(Command::Status("m".into())).unwrap().outcome == Some(Outcome::Ready)
    });
    let started = server.backend.started.lock().unwrap().clone();
    assert_eq!(&started[..3], ["busy", "shared", "murph"]);
    assert_eq!(started.iter().filter(|s| *s == "shared").count(), 1);
}

#[test]
fn socket_ownership_is_exclusive_even_with_another_state_directory() {
    let server = Server::start();
    let mut config = server.config.clone();
    config.state_dir = server._temp.path().join("another-state");
    let error = serve(
        config,
        server.backend.clone(),
        Arc::new(AtomicBool::new(true)),
    )
    .unwrap_err();
    assert!(error.contains("socket lease held"), "{error}");
    server.client().call(Command::Inspect).unwrap();
}

#[test]
fn one_builder_result_reconciles_all_valid_named_outputs() {
    let server = Server::start();
    let mut req = request("multi", "multi");
    let mut dev = goal("multi");
    dev.output = "dev".into();
    req.roots.insert(dev);
    server.client().call(Command::Submit(req)).unwrap();
    eventually(|| {
        server
            .client()
            .call(Command::Status("multi".into()))
            .unwrap()
            .outcome
            == Some(Outcome::Ready)
    });
    assert_eq!(server.backend.started.lock().unwrap().len(), 1);
}

#[test]
fn held_intake_cannot_dispatch_before_live_preparation_and_explicit_admission() {
    let server = Server::start();
    let client = server.client();
    client
        .call(Command::Register(request("m", "murph")))
        .unwrap();
    assert_eq!(client.call(Command::Inspect).unwrap().running, 0);
    client.call(Command::Admit("m".into())).unwrap();
    eventually(|| {
        client.call(Command::Status("m".into())).unwrap().outcome == Some(Outcome::Ready)
    });
    assert_eq!(
        server.backend.started.lock().unwrap().as_slice(),
        ["shared", "murph"]
    );
}

#[test]
fn durable_fence_survives_service_restart_and_rejects_wrong_policy() {
    let mut server = Server::start();
    let client = server.client();
    client
        .call(Command::Register(request("m", "murph")))
        .unwrap();
    let fence = client.drain("m", Duration::from_secs(1)).unwrap();
    client.call(Command::Admit("m".into())).unwrap();
    server.shutdown();
    server.restart();
    assert_eq!(
        client.call(Command::Inspect).unwrap().fence.unwrap().token,
        fence.token
    );
    assert!(server.backend.started.lock().unwrap().is_empty());
    let wrong = Client {
        socket: client.socket.clone(),
        policy: "other-policy".into(),
    };
    assert!(wrong.call(Command::Cancel("m".into())).is_err());
    client.call(Command::ReleaseFence(fence.token)).unwrap();
    eventually(|| {
        client.call(Command::Status("m".into())).unwrap().outcome == Some(Outcome::Ready)
    });
}

#[test]
fn bounded_drain_keeps_fence_and_cancellation_removes_only_one_interest() {
    let server = Server::start();
    let client = server.client();
    client.call(Command::Submit(request("a", "atlas"))).unwrap();
    eventually(|| client.call(Command::Inspect).unwrap().running == 1);
    client.call(Command::Submit(request("m", "murph"))).unwrap();
    assert!(
        client
            .drain("a", Duration::ZERO)
            .unwrap_err()
            .contains("fence")
    );
    client.call(Command::Cancel("m".into())).unwrap();
    assert_eq!(
        client.call(Command::Status("m".into())).unwrap().outcome,
        Some(Outcome::Cancelled)
    );
    let fence = client.call(Command::Inspect).unwrap().fence.unwrap();
    assert!(
        client
            .call(Command::ReleaseFence(fence.token.clone()))
            .is_err()
    );
    *server.backend.released.lock().unwrap() = true;
    server.backend.gate.notify_all();
    eventually(|| client.call(Command::Inspect).unwrap().running == 0);
    client.call(Command::ReleaseFence(fence.token)).unwrap();
    eventually(|| {
        client.call(Command::Status("a".into())).unwrap().outcome == Some(Outcome::Ready)
    });
    assert!(
        !server
            .backend
            .started
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == "murph")
    );
}

#[test]
fn preparation_is_process_shared_exact_keyed_and_caches_only_passes() {
    use preparation::{Evidence, Key};
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("preparation");
    let key = Key::Evaluation {
        source: "frozen".into(),
        inputs: "lock-hash".into(),
        attribute: "out".into(),
        evaluator: "nix-2.34".into(),
        settings: BTreeMap::new(),
    };
    assert_eq!(
        preparation::run(&directory, &key, Duration::ZERO, || Ok(Evidence::Passed(
            "drv".to_string()
        )))
        .unwrap(),
        Evidence::Passed("drv".into())
    );
    assert_eq!(
        preparation::run::<String>(&directory, &key, Duration::ZERO, || panic!(
            "exact evaluation should be reused"
        ))
        .unwrap(),
        Evidence::Passed("drv".into())
    );
    let cargo = Key::Cargo {
        source: "src".into(),
        lockfile: "lock".into(),
        toolchain: "rust".into(),
        target: "host".into(),
        features: vec![],
        command: vec!["check".into()],
        environment: "drv-env".into(),
    };
    assert!(
        preparation::run::<()>(&directory, &cargo, Duration::ZERO, || Err("failed".into()))
            .is_err()
    );
    assert_eq!(
        preparation::run::<()>(&directory, &cargo, Duration::ZERO, || Ok(
            Evidence::Skipped("missing cargo".into())
        ))
        .unwrap(),
        Evidence::Skipped("missing cargo".into())
    );
    assert_eq!(
        preparation::run(&directory, &cargo, Duration::ZERO, || Ok(Evidence::Passed(
            ()
        )))
        .unwrap(),
        Evidence::Passed(())
    );
}
