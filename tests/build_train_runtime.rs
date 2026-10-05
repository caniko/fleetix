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
    block_plan: AtomicBool,
    plan_entered: AtomicBool,
    plan_released: Mutex<bool>,
    plan_gate: Condvar,
    release_fails: AtomicBool,
    released_requests: Mutex<Vec<String>>,
}
impl Backend for TestBackend {
    fn plan(&self, request: &Request) -> Result<Graph, String> {
        if request.target == "murph" && self.block_plan.load(Ordering::Relaxed) {
            self.plan_entered.store(true, Ordering::Relaxed);
            let mut released = self.plan_released.lock().unwrap();
            while !*released {
                released = self.plan_gate.wait(released).unwrap();
            }
        }
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
    fn release(&self, request: &Request, _: &Graph) -> Result<(), String> {
        if self.release_fails.load(Ordering::Relaxed) {
            return Err("injected root release failure".into());
        }
        self.released_requests
            .lock()
            .unwrap()
            .push(request.attempt.clone());
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
        Self::with_limit(8)
    }
    fn with_limit(queue_limit: usize) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            socket: temp.path().join("runtime/coordinator.sock"),
            state_dir: temp.path().join("state"),
            policy: "exact-policy".into(),
            workers: 1,
            queue_limit,
            aging_seconds: 60,
            planning_workers: 1,
            planning_timeout_seconds: 10,
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
        *self.backend.plan_released.lock().unwrap() = true;
        self.backend.plan_gate.notify_all();
        self.worker.take().unwrap().join().unwrap().unwrap();
    }
}

fn responsive(client: &Client, command: Command) -> Reply {
    let (tx, rx) = std::sync::mpsc::channel();
    let client = client.clone();
    std::thread::spawn(move || {
        let _ = tx.send(client.call(command));
    });
    rx.recv_timeout(Duration::from_secs(1))
        .expect("control plane blocked behind graph preparation")
        .unwrap()
}

#[test]
fn blocked_planning_does_not_block_completion_status_cancellation_or_fences() {
    let server = Server::start();
    let client = server.client();
    client.call(Command::Submit(request("a", "atlas"))).unwrap();
    eventually(|| server.backend.started.lock().unwrap().as_slice() == ["busy"]);
    server.backend.block_plan.store(true, Ordering::Relaxed);
    let joining = client.clone();
    let registration =
        std::thread::spawn(move || joining.call(Command::Register(request("m", "murph"))));
    eventually(|| server.backend.plan_entered.load(Ordering::Relaxed));
    assert_eq!(
        responsive(&client, Command::Status("m".into())).outcome,
        Some(Outcome::Pending)
    );
    assert_eq!(
        responsive(&client, Command::Cancel("m".into())).outcome,
        Some(Outcome::Cancelled)
    );
    let fence = responsive(&client, Command::Fence("a".into()))
        .fence
        .unwrap();
    *server.backend.released.lock().unwrap() = true;
    server.backend.gate.notify_all();
    eventually(|| responsive(&client, Command::Inspect).running == 0);
    responsive(&client, Command::ReleaseFence(fence.token));
    *server.backend.plan_released.lock().unwrap() = true;
    server.backend.plan_gate.notify_all();
    assert_eq!(
        registration.join().unwrap().unwrap().outcome,
        Some(Outcome::Cancelled)
    );
    assert!(
        !server
            .backend
            .started
            .lock()
            .unwrap()
            .iter()
            .any(|target| target == "murph")
    );
}

#[test]
fn terminal_history_does_not_exhaust_active_request_capacity() {
    let server = Server::with_limit(1);
    let client = server.client();
    client
        .call(Command::Submit(request("first", "murph")))
        .unwrap();
    eventually(|| {
        client
            .call(Command::Status("first".into()))
            .unwrap()
            .outcome
            == Some(Outcome::Ready)
    });
    client
        .call(Command::Submit(request("second", "murph")))
        .unwrap();
    eventually(|| {
        client
            .call(Command::Status("second".into()))
            .unwrap()
            .outcome
            == Some(Outcome::Ready)
    });
    assert_eq!(
        client
            .call(Command::Status("first".into()))
            .unwrap()
            .outcome,
        Some(Outcome::Ready)
    );
}

#[test]
fn retirement_archives_before_root_release_and_retries_after_restart() {
    let mut server = Server::start();
    let client = server.client();
    client
        .call(Command::Submit(request("old", "murph")))
        .unwrap();
    eventually(|| {
        client.call(Command::Status("old".into())).unwrap().outcome == Some(Outcome::Ready)
    });
    client
        .call(Command::Submit(request("new", "murph")))
        .unwrap();
    eventually(|| {
        client.call(Command::Status("new".into())).unwrap().outcome == Some(Outcome::Ready)
    });
    server.backend.release_fails.store(true, Ordering::Relaxed);
    assert!(
        client
            .call(Command::Retire("new".into()))
            .unwrap_err()
            .contains("root release")
    );
    let state: Train = serde_json::from_reader(
        std::fs::File::open(server.config.state_dir.join("train.json")).unwrap(),
    )
    .unwrap();
    assert!(!state.requests.contains_key("new"));
    assert!(state.requests.contains_key("old"));
    assert!(server.backend.released_requests.lock().unwrap().is_empty());
    server.shutdown();
    server.restart();
    assert_eq!(
        client.call(Command::Status("new".into())).unwrap().outcome,
        Some(Outcome::Ready)
    );
    assert!(
        client
            .call(Command::AuthorizeActivation("old".into()))
            .unwrap_err()
            .contains("superseded")
    );
    assert!(
        client
            .call(Command::Submit(request("new", "murph")))
            .unwrap_err()
            .contains("retired")
    );
    server.backend.release_fails.store(false, Ordering::Relaxed);
    client.call(Command::Retire("new".into())).unwrap();
    client.call(Command::Retire("new".into())).unwrap();
    assert_eq!(*server.backend.released_requests.lock().unwrap(), ["new"]);
    assert_eq!(
        client.call(Command::Status("old".into())).unwrap().outcome,
        Some(Outcome::Ready)
    );
}

#[test]
fn retirement_requires_terminal_state_drained_interest_and_no_owned_fence() {
    let server = Server::start();
    let client = server.client();
    client
        .call(Command::Register(request("a", "atlas")))
        .unwrap();
    assert!(
        client
            .call(Command::Retire("a".into()))
            .unwrap_err()
            .contains("pending")
    );
    client.call(Command::Admit("a".into())).unwrap();
    eventually(|| client.call(Command::Inspect).unwrap().running == 1);
    client.call(Command::Cancel("a".into())).unwrap();
    assert!(
        client
            .call(Command::Retire("a".into()))
            .unwrap_err()
            .contains("running work")
    );
    let fence = client
        .call(Command::Fence("a".into()))
        .unwrap()
        .fence
        .unwrap();
    *server.backend.released.lock().unwrap() = true;
    server.backend.gate.notify_all();
    eventually(|| client.call(Command::Inspect).unwrap().running == 0);
    assert!(
        client
            .call(Command::Retire("a".into()))
            .unwrap_err()
            .contains("fence owner")
    );
    client.call(Command::ReleaseFence(fence.token)).unwrap();
    client.call(Command::Retire("a".into())).unwrap();
    assert_eq!(
        client.call(Command::Status("a".into())).unwrap().outcome,
        Some(Outcome::Cancelled)
    );
}

#[test]
fn durable_unprepared_intake_resumes_after_restart_in_held_admission() {
    let mut server = Server::start();
    server.shutdown();
    let mut state: Train = serde_json::from_reader(
        std::fs::File::open(server.config.state_dir.join("train.json")).unwrap(),
    )
    .unwrap();
    state.register(request("m", "murph"), false).unwrap();
    std::fs::write(
        server.config.state_dir.join("train.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
    server.restart();
    let client = server.client();
    client
        .call(Command::Register(request("m", "murph")))
        .unwrap();
    assert!(server.backend.started.lock().unwrap().is_empty());
    client.call(Command::Admit("m".into())).unwrap();
    eventually(|| {
        client.call(Command::Status("m".into())).unwrap().outcome == Some(Outcome::Ready)
    });
}

#[test]
fn incompatible_journal_is_rejected_without_rewriting_evidence() {
    let mut server = Server::start();
    server.shutdown();
    let state_path = server.config.state_dir.join("train.json");
    let mut state: serde_json::Value =
        serde_json::from_reader(std::fs::File::open(&state_path).unwrap()).unwrap();
    state["version"] = serde_json::json!(1);
    let evidence = serde_json::to_vec(&state).unwrap();
    std::fs::write(&state_path, &evidence).unwrap();
    let error = serve(
        server.config.clone(),
        server.backend.clone(),
        server.stop.clone(),
    )
    .unwrap_err();
    assert!(error.contains("persisted train protocol/policy mismatch"));
    assert_eq!(std::fs::read(&state_path).unwrap(), evidence);
    assert!(!server.config.socket.exists());
}

#[test]
fn preparation_deadline_retains_planner_capacity_and_requires_explicit_retry() {
    let mut server = Server::start();
    server.shutdown();
    server.config.planning_timeout_seconds = 1;
    server.backend.block_plan.store(true, Ordering::Relaxed);
    *server.backend.plan_released.lock().unwrap() = false;
    server.restart();
    let client = server.client();
    let joining = client.clone();
    let registration =
        std::thread::spawn(move || joining.call(Command::Register(request("m", "murph"))));
    eventually(|| server.backend.plan_entered.load(Ordering::Relaxed));
    assert!(
        registration
            .join()
            .unwrap()
            .unwrap_err()
            .contains("deadline")
    );
    responsive(&client, Command::Inspect);
    assert!(
        client
            .call(Command::Retry("m".into()))
            .unwrap_err()
            .contains("planner has not exited")
    );
    assert!(
        client
            .call(Command::Retire("m".into()))
            .unwrap_err()
            .contains("planner has not exited")
    );
    *server.backend.plan_released.lock().unwrap() = true;
    server.backend.plan_gate.notify_all();
    eventually(|| client.call(Command::Retry("m".into())).is_ok());
    client
        .call(Command::Register(request("m", "murph")))
        .unwrap();
    assert!(server.backend.started.lock().unwrap().is_empty());
    client.call(Command::Admit("m".into())).unwrap();
    eventually(|| {
        client.call(Command::Status("m".into())).unwrap().outcome == Some(Outcome::Ready)
    });
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
