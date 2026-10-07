//! Private, builder-local coordinator. Requests contain data, never commands.
//!
//! Disconnect detaches. Cancellation removes one request's interests and lets
//! already-running Nix work finish. A graceful service stop drains workers. On
//! restart, the backend rechecks the store; Nix retains ownership of its own output
//! locks if an old daemon worker outlives the coordinator.

use super::*;
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::{
    fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    net::{UnixListener, UnixStream},
};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_FRAME: u64 = 1024 * 1024;

mod ipc;
pub mod preparation;
mod rollover;
mod wire;
pub use rollover::rollover;

/// Specialist backend owns graph discovery, substitution, GC roots and realization.
pub trait Backend: Send + Sync + 'static {
    /// Side-effect-free identity validation before durable intake. Retention
    /// runs only after the journal owns the request; deterministic malformed
    /// identities must never become recovery obligations. Existing specialist
    /// backends may rely solely on the coordinator's generic validation.
    fn validate_request(&self, _request: &Request) -> Result<(), String> {
        Ok(())
    }
    /// Bounded graph inspection. The coordinator runs this on dedicated planners.
    fn plan(&self, request: &Request) -> Result<Graph, String>;
    fn retain(&self, request: &Request, graph: &Graph) -> Result<(), String>;
    fn valid(&self, goal: &Goal, definition: &Definition) -> Result<bool, String>;
    fn realise(&self, dispatch: &Dispatch) -> Result<(), String>;
    /// Release only this request's roots; other owners must retain their own pins.
    /// Backends without request-scoped roots conservatively retain them.
    fn release(&self, _request: &Request, _graph: &Graph) -> Result<(), String> {
        Ok(())
    }
}

/// Configuration is deployment-owned and immutable for this coordinator lifetime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub socket: PathBuf,
    pub state_dir: PathBuf,
    pub policy: String,
    pub workers: usize,
    pub queue_limit: usize,
    pub aging_seconds: u64,
    /// Separate bounded capacity for graph preparation, never build-worker slots.
    #[serde(default = "default_planning_workers")]
    pub planning_workers: usize,
    /// Reply deadline. A tardy backend still occupies its slot until it exits.
    #[serde(default = "default_planning_timeout")]
    pub planning_timeout_seconds: u64,
}

fn default_planning_workers() -> usize {
    1
}
fn default_planning_timeout() -> u64 {
    180
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    content = "arguments",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum Command {
    Submit(Request),
    Register(Request),
    Admit(String),
    Retry(String),
    Retire(String),
    Status(String),
    Cancel(String),
    Fence(String),
    ReleaseFence(String),
    AuthorizeActivation(String),
    Inspect,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    policy: String,
    command: Command,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reply {
    pub version: u32,
    pub policy: String,
    pub outcome: Option<Outcome>,
    pub fence: Option<Fence>,
    pub running: usize,
    pub error: Option<String>,
    pub outputs: BTreeMap<Goal, String>,
}

/// Stateless library client. Every reconnect validates protocol and policy.
#[derive(Clone)]
pub struct Client {
    pub socket: PathBuf,
    pub policy: String,
}

impl Client {
    pub fn call(&self, command: Command) -> Result<Reply, String> {
        self.call_until(
            command,
            std::time::Instant::now() + Duration::from_secs(300),
        )
    }

    fn call_until(&self, command: Command, deadline: std::time::Instant) -> Result<Reply, String> {
        self.call_until_stoppable(
            command,
            deadline.min(std::time::Instant::now() + Duration::from_secs(300)),
            None,
        )
    }

    fn call_until_stoppable(
        &self,
        command: Command,
        deadline: std::time::Instant,
        stop: Option<&AtomicBool>,
    ) -> Result<Reply, String> {
        let mut raw = serde_json::to_vec(&Envelope {
            version: VERSION,
            policy: self.policy.clone(),
            command,
        })
        .map_err(|e| e.to_string())?;
        if raw.len() as u64 >= MAX_FRAME {
            return Err("request exceeds protocol limit".into());
        }
        raw.push(b'\n');
        let reply: Reply =
            serde_json::from_slice(&ipc::exchange(&self.socket, &raw, deadline, stop)?)
                .map_err(|e| e.to_string())?;
        if reply.version != VERSION || reply.policy != self.policy {
            return Err("coordinator protocol or policy changed".into());
        }
        if let Some(error) = &reply.error {
            return Err(error.clone());
        }
        Ok(reply)
    }

    /// Await held registration and graph preparation under the caller's explicit
    /// deadline. Delivery failure only detaches; inspect the durable attempt before
    /// retrying, because the coordinator may have accepted it.
    pub fn register_for(
        &self,
        request: Request,
        wait: Duration,
        stop: &AtomicBool,
    ) -> Result<Reply, String> {
        let deadline = std::time::Instant::now()
            .checked_add(wait)
            .ok_or("invalid registration wait duration")?;
        let attempt = request.attempt.clone();
        self.call_until_stoppable(Command::Register(request), deadline, Some(stop))
            .map_err(|error| format!(
                "registration outcome uncertain for request {attempt}; detached; inspect before retry: {error}"
            ))
    }

    pub fn wait(&self, attempt: &str, stop: &AtomicBool) -> Result<Outcome, String> {
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err("detached; construction continues; cancel explicitly".into());
            }
            let outcome = self
                .call_until_stoppable(
                    Command::Status(attempt.into()),
                    std::time::Instant::now() + Duration::from_secs(300),
                    Some(stop),
                )
                .map_err(|error| {
                    format!(
                        "wait failed; detached; construction continues; cancel explicitly: {error}"
                    )
                })?
                .outcome
                .ok_or("missing outcome; detached; construction continues; cancel explicitly")?;
            if outcome != Outcome::Pending {
                return Ok(outcome);
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Wait under one absolute deadline, including socket delivery. Expiry and
    /// interruption only detach; the durable request must be cancelled explicitly.
    pub fn wait_for(
        &self,
        attempt: &str,
        wait: Duration,
        stop: &AtomicBool,
    ) -> Result<Outcome, String> {
        let deadline = std::time::Instant::now()
            .checked_add(wait)
            .ok_or("invalid completion wait duration")?;
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err("detached; construction continues; cancel explicitly".into());
            }
            if std::time::Instant::now() >= deadline {
                return Err(
                    "wait timed out; detached; construction continues; cancel explicitly".into(),
                );
            }
            let outcome = self
                .call_until_stoppable(
                    Command::Status(attempt.into()),
                    deadline.min(std::time::Instant::now() + Duration::from_secs(300)),
                    Some(stop),
                )
                .map_err(|error| {
                    let reason = if std::time::Instant::now() >= deadline {
                        "wait timed out"
                    } else {
                        "wait failed"
                    };
                    format!(
                        "{reason}; detached; construction continues; cancel explicitly: {error}"
                    )
                })?
                .outcome
                .ok_or("missing outcome; detached; construction continues; cancel explicitly")?;
            if outcome != Outcome::Pending {
                return Ok(outcome);
            }
            thread::sleep(
                deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .min(Duration::from_millis(100)),
            );
        }
    }

    /// Close dispatch atomically before waiting. Timeout leaves the named fence
    /// intact so a disconnected activation cannot accidentally restart builders.
    pub fn drain(&self, attempt: &str, wait: Duration) -> Result<Fence, String> {
        let deadline = std::time::Instant::now()
            .checked_add(wait)
            .ok_or("invalid drain wait duration")?;
        // Zero means inspect-and-fail without waiting for workers. Allow only a
        // bounded control-plane round trip to establish the durable fence.
        let fence_deadline = if wait.is_zero() {
            std::time::Instant::now() + Duration::from_secs(5)
        } else {
            deadline
        };
        let reply = self
            .call_until(Command::Fence(attempt.into()), fence_deadline)
            .map_err(|error| {
                format!(
                    "fence outcome uncertain for request {attempt}; inspect before retry: {error}"
                )
            })?;
        let fence = reply.fence.ok_or("missing fence")?;
        if fence.attempt != attempt {
            return Err("coordinator returned another request's fence".into());
        }
        let mut running = reply.running;
        while running != 0 {
            if std::time::Instant::now() >= deadline {
                return Err(format!(
                    "drain timed out; fence {} retained for request {}",
                    fence.token, attempt
                ));
            }
            thread::sleep(
                deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .min(Duration::from_millis(100)),
            );
            running = self
                .call_until(Command::Inspect, deadline)
                .map_err(|error| {
                    format!(
                        "drain failed; fence {} retained for request {attempt}: {error}",
                        fence.token
                    )
                })?
                .running;
        }
        Ok(fence)
    }
}

fn same_uid(stream: &UnixStream) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let mut credential = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: credential and size are writable, correctly sized buffers. The
        // live stream owns the descriptor. getsockopt initializes no references.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credential as *mut libc::ucred).cast(),
                &mut size,
            )
        };
        if result != 0 || size as usize != std::mem::size_of::<libc::ucred>() {
            return Err("cannot authenticate coordinator peer".into());
        }
        // SAFETY: geteuid has no preconditions.
        if credential.uid != unsafe { libc::geteuid() } {
            return Err("peer is outside the operator domain".into());
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stream;
        return Err("coordinator peer authentication requires Linux".into());
    }
    Ok(())
}

fn private_dir(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("coordinator paths must be absolute".into());
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            // SAFETY: geteuid has no preconditions.
            if !metadata.is_dir()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o077 != 0
            {
                return Err(format!(
                    "{} must be an euid-owned private directory",
                    path.display()
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(path)
                .map_err(|e| e.to_string())?;
        }
        Err(error) => return Err(error.to_string()),
    }
    Ok(())
}

use std::os::unix::fs::DirBuilderExt;

fn save(config: &Config, train: &Train) -> Result<(), String> {
    crate::fsutil::atomic_write(
        &config.state_dir.join("train.json"),
        &serde_json::to_vec(train).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[derive(Serialize, Deserialize)]
struct Archive {
    version: u32,
    policy: String,
    record: RequestRecord,
    outcome: Outcome,
    graph: Graph,
    roots_released: bool,
}

fn archive_path(config: &Config, attempt: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    config
        .state_dir
        .join("archive")
        .join(format!("{:x}.json", Sha256::digest(attempt.as_bytes())))
}

fn read_archive(config: &Config, attempt: &str) -> Result<Option<Archive>, String> {
    match File::open(archive_path(config, attempt)) {
        Ok(file) => {
            let archive: Archive = serde_json::from_reader(file).map_err(|e| e.to_string())?;
            if archive.version != VERSION
                || archive.policy != config.policy
                || archive.record.request.attempt != attempt
            {
                return Err("archive identity mismatch".into());
            }
            Ok(Some(archive))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

fn save_archive(config: &Config, archive: &Archive) -> Result<(), String> {
    crate::fsutil::atomic_write(
        &archive_path(config, &archive.record.request.attempt),
        &serde_json::to_vec(archive).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn retire(
    config: &Config,
    train: &mut Train,
    backend: &dyn Backend,
    attempt: &str,
) -> Result<Reply, String> {
    let mut archive = if train.requests.contains_key(attempt) {
        let outcome = train.outcome(attempt)?;
        let graph = train.retained_graph(attempt)?;
        let mut next = train.clone();
        let record = next.retire(attempt)?;
        let archive = Archive {
            version: VERSION,
            policy: config.policy.clone(),
            record,
            outcome,
            graph,
            roots_released: false,
        };
        // Evidence first, membership second, roots last. Every interrupted phase is retryable.
        save_archive(config, &archive)?;
        save(config, &next)?;
        *train = next;
        archive
    } else {
        read_archive(config, attempt)?.ok_or("unknown request")?
    };
    if !archive.roots_released {
        backend.release(&archive.record.request, &archive.graph)?;
        archive.roots_released = true;
        save_archive(config, &archive)?;
    }
    Ok(archive_reply(config, train, &archive))
}

fn archive_reply(config: &Config, train: &Train, archive: &Archive) -> Reply {
    let mut result = reply(config, train, Some(archive.outcome.clone()), None);
    result.outputs = archive
        .record
        .request
        .roots
        .iter()
        .filter_map(|goal| {
            archive
                .graph
                .get(goal)
                .map(|d| (goal.clone(), d.output_path.clone()))
        })
        .collect();
    result
}

struct Planner {
    worker: thread::JoinHandle<()>,
    started: std::time::Instant,
    timed_out: bool,
}

enum Preparation {
    Planned(Result<Graph, String>),
    Retained(Result<(), String>),
}

fn queue_reply(
    outgoing: &mut Vec<ipc::Outgoing>,
    stream: UnixStream,
    result: &Reply,
) -> Result<(), String> {
    outgoing.push(ipc::Outgoing::new(stream, wire::encode(result)?));
    Ok(())
}

fn flush_waiters(
    config: &Config,
    train: &Train,
    waiters: &mut BTreeMap<String, Vec<UnixStream>>,
    outgoing: &mut Vec<ipc::Outgoing>,
    attempt: &str,
) -> Result<(), String> {
    if let Some(streams) = waiters.remove(attempt) {
        let outcome = train.outcome(attempt)?;
        let mut result = reply(config, train, Some(outcome.clone()), Some(attempt));
        if let Outcome::Failed(error) = outcome {
            result.error = Some(error);
        }
        for stream in streams {
            queue_reply(outgoing, stream, &result)?;
        }
    }
    Ok(())
}

/// Run one exclusive coordinator. No evaluation lease is held by this service.
pub fn serve(
    config: Config,
    backend: Arc<dyn Backend>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    if config.workers == 0
        || config.workers > 64
        || config.queue_limit == 0
        || config.policy.is_empty()
        || config.policy.len() > wire::FIELD_LIMIT
        || !(1..=16).contains(&config.planning_workers)
        || !(1..=86_400).contains(&config.planning_timeout_seconds)
    {
        return Err("invalid coordinator capacity or policy".into());
    }
    private_dir(&config.state_dir)?;
    private_dir(&config.state_dir.join("archive"))?;
    private_dir(config.socket.parent().ok_or("socket has no parent")?)?;
    let lease = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(config.state_dir.join("coordinator.lock"))
        .map_err(|e| e.to_string())?;
    lease
        .try_lock_exclusive()
        .map_err(|e| format!("coordinator lease held: {e}"))?;
    let socket_lease = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(config.socket.with_extension("lock"))
        .map_err(|e| e.to_string())?;
    socket_lease
        .try_lock_exclusive()
        .map_err(|e| format!("coordinator socket lease held: {e}"))?;
    if config
        .state_dir
        .join("rollover.json")
        .try_exists()
        .map_err(|e| e.to_string())?
    {
        return Err("interrupted policy rollover; repeat the exact offline rollover before starting the coordinator".into());
    }
    let mut train: Train = match File::open(config.state_dir.join("train.json")) {
        Ok(file) => {
            serde_json::from_reader(file).map_err(|e| format!("invalid coordinator state: {e}"))?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Train::new(config.policy.clone(), config.aging_seconds)
        }
        Err(error) => return Err(error.to_string()),
    };
    if train.version != VERSION
        || train.policy != config.policy
        || train.aging_seconds != config.aging_seconds
    {
        return Err("persisted train protocol/policy mismatch; retain the old coordinator until its requests finish".into());
    }
    let mut valid = BTreeSet::new();
    for (attempt, record) in &train.requests {
        let graph = train.retained_graph(attempt)?;
        backend.retain(&record.request, &graph)?;
    }
    for (goal, node) in &train.nodes {
        if backend.valid(goal, &node.definition)? {
            valid.insert(goal.clone());
        }
    }
    train.reconcile(&valid);
    save(&config, &train)?;
    if config.socket.exists() {
        if UnixStream::connect(&config.socket).is_ok() {
            return Err("a live coordinator already owns this socket".into());
        }
        use std::os::unix::fs::FileTypeExt;
        let metadata = fs::symlink_metadata(&config.socket).map_err(|e| e.to_string())?;
        if !metadata.file_type().is_socket() {
            return Err("refusing to replace a non-socket endpoint".into());
        }
        fs::remove_file(&config.socket).map_err(|e| e.to_string())?;
    }
    let listener = UnixListener::bind(&config.socket).map_err(|e| e.to_string())?;
    fs::set_permissions(&config.socket, fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let (completed_tx, completed_rx) =
        mpsc::channel::<(Dispatch, Result<(), String>, BTreeSet<Goal>)>();
    let (planned_tx, planned_rx) = mpsc::channel::<(String, Preparation)>();
    let mut planners: BTreeMap<String, Planner> = BTreeMap::new();
    let mut waiters: BTreeMap<String, Vec<UnixStream>> = BTreeMap::new();
    let mut incoming = Vec::<ipc::Incoming>::new();
    let mut outgoing = Vec::<ipc::Outgoing>::new();
    let mut workers = Vec::new();
    while !stop.load(Ordering::Relaxed) || train.running() != 0 || !planners.is_empty() {
        while let Ok((attempt, receipt)) = planned_rx.try_recv() {
            let planner = planners.remove(&attempt).ok_or("unknown planner receipt")?;
            let _ = planner.worker.join();
            let mut next = train.clone();
            if !planner.timed_out && !next.requests[&attempt].cancelled {
                let result = match receipt {
                    Preparation::Planned(result) => result.and_then(|graph| {
                        let request = next.requests[&attempt].request.clone();
                        next.submit(request.clone(), graph.clone(), now())?;
                        // Journal the complete ownership closure before retention can
                        // create even one root. Keep dispatch held until retention
                        // succeeds; cancellation, timeout, panic and restart can all
                        // retire partial roots using this version-2 journal evidence.
                        next.requests
                            .get_mut(&attempt)
                            .ok_or("missing request")?
                            .prepared = false;
                        save(&config, &next)?;
                        let backend = Arc::clone(&backend);
                        let tx = planned_tx.clone();
                        let id = attempt.clone();
                        let worker = thread::spawn(move || {
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    backend.retain(&request, &graph)
                                }))
                                .unwrap_or_else(|_| Err("backend retention panicked".into()));
                            let _ = tx.send((id, Preparation::Retained(result)));
                        });
                        planners.insert(
                            attempt.clone(),
                            Planner {
                                worker,
                                started: planner.started,
                                timed_out: false,
                            },
                        );
                        Ok(())
                    }),
                    Preparation::Retained(result) => result.and_then(|()| {
                        next.requests
                            .get_mut(&attempt)
                            .ok_or("missing request")?
                            .prepared = true;
                        Ok(())
                    }),
                };
                if let Err(error) = result {
                    next = train.clone();
                    next.preparation_failed(&attempt, error)?;
                }
                if !planners.contains_key(&attempt) {
                    save(&config, &next)?;
                }
                train = next;
            }
            if !planners.contains_key(&attempt) {
                flush_waiters(&config, &train, &mut waiters, &mut outgoing, &attempt)?;
            }
        }
        for (attempt, planner) in &mut planners {
            if !planner.timed_out
                && planner.started.elapsed() >= Duration::from_secs(config.planning_timeout_seconds)
            {
                planner.timed_out = true;
                train.preparation_failed(
                    attempt,
                    "graph preparation deadline exceeded; retry after the planner exits".into(),
                )?;
                save(&config, &train)?;
                flush_waiters(&config, &train, &mut waiters, &mut outgoing, attempt)?;
            }
        }
        while let Ok((dispatch, result, valid)) = completed_rx.try_recv() {
            train.finish(&dispatch, result);
            // One native builder may have produced multiple selected outputs.
            // Store evidence, rather than the worker's exit code, satisfies them.
            for goal in valid {
                if let Some(node) = train.nodes.get_mut(&goal) {
                    node.state = NodeState::Complete;
                }
            }
            save(&config, &train)?;
        }
        workers.retain(|worker: &thread::JoinHandle<()>| !worker.is_finished());
        outgoing.retain_mut(ipc::Outgoing::pending);
        if !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let connections = incoming.len()
                        + outgoing.len()
                        + waiters.values().map(Vec::len).sum::<usize>();
                    if connections < ipc::MAX_CONNECTIONS {
                        if let Ok(connection) = ipc::Incoming::new(stream) {
                            incoming.push(connection);
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.to_string()),
            }
            let mut pending = Vec::new();
            for mut connection in incoming.drain(..) {
                let raw = match connection.receive() {
                    Ok(Some(raw)) => raw,
                    Ok(None) => {
                        pending.push(connection);
                        continue;
                    }
                    Err(_) => continue,
                };
                let result = serde_json::from_slice::<Envelope>(&raw)
                    .map_err(|e| e.to_string())
                    .and_then(|envelope| {
                        if envelope.version != VERSION {
                            return Err(
                                "incompatible coordinator protocol or execution policy".into()
                            );
                        }
                        if envelope.policy != config.policy {
                            return historical_reply(&config, envelope).map(|reply| (reply, None));
                        }
                        let subject = match &envelope.command {
                            Command::Submit(request) | Command::Register(request) => {
                                Some(request.attempt.clone())
                            }
                            _ => None,
                        };
                        if let Command::Retry(attempt) | Command::Retire(attempt) =
                            &envelope.command
                        {
                            if planners.contains_key(attempt) {
                                return Err("request graph planner has not exited".into());
                            }
                        }
                        if subject.as_ref().is_some_and(|id| {
                            waiters.get(id).is_some_and(|streams| streams.len() >= 8)
                        }) {
                            return Err("too many waiting registration clients".into());
                        }
                        apply(&config, &mut train, backend.as_ref(), envelope.command)
                            .map(|result| (result, subject))
                    });
                let mut endpoint = Some(connection.stream);
                let reply = match result {
                    Ok((result, subject)) => {
                        if let Some(attempt) = subject {
                            let record = &train.requests[&attempt];
                            if !record.prepared && train.outcome(&attempt)? == Outcome::Pending {
                                waiters
                                    .entry(attempt)
                                    .or_default()
                                    .push(endpoint.take().ok_or("missing registration endpoint")?);
                            }
                        }
                        result
                    }
                    Err(error) => Reply {
                        version: VERSION,
                        policy: config.policy.clone(),
                        outcome: None,
                        fence: train.fence.clone(),
                        running: train.running(),
                        error: Some(error),
                        outputs: BTreeMap::new(),
                    },
                };
                if let Some(stream) = endpoint {
                    queue_reply(&mut outgoing, stream, &reply)?;
                }
            }
            incoming = pending;
            for attempt in waiters.keys().cloned().collect::<Vec<_>>() {
                if train.requests[&attempt].prepared || train.outcome(&attempt)? != Outcome::Pending
                {
                    flush_waiters(&config, &train, &mut waiters, &mut outgoing, &attempt)?;
                }
            }
            while planners.len() < config.planning_workers {
                let attempt = train
                    .requests
                    .iter()
                    .filter(|(id, record)| {
                        !record.prepared
                            && !planners.contains_key(*id)
                            && train.outcome(id).ok() == Some(Outcome::Pending)
                    })
                    .min_by_key(|(_, record)| record.sequence)
                    .map(|(id, _)| id.clone());
                let Some(attempt) = attempt else {
                    break;
                };
                let request = train.requests[&attempt].request.clone();
                let backend = Arc::clone(&backend);
                let tx = planned_tx.clone();
                let id = attempt.clone();
                let worker = thread::spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let graph = backend.plan(&request)?;
                        if graph.len() > 200_000 {
                            return Err("backend graph limit exceeded".into());
                        }
                        wire::validate_outputs(&request, &graph)?;
                        Ok(graph)
                    }))
                    .unwrap_or_else(|_| Err("backend planner panicked".into()));
                    let _ = tx.send((id, Preparation::Planned(result)));
                });
                planners.insert(
                    attempt,
                    Planner {
                        worker,
                        started: std::time::Instant::now(),
                        timed_out: false,
                    },
                );
            }
            while train.running() < config.workers {
                let Some(dispatch) = train.dispatch(now())? else {
                    break;
                };
                save(&config, &train)?; // Receipt exists before any worker starts.
                let backend = Arc::clone(&backend);
                let tx = completed_tx.clone();
                let siblings: Graph = train
                    .nodes
                    .iter()
                    .filter(|(goal, _)| goal.derivation == dispatch.goal.derivation)
                    .map(|(goal, node)| (goal.clone(), node.definition.clone()))
                    .collect();
                workers.push(thread::spawn(move || {
                    let (result, valid) =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let mut result = backend.realise(&dispatch);
                            let mut valid = BTreeSet::new();
                            for (goal, definition) in siblings {
                                match backend.valid(&goal, &definition) {
                                    Ok(true) => {
                                        valid.insert(goal);
                                    }
                                    Ok(false) => {}
                                    Err(error) => result = Err(error),
                                }
                            }
                            if result.is_ok() && !valid.contains(&dispatch.goal) {
                                result =
                                    Err("backend returned success without valid output evidence"
                                        .into());
                            }
                            (result, valid)
                        }))
                        .unwrap_or_else(|_| {
                            (Err("backend worker panicked".into()), BTreeSet::new())
                        });
                    let _ = tx.send((dispatch, result, valid));
                }));
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    for attempt in waiters.keys().cloned().collect::<Vec<_>>() {
        flush_waiters(&config, &train, &mut waiters, &mut outgoing, &attempt)?;
    }
    while !outgoing.is_empty() {
        outgoing.retain_mut(ipc::Outgoing::pending);
        thread::sleep(Duration::from_millis(10));
    }
    for worker in workers {
        let _ = worker.join();
    }
    // The lease anchor is deliberately retained.
    fs::remove_file(&config.socket).map_err(|e| e.to_string())?;
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn apply(
    config: &Config,
    train: &mut Train,
    backend: &dyn Backend,
    command: Command,
) -> Result<Reply, String> {
    // Polling must not clone/rewrite a large graph or contend with worker IO.
    match &command {
        Command::Inspect => return Ok(reply(config, train, None, None)),
        Command::Status(attempt) => {
            if !train.requests.contains_key(attempt) {
                return read_archive(config, attempt)?
                    .map(|archive| archive_reply(config, train, &archive))
                    .ok_or("unknown request".into());
            }
            return Ok(reply(
                config,
                train,
                Some(train.outcome(attempt)?),
                Some(attempt),
            ));
        }
        Command::AuthorizeActivation(attempt) => {
            train.authorize_activation(attempt)?;
            return Ok(reply(config, train, None, None));
        }
        Command::Retire(attempt) => return retire(config, train, backend, attempt),
        _ => {}
    }
    // Failed persistence never makes a new operation visible to dispatch.
    let mut next = train.clone();
    let mut outcome = None;
    let held = matches!(&command, Command::Register(_));
    let subject = match &command {
        Command::Submit(request) | Command::Register(request) => Some(request.attempt.clone()),
        Command::Status(attempt) | Command::Cancel(attempt) => Some(attempt.clone()),
        _ => None,
    };
    match command {
        Command::Submit(request) | Command::Register(request) => {
            if [&request.attempt, &request.target, &request.source]
                .iter()
                .any(|field| field.len() > wire::FIELD_LIMIT)
            {
                return Err("request identity exceeds protocol limit".into());
            }
            if !next.requests.contains_key(&request.attempt)
                && active_requests(&next)? >= config.queue_limit
            {
                return Err("coordinator request limit reached".into());
            }
            if let Some(record) = next.requests.get(&request.attempt) {
                if record.request != request {
                    return Err("attempt has a different frozen identity".into());
                }
            } else {
                if read_archive(config, &request.attempt)?.is_some() {
                    return Err(
                        "retired attempt cannot be resubmitted; use a new attempt identity".into(),
                    );
                }
                backend.validate_request(&request)?;
                next.register(request.clone(), !held)?;
                // Intake owns source/derivation roots too. Validate and journal
                // its immutable identity before retention can create any root;
                // a partial retention failure remains a known terminal request.
                save(config, &next)?;
                *train = next.clone();
                if let Err(error) = backend.retain(&request, &Graph::new()) {
                    next.preparation_failed(&request.attempt, error.clone())?;
                    save(config, &next)?;
                    *train = next;
                    return Err(error);
                }
                return Ok(reply(
                    config,
                    train,
                    Some(train.outcome(&request.attempt)?),
                    Some(&request.attempt),
                ));
            }
            outcome = Some(next.outcome(&request.attempt)?);
        }
        Command::Status(attempt) => outcome = Some(next.outcome(&attempt)?),
        Command::Admit(attempt) => next.admit(&attempt)?,
        Command::Retry(attempt) => {
            if next.outcome(&attempt)? != Outcome::Pending
                && active_requests(&next)? >= config.queue_limit
            {
                return Err("coordinator request limit reached".into());
            }
            backend.retain(
                &next.requests[&attempt].request,
                &next.retained_graph(&attempt)?,
            )?;
            next.retry(&attempt)?;
        }
        Command::Cancel(attempt) => {
            next.cancel(&attempt)?;
            outcome = Some(next.outcome(&attempt)?);
        }
        Command::Fence(attempt) => {
            next.fence(&attempt)?;
        }
        Command::ReleaseFence(token) => next.release_fence(&token)?,
        Command::AuthorizeActivation(attempt) => next.authorize_activation(&attempt)?,
        Command::Inspect => {}
        Command::Retire(_) => unreachable!("handled before mutation"),
    }
    save(config, &next)?;
    *train = next;
    Ok(reply(config, train, outcome, subject.as_deref()))
}

fn active_requests(train: &Train) -> Result<usize, String> {
    train
        .requests
        .keys()
        .map(|id| train.outcome(id))
        .collect::<Result<Vec<_>, _>>()
        .map(|outcomes| {
            outcomes
                .into_iter()
                .filter(|outcome| *outcome == Outcome::Pending)
                .count()
        })
}

fn historical_reply(config: &Config, envelope: Envelope) -> Result<Reply, String> {
    let attempt = match envelope.command {
        Command::Status(attempt) | Command::Retire(attempt) => attempt,
        _ => return Err("incompatible coordinator execution policy; historical attempts cannot be admitted, retried or activated".into()),
    };
    let archive: Archive = serde_json::from_reader(
        File::open(archive_path(config, &attempt)).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if archive.version != VERSION
        || archive.policy != envelope.policy
        || archive.record.request.attempt != attempt
        || !archive.roots_released
    {
        return Err("historical retirement evidence is incompatible or incomplete".into());
    }
    // This is retired evidence, never an attachment to the new train or fence.
    let mut reply = archive_reply(
        config,
        &Train::new(envelope.policy.clone(), config.aging_seconds),
        &archive,
    );
    reply.policy = envelope.policy;
    Ok(reply)
}

fn reply(config: &Config, train: &Train, outcome: Option<Outcome>, subject: Option<&str>) -> Reply {
    let outputs = subject
        .and_then(|id| train.requests.get(id))
        .filter(|record| record.prepared)
        .map(|record| {
            record
                .request
                .roots
                .iter()
                .filter_map(|goal| {
                    train
                        .nodes
                        .get(goal)
                        .map(|node| (goal.clone(), node.definition.output_path.clone()))
                })
                .collect()
        })
        .unwrap_or_default();
    Reply {
        version: VERSION,
        policy: config.policy.clone(),
        outcome,
        fence: train.fence.clone(),
        running: train.running(),
        error: None,
        outputs,
    }
}
