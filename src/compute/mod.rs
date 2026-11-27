//! Pure compute availability and placement state machine. The caller supplies
//! the clock, trusted host observations, claim IDs, and durable storage.
//! Persist grants before returning them to workers; telemetry is never restored.
//! Pass the caller's authenticated principal separately from request labels.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Deserializer, Serialize};

/// Wire and persistence schema accepted by this module.
pub const SCHEMA_VERSION: u32 = 1;
/// Maximum number of live tickets unless the caller overrides it.
pub const DEFAULT_MAX_WAITERS: usize = 64;
const DEFAULT_CLAIM_TTL: u64 = 90;

/// Admission policy assigned to a host by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HostRole {
    /// Shared host whose available budget includes locally admitted work.
    Primary,
    /// Attached worker requiring AC power and a complete, empty workload inventory.
    Adjacent,
}

/// Whether telemetry and existing claims permit new placements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HostAvailability {
    /// Available in principle; each request still has to fit.
    Free,
    /// Workloads, claims, or occupied slots prevent placement.
    Busy,
    /// An adjacent host is not confirmed attached and on AC power.
    Detached,
    /// Memory headroom, admission capacity, or pressure limits prevent placement.
    Saturated,
    /// Telemetry is missing, stale, or incomplete.
    Unknown,
}

/// Kind of work associated with a placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClaimKind {
    /// Adjacent run; on an adjacent host this reserves the reported project ceiling.
    AdjacentRun,
    /// Build on the selected host.
    LocalBuild,
    /// Offloaded Nix build.
    NixOffload,
    /// Work without kind-specific capacity rules.
    Generic,
}

/// Whether a claim can still be used or is waiting for reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClaimPhase {
    /// The claim may be renewed before expiry.
    Active,
    /// The claim is fenced but can still block placement.
    Reconciling,
}

/// Launch state reported by the worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClaimStage {
    /// Capacity has been reserved but the worker has not reported a launch.
    PendingStart,
    /// The caller has started the work; newer AMC samples account for it.
    Running,
}

/// A workload found in host telemetry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservedWorkload {
    pub kind: String,
    pub name: String,
    pub project: Option<String>,
    pub memory_current_bytes: Option<u64>,
}

/// Resource ceilings reported for a project on a host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCapability {
    pub memory_max_bytes: u64,
    pub swap_max_bytes: u64,
}

/// Caller-authenticated observation of a host's capacity and workload inventory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostTelemetrySample {
    #[serde(deserialize_with = "deserialize_version")]
    pub schema_version: u32,
    pub host: String,
    pub role: HostRole,
    pub boot_id: String,
    pub sequence: u64,
    pub observed_at: DateTime<Utc>,
    pub ttl_secs: u64,
    pub attached: Option<bool>,
    pub ac_power: Option<bool>,
    pub mem_total_bytes: Option<u64>,
    pub mem_available_bytes: Option<u64>,
    pub reserve_bytes: Option<u64>,
    pub committed_bytes: Option<u64>,
    /// AMC headroom; absence is not an observation of zero headroom.
    pub usable_headroom_bytes: Option<u64>,
    pub usable_budget_bytes: Option<u64>,
    #[serde(default)]
    pub contract_memory_max_bytes: Option<u64>,
    pub amc_source: Option<String>,
    pub amc_admissible: Option<bool>,
    pub memory_full_avg10: Option<f64>,
    pub max_memory_full_psi: Option<f64>,
    pub local_slots: Option<u32>,
    pub occupied_slots: Option<u32>,
    pub fence_generation: Option<u64>,
    /// Workloads observed on this boot; absence is only authoritative if complete.
    pub active_workloads: Vec<ObservedWorkload>,
    #[serde(default)]
    pub workload_inventory_complete: bool,
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectCapability>,
    pub probed_by: Option<String>,
}

/// A durable reservation granted to an authenticated principal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputeClaim {
    #[serde(deserialize_with = "deserialize_version")]
    pub schema_version: u32,
    pub id: String,
    pub idempotency_key: String,
    pub generation: u64,
    pub host: String,
    pub host_boot_id: String,
    pub project: Option<String>,
    /// Display label, not an authorization credential.
    pub holder: String,
    /// Authenticated principal required to renew or release this claim.
    #[serde(deserialize_with = "deserialize_owner")]
    pub owner_principal: String,
    pub kind: ClaimKind,
    pub phase: ClaimPhase,
    pub stage: ClaimStage,
    pub exclusive: bool,
    pub reserved_bytes: u64,
    pub unit: Option<String>,
    pub claimed_at: DateTime<Utc>,
    pub heartbeat_at: DateTime<Utc>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    pub reconciling_since: Option<DateTime<Utc>>,
}

/// Requested capacity, candidate order, and claim options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacementRequest {
    #[serde(deserialize_with = "deserialize_version")]
    pub schema_version: u32,
    pub idempotency_key: String,
    pub holder: String,
    pub project: Option<String>,
    pub kind: ClaimKind,
    /// Candidate hosts in caller-defined order. Empty means no candidates.
    #[serde(default)]
    pub prefer: Vec<String>,
    /// Optional filter on the candidates in [`Self::prefer`].
    pub allow_hosts: Option<Vec<String>>,
    /// Memory requirement for hosts without an override.
    pub required_bytes: u64,
    /// Host-specific memory requirements that replace the default.
    #[serde(default)]
    pub host_required_bytes: BTreeMap<String, u64>,
    /// Defaults to exclusive on adjacent hosts.
    pub exclusive: Option<bool>,
    /// Requested lease duration; grants clamp it to 15–300 seconds.
    #[serde(default = "default_claim_ttl")]
    pub ttl_secs: u64,
}

impl Default for PlacementRequest {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            idempotency_key: String::new(),
            holder: String::new(),
            project: None,
            kind: ClaimKind::Generic,
            prefer: Vec::new(),
            allow_hosts: None,
            required_bytes: 0,
            host_required_bytes: BTreeMap::new(),
            exclusive: None,
            ttl_secs: DEFAULT_CLAIM_TTL,
        }
    }
}

fn default_claim_ttl() -> u64 {
    DEFAULT_CLAIM_TTL
}

/// A renewal and optional launch update for an existing claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatRequest {
    /// Display label; the caller supplies the authenticating principal separately.
    pub holder: String,
    /// Requested lease duration; omitted to reuse the previous duration.
    pub ttl_secs: Option<u64>,
    /// Work may transition to running, but never back to pending.
    pub stage: Option<ClaimStage>,
    /// Workload unit name, immutable once recorded.
    pub unit: Option<String>,
}

/// Evidence supplied by the owner when giving up a reservation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseRequest {
    /// Display label; the caller supplies the authenticating principal separately.
    pub holder: String,
    /// The worker confirms that a running claim's unit has stopped.
    pub unit_confirmed_stopped: bool,
    /// Explicit confirmation that an unregistered pending claim never launched.
    #[serde(default)]
    pub never_started: bool,
}

/// A queued request bound to its authenticated owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitTicket {
    pub idempotency_key: String,
    #[serde(deserialize_with = "deserialize_owner")]
    pub owner_principal: String,
    pub request: PlacementRequest,
    pub enqueued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// A host's admission state, reported capacity, and reservations at snapshot time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostAvailabilityView {
    pub host: String,
    pub role: HostRole,
    pub state: HostAvailability,
    pub stale: bool,
    pub boot_id: Option<String>,
    pub observed_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub attached: Option<bool>,
    pub ac_power: Option<bool>,
    pub usable_headroom_bytes: Option<u64>,
    /// Includes running claims not yet reflected in primary host telemetry.
    pub pending_start_reserved_bytes: u64,
    /// Headroom after reservation debits and, on primary hosts, the budget cap.
    pub remaining_headroom_bytes: Option<u64>,
    pub usable_budget_bytes: Option<u64>,
    #[serde(default)]
    pub contract_memory_max_bytes: Option<u64>,
    pub memory_full_avg10: Option<f64>,
    pub max_memory_full_psi: Option<f64>,
    pub local_slots: Option<u32>,
    pub occupied_slots: Option<u32>,
    /// Includes reconciling claims, which may still hold the host.
    pub active_claims: Vec<ComputeClaim>,
    pub observed_workloads: Vec<ObservedWorkload>,
    pub projects: BTreeMap<String, ProjectCapability>,
    pub reasons: Vec<String>,
}

/// A fleet-wide view for a single tracker epoch, revision, and timestamp.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputeAvailabilitySnapshot {
    #[serde(deserialize_with = "deserialize_version")]
    pub schema_version: u32,
    pub epoch: String,
    pub revision: u64,
    pub generated_at: DateTime<Utc>,
    pub hosts: BTreeMap<String, HostAvailabilityView>,
    /// First free host in the explicit recommendation order, if supplied.
    pub recommended_host: Option<String>,
    pub waiter_count: usize,
}

/// Replay protection carried across tracker restarts for one host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostWatermark {
    pub boot_id: String,
    pub last_sequence: u64,
    /// Most recent observation timestamp seen for this boot.
    pub last_observed_at: DateTime<Utc>,
    #[serde(default)]
    pub retired_boots: BTreeSet<String>,
}

/// Durable tracker state; live telemetry and wait tickets are not persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedState {
    #[serde(deserialize_with = "deserialize_version")]
    pub schema_version: u32,
    pub next_generation: u64,
    pub claims: Vec<ComputeClaim>,
    #[serde(default)]
    pub host_watermarks: BTreeMap<String, HostWatermark>,
}

/// Result of a valid placement attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::large_enum_variant,
    reason = "return the owned claim without a second allocation"
)]
pub enum ClaimOutcome {
    /// A newly granted or idempotently returned reservation.
    Granted(ComputeClaim),
    /// No candidate currently fits or can be used ahead of earlier tickets.
    Unavailable,
}

/// Result of an authorized release request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReleaseOutcome {
    /// The claim no longer reserves the host.
    Released,
    /// The claim is fenced until the caller or fresh telemetry confirms it is safe to remove.
    Reconciling,
}

/// Invalid input or a state transition the tracker cannot accept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputeError {
    UnsupportedSchemaVersion(u32),
    UnknownHost(String),
    InvalidSample(&'static str),
    StaleSequence,
    StaleBoot,
    InvalidRequest(&'static str),
    InvalidPersistedState(&'static str),
    DuplicateClaimId,
    RequestMismatch,
    GenerationExhausted,
    UnknownClaim,
    HolderMismatch,
    ClaimReconciling,
    ClaimFenced,
    QueueFull,
}

impl fmt::Display for ComputeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::UnsupportedSchemaVersion(version) => {
                return write!(f, "unsupported compute schema version {version}");
            }
            Self::UnknownHost(host) => return write!(f, "unknown compute host {host}"),
            Self::InvalidSample(reason) => return write!(f, "invalid telemetry: {reason}"),
            Self::InvalidRequest(reason) => return write!(f, "invalid request: {reason}"),
            Self::InvalidPersistedState(reason) => {
                return write!(f, "invalid persisted compute state: {reason}");
            }
            Self::StaleSequence => "telemetry sequence did not increase",
            Self::StaleBoot => "telemetry is from a retired boot",
            Self::DuplicateClaimId => "claim ID is already in use",
            Self::RequestMismatch => "wait ticket belongs to a different principal or request",
            Self::GenerationExhausted => "claim generation counter is exhausted",
            Self::UnknownClaim => "unknown compute claim",
            Self::HolderMismatch => "claim owner principal does not match",
            Self::ClaimReconciling => "the previous claim is still reconciling",
            Self::ClaimFenced => "the claim is fenced and cannot be renewed",
            Self::QueueFull => "compute wait queue is full",
        };
        f.write_str(message)
    }
}

impl std::error::Error for ComputeError {}

fn validate_version(version: u32) -> Result<(), ComputeError> {
    if version != SCHEMA_VERSION {
        return Err(ComputeError::UnsupportedSchemaVersion(version));
    }
    Ok(())
}

fn deserialize_version<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let version = u32::deserialize(deserializer)?;
    validate_version(version).map_err(serde::de::Error::custom)?;
    Ok(version)
}

fn validate_owner(owner_principal: &str) -> Result<(), ComputeError> {
    if owner_principal.trim().is_empty() {
        return Err(ComputeError::InvalidRequest(
            "owner principal must be non-empty",
        ));
    }
    Ok(())
}

fn deserialize_owner<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let owner = String::deserialize(deserializer)?;
    validate_owner(&owner).map_err(serde::de::Error::custom)?;
    Ok(owner)
}

impl WaitTicket {
    fn check_request(
        &self,
        req: &PlacementRequest,
        owner_principal: &str,
    ) -> Result<(), ComputeError> {
        if self.owner_principal != owner_principal || self.request != *req {
            return Err(ComputeError::RequestMismatch);
        }
        Ok(())
    }
}

impl PlacementRequest {
    /// The host-specific memory requirement, or the request's default.
    pub fn required_bytes_for(&self, host: &str) -> u64 {
        self.host_required_bytes
            .get(host)
            .copied()
            .unwrap_or(self.required_bytes)
    }

    /// Reject an unsupported schema or missing request identity.
    pub fn validate(&self) -> Result<(), ComputeError> {
        validate_version(self.schema_version)?;
        if self.idempotency_key.trim().is_empty() || self.holder.trim().is_empty() {
            return Err(ComputeError::InvalidRequest(
                "idempotency key and holder must be non-empty",
            ));
        }
        Ok(())
    }
}

impl ComputeAvailabilitySnapshot {
    /// Check the schema version and the owner and version of every claim.
    pub fn validate(&self) -> Result<(), ComputeError> {
        validate_version(self.schema_version)?;
        for claim in self.hosts.values().flat_map(|host| &host.active_claims) {
            validate_version(claim.schema_version)?;
            validate_owner(&claim.owner_principal)?;
        }
        Ok(())
    }

    /// First preferred host that fits at snapshot time, without FIFO or idempotency.
    pub fn eligible_host_for(&self, req: &PlacementRequest) -> Option<String> {
        req.validate().ok()?;
        req.prefer.iter().find_map(|host| {
            let view = self.hosts.get(host)?;
            PlacementTarget {
                host,
                role: view.role,
                state: view.state,
                remaining: view.remaining_headroom_bytes,
                contract_memory_max_bytes: view.contract_memory_max_bytes,
                projects: &view.projects,
                has_claims: !view.active_claims.is_empty(),
            }
            .fit(req)?;
            Some(host.clone())
        })
    }
}

impl HostTelemetrySample {
    /// Check schema, boot identity, TTL, pressure readings, and admission source.
    pub fn validate(&self) -> Result<(), ComputeError> {
        validate_version(self.schema_version)?;
        if self.boot_id.trim().is_empty() {
            return Err(ComputeError::InvalidSample("missing boot ID"));
        }
        if !(1..=300).contains(&self.ttl_secs) {
            return Err(ComputeError::InvalidSample(
                "TTL must be in 1..=300 seconds",
            ));
        }
        if [self.memory_full_avg10, self.max_memory_full_psi]
            .into_iter()
            .flatten()
            .any(|psi| !psi.is_finite() || psi < 0.0)
        {
            return Err(ComputeError::InvalidSample(
                "PSI must be finite and non-negative",
            ));
        }
        if !matches!(
            self.amc_source.as_deref(),
            None | Some("host" | "user" | "none")
        ) {
            return Err(ComputeError::InvalidSample("unknown AMC source"));
        }
        Ok(())
    }
}

fn claim_expiry(now: DateTime<Utc>, ttl_secs: u64) -> Result<DateTime<Utc>, ComputeError> {
    now.checked_add_signed(TimeDelta::seconds(ttl_secs.clamp(15, 300) as i64))
        .ok_or(ComputeError::InvalidRequest(
            "claim expiration is out of range",
        ))
}

impl ComputeClaim {
    fn active_at(&self, now: DateTime<Utc>) -> bool {
        self.phase == ClaimPhase::Active && self.expires_at > now
    }

    fn reconcile(&mut self, now: DateTime<Utc>) -> bool {
        if self.phase == ClaimPhase::Reconciling {
            return false;
        }
        self.phase = ClaimPhase::Reconciling;
        self.reconciling_since = Some(now);
        true
    }
}

#[derive(Clone)]
struct Observation {
    sample: HostTelemetrySample,
    expires_at: DateTime<Utc>,
    stale: bool,
}

impl Observation {
    fn stale_at(&self, now: DateTime<Utc>) -> bool {
        self.stale || self.expires_at <= now || self.sample.observed_at > now
    }
}

/// One caller-owned tracker. Use a new epoch when restarting from durable state.
#[derive(Clone)]
pub struct ComputeTracker {
    allowed_hosts: BTreeMap<String, HostRole>,
    epoch: String,
    revision: u64,
    max_waiters: usize,
    next_generation: u64,
    samples: BTreeMap<String, Observation>,
    host_watermarks: BTreeMap<String, HostWatermark>,
    claims: BTreeMap<String, ComputeClaim>,
    waiters: VecDeque<WaitTicket>,
}

impl ComputeTracker {
    /// Start an empty tracker with the default wait queue limit.
    pub fn new(allowed_hosts: BTreeMap<String, HostRole>, epoch: impl Into<String>) -> Self {
        Self::with_max_waiters(allowed_hosts, epoch, DEFAULT_MAX_WAITERS)
    }

    /// Start an empty tracker with a caller-defined wait queue limit.
    pub fn with_max_waiters(
        allowed_hosts: BTreeMap<String, HostRole>,
        epoch: impl Into<String>,
        max_waiters: usize,
    ) -> Self {
        Self {
            allowed_hosts,
            epoch: epoch.into(),
            revision: 0,
            max_waiters,
            next_generation: 1,
            samples: BTreeMap::new(),
            host_watermarks: BTreeMap::new(),
            claims: BTreeMap::new(),
            waiters: VecDeque::new(),
        }
    }

    /// Restore durable state into a new epoch with the default wait queue limit.
    pub fn from_persisted(
        allowed_hosts: BTreeMap<String, HostRole>,
        epoch: impl Into<String>,
        persisted: PersistedState,
        now: DateTime<Utc>,
    ) -> Result<Self, ComputeError> {
        Self::from_persisted_with_max_waiters(
            allowed_hosts,
            epoch,
            persisted,
            DEFAULT_MAX_WAITERS,
            now,
        )
    }

    /// Restore durable state with a custom wait queue limit, fencing expired claims.
    pub fn from_persisted_with_max_waiters(
        allowed_hosts: BTreeMap<String, HostRole>,
        epoch: impl Into<String>,
        persisted: PersistedState,
        max_waiters: usize,
        now: DateTime<Utc>,
    ) -> Result<Self, ComputeError> {
        validate_version(persisted.schema_version)?;
        let mut tracker = Self::with_max_waiters(allowed_hosts, epoch, max_waiters);
        let mut keys = BTreeSet::new();
        let mut generations = BTreeSet::new();
        for mut claim in persisted.claims {
            validate_version(claim.schema_version)?;
            if !tracker.allowed_hosts.contains_key(&claim.host) {
                return Err(ComputeError::UnknownHost(claim.host));
            }
            if claim.id.trim().is_empty()
                || claim.idempotency_key.trim().is_empty()
                || claim.holder.trim().is_empty()
                || claim.owner_principal.trim().is_empty()
                || claim.host_boot_id.trim().is_empty()
            {
                return Err(ComputeError::InvalidPersistedState(
                    "missing claim identity",
                ));
            }
            if claim.generation >= persisted.next_generation
                || !generations.insert(claim.generation)
            {
                return Err(ComputeError::InvalidPersistedState(
                    "invalid claim generation",
                ));
            }
            if !keys.insert(claim.idempotency_key.clone()) {
                return Err(ComputeError::InvalidPersistedState(
                    "duplicate idempotency key",
                ));
            }
            if claim.expires_at <= now {
                claim.reconcile(now);
            }
            if tracker.claims.insert(claim.id.clone(), claim).is_some() {
                return Err(ComputeError::DuplicateClaimId);
            }
        }
        tracker.next_generation = persisted.next_generation;
        tracker.host_watermarks = persisted.host_watermarks;
        Ok(tracker)
    }

    /// The caller-defined lifetime identifier for this tracker.
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// The current revision within this epoch.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Export the claims, generation counter, and replay protection for persistence.
    pub fn persisted_state(&self) -> PersistedState {
        PersistedState {
            schema_version: SCHEMA_VERSION,
            next_generation: self.next_generation,
            claims: self.claims.values().cloned().collect(),
            host_watermarks: self.host_watermarks.clone(),
        }
    }

    /// Record a valid sample and reconcile only on fresh evidence.
    pub fn record_sample(
        &mut self,
        sample: HostTelemetrySample,
        now: DateTime<Utc>,
    ) -> Result<bool, ComputeError> {
        sample.validate()?;
        match self.allowed_hosts.get(&sample.host) {
            None => return Err(ComputeError::UnknownHost(sample.host)),
            Some(role) if *role != sample.role => {
                return Err(ComputeError::InvalidSample(
                    "role does not match configured host",
                ));
            }
            _ => {}
        }
        let expires_at = sample
            .observed_at
            .checked_add_signed(TimeDelta::seconds(sample.ttl_secs as i64))
            .ok_or(ComputeError::InvalidSample(
                "sample expiration is out of range",
            ))?;
        if sample.observed_at > now {
            return Err(ComputeError::InvalidSample("observation is in the future"));
        }
        let stale = expires_at <= now;
        let previous = self.host_watermarks.get(&sample.host);
        if let Some(previous) = previous {
            if previous.boot_id == sample.boot_id {
                if sample.sequence <= previous.last_sequence {
                    return Err(ComputeError::StaleSequence);
                }
            } else if previous.retired_boots.contains(&sample.boot_id)
                || stale
                || sample.observed_at < previous.last_observed_at
            {
                return Err(ComputeError::StaleBoot);
            }
        }

        self.advance(now);
        if !stale {
            let previous_boot = self
                .host_watermarks
                .get(&sample.host)
                .map(|watermark| watermark.boot_id.as_str());
            self.claims.retain(|_, claim| {
                if sample.host != claim.host {
                    return true;
                }
                if sample.boot_id != claim.host_boot_id {
                    // Without a verified boot transition, a pre-grant observation
                    // could be from an even older boot, especially after restore.
                    return previous_boot != Some(claim.host_boot_id.as_str())
                        && sample.observed_at <= claim.claimed_at;
                }
                claim.phase != ClaimPhase::Reconciling
                    || !sample.workload_inventory_complete
                    || sample.observed_at <= claim.reconciling_since.unwrap_or(claim.expires_at)
                    || match &claim.unit {
                        Some(unit) => sample
                            .active_workloads
                            .iter()
                            .any(|work| work.name == *unit),
                        None => !sample.active_workloads.is_empty(),
                    }
            });
        }
        let watermark = self
            .host_watermarks
            .entry(sample.host.clone())
            .or_insert_with(|| HostWatermark {
                boot_id: sample.boot_id.clone(),
                last_sequence: sample.sequence,
                last_observed_at: sample.observed_at,
                retired_boots: BTreeSet::new(),
            });
        if watermark.boot_id != sample.boot_id {
            watermark.retired_boots.insert(std::mem::replace(
                &mut watermark.boot_id,
                sample.boot_id.clone(),
            ));
        }
        watermark.last_sequence = sample.sequence;
        watermark.last_observed_at = watermark.last_observed_at.max(sample.observed_at);
        self.samples.insert(
            sample.host.clone(),
            Observation {
                sample,
                expires_at,
                stale,
            },
        );
        self.revision += 1;
        Ok(true)
    }

    /// Apply timer transitions once; expired claims keep blocking until reconciled.
    pub fn advance(&mut self, now: DateTime<Utc>) -> bool {
        let mut changed = false;
        for claim in self.claims.values_mut() {
            if claim.expires_at <= now {
                changed |= claim.reconcile(now);
            }
        }
        let waiter_count = self.waiters.len();
        self.waiters.retain(|ticket| ticket.expires_at > now);
        changed |= waiter_count != self.waiters.len();
        for observation in self.samples.values_mut() {
            if !observation.stale && observation.expires_at <= now {
                observation.stale = true;
                changed = true;
            }
        }
        if changed {
            self.revision += 1;
        }
        changed
    }

    /// Earliest pending telemetry, claim, or waiter expiration.
    pub fn next_deadline(&self) -> Option<DateTime<Utc>> {
        self.samples
            .values()
            .filter(|observation| !observation.stale)
            .map(|observation| observation.expires_at)
            .chain(
                self.claims
                    .values()
                    .filter(|claim| claim.phase == ClaimPhase::Active)
                    .map(|claim| claim.expires_at),
            )
            .chain(self.waiters.iter().map(|ticket| ticket.expires_at))
            .min()
    }

    /// Reuse the owner's active claim or try to reserve a candidate without jumping the queue.
    pub fn try_claim(
        &mut self,
        req: PlacementRequest,
        new_claim_id: impl Into<String>,
        owner_principal: &str,
        now: DateTime<Utc>,
    ) -> Result<ClaimOutcome, ComputeError> {
        self.advance(now);
        req.validate()?;
        validate_owner(owner_principal)?;
        if let Some(claim) = self.existing_claim(&req, owner_principal, now)? {
            return Ok(ClaimOutcome::Granted(claim.clone()));
        }
        self.check_waiter_request(&req, owner_principal, now)?;
        let Some((sample, required, exclusive)) = self.placement(&req, now) else {
            return Ok(ClaimOutcome::Unavailable);
        };
        let id = new_claim_id.into();
        if id.trim().is_empty() {
            return Err(ComputeError::InvalidRequest("claim ID must be non-empty"));
        }
        if self.claims.contains_key(&id) {
            return Err(ComputeError::DuplicateClaimId);
        }
        let next_generation = self
            .next_generation
            .checked_add(1)
            .ok_or(ComputeError::GenerationExhausted)?;
        let claim = ComputeClaim {
            schema_version: SCHEMA_VERSION,
            id,
            idempotency_key: req.idempotency_key,
            generation: self.next_generation,
            host: sample.host.clone(),
            host_boot_id: sample.boot_id.clone(),
            project: req.project,
            holder: req.holder,
            owner_principal: owner_principal.into(),
            kind: req.kind,
            phase: ClaimPhase::Active,
            stage: ClaimStage::PendingStart,
            exclusive,
            reserved_bytes: required,
            unit: None,
            claimed_at: now,
            heartbeat_at: now,
            started_at: None,
            expires_at: claim_expiry(now, req.ttl_secs)?,
            reconciling_since: None,
        };
        self.next_generation = next_generation;
        self.waiters
            .retain(|ticket| ticket.idempotency_key != claim.idempotency_key);
        self.claims.insert(claim.id.clone(), claim.clone());
        self.revision += 1;
        Ok(ClaimOutcome::Granted(claim))
    }

    /// Renewing a ticket changes its deadline, not its position or request.
    pub fn enqueue_waiter(
        &mut self,
        req: PlacementRequest,
        owner_principal: &str,
        wait_until: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), ComputeError> {
        self.advance(now);
        req.validate()?;
        validate_owner(owner_principal)?;
        if self.existing_claim(&req, owner_principal, now)?.is_some() {
            return Ok(());
        }
        let ticket = self
            .waiters
            .iter_mut()
            .find(|ticket| ticket.idempotency_key == req.idempotency_key);
        if let Some(ticket) = &ticket {
            ticket.check_request(&req, owner_principal)?;
        }
        if wait_until <= now {
            return Err(ComputeError::InvalidRequest(
                "wait deadline must be in the future",
            ));
        }
        if let Some(ticket) = ticket {
            if ticket.expires_at == wait_until {
                return Ok(());
            }
            ticket.expires_at = wait_until;
        } else {
            if self.waiters.len() >= self.max_waiters {
                return Err(ComputeError::QueueFull);
            }
            self.waiters.push_back(WaitTicket {
                idempotency_key: req.idempotency_key.clone(),
                owner_principal: owner_principal.into(),
                request: req,
                enqueued_at: now,
                expires_at: wait_until,
            });
        }
        self.revision += 1;
        Ok(())
    }

    /// Remove a live ticket only if its key and authenticated principal match.
    pub fn remove_waiter(
        &mut self,
        idempotency_key: &str,
        owner_principal: &str,
        now: DateTime<Utc>,
    ) -> bool {
        self.advance(now);
        let len = self.waiters.len();
        self.waiters.retain(|ticket| {
            ticket.idempotency_key != idempotency_key || ticket.owner_principal != owner_principal
        });
        if self.waiters.len() == len {
            return false;
        }
        self.revision += 1;
        true
    }

    /// First live waiter able to proceed; a blocked ticket doesn't idle unrelated hosts.
    pub fn head_ready_waiter(&self, now: DateTime<Utc>) -> Option<&WaitTicket> {
        self.waiters
            .iter()
            .filter(|ticket| ticket.expires_at > now)
            .find(|ticket| {
                self.is_eligible_for_request(&ticket.request, &ticket.owner_principal, now)
                    .is_some()
            })
    }

    /// Renew an active claim and optionally register its launch or workload unit.
    pub fn heartbeat(
        &mut self,
        claim_id: &str,
        req: HeartbeatRequest,
        owner_principal: &str,
        now: DateTime<Utc>,
    ) -> Result<ComputeClaim, ComputeError> {
        self.advance(now);
        let claim = self.claim_for_owner(claim_id, owner_principal)?;
        if claim.phase == ClaimPhase::Reconciling {
            return Err(ComputeError::ClaimFenced);
        }
        if now < claim.heartbeat_at {
            return Err(ComputeError::InvalidRequest(
                "heartbeat moved backwards in time",
            ));
        }
        if claim.stage == ClaimStage::Running && req.stage == Some(ClaimStage::PendingStart) {
            return Err(ComputeError::InvalidRequest(
                "claim stage cannot move backwards",
            ));
        }
        if req.unit.as_ref().is_some_and(|unit| {
            unit.trim().is_empty() || claim.unit.as_ref().is_some_and(|current| current != unit)
        }) {
            return Err(ComputeError::InvalidRequest(
                "claim unit must be non-empty and immutable",
            ));
        }
        let ttl = req
            .ttl_secs
            .unwrap_or_else(|| (claim.expires_at - claim.heartbeat_at).num_seconds().max(0) as u64);
        claim.expires_at = claim_expiry(now, ttl)?;
        claim.heartbeat_at = now;
        if let Some(stage) = req.stage {
            if claim.stage == ClaimStage::PendingStart && stage == ClaimStage::Running {
                claim.started_at = Some(now);
            }
            claim.stage = stage;
        }
        if req.unit.is_some() {
            claim.unit = req.unit;
        }
        let claim = claim.clone();
        self.revision += 1;
        Ok(claim)
    }

    /// Remove a claim with sufficient stop evidence; otherwise fence it for reconciliation.
    pub fn release(
        &mut self,
        claim_id: &str,
        req: ReleaseRequest,
        owner_principal: &str,
        now: DateTime<Utc>,
    ) -> Result<ReleaseOutcome, ComputeError> {
        self.advance(now);
        let claim = self.claim_for_owner(claim_id, owner_principal)?;
        let removable = match claim.stage {
            ClaimStage::PendingStart => req.never_started && claim.unit.is_none(),
            ClaimStage::Running => req.unit_confirmed_stopped,
        };
        if removable {
            self.claims.remove(claim_id);
            self.revision += 1;
            return Ok(ReleaseOutcome::Released);
        }
        if claim.reconcile(now) {
            self.revision += 1;
        }
        Ok(ReleaseOutcome::Reconciling)
    }

    /// Read-only counterpart to [`Self::try_claim`], including idempotency and FIFO.
    pub fn is_eligible_for_request(
        &self,
        req: &PlacementRequest,
        owner_principal: &str,
        now: DateTime<Utc>,
    ) -> Option<String> {
        req.validate().ok()?;
        validate_owner(owner_principal).ok()?;
        if let Some(claim) = self.existing_claim(req, owner_principal, now).ok()? {
            return Some(claim.host.clone());
        }
        self.check_waiter_request(req, owner_principal, now).ok()?;
        self.placement(req, now)
            .map(|(sample, _, _)| sample.host.clone())
    }

    /// View availability without recommending a host.
    /// Call [`Self::advance`] first to publish elapsed timer transitions with a new revision.
    pub fn snapshot(&self, now: DateTime<Utc>) -> ComputeAvailabilitySnapshot {
        self.snapshot_with_preference(now, &[])
    }

    /// View availability and recommend the first [`HostAvailability::Free`] host in `preferred`.
    ///
    /// The recommendation does not evaluate a specific request or the wait queue.
    /// Call [`Self::advance`] first to publish elapsed timer transitions with a new revision.
    pub fn snapshot_with_preference(
        &self,
        now: DateTime<Utc>,
        preferred: &[&str],
    ) -> ComputeAvailabilitySnapshot {
        let hosts: BTreeMap<_, _> = self
            .allowed_hosts
            .iter()
            .map(|(host, &role)| (host.clone(), self.host_view(host, role, now)))
            .collect();
        let recommended_host = preferred
            .iter()
            .copied()
            .find(|host| {
                hosts
                    .get(*host)
                    .is_some_and(|view| view.state == HostAvailability::Free)
            })
            .map(str::to_owned);
        ComputeAvailabilitySnapshot {
            schema_version: SCHEMA_VERSION,
            epoch: self.epoch.clone(),
            revision: self.revision,
            generated_at: now,
            hosts,
            recommended_host,
            waiter_count: self
                .waiters
                .iter()
                .filter(|ticket| ticket.expires_at > now)
                .count(),
        }
    }

    fn claim_for_owner(
        &mut self,
        id: &str,
        owner_principal: &str,
    ) -> Result<&mut ComputeClaim, ComputeError> {
        let claim = self.claims.get_mut(id).ok_or(ComputeError::UnknownClaim)?;
        if claim.owner_principal != owner_principal {
            return Err(ComputeError::HolderMismatch);
        }
        Ok(claim)
    }

    fn existing_claim(
        &self,
        req: &PlacementRequest,
        owner_principal: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<&ComputeClaim>, ComputeError> {
        let Some(claim) = self
            .claims
            .values()
            .find(|claim| claim.idempotency_key == req.idempotency_key)
        else {
            return Ok(None);
        };
        if claim.owner_principal != owner_principal {
            return Err(ComputeError::HolderMismatch);
        }
        if !claim.active_at(now) {
            return Err(ComputeError::ClaimReconciling);
        }
        Ok(Some(claim))
    }

    fn check_waiter_request(
        &self,
        req: &PlacementRequest,
        owner_principal: &str,
        now: DateTime<Utc>,
    ) -> Result<(), ComputeError> {
        if let Some(ticket) = self
            .waiters
            .iter()
            .find(|ticket| ticket.expires_at > now && ticket.idempotency_key == req.idempotency_key)
        {
            ticket.check_request(req, owner_principal)?;
        }
        Ok(())
    }

    fn claims_on<'a>(&'a self, host: &'a str) -> impl Iterator<Item = &'a ComputeClaim> {
        self.claims.values().filter(move |claim| claim.host == host)
    }

    fn placement(
        &self,
        req: &PlacementRequest,
        now: DateTime<Utc>,
    ) -> Option<(&HostTelemetrySample, u64, bool)> {
        // ponytail: scan the bounded queue per host; index only if larger queues need it.
        req.prefer.iter().find_map(|host| {
            let eligible = self.eligible_on(req, host, now)?;
            let earlier_can_use_host = self
                .waiters
                .iter()
                .filter(|ticket| ticket.expires_at > now)
                .take_while(|ticket| ticket.idempotency_key != req.idempotency_key)
                .any(|ticket| {
                    ticket.request.prefer.contains(host)
                        && self.eligible_on(&ticket.request, host, now).is_some()
                });
            (!earlier_can_use_host).then_some(eligible)
        })
    }

    fn eligible_on(
        &self,
        req: &PlacementRequest,
        host: &str,
        now: DateTime<Utc>,
    ) -> Option<(&HostTelemetrySample, u64, bool)> {
        let sample = &self.samples.get(host)?.sample;
        let capacity = self.capacity(host, now);
        let (required, exclusive) = PlacementTarget {
            host,
            role: sample.role,
            state: capacity.state,
            remaining: capacity.remaining,
            contract_memory_max_bytes: sample.contract_memory_max_bytes,
            projects: &sample.projects,
            has_claims: self.claims_on(host).next().is_some(),
        }
        .fit(req)?;
        Some((sample, required, exclusive))
    }

    fn capacity(&self, host: &str, now: DateTime<Utc>) -> Capacity {
        let observation = self.samples.get(host);
        let sample = observation.map(|obs| &obs.sample);
        let pending = self.claims_on(host).filter(|claim| {
            if !claim.active_at(now) {
                return false;
            }
            if claim.stage == ClaimStage::PendingStart {
                return true;
            }
            let Some(sample) = sample else {
                return true;
            };
            if sample.role != HostRole::Primary {
                return false;
            }
            let Some(unit) = claim.unit.as_deref() else {
                return true;
            };
            let amc_accounted = sample
                .amc_source
                .as_deref()
                .is_some_and(|source| source == "host" || source == "user")
                && sample.observed_at > claim.started_at.unwrap_or(claim.heartbeat_at)
                && sample
                    .active_workloads
                    .iter()
                    .any(|work| work.kind == "amc" && work.name == unit);
            !amc_accounted
        });
        let (pending_bytes, pending_slots) = pending.fold((0u64, 0u32), |(bytes, slots), claim| {
            (
                bytes.saturating_add(claim.reserved_bytes),
                slots.saturating_add(1),
            )
        });
        let remaining = sample.and_then(|sample| {
            let headroom = sample.usable_headroom_bytes?.saturating_sub(pending_bytes);
            let remaining_budget = sample
                .usable_budget_bytes
                .map(|b| b.saturating_sub(pending_bytes));
            Some(match (sample.role, remaining_budget) {
                (HostRole::Primary, Some(budget)) => headroom.min(budget),
                _ => headroom,
            })
        });
        let (state, reason) = self.host_state(host, observation, remaining, pending_slots, now);
        Capacity {
            state,
            reason,
            pending_bytes,
            remaining,
        }
    }

    fn host_state(
        &self,
        host: &str,
        observation: Option<&Observation>,
        remaining: Option<u64>,
        pending_slots: u32,
        now: DateTime<Utc>,
    ) -> (HostAvailability, Option<&'static str>) {
        use HostAvailability::*;
        let Some(observation) = observation else {
            return (Unknown, Some("no telemetry"));
        };
        if self.claims_on(host).any(|claim| !claim.active_at(now)) {
            return (Busy, Some("host has reconciling claim"));
        }
        if observation.stale_at(now) {
            return (Unknown, Some("telemetry expired"));
        }
        let sample = &observation.sample;
        let adjacent = sample.role == HostRole::Adjacent;
        if adjacent {
            if sample.attached != Some(true) || sample.ac_power != Some(true) {
                return (Detached, Some("attachment and AC power are required"));
            }
            if !sample.active_workloads.is_empty() {
                return (Busy, Some("adjacent host has observed workloads"));
            }
            if !sample.workload_inventory_complete {
                return (Unknown, Some("workload inventory incomplete"));
            }
        }
        if self
            .claims_on(host)
            .any(|claim| adjacent || claim.exclusive)
        {
            return (Busy, Some("host has a conflicting claim"));
        }
        match (sample.memory_full_avg10, sample.max_memory_full_psi) {
            (Some(psi), Some(limit)) if psi > limit => {
                return (Saturated, Some("memory PSI exceeds limit"));
            }
            (Some(_), Some(_)) => {}
            _ if adjacent => return (Unknown, Some("memory PSI or its limit is unknown")),
            _ => {}
        }
        if sample.amc_admissible == Some(false) {
            return (Saturated, Some("AMC reports no admission capacity"));
        }
        if sample.amc_source.as_deref() == Some("none") || remaining.is_none() {
            return (Unknown, Some("AMC headroom is unknown"));
        }
        if remaining == Some(0) {
            return (Saturated, Some("no remaining memory headroom"));
        }
        if let Some(slots) = sample.local_slots {
            if slots <= pending_slots
                || sample
                    .occupied_slots
                    .is_some_and(|used| slots <= used.saturating_add(pending_slots))
            {
                return (Busy, Some("no local slots available"));
            }
            if sample.occupied_slots.is_none() {
                return (Unknown, Some("local slot occupancy is unknown"));
            }
        }
        (Free, None)
    }

    fn host_view(&self, host: &str, role: HostRole, now: DateTime<Utc>) -> HostAvailabilityView {
        let observation = self.samples.get(host);
        let sample = observation.map(|observation| &observation.sample);
        let capacity = self.capacity(host, now);
        HostAvailabilityView {
            host: host.into(),
            role,
            state: capacity.state,
            stale: observation.is_none_or(|observation| observation.stale_at(now)),
            boot_id: sample.map(|sample| sample.boot_id.clone()),
            observed_at: sample.map(|sample| sample.observed_at),
            expires_at: observation.map(|observation| observation.expires_at),
            attached: sample.and_then(|sample| sample.attached),
            ac_power: sample.and_then(|sample| sample.ac_power),
            usable_headroom_bytes: sample.and_then(|sample| sample.usable_headroom_bytes),
            pending_start_reserved_bytes: capacity.pending_bytes,
            remaining_headroom_bytes: capacity.remaining,
            usable_budget_bytes: sample.and_then(|sample| sample.usable_budget_bytes),
            contract_memory_max_bytes: sample.and_then(|sample| sample.contract_memory_max_bytes),
            memory_full_avg10: sample.and_then(|sample| sample.memory_full_avg10),
            max_memory_full_psi: sample.and_then(|sample| sample.max_memory_full_psi),
            local_slots: sample.and_then(|sample| sample.local_slots),
            occupied_slots: sample.and_then(|sample| sample.occupied_slots),
            active_claims: self.claims_on(host).cloned().collect(),
            observed_workloads: sample
                .map(|sample| sample.active_workloads.clone())
                .unwrap_or_default(),
            projects: sample
                .map(|sample| sample.projects.clone())
                .unwrap_or_default(),
            reasons: capacity.reason.into_iter().map(str::to_owned).collect(),
        }
    }
}

struct Capacity {
    state: HostAvailability,
    reason: Option<&'static str>,
    pending_bytes: u64,
    remaining: Option<u64>,
}

struct PlacementTarget<'a> {
    host: &'a str,
    role: HostRole,
    state: HostAvailability,
    remaining: Option<u64>,
    contract_memory_max_bytes: Option<u64>,
    projects: &'a BTreeMap<String, ProjectCapability>,
    has_claims: bool,
}

impl PlacementTarget<'_> {
    fn fit(self, req: &PlacementRequest) -> Option<(u64, bool)> {
        let adjacent = self.role == HostRole::Adjacent;
        let exclusive = req.exclusive.unwrap_or(adjacent);
        if self.state != HostAvailability::Free
            || (exclusive && self.has_claims)
            || req
                .allow_hosts
                .as_ref()
                .is_some_and(|allowed| !allowed.iter().any(|name| name == self.host))
        {
            return None;
        }
        let requested = req.required_bytes_for(self.host);
        let required = if adjacent && req.kind == ClaimKind::AdjacentRun {
            let cap = self.projects.get(req.project.as_ref()?)?.memory_max_bytes;
            if requested > cap {
                return None;
            }
            cap
        } else {
            if !adjacent
                && self
                    .contract_memory_max_bytes
                    .is_some_and(|cap| requested > cap)
            {
                return None;
            }
            requested
        };
        (self.remaining? >= required).then_some((required, exclusive))
    }
}

#[cfg(test)]
mod tests;
