//! Pure service-surface release contract for a paired dev/prod NixOS-container
//! service stack.
//!
//! Nothing in this module performs I/O or reads a clock. A release artifact is
//! an exact realized NixOS guest system output store path. Both containers run
//! the same closure with independent root filesystems and their own runtime
//! bindings; promotion is therefore a store-path identity operation, never a
//! rebuild, copy of application state, or secret transfer.
//!
//! The trusted admission command is operator policy, not candidate
//! self-attestation. It receives enough argv to evaluate a candidate without
//! running it on the host. Health is candidate-owned but immutable and runs
//! inside the selected guest.
//!
//! Trust boundaries:
//!
//! - [`Policy`] is installed by the host module and is fully validated here
//!   only for shape. Runtime code additionally checks ownership, symlinks and
//!   filesystem placement when elevated.
//! - [`Pending`] is a write-ahead journal. Its [`Pending::state_before`] is an
//!   exact committed-state snapshot. Recovery restores that snapshot, never a
//!   guessed or partially observed state.
//! - Store-path checks are shape checks. They do not establish that a release
//!   is compatible with live data; that judgement is delegated to the admission
//!   command. Recovery and rollback restore the system closure only and never
//!   claim to roll back a database.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::{Component, Path, PathBuf};

#[cfg(feature = "service-surfaces-cli")]
pub mod cli;
#[cfg(feature = "service-surfaces-runtime")]
pub mod native;
#[cfg(feature = "service-surfaces-runtime")]
pub mod runtime;

/// Wire and persistence schema accepted by this contract.
pub const SCHEMA_VERSION: u32 = 1;

/// Conventional policy location. The CLI also accepts an explicit path.
pub const DEFAULT_POLICY_PATH: &str = "/etc/fleetix-surfaces/policy.json";

/// Release manifest embedded in every guest system output.
pub const MANIFEST_FILE: &str = "fleetix-surface.json";

/// Health command embedded in every guest system output.
pub const HEALTH_BIN: &str = "bin/fleetix-surface-health";

/// Fixed surface roles. Promotion never swaps these roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Surface {
    Dev,
    Prod,
}

impl Surface {
    pub fn as_str(self) -> &'static str {
        match self {
            Surface::Dev => "dev",
            Surface::Prod => "prod",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "dev" => Ok(Surface::Dev),
            "prod" => Ok(Surface::Prod),
            _ => Err(format!("unknown service surface {value:?}")),
        }
    }
}

impl fmt::Display for Surface {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Container names for the paired stack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Containers {
    pub dev: String,
    pub prod: String,
}

impl Containers {
    pub fn get(&self, surface: Surface) -> &str {
        match surface {
            Surface::Dev => &self.dev,
            Surface::Prod => &self.prod,
        }
    }
}

/// Explicit host tool paths. Commands are invoked with argv only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tools {
    pub systemctl: PathBuf,
    pub nixos_container: PathBuf,
    pub nix_store: PathBuf,
}

impl Tools {
    pub fn all(&self) -> [(&'static str, &Path); 3] {
        [
            ("systemctl", &self.systemctl),
            ("nixos_container", &self.nixos_container),
            ("nix_store", &self.nix_store),
        ]
    }
}

/// Installed policy, not a user-supplied release request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub version: u32,
    pub host: String,
    pub state_dir: PathBuf,
    pub lock_path: PathBuf,
    pub containers: Containers,
    /// Logical service IDs the release manifest must cover exactly.
    pub required_services: Vec<String>,
    pub tools: Tools,
    /// Root-installed, immutable operator admission executable. It evaluates
    /// candidates without executing candidate code on the host.
    pub admission_check: PathBuf,
    /// Declarative NixOS container configuration directory. Defaults keep
    /// module-generated JSON compatible.
    #[serde(default = "default_container_config_dir")]
    pub container_config_dir: PathBuf,
    /// Per-command ceiling for admission, store, systemctl and one health
    /// attempt. Defaults keep module-generated JSON compatible.
    #[serde(default = "default_command_timeout_seconds")]
    pub command_timeout_seconds: u64,
    /// Total readiness window for health retries after a restart. App databases
    /// may become ready after systemd considers the container started.
    #[serde(default = "default_health_timeout_seconds")]
    pub health_timeout_seconds: u64,
    #[serde(default = "default_health_interval_seconds")]
    pub health_interval_seconds: u64,
}

fn default_container_config_dir() -> PathBuf {
    PathBuf::from("/etc/nixos-containers")
}

fn default_command_timeout_seconds() -> u64 {
    120
}

fn default_health_timeout_seconds() -> u64 {
    120
}

fn default_health_interval_seconds() -> u64 {
    5
}

impl Policy {
    pub fn surface_container(&self, surface: Surface) -> &str {
        self.containers.get(surface)
    }

    /// Shape validation with no filesystem access.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported service-surface policy version {}",
                self.version
            ));
        }
        validate_slug("host", &self.host, 64)?;
        validate_container_name("containers.dev", &self.containers.dev)?;
        validate_container_name("containers.prod", &self.containers.prod)?;
        if self.containers.dev == self.containers.prod {
            return Err("dev and prod containers must have different names".into());
        }
        validate_absolute_path("state_dir", &self.state_dir)?;
        validate_absolute_path("lock_path", &self.lock_path)?;
        if self.lock_path.starts_with(&self.state_dir) || self.state_dir == self.lock_path {
            return Err("the host activation lock must live outside the state directory".into());
        }
        if self.required_services.is_empty() {
            return Err(
                "required_services must list the complete nonempty service inventory".into(),
            );
        }
        let mut seen = std::collections::BTreeSet::new();
        for id in &self.required_services {
            validate_service_id("required_services", id)?;
            if !seen.insert(id) {
                return Err(format!("required service {id:?} is listed twice"));
            }
        }
        for (name, path) in self.tools.all() {
            validate_absolute_path(name, path)?;
        }
        validate_store_executable_path("admission_check", &self.admission_check)?;
        validate_absolute_path("container_config_dir", &self.container_config_dir)?;
        if !(1..=3600).contains(&self.command_timeout_seconds) {
            return Err("command_timeout_seconds must be between 1 and 3600".into());
        }
        if !(1..=3600).contains(&self.health_timeout_seconds) {
            return Err("health_timeout_seconds must be between 1 and 3600".into());
        }
        if !(1..=300).contains(&self.health_interval_seconds)
            || self.health_interval_seconds > self.health_timeout_seconds
        {
            return Err(
                "health_interval_seconds must be between 1 and min(300, health_timeout_seconds)"
                    .into(),
            );
        }
        Ok(())
    }

    /// Digest over the complete policy. Journals and committed state bind this
    /// digest, so an edited policy can never resume an old operation.
    pub fn digest(&self) -> Result<String, String> {
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
    }
}

/// One surface's committed release identity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
    /// The release this surface used before its most recent successful change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
}

impl SurfaceRecord {
    pub fn artifact(&self) -> Option<&str> {
        self.artifact.as_deref()
    }
}

/// Durable committed state for both independent surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub version: u32,
    pub policy_digest: String,
    pub generation: u64,
    pub dev: SurfaceRecord,
    pub prod: SurfaceRecord,
    /// Set exactly when a committed state write records an operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_operation: Option<String>,
}

impl State {
    pub fn new(policy: &Policy) -> Result<Self, String> {
        policy.validate()?;
        Ok(Self {
            version: SCHEMA_VERSION,
            policy_digest: policy.digest()?,
            generation: 0,
            dev: SurfaceRecord::default(),
            prod: SurfaceRecord::default(),
            last_operation: None,
        })
    }

    pub fn record(&self, surface: Surface) -> &SurfaceRecord {
        match surface {
            Surface::Dev => &self.dev,
            Surface::Prod => &self.prod,
        }
    }

    pub fn record_mut(&mut self, surface: Surface) -> &mut SurfaceRecord {
        match surface {
            Surface::Dev => &mut self.dev,
            Surface::Prod => &mut self.prod,
        }
    }

    pub fn validate(&self, policy: &Policy) -> Result<(), String> {
        if self.version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported service-surface state version {}",
                self.version
            ));
        }
        if self.policy_digest != policy.digest()? {
            return Err("service-surface state was written for a different policy".into());
        }
        for (name, record) in [("dev", &self.dev), ("prod", &self.prod)] {
            if let Some(artifact) = &record.artifact {
                validate_store_path(&format!("{name}.artifact"), Path::new(artifact))?;
            }
            if let Some(previous) = &record.previous {
                validate_store_path(&format!("{name}.previous"), Path::new(previous))?;
            }
        }
        if let Some(operation) = &self.last_operation {
            validate_operation_id(operation)?;
        }
        Ok(())
    }
}

/// Operation that produced, or is producing, a pending journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum Operation {
    DeployDev {
        artifact: String,
    },
    Promote {
        expected_dev: String,
    },
    RollbackProd {
        expected_prod: String,
        target: String,
    },
}

impl Operation {
    pub fn label(&self) -> &'static str {
        match self {
            Operation::DeployDev { .. } => "deploy-dev",
            Operation::Promote { .. } => "promote",
            Operation::RollbackProd { .. } => "rollback",
        }
    }

    pub fn surface(&self) -> Surface {
        match self {
            Operation::DeployDev { .. } => Surface::Dev,
            Operation::Promote { .. } | Operation::RollbackProd { .. } => Surface::Prod,
        }
    }

    /// Artifact this operation intends to make current on its surface.
    pub fn candidate(&self) -> &str {
        match self {
            Operation::DeployDev { artifact } => artifact,
            Operation::Promote { expected_dev } => expected_dev,
            Operation::RollbackProd { target, .. } => target,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Operation::DeployDev { artifact } => {
                validate_store_path("deploy-dev artifact", Path::new(artifact))
            }
            Operation::Promote { expected_dev } => {
                validate_store_path("expected dev release", Path::new(expected_dev))
            }
            Operation::RollbackProd {
                expected_prod,
                target,
            } => {
                validate_store_path("expected prod release", Path::new(expected_prod))?;
                validate_store_path("rollback target", Path::new(target))?;
                if expected_prod == target {
                    return Err("rollback target equals the current prod release".into());
                }
                Ok(())
            }
        }
    }
}

/// Explicit journal phases. Success is never inferred from a missing file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// Journal is durable; no external action has begun.
    Prepared,
    /// Trusted admission accepted the operation.
    Admitted,
    /// Written before the first activation action. Activation may have started.
    Activating,
    /// Activation command returned success; verification has not completed.
    Activated,
    /// Health and live identity verification succeeded; commit is pending.
    Verified,
    /// Committed state is durable; this journal is waiting to be removed.
    Committed,
    /// Explicit recovery may have started restoring the previous release.
    Recovering,
    /// Recovery attempted and failed; the journal stays for an explicit retry.
    RecoveryFailed,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Prepared => "prepared",
            Phase::Admitted => "admitted",
            Phase::Activating => "activating",
            Phase::Activated => "activated",
            Phase::Verified => "verified",
            Phase::Committed => "committed",
            Phase::Recovering => "recovering",
            Phase::RecoveryFailed => "recovery-failed",
        }
    }

    pub fn activation_may_have_started(self) -> bool {
        !matches!(self, Phase::Prepared | Phase::Admitted)
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    pub phase: Phase,
    pub message: String,
    pub unix_seconds: u64,
}

/// Bounded audit evidence captured from a trusted command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub action: String,
    pub command: Vec<String>,
    pub exit_code: Option<i32>,
    pub output: String,
    pub unix_seconds: u64,
}

/// Durable write-ahead operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    pub version: u32,
    pub policy_digest: String,
    pub id: String,
    pub operation: Operation,
    /// Exact committed state at operation start. Recovery restores it.
    pub state_before: State,
    pub phase: Phase,
    /// True once explicit recovery began. Distinguishes a recovery commit from
    /// a forward commit when only the journal survives a crash.
    pub recovered: bool,
    pub created_unix_seconds: u64,
    pub updated_unix_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
}

impl Pending {
    pub fn new(
        policy: &Policy,
        state: &State,
        operation: Operation,
        unix_seconds: u64,
    ) -> Result<Self, String> {
        policy.validate()?;
        state.validate(policy)?;
        operation.validate()?;
        if unix_seconds == 0 {
            return Err("journal timestamp must be greater than zero".into());
        }
        let digest = policy.digest()?;
        let identity = serde_json::to_vec(&(&digest, state.generation, &operation, unix_seconds))
            .map_err(|error| error.to_string())?;
        let id = format!("{:x}", Sha256::digest(identity));
        Ok(Self {
            version: SCHEMA_VERSION,
            policy_digest: digest,
            id: id[..16].to_owned(),
            operation,
            state_before: state.clone(),
            phase: Phase::Prepared,
            recovered: false,
            created_unix_seconds: unix_seconds,
            updated_unix_seconds: unix_seconds,
            failure: None,
            evidence: Vec::new(),
        })
    }

    pub fn surface(&self) -> Surface {
        self.operation.surface()
    }

    pub fn candidate(&self) -> &str {
        self.operation.candidate()
    }

    /// Release that was current on this operation's surface before it started.
    pub fn previous(&self) -> Option<&str> {
        self.state_before.record(self.surface()).artifact()
    }

    pub fn validate(&self, policy: &Policy) -> Result<(), String> {
        if self.version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported service-surface journal version {}",
                self.version
            ));
        }
        if self.policy_digest != policy.digest()? {
            return Err("pending journal was written for a different policy".into());
        }
        validate_operation_id(&self.id)?;
        self.operation.validate()?;
        self.state_before.validate(policy)?;
        if self.created_unix_seconds == 0 || self.updated_unix_seconds < self.created_unix_seconds {
            return Err("journal timestamps are invalid".into());
        }
        match self.phase {
            Phase::Committed => {
                if self.failure.is_some() {
                    return Err("committed journal must not carry a failure".into());
                }
            }
            Phase::RecoveryFailed => {
                if !self.recovered || self.failure.is_none() {
                    return Err("failed recovery must carry its failure".into());
                }
            }
            Phase::Recovering => {
                if !self.recovered {
                    return Err("recovering journal must be marked recovered".into());
                }
            }
            _ => {
                if self.recovered {
                    return Err("non-recovery journal must not be marked recovered".into());
                }
            }
        }
        if let Some(failure) = &self.failure {
            if failure.phase != self.phase {
                return Err("failure phase does not match the journal phase".into());
            }
            if failure.message.is_empty() || failure.message.len() > 4096 {
                return Err("journal failure message is invalid".into());
            }
        }
        if self.evidence.len() > 16 {
            return Err("journal evidence is unbounded".into());
        }
        for evidence in &self.evidence {
            if evidence.action.is_empty()
                || evidence.action.len() > 64
                || evidence.command.len() > 16
                || evidence.command.iter().any(|arg| arg.len() > 4096)
                || evidence.output.len() > 8192
            {
                return Err("journal evidence entry is invalid".into());
            }
        }
        Ok(())
    }

    /// Exact committed state this operation writes on success.
    pub fn expected_after_commit(&self) -> Result<State, String> {
        let mut after = self.state_before.clone();
        after.generation = after
            .generation
            .checked_add(1)
            .ok_or("state generation overflow")?;
        after.last_operation = Some(self.id.clone());
        if !self.recovered {
            let candidate = self.candidate().to_owned();
            let record = after.record_mut(self.surface());
            record.previous = record.artifact.clone();
            record.artifact = Some(candidate);
        }
        Ok(after)
    }

    /// Compare durable state with this journal.
    ///
    /// `Ok(false)` means the operation is still active and the state is exactly
    /// its start snapshot. `Ok(true)` means the state proves the operation
    /// committed and only journal removal remains. Anything else is a refusal,
    /// never a repair guess.
    pub fn committed(&self, state: &State) -> Result<bool, String> {
        if state == &self.state_before {
            if state.last_operation.as_deref() == Some(self.id.as_str()) {
                return Err("state already names the pending operation without a commit".into());
            }
            return Ok(false);
        }
        if state == &self.expected_after_commit()? {
            return Ok(true);
        }
        Err(format!(
            "durable state does not match pending operation {} baseline",
            self.id
        ))
    }
}

/// Guest release manifest. Services must exactly cover policy.required_services.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub services: Vec<String>,
    pub units: Vec<String>,
}

impl Manifest {
    pub fn validate(&self, policy: &Policy) -> Result<(), String> {
        if self.version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported service-surface manifest version {}",
                self.version
            ));
        }
        if self.services.is_empty() {
            return Err("release manifest services must be the complete nonempty inventory".into());
        }
        if self.units.is_empty() {
            return Err(
                "release manifest units must be the complete nonempty enabled service list".into(),
            );
        }
        let mut services = std::collections::BTreeSet::new();
        for id in &self.services {
            validate_service_id("manifest.services", id)?;
            if !services.insert(id.as_str()) {
                return Err(format!("manifest service {id:?} is listed twice"));
            }
        }
        let required: std::collections::BTreeSet<_> = policy
            .required_services
            .iter()
            .map(String::as_str)
            .collect();
        if services != required {
            return Err(format!(
                "release manifest services {:?} do not exactly cover policy required_services {:?}",
                services.into_iter().collect::<Vec<_>>(),
                required.into_iter().collect::<Vec<_>>()
            ));
        }
        let mut units = std::collections::BTreeSet::new();
        for unit in &self.units {
            validate_unit_name(unit)?;
            if !units.insert(unit.as_str()) {
                return Err(format!("manifest unit {unit:?} is listed twice"));
            }
        }
        Ok(())
    }
}

pub fn validate_container_name(field: &str, value: &str) -> Result<(), String> {
    if crate::topology::ServiceSurfaces::valid_container_name(value) {
        Ok(())
    } else {
        Err(format!("invalid {field} container name {value:?}"))
    }
}

pub fn validate_service_id(field: &str, value: &str) -> Result<(), String> {
    validate_slug(field, value, 128)
}

pub fn validate_slug(field: &str, value: &str, max: usize) -> Result<(), String> {
    let mut chars = value.chars();
    if value.is_empty()
        || value.len() > max
        || !chars
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | ':' | '-' | '+'))
    {
        return Err(format!("invalid {field} {value:?}"));
    }
    Ok(())
}

pub fn validate_operation_id(value: &str) -> Result<(), String> {
    if value.len() != 16
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!("invalid operation id {value:?}"));
    }
    Ok(())
}

pub fn validate_absolute_path(field: &str, path: &Path) -> Result<(), String> {
    if !path.is_absolute() || path == Path::new("/") {
        return Err(format!("{field} must be an absolute non-root path"));
    }
    let mut normal = 0usize;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(_) => normal += 1,
            _ => {
                return Err(format!(
                    "{field} must not contain '.', '..' or a prefix: {}",
                    path.display()
                ));
            }
        }
    }
    if normal == 0 {
        return Err(format!("{field} must name a path below /"));
    }
    Ok(())
}

/// Exact realized store output. Derivations, flake references, URLs, subpaths
/// and malformed names are rejected before any command is invoked.
pub fn validate_store_path(field: &str, path: &Path) -> Result<(), String> {
    let value = path
        .to_str()
        .ok_or_else(|| format!("{field} is not valid UTF-8"))?;
    let rest = value
        .strip_prefix("/nix/store/")
        .ok_or_else(|| format!("{field} must be an absolute /nix/store path"))?;
    let (hash, name) = rest
        .split_once('-')
        .ok_or_else(|| format!("{field} must contain a store hash and name"))?;
    if hash.len() != 32
        || !hash
            .bytes()
            .all(|byte| b"0123456789abcdfghijklmnpqrsvwxyz".contains(&byte))
    {
        return Err(format!("{field} has a malformed store hash"));
    }
    if name.is_empty()
        || name.len() > 211
        || name.contains('/')
        || matches!(name, "." | "..")
        || name.ends_with(".drv")
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+-._?=".contains(&byte))
    {
        return Err(format!(
            "{field} must be one exact realized store output, not a derivation or subpath"
        ));
    }
    Ok(())
}

/// Executables may be a store file (for example `writeShellScript`) or a safe
/// subpath of a store output. Native validation additionally requires a regular
/// executable, so accepting a root here does not make directories executable.
pub fn validate_store_executable_path(field: &str, path: &Path) -> Result<(), String> {
    let value = path
        .to_str()
        .ok_or_else(|| format!("{field} is not valid UTF-8"))?;
    let rest = value
        .strip_prefix("/nix/store/")
        .ok_or_else(|| format!("{field} must be an absolute /nix/store path"))?;
    let Some((store, subpath)) = rest.split_once('/') else {
        return validate_store_path(field, path);
    };
    validate_store_path(field, Path::new(&format!("/nix/store/{store}")))?;
    if subpath.is_empty() {
        return Err(format!(
            "{field} must name an executable inside a store output"
        ));
    }
    for component in subpath.split('/') {
        if component.is_empty() || matches!(component, "." | "..") {
            return Err(format!("{field} contains an unsafe subpath component"));
        }
    }
    Ok(())
}

fn validate_unit_name(value: &str) -> Result<(), String> {
    if !value.ends_with(".service")
        || value.len() > 255
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte == b'/' || byte == 0)
    {
        return Err(format!("invalid manifest systemd unit {value:?}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_paths_are_exact_outputs() {
        let hash = "0123456789abcdfghijklmnpqrsvwxyz";
        assert!(
            validate_store_path(
                "artifact",
                Path::new(&format!("/nix/store/{hash}-guest-system"))
            )
            .is_ok()
        );
        assert!(
            validate_store_path(
                "artifact",
                Path::new(&format!("/nix/store/{hash}-guest-system.drv"))
            )
            .is_err()
        );
        assert!(
            validate_store_path(
                "artifact",
                Path::new(&format!(
                    "/nix/store/{hash}-guest-system/bin/switch-to-configuration"
                ))
            )
            .is_err()
        );
        assert!(validate_store_path("artifact", Path::new("github:caniko/fleetix")).is_err());
        assert!(validate_store_path("artifact", Path::new("/nix/store/../etc/passwd")).is_err());
    }

    #[test]
    fn admission_accepts_store_files_or_subpaths_but_release_rejects_subpaths() {
        let hash = "0123456789abcdfghijklmnpqrsvwxyz";
        let release = format!("/nix/store/{hash}-coreutils");
        let executable = format!("{release}/bin/false");
        assert!(validate_store_path("release", Path::new(&release)).is_ok());
        assert!(validate_store_path("release", Path::new(&executable)).is_err());
        assert!(validate_store_executable_path("admission_check", Path::new(&executable)).is_ok());
        assert!(validate_store_executable_path("admission_check", Path::new(&release)).is_ok());
        assert!(
            validate_store_executable_path(
                "admission_check",
                Path::new(&format!("/nix/store/{hash}-admit"))
            )
            .is_ok()
        );
        for unsafe_path in [
            format!("{release}/../escape"),
            format!("{release}/bin/../false"),
            format!("{release}//bin/false"),
        ] {
            assert!(
                validate_store_executable_path("admission_check", Path::new(&unsafe_path)).is_err()
            );
        }
    }
}
