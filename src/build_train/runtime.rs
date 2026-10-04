//! Private, builder-local coordinator. Requests contain data, never commands.
//!
//! Disconnect detaches. Cancellation removes one request's interests and lets
//! already-running Nix work finish. A graceful service stop drains workers. On
//! restart, the backend rechecks the store; Nix retains ownership of its own output
//! locks if an old daemon worker outlives the coordinator.

use super::*;
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
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

pub mod preparation;

/// Specialist backend owns graph discovery, substitution, GC roots and realization.
pub trait Backend: Send + Sync + 'static {
    fn plan(&self, request: &Request) -> Result<Graph, String>;
    fn retain(&self, request: &Request, graph: &Graph) -> Result<(), String>;
    fn valid(&self, goal: &Goal, definition: &Definition) -> Result<bool, String>;
    fn realise(&self, dispatch: &Dispatch) -> Result<(), String>;
}

/// Configuration is deployment-owned and immutable for this coordinator lifetime.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub socket: PathBuf,
    pub state_dir: PathBuf,
    pub policy: String,
    pub workers: usize,
    pub queue_limit: usize,
    pub aging_seconds: u64,
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
        let mut stream = UnixStream::connect(&self.socket)
            .map_err(|e| format!("connect {}: {e}", self.socket.display()))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(300)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .map_err(|e| e.to_string())?;
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
        stream.write_all(&raw).map_err(|e| e.to_string())?;
        let reply: Reply =
            serde_json::from_slice(&read_frame(&stream)?).map_err(|e| e.to_string())?;
        if reply.version != VERSION || reply.policy != self.policy {
            return Err("coordinator protocol or policy changed".into());
        }
        if let Some(error) = &reply.error {
            return Err(error.clone());
        }
        Ok(reply)
    }

    pub fn wait(&self, attempt: &str, stop: &AtomicBool) -> Result<Outcome, String> {
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err("detached; construction continues; cancel explicitly".into());
            }
            let outcome = self
                .call(Command::Status(attempt.into()))?
                .outcome
                .ok_or("missing outcome")?;
            if outcome != Outcome::Pending {
                return Ok(outcome);
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Close dispatch atomically before waiting. Timeout leaves the named fence
    /// intact so a disconnected activation cannot accidentally restart builders.
    pub fn drain(&self, attempt: &str, wait: Duration) -> Result<Fence, String> {
        let reply = self.call(Command::Fence(attempt.into()))?;
        let fence = reply.fence.ok_or("missing fence")?;
        let deadline = std::time::Instant::now() + wait;
        let mut running = reply.running;
        while running != 0 {
            if std::time::Instant::now() >= deadline {
                return Err(format!(
                    "drain timed out; fence {} retained for request {}",
                    fence.token, attempt
                ));
            }
            thread::sleep(Duration::from_millis(100));
            running = self.call(Command::Inspect)?.running;
        }
        Ok(fence)
    }
}

fn read_frame(stream: &UnixStream) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    BufReader::new(stream.take(MAX_FRAME))
        .read_until(b'\n', &mut raw)
        .map_err(|e| e.to_string())?;
    if raw.last() != Some(&b'\n') {
        return Err("truncated or oversized protocol frame".into());
    }
    Ok(raw)
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
    {
        return Err("invalid coordinator capacity or policy".into());
    }
    private_dir(&config.state_dir)?;
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
    let mut train: Train = match File::open(config.state_dir.join("train.json")) {
        Ok(file) => {
            serde_json::from_reader(file).map_err(|e| format!("invalid coordinator state: {e}"))?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Train::new(config.policy.clone(), config.aging_seconds)
        }
        Err(error) => return Err(error.to_string()),
    };
    if train.version != VERSION || train.policy != config.policy {
        return Err("persisted train protocol/policy mismatch; retain the old coordinator until its requests finish".into());
    }
    let mut valid = BTreeSet::new();
    for record in train.requests.values() {
        let graph = train
            .nodes
            .iter()
            .map(|(g, n)| (g.clone(), n.definition.clone()))
            .collect();
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
    let (completed_tx, completed_rx) = mpsc::channel::<(Dispatch, Result<(), String>)>();
    let mut workers = Vec::new();
    while !stop.load(Ordering::Relaxed) || train.running() != 0 {
        while let Ok((dispatch, result)) = completed_rx.try_recv() {
            train.finish(&dispatch, result);
            // One native builder may have produced multiple selected outputs.
            // Store evidence, rather than the worker's exit code, satisfies them.
            for (goal, node) in &mut train.nodes {
                if goal.derivation == dispatch.goal.derivation
                    && backend.valid(goal, &node.definition)?
                {
                    node.state = NodeState::Complete;
                }
            }
            save(&config, &train)?;
        }
        workers.retain(|worker: &thread::JoinHandle<()>| !worker.is_finished());
        if !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .map_err(|e| e.to_string())?;
                    stream
                        .set_write_timeout(Some(Duration::from_secs(5)))
                        .map_err(|e| e.to_string())?;
                    let result = same_uid(&stream)
                        .and_then(|()| read_frame(&stream))
                        .and_then(|raw| {
                            serde_json::from_slice::<Envelope>(&raw).map_err(|e| e.to_string())
                        })
                        .and_then(|envelope| {
                            if envelope.version != VERSION || envelope.policy != config.policy {
                                return Err(
                                    "incompatible coordinator protocol or execution policy".into(),
                                );
                            }
                            apply(&config, &mut train, backend.as_ref(), envelope.command)
                        });
                    let reply = match result {
                        Ok(reply) => reply,
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
                    let mut raw = serde_json::to_vec(&reply).map_err(|e| e.to_string())?;
                    raw.push(b'\n');
                    // The durable operation remains committed after a disconnect.
                    let _ = stream.write_all(&raw);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.to_string()),
            }
            while train.running() < config.workers {
                let Some(dispatch) = train.dispatch(now())? else {
                    break;
                };
                save(&config, &train)?; // Receipt exists before any worker starts.
                let backend = Arc::clone(&backend);
                let tx = completed_tx.clone();
                workers.push(thread::spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        backend.realise(&dispatch)?;
                        if !backend.valid(&dispatch.goal, &dispatch.definition)? {
                            return Err(
                                "backend returned success without valid output evidence".into()
                            );
                        }
                        Ok(())
                    }))
                    .unwrap_or_else(|_| Err("backend worker panicked".into()));
                    let _ = tx.send((dispatch, result));
                }));
            }
        }
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
            if !next.requests.contains_key(&request.attempt)
                && next.requests.len() >= config.queue_limit
            {
                return Err("coordinator request limit reached".into());
            }
            if let Some(record) = next.requests.get(&request.attempt) {
                if record.request != request {
                    return Err("attempt has a different frozen identity".into());
                }
            } else {
                backend.retain(&request, &Graph::new())?;
                let graph = backend.plan(&request)?;
                if graph.len() > 200_000 {
                    return Err("backend graph limit exceeded".into());
                }
                // Retention precedes journal commit. A failed commit may leave extra
                // roots but cannot expose unrooted work to workers or GC.
                backend.retain(&request, &graph)?;
                next.submit(request.clone(), graph, now())?;
                if held {
                    next.hold(&request.attempt)?;
                }
            }
            outcome = Some(next.outcome(&request.attempt)?);
        }
        Command::Status(attempt) => outcome = Some(next.outcome(&attempt)?),
        Command::Admit(attempt) => next.admit(&attempt)?,
        Command::Retry(attempt) => next.retry(&attempt)?,
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
    }
    save(config, &next)?;
    *train = next;
    Ok(reply(config, train, outcome, subject.as_deref()))
}

fn reply(config: &Config, train: &Train, outcome: Option<Outcome>, subject: Option<&str>) -> Reply {
    let outputs = subject
        .and_then(|id| train.requests.get(id))
        .map(|record| {
            record
                .request
                .roots
                .iter()
                .map(|goal| {
                    (
                        goal.clone(),
                        train.nodes[goal].definition.output_path.clone(),
                    )
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
