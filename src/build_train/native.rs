//! Native Nix integration over the shared construction engine. Publication,
//! live admission checks and host activation remain caller-owned.
use super::{
    runtime::{self, Backend, Client, Config},
    *,
};
use nix_manager_core::build::frontier::{Native, Node as NativeNode, Output};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};

/// Deployment-generated client contract; contains no credentials.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub builder: String,
    pub socket: PathBuf,
    pub policy: String,
    pub preparation_dir: PathBuf,
    pub gc_roots: PathBuf,
}

impl Connection {
    /// An absent default permits standalone operation. Explicit, malformed,
    /// dangling or mismatched contracts fail closed.
    pub fn discover(
        builder: &str,
        explicit: Option<&Path>,
        default: &Path,
    ) -> Result<Option<Self>, String> {
        let path = explicit.unwrap_or(default);
        match std::fs::symlink_metadata(path) {
            Err(e) if explicit.is_none() && e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(e) => return Err(format!("inspect train connection {}: {e}", path.display())),
            Ok(_) => {}
        }
        let connection = Self::load(path)?;
        if connection.builder != builder {
            return Err("train connection does not match the selected build host".into());
        }
        Ok(Some(connection))
    }

    /// Load an explicitly selected connection; no consumer paths are assumed.
    pub fn load(path: &Path) -> Result<Self, String> {
        let config: Self =
            serde_json::from_reader(std::fs::File::open(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if config.builder.is_empty()
            || config.policy.is_empty()
            || !config.socket.is_absolute()
            || !config.preparation_dir.is_absolute()
            || !config.gc_roots.starts_with("/nix/var/nix/gcroots/")
        {
            return Err("invalid build train connection".into());
        }
        Ok(config)
    }

    pub fn client(&self) -> Client {
        Client {
            socket: self.socket.clone(),
            policy: self.policy.clone(),
        }
    }
}

/// Immutable service configuration supplied by the deployer. The admission
/// identity binds an external host policy; it is not a resource-admission engine.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    pub builder: String,
    pub admission_contract: String,
    pub coordinator: Config,
    pub native: Native,
    pub memory_max: String,
}

/// Version-2 positional JSON, mirrored by the NixOS module. Arrays remain stable
/// under serde_json/preserve_order feature unification.
pub fn policy_identity(service: &Service) -> Result<String, String> {
    let native = &service.native;
    let config = &service.coordinator;
    let value = (
        "fleetix-train-policy",
        2,
        &service.builder,
        &service.admission_contract,
        (
            &native.nix,
            &native.timeout,
            native.timeout_seconds,
            native.query_timeout_seconds,
            &native.system,
            &native.gc_roots,
            native.substitutes,
        ),
        (
            &config.socket,
            &config.state_dir,
            config.workers,
            config.planning_workers,
            config.queue_limit,
            config.aging_seconds,
            config.planning_timeout_seconds,
        ),
        &service.memory_max,
    );
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&value).map_err(|e| e.to_string())?)
    ))
}

struct NixBackend(Native);

impl NixBackend {
    fn request_native(&self, request: &Request, create: bool) -> Result<Native, String> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        let base = std::fs::symlink_metadata(&self.0.gc_roots).map_err(|e| e.to_string())?;
        if !base.is_dir() || base.mode() & 0o077 != 0 {
            return Err("train GC root base must be a private directory".into());
        }
        let mut native = self.0.clone();
        let namespace = self.0.gc_roots.join("requests");
        let digest = format!("{:x}", Sha256::digest(request.attempt.as_bytes()));
        native.gc_roots = namespace.join(digest);
        for path in [&namespace, &native.gc_roots] {
            if create {
                match std::fs::DirBuilder::new().mode(0o700).create(path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(e.to_string()),
                }
            }
            match std::fs::symlink_metadata(path) {
                Ok(meta)
                    if meta.is_dir() && meta.uid() == base.uid() && meta.mode() & 0o077 == 0 => {}
                Err(e) if !create && e.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err("unsafe request GC root namespace".into()),
            }
        }
        Ok(native)
    }
}

fn output(goal: &Goal) -> Output {
    Output {
        derivation: goal.derivation.clone(),
        name: goal.output.clone(),
    }
}

fn native_node(goal: &Goal, definition: &Definition) -> NativeNode {
    NativeNode {
        output: output(goal),
        path: definition.output_path.clone(),
        dependencies: definition.dependencies.iter().map(output).collect(),
        restore_only: definition.operation == Operation::Restore,
    }
}

/// Archive and root-release ownership covers the request's evidence closure,
/// including build-only dependencies, rather than unrelated sibling outputs.
fn request_paths(request: &Request, graph: &Graph) -> std::collections::BTreeSet<String> {
    let source = request
        .source
        .split_once('#')
        .map_or(request.source.as_str(), |(path, _)| path);
    let mut paths = std::collections::BTreeSet::from([source.to_owned()]);
    paths.extend(request.roots.iter().map(|goal| goal.derivation.clone()));
    let mut visited = std::collections::BTreeSet::new();
    let mut pending: Vec<_> = request.roots.iter().collect();
    while let Some(goal) = pending.pop() {
        if !visited.insert(goal) {
            continue;
        }
        if let Some(definition) = graph.get(goal) {
            paths.insert(goal.derivation.clone());
            paths.insert(definition.output_path.clone());
            pending.extend(&definition.dependencies);
        }
    }
    paths
}

fn validated_request_paths(
    request: &Request,
    graph: &Graph,
) -> Result<std::collections::BTreeSet<String>, String> {
    let paths = request_paths(request, graph);
    // The engine's parser also accepts store basenames in derivation JSON.
    // Root targets must already be absolute, canonical store children: never
    // pass a relative basename through to the root symlink writer.
    for path in &paths {
        let (hash, name) = path
            .strip_prefix("/nix/store/")
            .and_then(|s| s.split_once('-'))
            .ok_or("root evidence must be an exact absolute store path")?;
        if hash.len() != 32
            || !hash
                .bytes()
                .all(|c| b"0123456789abcdfghijklmnpqrsvwxyz".contains(&c))
            || name.is_empty()
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"+-._?=".contains(&c))
        {
            return Err("root evidence must be an exact absolute store path".into());
        }
    }
    for goal in request.roots.iter().chain(graph.keys()) {
        output(goal).installable().map_err(|e| e.to_string())?;
    }
    Ok(paths)
}

impl Backend for NixBackend {
    fn validate_request(&self, request: &Request) -> Result<(), String> {
        validated_request_paths(request, &Graph::new()).map(|_| ())
    }
    fn plan(&self, request: &Request) -> Result<Graph, String> {
        self.0
            .plan(&request.roots.iter().map(output).collect::<Vec<_>>())
            .map(|nodes| {
                nodes
                    .into_iter()
                    .map(|node| {
                        (
                            Goal {
                                derivation: node.output.derivation,
                                output: node.output.name,
                            },
                            Definition {
                                output_path: node.path,
                                dependencies: node
                                    .dependencies
                                    .into_iter()
                                    .map(|o| Goal {
                                        derivation: o.derivation,
                                        output: o.name,
                                    })
                                    .collect(),
                                operation: if node.restore_only {
                                    Operation::Restore
                                } else {
                                    Operation::Build
                                },
                            },
                        )
                    })
                    .collect()
            })
            .map_err(|e| e.to_string())
    }
    fn retain(&self, request: &Request, graph: &Graph) -> Result<(), String> {
        let paths = validated_request_paths(request, graph)?;
        let native = self.request_native(request, true)?;
        nix_manager_core::build::frontier::retain_paths(&native.gc_roots, &paths)
            .map_err(|e| e.to_string())
    }
    fn release(&self, request: &Request, graph: &Graph) -> Result<(), String> {
        let native = self.request_native(request, false)?;
        let owned = request_paths(request, graph);
        let entries = match std::fs::read_dir(&native.gc_roots) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.to_string()),
        };
        let mut paths = Vec::new();
        for entry in entries {
            let path = entry.map_err(|e| e.to_string())?.path();
            let target = std::fs::read_link(&path).map_err(|e| e.to_string())?;
            if !target.starts_with("/nix/store")
                || target.parent() != Some(Path::new("/nix/store"))
                || path.file_name() != target.file_name()
                || !target.to_str().is_some_and(|p| owned.contains(p))
            {
                return Err("unexpected request GC root entry; retained for inspection".into());
            }
            paths.push(path);
        }
        for path in paths {
            std::fs::remove_file(path).map_err(|e| e.to_string())?;
        }
        std::fs::remove_dir(&native.gc_roots).map_err(|e| e.to_string())?;
        std::fs::File::open(native.gc_roots.parent().ok_or("missing root namespace")?)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())
    }
    fn valid(&self, _: &Goal, definition: &Definition) -> Result<bool, String> {
        self.0
            .valid(&definition.output_path)
            .map_err(|e| e.to_string())
    }
    fn realise(&self, dispatch: &Dispatch) -> Result<(), String> {
        self.0
            .realise(&native_node(&dispatch.goal, &dispatch.definition))
            .map_err(|e| e.to_string())
    }
}

pub fn serve(service: Service, stop: Arc<AtomicBool>) -> Result<(), String> {
    validate_service(&service)?;
    runtime::serve(
        service.coordinator,
        Arc::new(NixBackend(service.native)),
        stop,
    )
}

fn validate_service(service: &Service) -> Result<(), String> {
    if service.coordinator.policy != policy_identity(service)? {
        return Err("build train policy digest does not match its backend contract".into());
    }
    if service.native.query_timeout_seconds == 0
        || service.native.query_timeout_seconds > 300
        || service.coordinator.planning_timeout_seconds
            < service
                .native
                .query_timeout_seconds
                .saturating_mul(2)
                .saturating_add(20)
    {
        return Err(
            "planning deadline must cover two bounded native queries and kill grace periods".into(),
        );
    }
    Ok(())
}

/// Caller holds the host activation lease and stops the old service first.
pub fn rollover(previous: Service, next: Service, token: &str) -> Result<PathBuf, String> {
    validate_service(&previous)?;
    validate_service(&next)?;
    if previous.builder != next.builder || previous.native.gc_roots != next.native.gc_roots {
        return Err("policy rollover cannot move builder or request-root ownership".into());
    }
    // New modules bind operator custody inside the existing opaque admission
    // string, preserving positional v2 digests for historical service files.
    let operator = |service: &Service| {
        serde_json::from_str::<(String, String, String)>(&service.admission_contract)
            .ok()
            .filter(|(marker, _, _)| marker == "fleetix-train-operator")
            .map(|(_, user, _)| user)
    };
    if let (Some(previous), Some(next)) = (operator(&previous), operator(&next)) {
        if previous != next {
            return Err("policy rollover cannot transfer operator ownership".into());
        }
    }
    runtime::rollover(
        &previous.coordinator,
        &next.coordinator,
        &NixBackend(previous.native),
        token,
    )
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
