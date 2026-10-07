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
    fail_after_materialize: AtomicBool,
    released_requests: Mutex<Vec<String>>,
    retained: Mutex<BTreeMap<String, BTreeSet<Goal>>>,
    retain_fails: AtomicBool,
    block_retain: AtomicBool,
    retain_entered: AtomicBool,
    retain_released: Mutex<bool>,
    retain_gate: Condvar,
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
        if request.target == "large" {
            return Ok(BTreeMap::from([(
                goal("large"),
                Definition {
                    output_path: format!("/store/{}", "x".repeat(800_000)),
                    dependencies: BTreeSet::new(),
                    operation: Operation::Build,
                },
            )]));
        }
        if request.target == "oversized-output" {
            return Ok(BTreeMap::from([(
                goal("oversized-output"),
                Definition {
                    output_path: format!("/store/{}", "x".repeat(1_100_000)),
                    dependencies: BTreeSet::new(),
                    operation: Operation::Build,
                },
            )]));
        }
        if request.target == "many-roots" {
            return Ok(request
                .roots
                .iter()
                .cloned()
                .map(|goal| {
                    let path = goal.derivation.trim_end_matches(".drv").to_owned();
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
        if request.target == "verbose-failure" {
            return Err(format!("backend diagnostic: {}", "é".repeat(600_000)));
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
    fn retain(&self, request: &Request, graph: &Graph) -> Result<(), String> {
        let mut owned = self.retained.lock().unwrap();
        let paths = owned.entry(request.attempt.clone()).or_default();
        let mut todo: Vec<_> = request.roots.iter().cloned().collect();
        while let Some(goal) = todo.pop() {
            if let Some(definition) = graph.get(&goal) {
                if paths.insert(goal) {
                    todo.extend(definition.dependencies.iter().cloned());
                }
            }
        }
        drop(owned);
        if !graph.is_empty() && self.block_retain.load(Ordering::Relaxed) {
            self.retain_entered.store(true, Ordering::Relaxed);
            let mut released = self.retain_released.lock().unwrap();
            while !*released {
                released = self.retain_gate.wait(released).unwrap();
            }
        }
        if !graph.is_empty() && self.retain_fails.load(Ordering::Relaxed) {
            return Err("injected partial retention failure".into());
        }
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
        if self.fail_after_materialize.load(Ordering::Relaxed) {
            return Err("backend failure with materialized output".into());
        }
        Ok(())
    }
    fn release(&self, request: &Request, graph: &Graph) -> Result<(), String> {
        let mut owned = self.retained.lock().unwrap();
        if owned
            .get(&request.attempt)
            .is_some_and(|paths| paths.iter().any(|goal| !graph.contains_key(goal)))
        {
            return Err("retained graph evidence missing".into());
        }
        if self.release_fails.load(Ordering::Relaxed) {
            return Err("injected root release failure".into());
        }
        self.released_requests
            .lock()
            .unwrap()
            .push(request.attempt.clone());
        owned.remove(&request.attempt);
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
        *self.backend.retain_released.lock().unwrap() = true;
        self.backend.retain_gate.notify_all();
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
fn oversized_root_evidence_fails_before_admission_and_keeps_status_available() {
    let server = Server::start();
    let client = server.client();
    let error = client
        .call(Command::Register(request("oversized", "oversized-output")))
        .unwrap_err();
    assert!(
        error.contains("root output evidence exceeds protocol limit"),
        "{error}"
    );
    let status = responsive(&client, Command::Status("oversized".into()));
    assert!(matches!(status.outcome, Some(Outcome::Failed(_))));
    assert!(status.outputs.is_empty());
    assert!(server.backend.started.lock().unwrap().is_empty());
    responsive(&client, Command::Inspect);
}

#[test]
fn accepted_multi_root_frame_cannot_create_an_undeliverable_reply() {
    let server = Server::start();
    let client = server.client();
    let mut request = request("many", "many-roots");
    request.roots = (0..10_000)
        .map(|i| {
            goal(&format!(
                "/nix/store/00000000000000000000000000000000-package-{i:05}.drv"
            ))
        })
        .collect();
    let envelope = serde_json::json!({
        "version": VERSION, "policy": client.policy, "command": Command::Register(request.clone())
    });
    assert!(serde_json::to_vec(&envelope).unwrap().len() + 1 < 1024 * 1024);
    let error = client.call(Command::Register(request)).unwrap_err();
    assert!(
        error.contains("root output evidence exceeds protocol limit"),
        "{error}"
    );
    let status = responsive(&client, Command::Status("many".into()));
    assert!(matches!(status.outcome, Some(Outcome::Failed(_))));
    assert!(status.outputs.is_empty());
    assert!(server.backend.started.lock().unwrap().is_empty());
}

#[test]
fn oversized_failure_details_are_bounded_without_losing_terminal_status() {
    let server = Server::start();
    let client = server.client();
    let error = client
        .call(Command::Register(request("verbose", "verbose-failure")))
        .unwrap_err();
    assert!(error.starts_with("backend diagnostic:"), "{error}");
    assert!(error.contains("truncated"));
    assert!(error.len() < 8192);
    let status = responsive(&client, Command::Status("verbose".into()));
    let Some(Outcome::Failed(detail)) = status.outcome else {
        panic!("missing terminal failure");
    };
    assert!(detail.starts_with("backend diagnostic:"));
    assert!(detail.contains("truncated"));
    assert!(detail.len() < 8192);
    let state: Train =
        serde_json::from_slice(&std::fs::read(server.config.state_dir.join("train.json")).unwrap())
            .unwrap();
    assert!(
        state.requests["verbose"]
            .preparation_error
            .as_ref()
            .unwrap()
            .len()
            > 1024 * 1024
    );
    responsive(&client, Command::Inspect);
}

#[test]
fn unread_reply_does_not_block_other_control_clients() {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    let mut server = Server::start();
    let client = server.client();
    client
        .call(Command::Submit(request("large", "large")))
        .unwrap();
    client.wait("large", &AtomicBool::new(false)).unwrap();
    let mut unread = UnixStream::connect(&server.config.socket).unwrap();
    let mut raw = serde_json::to_vec(&serde_json::json!({
        "version": VERSION, "policy": client.policy, "command": Command::Status("large".into())
    }))
    .unwrap();
    raw.push(b'\n');
    unread.write_all(&raw).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(client.call(Command::Inspect));
    });
    let result = rx.recv_timeout(Duration::from_secs(1));
    drop(unread);
    server.shutdown();
    assert!(
        result.is_ok(),
        "control plane blocked behind unread reply: {result:?}"
    );
    result.unwrap().unwrap();
}

#[test]
fn oversized_legacy_fence_returns_a_diagnostic_and_survives_restart() {
    let mut server = Server::start();
    server.shutdown();
    let mut state = Train::new(server.config.policy.clone(), server.config.aging_seconds);
    // Version 2 previously accepted this identity within a one-MiB request.
    // Its fence repeats the identity, so the retained reply no longer fits.
    let mut legacy = request("legacy", "atlas");
    legacy.attempt = "x".repeat(600_000);
    state.register(legacy.clone(), false).unwrap();
    let token = state.fence(&legacy.attempt).unwrap();
    std::fs::write(
        server.config.state_dir.join("train.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
    server.stop.store(false, Ordering::Relaxed);
    let config = server.config.clone();
    let backend = server.backend.clone();
    let stop = server.stop.clone();
    let worker = std::thread::spawn(move || serve(config, backend, stop));
    eventually(|| server.config.socket.exists());
    let first = server.client().call(Command::Inspect);
    let second = server.client().call(Command::Inspect);
    server.stop.store(true, Ordering::Relaxed);
    let exit = worker.join().unwrap();
    for response in [first, second] {
        let error = response.unwrap_err();
        assert!(
            error.contains("coordinator reply exceeds protocol limit"),
            "{error}"
        );
        assert!(error.contains("journal"), "{error}");
    }
    exit.unwrap();
    let retained: Train =
        serde_json::from_slice(&std::fs::read(server.config.state_dir.join("train.json")).unwrap())
            .unwrap();
    assert_eq!(retained.fence.unwrap().token, token);
    assert!(server.backend.started.lock().unwrap().is_empty());
}

#[test]
fn partial_request_does_not_block_status_cancellation_fences_or_worker_completion() {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    let mut server = Server::start();
    let client = server.client();
    client.call(Command::Submit(request("a", "atlas"))).unwrap();
    eventually(|| server.backend.started.lock().unwrap().as_slice() == ["busy"]);
    client
        .call(Command::Register(request("m", "murph")))
        .unwrap();
    let mut partial = UnixStream::connect(&server.config.socket).unwrap();
    partial.write_all(b"{").unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let (tx, rx) = std::sync::mpsc::channel();
    let inspecting = client.clone();
    std::thread::spawn(move || {
        let _ = tx.send(inspecting.call(Command::Status("a".into())));
    });
    let status = rx.recv_timeout(Duration::from_secs(1));
    if status.is_err() {
        drop(partial);
        server.shutdown();
        panic!("status blocked behind a partial request: {status:?}");
    }
    assert_eq!(status.unwrap().unwrap().outcome, Some(Outcome::Pending));
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
    drop(partial);
    server.shutdown();
}

#[test]
fn trickling_request_has_one_absolute_delivery_deadline() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let mut server = Server::start();
    let mut stream = UnixStream::connect(&server.config.socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(7)))
        .unwrap();
    let mut sending = stream.try_clone().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let writer = std::thread::spawn(move || {
        while !stopping.load(Ordering::Relaxed) && sending.write_all(b" ").is_ok() {
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    let started = Instant::now();
    let result = stream.read(&mut [0u8; 1]);
    let elapsed = started.elapsed();
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();
    drop(stream);
    server.shutdown();
    // Closing with unread bytes may deliver ECONNRESET rather than clean EOF.
    // Either proves expiry; a timeout or a protocol response does not.
    let closed = match &result {
        Ok(0) => true,
        Err(error) => error.kind() == std::io::ErrorKind::ConnectionReset,
        _ => false,
    };
    assert!(closed, "trickling request was never expired: {result:?}");
    assert!(
        elapsed < Duration::from_secs(6),
        "delivery deadline reset: {elapsed:?}"
    );
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
fn shared_failure_retry_preserves_capacity_and_other_outcomes_after_restart() {
    let mut server = Server::with_limit(1);
    server.shutdown();
    let state_path = server.config.state_dir.join("train.json");
    let mut state: Train =
        serde_json::from_reader(std::fs::File::open(&state_path).unwrap()).unwrap();
    for attempt in ["first", "second"] {
        let request = request(attempt, "murph");
        let graph = server.backend.plan(&request).unwrap();
        state.submit(request, graph, 0).unwrap();
    }
    let work = state.dispatch(0).unwrap().unwrap();
    state.finish(&work, Err("shared compiler failure".into()));
    let first_failure = state.outcome("first").unwrap();
    assert!(matches!(first_failure, Outcome::Failed(_)));
    std::fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    server.restart();
    let client = server.client();

    client.call(Command::Retry("second".into())).unwrap();
    assert_eq!(
        client
            .call(Command::Status("first".into()))
            .unwrap()
            .outcome,
        Some(first_failure.clone())
    );
    assert_eq!(
        client
            .call(Command::Status("second".into()))
            .unwrap()
            .outcome,
        Some(Outcome::Pending)
    );
    assert!(server.backend.started.lock().unwrap().is_empty());
    assert!(
        client
            .call(Command::Register(request("third", "atlas")))
            .unwrap_err()
            .contains("request limit")
    );

    server.shutdown();
    server.restart();
    assert_eq!(
        client
            .call(Command::Status("first".into()))
            .unwrap()
            .outcome,
        Some(first_failure.clone())
    );
    assert!(server.backend.started.lock().unwrap().is_empty());
    client.call(Command::Admit("second".into())).unwrap();
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
        Some(first_failure.clone())
    );

    server.shutdown();
    server.restart();
    assert_eq!(
        client
            .call(Command::Status("first".into()))
            .unwrap()
            .outcome,
        Some(first_failure)
    );
    client.call(Command::Retry("first".into())).unwrap();
    assert_eq!(
        client
            .call(Command::Status("first".into()))
            .unwrap()
            .outcome,
        Some(Outcome::Ready)
    );
    assert!(
        client
            .call(Command::AuthorizeActivation("first".into()))
            .unwrap_err()
            .contains("admission")
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
fn materialized_output_does_not_clear_a_worker_failure_on_completion_or_restart() {
    let mut server = Server::start();
    server
        .backend
        .fail_after_materialize
        .store(true, Ordering::Relaxed);
    let client = server.client();
    client
        .call(Command::Submit(request("failed", "multi")))
        .unwrap();
    eventually(|| {
        matches!(
            client
                .call(Command::Status("failed".into()))
                .unwrap()
                .outcome,
            Some(Outcome::Failed(_))
        )
    });
    let failure = client
        .call(Command::Status("failed".into()))
        .unwrap()
        .outcome;
    assert!(
        client
            .call(Command::AuthorizeActivation("failed".into()))
            .is_err()
    );
    server.shutdown();
    server.restart();
    assert_eq!(
        client
            .call(Command::Status("failed".into()))
            .unwrap()
            .outcome,
        failure
    );
    assert_eq!(server.backend.started.lock().unwrap().len(), 1);
    client.call(Command::Retry("failed".into())).unwrap();
    assert_eq!(
        client
            .call(Command::Status("failed".into()))
            .unwrap()
            .outcome,
        Some(Outcome::Ready)
    );
    assert!(
        client
            .call(Command::AuthorizeActivation("failed".into()))
            .is_err()
    );
    client.call(Command::Admit("failed".into())).unwrap();
    client
        .call(Command::AuthorizeActivation("failed".into()))
        .unwrap();
    server.shutdown();
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

#[test]
fn discarded_and_partially_retained_plans_remain_retirable_after_restart() {
    for reason in ["cancelled", "timed-out", "partial-retention"] {
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
        if reason == "cancelled" {
            client.call(Command::Cancel("m".into())).unwrap();
        } else if reason == "timed-out" {
            eventually(|| {
                matches!(
                    client.call(Command::Status("m".into())).unwrap().outcome,
                    Some(Outcome::Failed(_))
                )
            });
        } else {
            server.backend.retain_fails.store(true, Ordering::Relaxed);
        }
        *server.backend.plan_released.lock().unwrap() = true;
        server.backend.plan_gate.notify_all();
        let result = registration.join().unwrap();
        if reason == "cancelled" {
            assert_eq!(result.unwrap().outcome, Some(Outcome::Cancelled));
        } else {
            assert!(result.is_err());
        }
        server.shutdown();
        server.backend.retain_fails.store(false, Ordering::Relaxed);
        server.restart();
        assert!(
            client.call(Command::Retire("m".into())).is_ok(),
            "cannot retire {reason}"
        );
        assert_eq!(*server.backend.released_requests.lock().unwrap(), ["m"]);
        assert!(server.backend.retained.lock().unwrap().is_empty());
        assert!(server.backend.started.lock().unwrap().is_empty());
    }
}

#[test]
fn retention_journals_ownership_before_roots_and_remains_held_on_cancellation_or_timeout() {
    for cancelled in [true, false] {
        let mut server = Server::start();
        server.shutdown();
        server.config.planning_timeout_seconds = 1;
        server.backend.block_retain.store(true, Ordering::Relaxed);
        *server.backend.retain_released.lock().unwrap() = false;
        server.restart();
        let client = server.client();
        let joining = client.clone();
        let registration =
            std::thread::spawn(move || joining.call(Command::Register(request("m", "murph"))));
        eventually(|| server.backend.retain_entered.load(Ordering::Relaxed));
        let state: Train = serde_json::from_reader(
            std::fs::File::open(server.config.state_dir.join("train.json")).unwrap(),
        )
        .unwrap();
        assert!(!state.requests["m"].prepared);
        assert!(
            state
                .retained_graph("m")
                .unwrap()
                .contains_key(&goal("shared"))
        );
        if cancelled {
            client.call(Command::Cancel("m".into())).unwrap();
        } else {
            eventually(|| {
                matches!(
                    client.call(Command::Status("m".into())).unwrap().outcome,
                    Some(Outcome::Failed(_))
                )
            });
        }
        assert!(
            client
                .call(Command::Retire("m".into()))
                .unwrap_err()
                .contains("planner has not exited")
        );
        assert!(server.backend.started.lock().unwrap().is_empty());
        *server.backend.retain_released.lock().unwrap() = true;
        server.backend.retain_gate.notify_all();
        let result = registration.join().unwrap();
        if cancelled {
            assert_eq!(result.unwrap().outcome, Some(Outcome::Cancelled));
        } else {
            assert!(result.unwrap_err().contains("deadline"));
        }
        server.shutdown();
        server.backend.block_retain.store(false, Ordering::Relaxed);
        server.restart();
        client.call(Command::Retire("m".into())).unwrap();
        assert!(server.backend.retained.lock().unwrap().is_empty());
        assert!(server.backend.started.lock().unwrap().is_empty());
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

#[cfg(all(feature = "cli", feature = "build-train-cli"))]
#[test]
fn standalone_registration_deadline_detaches_and_allows_explicit_reattachment() {
    let server = Server::start();
    server.backend.block_plan.store(true, Ordering::Relaxed);
    let connection = server._temp.path().join("connection.json");
    let request_path = server._temp.path().join("request.json");
    std::fs::write(&connection, serde_json::to_vec(&serde_json::json!({
        "builder": "builder", "socket": server.config.socket, "policy": server.config.policy,
        "preparation_dir": server._temp.path().join("preparation"), "gc_roots": "/nix/var/nix/gcroots/per-user/operator/fleetix-train"
    })).unwrap()).unwrap();
    std::fs::write(
        &request_path,
        serde_json::to_vec(&request("m", "murph")).unwrap(),
    )
    .unwrap();
    let invoke = |seconds: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_fleetix"))
            .args([
                "build-train",
                "register",
                "--wait-seconds",
                seconds,
                "--connection",
            ])
            .arg(&connection)
            .arg("--request")
            .arg(&request_path)
            .output()
            .unwrap()
    };
    let started = Instant::now();
    let expired = invoke("1");
    assert!(!expired.status.success());
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(
        String::from_utf8_lossy(&expired.stderr).contains("detached"),
        "{}",
        String::from_utf8_lossy(&expired.stderr)
    );
    assert_eq!(
        server
            .client()
            .call(Command::Status("m".into()))
            .unwrap()
            .outcome,
        Some(Outcome::Pending)
    );
    let backend = Arc::clone(&server.backend);
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        *backend.plan_released.lock().unwrap() = true;
        backend.plan_gate.notify_all();
    });
    let reattached = invoke("620");
    release.join().unwrap();
    assert!(
        reattached.status.success(),
        "{}",
        String::from_utf8_lossy(&reattached.stderr)
    );
    let reply: Reply = serde_json::from_slice(&reattached.stdout).unwrap();
    assert!(reply.outputs.contains_key(&goal("murph")));
    assert!(server.backend.started.lock().unwrap().is_empty());
}

#[cfg(all(feature = "cli", feature = "build-train-cli"))]
#[test]
fn standalone_cli_joins_held_admission_waits_independently_and_reports_cancellation() {
    let server = Server::start();
    let directory = server._temp.path();
    let connection = directory.join("connection.json");
    std::fs::write(&connection, serde_json::to_vec(&serde_json::json!({
        "builder": "builder", "socket": server.config.socket, "policy": server.config.policy,
        "preparation_dir": directory.join("preparation"), "gc_roots": "/nix/var/nix/gcroots/per-user/operator/fleetix-train"
    })).unwrap()).unwrap();
    let invoke = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_fleetix"))
            .args(["build-train"])
            .args(args)
            .arg("--connection")
            .arg(&connection)
            .output()
            .unwrap()
    };
    for (id, target) in [("a", "atlas"), ("m", "murph")] {
        let path = directory.join(format!("{id}.json"));
        std::fs::write(&path, serde_json::to_vec(&request(id, target)).unwrap()).unwrap();
        let result = invoke(&["register", "--request", path.to_str().unwrap()]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        if id == "a" {
            assert_eq!(server.client().call(Command::Inspect).unwrap().running, 0);
            assert!(invoke(&["admit", "--attempt", id]).status.success());
            eventually(|| server.backend.started.lock().unwrap().as_slice() == ["busy"]);
        }
    }
    let timed_out = invoke(&["wait", "--attempt", "m", "--wait-seconds", "1"]);
    assert!(!timed_out.status.success());
    assert!(String::from_utf8_lossy(&timed_out.stderr).contains("detached"));
    assert_eq!(
        server
            .client()
            .call(Command::Status("m".into()))
            .unwrap()
            .outcome,
        Some(Outcome::Pending)
    );
    assert!(invoke(&["admit", "--attempt", "m"]).status.success());
    *server.backend.released.lock().unwrap() = true;
    server.backend.gate.notify_all();
    assert!(
        invoke(&["wait", "--attempt", "m", "--wait-seconds", "5"])
            .status
            .success()
    );
    let started = server.backend.started.lock().unwrap().clone();
    assert_eq!(&started[..3], ["busy", "shared", "murph"]);
    assert_eq!(started.iter().filter(|s| *s == "shared").count(), 1);
    server
        .client()
        .call(Command::Register(request("cancelled", "murph")))
        .unwrap();
    assert!(
        invoke(&["cancel", "--attempt", "cancelled"])
            .status
            .success()
    );
    let cancelled = invoke(&["wait", "--attempt", "cancelled", "--wait-seconds", "5"]);
    assert!(!cancelled.status.success());
    assert!(String::from_utf8_lossy(&cancelled.stderr).contains("cancelled"));
    server
        .backend
        .fail_after_materialize
        .store(true, Ordering::Relaxed);
    server
        .client()
        .call(Command::Register(request("failed", "multi")))
        .unwrap();
    assert!(invoke(&["admit", "--attempt", "failed"]).status.success());
    let failed = invoke(&["wait", "--attempt", "failed", "--wait-seconds", "5"]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("construction failed"));
    assert!(invoke(&["status", "--attempt", "failed"]).status.success());
    assert!(invoke(&["retry", "--attempt", "failed"]).status.success());
    let unadmitted = invoke(&["authorize-activation", "--attempt", "failed"]);
    assert!(!unadmitted.status.success());
    assert!(String::from_utf8_lossy(&unadmitted.stderr).contains("admission"));
    assert!(invoke(&["cancel", "--attempt", "failed"]).status.success());
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
fn policy_rollover_requires_a_stopped_drained_fenced_terminal_train() {
    let mut server = Server::start();
    let old = server.config.clone();
    let next = Config {
        policy: "new-policy".into(),
        ..old.clone()
    };
    let client = server.client();
    client.call(Command::Submit(request("m", "murph"))).unwrap();
    eventually(|| {
        client.call(Command::Status("m".into())).unwrap().outcome == Some(Outcome::Ready)
    });
    client
        .call(Command::Register(request("held", "atlas")))
        .unwrap();
    let fence = client.drain("m", Duration::from_secs(1)).unwrap();
    assert!(
        rollover(&old, &next, server.backend.as_ref(), &fence.token)
            .unwrap_err()
            .contains("stop")
    );
    server.shutdown();
    let journal = std::fs::read(old.state_dir.join("train.json")).unwrap();
    assert!(
        rollover(&old, &next, server.backend.as_ref(), "wrong")
            .unwrap_err()
            .contains("fence")
    );
    assert!(
        rollover(&old, &next, server.backend.as_ref(), &fence.token)
            .unwrap_err()
            .contains("cancel pending")
    );
    assert_eq!(
        journal,
        std::fs::read(old.state_dir.join("train.json")).unwrap()
    );
    assert!(!old.state_dir.join("rollover.json").exists());
    assert!(server.backend.released_requests.lock().unwrap().is_empty());
}

#[test]
fn policy_rollover_preserves_old_failure_fence_and_history_without_admitting_old_work() {
    let mut server = Server::start();
    let old = server.config.clone();
    let next = Config {
        policy: "new-policy".into(),
        ..old.clone()
    };
    let client = server.client();
    client.call(Command::Submit(request("m", "murph"))).unwrap();
    eventually(|| {
        client.call(Command::Status("m".into())).unwrap().outcome == Some(Outcome::Ready)
    });
    client
        .call(Command::Register(request("bad", "unsupported")))
        .unwrap_err();
    let failure = client.call(Command::Status("bad".into())).unwrap().outcome;
    client
        .call(Command::Register(request("cancelled", "atlas")))
        .unwrap();
    client.call(Command::Cancel("cancelled".into())).unwrap();
    client.call(Command::Retire("cancelled".into())).unwrap();
    let fence = client.drain("m", Duration::from_secs(1)).unwrap();
    let original = std::fs::read(old.state_dir.join("train.json")).unwrap();
    server.shutdown();
    let receipt = rollover(&old, &next, server.backend.as_ref(), &fence.token).unwrap();
    let snapshot: serde_json::Value = serde_json::from_slice(
        &std::fs::read(receipt.parent().unwrap().join("snapshot.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        snapshot["train"],
        serde_json::from_slice::<serde_json::Value>(&original).unwrap()
    );
    assert_eq!(snapshot["train"]["fence"]["token"], fence.token);
    assert_eq!(snapshot["archives"].as_array().unwrap().len(), 1);
    assert!(!old.state_dir.join("rollover.json").exists());
    assert_eq!(
        rollover(&old, &next, server.backend.as_ref(), &fence.token).unwrap(),
        receipt
    );
    assert_eq!(server.backend.released_requests.lock().unwrap().len(), 3);
    server.config = next.clone();
    server.restart();
    assert_eq!(
        client.call(Command::Status("bad".into())).unwrap().outcome,
        failure
    );
    assert_eq!(
        client.call(Command::Retire("m".into())).unwrap().outcome,
        Some(Outcome::Ready)
    );
    assert_eq!(
        client
            .call(Command::Status("cancelled".into()))
            .unwrap()
            .outcome,
        Some(Outcome::Cancelled)
    );
    for command in [
        Command::Retry("bad".into()),
        Command::Admit("m".into()),
        Command::AuthorizeActivation("m".into()),
        Command::Register(request("fresh-old", "murph")),
        Command::ReleaseFence(fence.token.clone()),
    ] {
        assert!(client.call(command).is_err());
    }
    let new_client = server.client();
    assert!(
        new_client
            .call(Command::Register(request("m", "murph")))
            .is_err()
    );
    new_client
        .call(Command::Submit(request("new", "murph")))
        .unwrap();
    eventually(|| {
        new_client
            .call(Command::Status("new".into()))
            .unwrap()
            .outcome
            == Some(Outcome::Ready)
    });
    let replacement_journal = std::fs::read(next.state_dir.join("train.json")).unwrap();
    server.shutdown();
    // Retrying a completed handover cannot reset work admitted by the new policy.
    assert_eq!(
        rollover(&old, &next, server.backend.as_ref(), &fence.token).unwrap(),
        receipt
    );
    assert_eq!(
        replacement_journal,
        std::fs::read(next.state_dir.join("train.json")).unwrap()
    );
}

#[test]
fn interrupted_policy_rollover_blocks_startup_and_recovers_only_with_exact_evidence() {
    let mut server = Server::start();
    let old = server.config.clone();
    let next = Config {
        policy: "new-policy".into(),
        ..old.clone()
    };
    let client = server.client();
    client.call(Command::Submit(request("m", "murph"))).unwrap();
    eventually(|| {
        client.call(Command::Status("m".into())).unwrap().outcome == Some(Outcome::Ready)
    });
    let fence = client.drain("m", Duration::from_secs(1)).unwrap();
    server.shutdown();
    server.backend.release_fails.store(true, Ordering::Relaxed);
    assert!(
        rollover(&old, &next, server.backend.as_ref(), &fence.token)
            .unwrap_err()
            .contains("injected")
    );
    assert!(old.state_dir.join("rollover.json").exists());
    for config in [&old, &next] {
        assert!(
            serve(
                config.clone(),
                server.backend.clone(),
                Arc::new(AtomicBool::new(true))
            )
            .unwrap_err()
            .contains("interrupted policy rollover")
        );
    }
    let wrong = Config {
        workers: 2,
        ..next.clone()
    };
    assert!(
        rollover(&old, &wrong, server.backend.as_ref(), &fence.token)
            .unwrap_err()
            .contains("different configs")
    );
    assert!(server.backend.released_requests.lock().unwrap().is_empty());
    server.backend.release_fails.store(false, Ordering::Relaxed);
    rollover(&old, &next, server.backend.as_ref(), &fence.token).unwrap();
    assert_eq!(
        server.backend.released_requests.lock().unwrap().as_slice(),
        ["m"]
    );
    server.config = next;
    server.restart();
    assert_eq!(
        client.call(Command::Status("m".into())).unwrap().outcome,
        Some(Outcome::Ready)
    );
    server.shutdown();
}

#[test]
fn successive_rollovers_keep_each_policy_history_and_survive_post_publication_interruption() {
    let mut server = Server::start();
    let first = server.config.clone();
    let second = Config {
        policy: "second-policy".into(),
        ..first.clone()
    };
    let third = Config {
        policy: "third-policy".into(),
        ..first.clone()
    };
    let first_client = server.client();
    first_client
        .call(Command::Submit(request("first", "murph")))
        .unwrap();
    eventually(|| {
        first_client
            .call(Command::Status("first".into()))
            .unwrap()
            .outcome
            == Some(Outcome::Ready)
    });
    let fence = first_client.drain("first", Duration::from_secs(1)).unwrap();
    server.shutdown();
    let receipt = rollover(&first, &second, server.backend.as_ref(), &fence.token).unwrap();
    // Recreate a crash after the replacement journal and receipt were durably
    // written, but before the marker was removed. No roots may be released twice.
    std::fs::copy(&receipt, first.state_dir.join("rollover.json")).unwrap();
    rollover(&first, &second, server.backend.as_ref(), &fence.token).unwrap();
    assert_eq!(
        server.backend.released_requests.lock().unwrap().as_slice(),
        ["first"]
    );
    server.config = second.clone();
    server.restart();
    let second_client = server.client();
    second_client
        .call(Command::Submit(request("second", "murph")))
        .unwrap();
    eventually(|| {
        second_client
            .call(Command::Status("second".into()))
            .unwrap()
            .outcome
            == Some(Outcome::Ready)
    });
    let second_fence = second_client
        .drain("second", Duration::from_secs(1))
        .unwrap();
    server.shutdown();
    rollover(
        &second,
        &third,
        server.backend.as_ref(),
        &second_fence.token,
    )
    .unwrap();
    server.config = third;
    server.restart();
    for (client, attempt) in [(&first_client, "first"), (&second_client, "second")] {
        assert_eq!(
            client
                .call(Command::Status(attempt.into()))
                .unwrap()
                .outcome,
            Some(Outcome::Ready)
        );
        assert_eq!(
            client
                .call(Command::Retire(attempt.into()))
                .unwrap()
                .outcome,
            Some(Outcome::Ready)
        );
        assert!(
            client
                .call(Command::AuthorizeActivation(attempt.into()))
                .is_err()
        );
    }
    assert_eq!(
        server.backend.released_requests.lock().unwrap().as_slice(),
        ["first", "second"]
    );
}

#[test]
fn completed_rollover_retry_verifies_snapshot_without_rewriting_replacement_work() {
    let mut server = Server::start();
    let old = server.config.clone();
    let next = Config {
        policy: "new-policy".into(),
        ..old.clone()
    };
    let client = server.client();
    client.call(Command::Submit(request("m", "murph"))).unwrap();
    eventually(|| {
        client.call(Command::Status("m".into())).unwrap().outcome == Some(Outcome::Ready)
    });
    let fence = client.drain("m", Duration::from_secs(1)).unwrap();
    server.shutdown();
    let receipt = rollover(&old, &next, server.backend.as_ref(), &fence.token).unwrap();
    let journal = std::fs::read(next.state_dir.join("train.json")).unwrap();
    let path = receipt.parent().unwrap().join("snapshot.json");
    let original = std::fs::read(&path).unwrap();
    let mut changed = original.clone();
    changed.push(b' ');
    std::fs::write(&path, changed).unwrap();
    assert!(
        rollover(&old, &next, server.backend.as_ref(), &fence.token)
            .unwrap_err()
            .contains("checksum changed")
    );
    assert_eq!(
        journal,
        std::fs::read(next.state_dir.join("train.json")).unwrap()
    );
    assert!(!old.state_dir.join("rollover.json").exists());
    assert_eq!(
        server.backend.released_requests.lock().unwrap().as_slice(),
        ["m"]
    );
    std::fs::write(path, original).unwrap();
    assert_eq!(
        rollover(&old, &next, server.backend.as_ref(), &fence.token).unwrap(),
        receipt
    );
    let journal_path = next.state_dir.join("train.json");
    let mut incompatible: serde_json::Value = serde_json::from_slice(&journal).unwrap();
    incompatible["version"] = serde_json::json!(fleetix::build_train::VERSION + 1);
    let raw = serde_json::to_vec(&incompatible).unwrap();
    std::fs::write(&journal_path, &raw).unwrap();
    assert!(
        rollover(&old, &next, server.backend.as_ref(), &fence.token)
            .unwrap_err()
            .contains("unsupported rollover journal version")
    );
    assert_eq!(std::fs::read(&journal_path).unwrap(), raw);
    assert_eq!(
        server.backend.released_requests.lock().unwrap().as_slice(),
        ["m"]
    );
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
fn drain_deadline_bounds_a_stalled_status_reply_and_reports_its_fence() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("coordinator.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let client = Client {
        socket,
        policy: "p".into(),
    };
    let peer = std::thread::spawn(move || {
        for running in [1, 0] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut command = String::new();
            BufReader::new(&stream).read_line(&mut command).unwrap();
            if running == 0 {
                std::thread::sleep(Duration::from_secs(1));
            }
            let mut wire = serde_json::to_vec(&Reply {
                version: VERSION,
                policy: "p".into(),
                outcome: None,
                fence: Some(Fence {
                    token: "a-1".into(),
                    attempt: "a".into(),
                }),
                running,
                error: None,
                outputs: BTreeMap::new(),
            })
            .unwrap();
            wire.push(b'\n');
            let _ = stream.write_all(&wire);
        }
    });
    let started = Instant::now();
    let result = client.drain("a", Duration::from_millis(300));
    let elapsed = started.elapsed();
    peer.join().unwrap();
    assert!(
        elapsed < Duration::from_millis(800),
        "drain took {elapsed:?}"
    );
    let error = result.unwrap_err();
    assert!(
        error.contains("a-1") && error.contains("retained"),
        "{error}"
    );
}

#[test]
fn bounded_wait_detaches_without_cancelling_held_work() {
    let server = Server::start();
    let client = server.client();
    client
        .call(Command::Register(request("m", "murph")))
        .unwrap();
    let stop = AtomicBool::new(false);
    let error = client
        .wait_for("m", Duration::from_millis(50), &stop)
        .unwrap_err();
    assert!(
        error.contains("detached") && error.contains("timed out"),
        "{error}"
    );
    assert_eq!(
        client.call(Command::Status("m".into())).unwrap().outcome,
        Some(Outcome::Pending)
    );
    client.call(Command::Admit("m".into())).unwrap();
    assert_eq!(
        client.wait_for("m", Duration::from_secs(5), &stop).unwrap(),
        Outcome::Ready
    );
}

#[test]
fn bounded_wait_caps_stalled_socket_delivery_and_preserves_interrupt_semantics() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("coordinator.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let client = Client {
        socket,
        policy: "p".into(),
    };
    let peer = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut command = String::new();
        BufReader::new(&stream).read_line(&mut command).unwrap();
        assert!(command.contains("status"));
        std::thread::sleep(Duration::from_secs(1));
    });
    let started = Instant::now();
    let error = client
        .wait_for("a", Duration::from_millis(100), &AtomicBool::new(false))
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_millis(800));
    assert!(error.contains("detached"), "{error}");
    peer.join().unwrap();
    let error = client
        .wait_for("a", Duration::from_secs(5), &AtomicBool::new(true))
        .unwrap_err();
    assert!(error.contains("cancel explicitly"), "{error}");
}

#[test]
fn wait_interrupt_detaches_during_stalled_socket_delivery() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("coordinator.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let client = Client {
        socket,
        policy: "p".into(),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let interrupt = Arc::clone(&stop);
    let peer = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut command = String::new();
        BufReader::new(&stream).read_line(&mut command).unwrap();
        interrupt.store(true, Ordering::Relaxed);
        std::thread::sleep(Duration::from_secs(1));
    });
    let started = Instant::now();
    let error = client
        .wait_for("a", Duration::from_secs(5), &stop)
        .unwrap_err();
    let elapsed = started.elapsed();
    peer.join().unwrap();
    assert!(
        elapsed < Duration::from_millis(800),
        "interrupt took {elapsed:?}"
    );
    assert!(error.contains("detached") && error.contains("cancel explicitly"));
}

#[test]
fn legacy_wait_interrupt_detaches_during_stalled_socket_delivery() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("coordinator.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let client = Client {
        socket,
        policy: "p".into(),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let interrupt = Arc::clone(&stop);
    let peer = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut command = String::new();
        BufReader::new(&stream).read_line(&mut command).unwrap();
        interrupt.store(true, Ordering::Relaxed);
        std::thread::sleep(Duration::from_secs(1));
    });
    let started = Instant::now();
    let error = client.wait("a", &stop).unwrap_err();
    let elapsed = started.elapsed();
    peer.join().unwrap();
    assert!(
        elapsed < Duration::from_millis(800),
        "legacy wait interrupt took {elapsed:?}"
    );
    assert!(error.contains("detached") && error.contains("cancel explicitly"));
}

#[test]
fn unrepresentable_drain_duration_fails_without_fencing() {
    let server = Server::start();
    let client = server.client();
    client
        .call(Command::Register(request("a", "atlas")))
        .unwrap();
    let error = client.drain("a", Duration::MAX).unwrap_err();
    assert!(error.contains("invalid drain wait duration"), "{error}");
    let status = client.call(Command::Inspect).unwrap();
    assert!(status.fence.is_none());
    assert_eq!(
        client.call(Command::Status("a".into())).unwrap().outcome,
        Some(Outcome::Pending)
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
