//! Fleet operations snapshot: one serialized contract between the tool that
//! observes a fleet and every surface that shows it.
//!
//! This module owns the shape and the judgements derived from it (is a host
//! outdated, what needs the operator). It performs no I/O: the caller probes
//! hosts, fetches the Gatus body and reads its own approval queue.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use miette::{IntoDiagnostic, Result, WrapErr};
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetStatus {
    pub schema_version: u32,
    /// RFC 3339 time the snapshot was assembled.
    pub generated_at: String,
    /// Configuration revision every host is expected to run: an opaque
    /// identifier compared for equality with each host's `revision`.
    #[serde(default)]
    pub target_revision: Option<String>,
    pub hosts: Vec<HostStatus>,
    pub monitoring: Monitoring,
    #[serde(default)]
    pub pending_ops: Vec<PendingOp>,
    #[serde(default)]
    pub nightly: Option<NightlyRun>,
    /// Secret lifecycle inventory. `None` means an old or unconfigured
    /// producer, which is not the same statement as a scope that reports no
    /// secrets.
    #[serde(default)]
    pub secrets: Option<SecretInventory>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStatus {
    pub name: String,
    pub health: Health,
    /// Configuration revision the running system was built from. Absent when
    /// the host is unreachable or was built from an uncommitted tree.
    #[serde(default)]
    pub revision: Option<String>,
    /// Identity of the running system, such as its store path.
    #[serde(default)]
    pub system: Option<String>,
    /// Failed health steps, one line each.
    #[serde(default)]
    pub failures: Vec<String>,
    /// RFC 3339 time of the probe.
    pub checked_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Health {
    Healthy,
    Degraded,
    Unreachable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateState {
    Current,
    Outdated,
    /// Either side of the comparison is unknown.
    Unknown,
}

impl HostStatus {
    pub fn update_state(&self, target_revision: Option<&str>) -> UpdateState {
        match (self.revision.as_deref(), target_revision) {
            (Some(running), Some(target)) if running == target => UpdateState::Current,
            (Some(_), Some(_)) => UpdateState::Outdated,
            _ => UpdateState::Unknown,
        }
    }
}

/// Service monitoring as last read. `error` set means the alert list is not
/// evidence that services are up.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Monitoring {
    #[serde(default)]
    pub error: Option<String>,
    /// Number of monitored endpoints in the reading.
    #[serde(default)]
    pub monitored: usize,
    #[serde(default)]
    pub alerts: Vec<ServiceAlert>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceAlert {
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub group: String,
    /// RFC 3339 time the endpoint turned unhealthy, when the monitor reports it.
    #[serde(default)]
    pub since: Option<String>,
    /// Errors and failed conditions of the latest result.
    #[serde(default)]
    pub detail: Vec<String>,
}

/// A privileged operation waiting for the operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingOp {
    pub id: String,
    pub host: String,
    pub title: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub requested_by: String,
    /// RFC 3339 time the operation was requested.
    pub created: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NightlyRun {
    pub started_at: String,
    /// Absent while the run is in progress or when it was interrupted.
    #[serde(default)]
    pub finished_at: Option<String>,
    /// Revision the run rolled out, comparable with a host's `revision`;
    /// absent when the update failed.
    #[serde(default)]
    pub revision: Option<String>,
    /// Source-control commit of that revision, for people.
    #[serde(default)]
    pub commit: Option<String>,
    /// Why the update itself failed, before any host was touched.
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub hosts: Vec<NightlyHost>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NightlyHost {
    pub host: String,
    pub outcome: NightlyOutcome,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NightlyOutcome {
    /// Switched to the run's revision and passed its health check.
    Switched,
    /// Not reachable when its turn came; it stays outdated until the next run.
    Offline,
    Failed,
}

impl NightlyRun {
    pub fn failed(&self) -> bool {
        self.error.is_some()
            || self
                .hosts
                .iter()
                .any(|host| host.outcome == NightlyOutcome::Failed)
    }
}

/// Secret lifecycle inventory for one monitored configuration scope.
///
/// Only identity, policy and safe observation metadata cross this boundary: no
/// credential value, account, provider configuration or runtime path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretInventory {
    /// Scope the inventory covers, such as `atlas/home/can`.
    pub scope: String,
    #[serde(default)]
    pub entries: Vec<SecretStatus>,
    /// Fixed code: the scope's policy or inventory could not be read, so
    /// `entries` are not evidence about the secrets behind them.
    #[serde(default)]
    pub error: Option<String>,
}

/// One secret's authored policy and resolved lifecycle at the snapshot instant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretStatus {
    pub scope: String,
    pub name: String,
    pub policy_kind: PolicyKind,
    /// Resolved expiry. `never` is this explicit type, never a sentinel date.
    pub expiry: ExpiryValue,
    pub state: SecretState,
    /// RFC 3339 time of the newest provider attempt, successful or not. An
    /// attempt time is never success evidence.
    #[serde(default)]
    pub last_attempt_at: Option<String>,
    /// RFC 3339 time of the newest successful provider observation.
    #[serde(default)]
    pub last_success_at: Option<String>,
    /// Fixed code naming the verification outcome, when the policy involves
    /// verification. Never provider response text.
    #[serde(default)]
    pub verification: Option<Verification>,
}

/// The lifecycle policy the fleet's configuration authors for a secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PolicyKind {
    /// An operator-asserted date; no credential read or provider call.
    DeclaredDate,
    /// Explicitly non-expiring, with no automatic verification.
    Never,
    /// The expiry is whatever the provider reports.
    ProviderReported,
    /// No policy: unknown, and never inferred to be non-expiring.
    Undeclared,
}

/// A secret's expiry as the fleet can state it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ExpiryValue {
    Date {
        #[serde(rename = "expiresAt")]
        expires_at: String,
    },
    /// Explicitly non-expiring: the provider answered null or the policy
    /// asserts `never`.
    Never,
    /// Unknown: the value is missing or unreadable, never assumed to be `never`.
    Unknown,
}

/// What the operator has to know about one secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SecretState {
    Ok,
    Expiring,
    Expired,
    Never,
    Undeclared,
    Unverifiable,
    Invalid,
}

impl SecretState {
    /// Whether this state is a concern the operator must see. `never`,
    /// `undeclared` and `ok` are inventory facts, not concerns.
    pub const fn needs_attention(self) -> bool {
        matches!(
            self,
            Self::Expiring | Self::Expired | Self::Invalid | Self::Unverifiable
        )
    }

    /// The plan's precedence: an invalid credential outranks a crossed
    /// deadline, which outranks missing evidence, which outranks a healthy
    /// answer.
    const fn precedence(self) -> u8 {
        match self {
            Self::Invalid => 4,
            Self::Expired => 3,
            Self::Expiring => 2,
            Self::Unverifiable => 1,
            Self::Never | Self::Undeclared | Self::Ok => 0,
        }
    }
}

/// Fixed code for the latest verification outcome. Each variant is a stable
/// code the surfaces render; provider error text never reaches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Verification {
    /// Fresh, successful evidence.
    Ok,
    /// Successful evidence older than the freshness window.
    Stale,
    /// A failed attempt is newer than the last success.
    Failed,
    /// No successful evidence was ever recorded.
    Missing,
    /// Evidence is unreadable: malformed timestamps, or evidence from the
    /// future beyond clock tolerance.
    Malformed,
    /// The provider reports the credential inactive.
    Inactive,
    /// The provider reports a not-before time that has not arrived yet.
    NotBefore,
    /// The authored policy itself is malformed.
    PolicyInvalid,
}

/// Authored lifecycle policy for one secret, as the fleet configuration
/// declares it. The producer's input to [`assess_secret`], never a wire type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoredPolicy {
    pub kind: PolicyKind,
    /// RFC 3339 deadline asserted by the `declaredDate` policy.
    pub expires_at: Option<String>,
}

impl AuthoredPolicy {
    pub fn declared_date(expires_at: impl Into<String>) -> Self {
        Self {
            kind: PolicyKind::DeclaredDate,
            expires_at: Some(expires_at.into()),
        }
    }

    pub const fn never() -> Self {
        Self {
            kind: PolicyKind::Never,
            expires_at: None,
        }
    }

    pub const fn provider_reported() -> Self {
        Self {
            kind: PolicyKind::ProviderReported,
            expires_at: None,
        }
    }

    pub const fn undeclared() -> Self {
        Self {
            kind: PolicyKind::Undeclared,
            expires_at: None,
        }
    }
}

/// What the provider collector last learned about one secret. The collector
/// translates the provider response into these fixed values before the
/// assessment sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderObservation {
    pub status: ProviderStatus,
    /// Expiry the provider reported: `Never` is an explicit non-expiring
    /// answer, `Unknown` a missing or unreadable one.
    pub expiry: ExpiryValue,
    /// RFC 3339 time of the newest attempt, successful or not.
    pub last_attempt_at: Option<String>,
    /// RFC 3339 time of the newest successful attempt.
    pub last_success_at: Option<String>,
}

/// The provider's statement about the credential itself, as opposed to its
/// expiry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderStatus {
    Active,
    /// An explicit provider-confirmed inactive value. A refusal alone does not
    /// prove revocation.
    Inactive,
    /// A provider-reported not-before time; a future one is not yet valid.
    NotBefore {
        not_before: String,
    },
}

/// Resolves one secret's lifecycle at `as_of`, a UTC RFC 3339 instant the
/// caller supplies so the judgement is reproducible from the snapshot alone.
/// Pure: no clock read, no I/O, no credential access.
pub fn assess_secret(
    scope: &str,
    name: &str,
    policy: &AuthoredPolicy,
    observation: Option<&ProviderObservation>,
    as_of: &str,
) -> SecretStatus {
    let mut secret = SecretStatus {
        scope: scope.to_owned(),
        name: name.to_owned(),
        policy_kind: policy.kind,
        expiry: ExpiryValue::Unknown,
        state: SecretState::Unverifiable,
        last_attempt_at: None,
        last_success_at: None,
        verification: None,
    };
    match policy.kind {
        // These policies make no time claim, so no instant can change them and
        // no provider statement may rewrite them.
        PolicyKind::Never => {
            secret.expiry = ExpiryValue::Never;
            secret.state = SecretState::Never;
        }
        PolicyKind::Undeclared => secret.state = SecretState::Undeclared,
        PolicyKind::DeclaredDate | PolicyKind::ProviderReported => {
            let Some(as_of) = parse_instant(as_of) else {
                // An unreadable instant cannot judge a deadline, and an
                // unjudged secret is never healthy.
                secret.verification = Some(Verification::Malformed);
                return secret;
            };
            if policy.kind == PolicyKind::DeclaredDate {
                assess_declared(&mut secret, policy, as_of);
            } else if let Some(observation) = observation {
                assess_provider(&mut secret, observation, as_of);
            } else {
                secret.verification = Some(Verification::Missing);
            }
        }
    }
    secret
}

fn assess_declared(secret: &mut SecretStatus, policy: &AuthoredPolicy, as_of: DateTime<Utc>) {
    let Some(expires_at) = policy.expires_at.as_deref().and_then(parse_instant) else {
        secret.state = SecretState::Invalid;
        secret.verification = Some(Verification::PolicyInvalid);
        return;
    };
    secret.expiry = ExpiryValue::Date {
        expires_at: format_instant(expires_at),
    };
    secret.state = deadline_state(expires_at, as_of);
}

fn assess_provider(
    secret: &mut SecretStatus,
    observation: &ProviderObservation,
    as_of: DateTime<Utc>,
) {
    secret.last_attempt_at = observation.last_attempt_at.clone();
    secret.last_success_at = observation.last_success_at.clone();
    // The last known expiry doubles as context when verification fails.
    secret.expiry = normalized_expiry(&observation.expiry);

    // A provider statement about the credential outranks every deadline.
    match &observation.status {
        ProviderStatus::Inactive => {
            secret.state = SecretState::Invalid;
            secret.verification = Some(Verification::Inactive);
            return;
        }
        ProviderStatus::NotBefore { not_before } => match parse_instant(not_before) {
            None => {
                secret.verification = Some(Verification::Malformed);
                return;
            }
            Some(start) if start > as_of => {
                secret.state = SecretState::Invalid;
                secret.verification = Some(Verification::NotBefore);
                return;
            }
            Some(_) => {}
        },
        ProviderStatus::Active => {}
    }

    let (deadline, evidence) = match &observation.expiry {
        ExpiryValue::Never => (SecretState::Never, Verification::Ok),
        ExpiryValue::Date { expires_at } => match parse_instant(expires_at) {
            Some(expires_at) => (deadline_state(expires_at, as_of), Verification::Ok),
            None => (SecretState::Unverifiable, Verification::Malformed),
        },
        ExpiryValue::Unknown => (SecretState::Unverifiable, Verification::Missing),
    };
    let freshness = freshness_problem(observation, as_of);
    secret.state = match freshness {
        None => deadline,
        // A stale or failed check still has a trusted earlier success, so a
        // deadline it reported that has already crossed stays visible.
        Some(Verification::Stale | Verification::Failed)
            if deadline.precedence() > SecretState::Unverifiable.precedence() =>
        {
            deadline
        }
        // Missing or untrustworthy evidence proves nothing, not even a date.
        Some(_) => SecretState::Unverifiable,
    };
    secret.verification = Some(freshness.unwrap_or(evidence));
}

/// Why the newest successful observation is not current evidence, if it isn't.
fn freshness_problem(
    observation: &ProviderObservation,
    as_of: DateTime<Utc>,
) -> Option<Verification> {
    let Some(raw) = observation.last_success_at.as_deref() else {
        return Some(Verification::Missing);
    };
    let Some(success) = parse_instant(raw) else {
        return Some(Verification::Malformed);
    };
    if success > as_of + Duration::seconds(60) {
        // Evidence from the future is evidence of a broken clock, not health.
        return Some(Verification::Malformed);
    }
    if let Some(raw) = observation.last_attempt_at.as_deref() {
        // An attempt timestamp is evidence too: unreadable or future ones fail
        // closed instead of being ignored.
        match parse_instant(raw) {
            Some(attempt) if attempt <= as_of + Duration::seconds(60) => {
                if attempt > success {
                    return Some(Verification::Failed);
                }
            }
            _ => return Some(Verification::Malformed),
        }
    }
    if as_of.signed_duration_since(success) > Duration::hours(48) {
        return Some(Verification::Stale);
    }
    None
}

/// The plan's single fixed lead time: exactly 30 days ahead still warns.
fn deadline_state(expires_at: DateTime<Utc>, as_of: DateTime<Utc>) -> SecretState {
    if expires_at <= as_of {
        SecretState::Expired
    } else if expires_at <= as_of + Duration::days(30) {
        SecretState::Expiring
    } else {
        SecretState::Ok
    }
}

/// The observation's expiry as it may appear in a snapshot: dates become UTC,
/// unreadable dates become `unknown` instead of repeating garbage.
fn normalized_expiry(expiry: &ExpiryValue) -> ExpiryValue {
    match expiry {
        ExpiryValue::Date { expires_at } => match parse_instant(expires_at) {
            Some(expires_at) => ExpiryValue::Date {
                expires_at: format_instant(expires_at),
            },
            None => ExpiryValue::Unknown,
        },
        ExpiryValue::Never => ExpiryValue::Never,
        ExpiryValue::Unknown => ExpiryValue::Unknown,
    }
}

/// Parses an RFC 3339 instant that carries an explicit offset and normalizes it
/// to UTC. A naive timestamp is rejected rather than guessed.
fn parse_instant(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

fn format_instant(at: DateTime<Utc>) -> String {
    // AutoSi keeps sub-second precision, so the stored deadline matches the
    // one the state was judged against.
    at.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

impl FleetStatus {
    /// Number of distinct things that need the operator.
    pub fn attention(&self) -> usize {
        let target = self.target_revision.as_deref();
        let hosts = self
            .hosts
            .iter()
            .filter(|host| {
                host.health != Health::Healthy || host.update_state(target) == UpdateState::Outdated
            })
            .count();
        hosts
            + self.monitoring.alerts.len()
            + usize::from(self.monitoring.error.is_some())
            + self.pending_ops.len()
            + usize::from(self.nightly.as_ref().is_some_and(NightlyRun::failed))
            + self.secrets.as_ref().map_or(0, |secrets| {
                // One concern per secret, plus one for a scope-wide failure.
                usize::from(secrets.error.is_some())
                    + secrets
                        .entries
                        .iter()
                        .filter(|secret| secret.state.needs_attention())
                        .count()
            })
    }
}

#[derive(Deserialize)]
struct GatusEndpoint {
    key: String,
    name: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    results: Vec<GatusResult>,
    #[serde(default)]
    events: Vec<GatusEvent>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GatusResult {
    success: bool,
    #[serde(default)]
    errors: Vec<String>,
    #[serde(default)]
    condition_results: Vec<GatusCondition>,
}

#[derive(Deserialize)]
struct GatusCondition {
    condition: String,
    success: bool,
}

#[derive(Deserialize)]
struct GatusEvent {
    #[serde(rename = "type")]
    kind: String,
    timestamp: String,
}

/// Reads the body of Gatus `GET /api/v1/endpoints/statuses`. An endpoint is an
/// alert when its latest result failed; one with no result yet is not.
pub fn monitoring_from_gatus(body: &str) -> Result<Monitoring> {
    let endpoints: Vec<GatusEndpoint> = serde_json::from_str(body)
        .into_diagnostic()
        .wrap_err("Gatus endpoint statuses are not the expected JSON")?;
    let monitored = endpoints.len();
    let alerts = endpoints
        .into_iter()
        .filter_map(|endpoint| {
            let latest = endpoint.results.into_iter().next_back()?;
            if latest.success {
                return None;
            }
            let failed = latest
                .condition_results
                .into_iter()
                .filter(|condition| !condition.success)
                .map(|condition| condition.condition);
            Some(ServiceAlert {
                key: endpoint.key,
                name: endpoint.name,
                group: endpoint.group,
                since: endpoint
                    .events
                    .into_iter()
                    .rev()
                    .find(|event| event.kind == "UNHEALTHY")
                    .map(|event| event.timestamp),
                detail: latest.errors.into_iter().chain(failed).collect(),
            })
        })
        .collect();
    Ok(Monitoring {
        error: None,
        monitored,
        alerts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str, health: Health, revision: Option<&str>) -> HostStatus {
        HostStatus {
            name: name.into(),
            health,
            revision: revision.map(Into::into),
            system: None,
            failures: Vec::new(),
            checked_at: "2026-11-10T03:00:00Z".into(),
        }
    }

    const SCOPE: &str = "atlas/home/can";
    const AS_OF: &str = "2026-11-15T00:00:00Z";

    fn date(expires_at: &str) -> ExpiryValue {
        ExpiryValue::Date {
            expires_at: expires_at.into(),
        }
    }

    fn secret(
        name: &str,
        policy: AuthoredPolicy,
        observation: Option<&ProviderObservation>,
    ) -> SecretStatus {
        assess_secret(SCOPE, name, &policy, observation, AS_OF)
    }

    fn observed(status: ProviderStatus, expiry: ExpiryValue, success: &str) -> ProviderObservation {
        ProviderObservation {
            status,
            expiry,
            last_attempt_at: Some(success.into()),
            last_success_at: Some(success.into()),
        }
    }

    fn fleet_with(secrets: Option<SecretInventory>) -> FleetStatus {
        FleetStatus {
            schema_version: SCHEMA_VERSION,
            generated_at: AS_OF.into(),
            target_revision: None,
            hosts: Vec::new(),
            monitoring: Monitoring::default(),
            pending_ops: Vec::new(),
            nightly: None,
            secrets,
        }
    }

    #[test]
    fn gatus_reports_only_endpoints_whose_latest_result_failed() {
        let body = r#"[
          {"name":"forgejo","group":"core","key":"core_forgejo",
           "results":[{"success":false,"errors":[],"conditionResults":[]},
                      {"success":true,"conditionResults":[{"condition":"[STATUS] == 200","success":true}]}],
           "events":[{"type":"UNHEALTHY","timestamp":"2026-11-09T10:00:00Z"},
                     {"type":"HEALTHY","timestamp":"2026-11-09T10:05:00Z"}]},
          {"name":"attic","group":"core","key":"core_attic",
           "results":[{"success":true},
                      {"success":false,"errors":["timeout"],
                       "conditionResults":[{"condition":"[STATUS] == 200","success":false},
                                           {"condition":"[CONNECTED] == true","success":true}]}],
           "events":[{"type":"START","timestamp":"2026-11-01T00:00:00Z"},
                     {"type":"UNHEALTHY","timestamp":"2026-11-10T02:00:00Z"}]},
          {"name":"fresh","key":"_fresh"}
        ]"#;
        let monitoring = monitoring_from_gatus(body).unwrap();
        assert_eq!(monitoring.monitored, 3);
        assert_eq!(monitoring.alerts.len(), 1);
        let alert = &monitoring.alerts[0];
        assert_eq!(alert.key, "core_attic");
        assert_eq!(alert.since.as_deref(), Some("2026-11-10T02:00:00Z"));
        assert_eq!(alert.detail, ["timeout", "[STATUS] == 200"]);
        assert!(monitoring_from_gatus("<html>login</html>").is_err());
    }

    #[test]
    fn attention_counts_each_operator_concern_once() {
        let mut status = FleetStatus {
            schema_version: SCHEMA_VERSION,
            generated_at: "2026-11-10T03:10:00Z".into(),
            target_revision: Some("new".into()),
            hosts: vec![
                host("atlas", Health::Healthy, Some("new")),
                host("nomad", Health::Healthy, Some("old")),
                host("murph", Health::Unreachable, None),
                // Built from an uncommitted tree: unknown, not outdated.
                host("starlord", Health::Healthy, None),
            ],
            monitoring: Monitoring::default(),
            pending_ops: Vec::new(),
            nightly: None,
            secrets: None,
        };
        assert_eq!(
            status.hosts[1].update_state(status.target_revision.as_deref()),
            UpdateState::Outdated
        );
        assert_eq!(
            status.hosts[3].update_state(Some("new")),
            UpdateState::Unknown
        );
        assert_eq!(status.attention(), 2);

        status.monitoring.error = Some("relay refused".into());
        status.nightly = Some(NightlyRun {
            started_at: "2026-11-10T03:00:00Z".into(),
            finished_at: Some("2026-11-10T03:09:00Z".into()),
            revision: Some("new".into()),
            commit: Some("0123abc".into()),
            error: None,
            hosts: vec![
                NightlyHost {
                    host: "nomad".into(),
                    outcome: NightlyOutcome::Failed,
                    detail: Some("health check failed".into()),
                },
                NightlyHost {
                    host: "murph".into(),
                    outcome: NightlyOutcome::Offline,
                    detail: None,
                },
            ],
        });
        assert_eq!(status.attention(), 4);

        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"targetRevision\":\"new\""));
        assert_eq!(serde_json::from_str::<FleetStatus>(&json).unwrap(), status);
    }

    #[test]
    fn secret_expiry_boundaries_are_utc_and_inclusive() {
        let assess =
            |expires_at: &str| secret("token", AuthoredPolicy::declared_date(expires_at), None);

        // The deadline itself is already expired; one second later warns.
        assert_eq!(assess("2026-11-15T00:00:00Z").state, SecretState::Expired);
        assert_eq!(assess("2026-11-15T00:00:01Z").state, SecretState::Expiring);
        // Exactly 30 days ahead still warns; one second beyond does not.
        assert_eq!(assess("2026-12-15T00:00:00Z").state, SecretState::Expiring);
        assert_eq!(assess("2026-12-15T00:00:01Z").state, SecretState::Ok);

        // The same instant written with an explicit offset is the same
        // boundary, and the snapshot resolves it to UTC.
        let offset = assess("2026-12-15T02:00:00+02:00");
        assert_eq!(offset.state, SecretState::Expiring);
        assert_eq!(offset.expiry, date("2026-12-15T00:00:00Z"));

        // No offset is ambiguous, so the authored policy is invalid rather than
        // guessed at.
        let naive = assess("2026-12-15T00:00:00");
        assert_eq!(naive.state, SecretState::Invalid);
        assert_eq!(naive.verification, Some(Verification::PolicyInvalid));

        // An unreadable judgement instant is unverified, never healthy.
        let unreadable = assess_secret(
            SCOPE,
            "token",
            &AuthoredPolicy::declared_date("2026-12-15T00:00:00Z"),
            None,
            "soon",
        );
        assert_eq!(unreadable.state, SecretState::Unverifiable);
        assert_eq!(unreadable.verification, Some(Verification::Malformed));
    }

    #[test]
    fn never_and_undeclared_are_distinct_and_not_attention() {
        let never = secret("never", AuthoredPolicy::never(), None);
        let undeclared = secret("undeclared", AuthoredPolicy::undeclared(), None);
        assert_eq!(never.state, SecretState::Never);
        assert_eq!(never.expiry, ExpiryValue::Never);
        assert_eq!(never.verification, None);
        assert_eq!(undeclared.state, SecretState::Undeclared);
        // No policy is unknown, never inferred to be non-expiring.
        assert_eq!(undeclared.expiry, ExpiryValue::Unknown);
        assert_ne!(never.expiry, undeclared.expiry);
        assert!(!never.state.needs_attention());
        assert!(!undeclared.state.needs_attention());

        // A provider statement cannot rewrite an authored `never` or turn an
        // absent policy into one.
        let inactive = observed(ProviderStatus::Inactive, ExpiryValue::Unknown, AS_OF);
        let authored_never = secret("never", AuthoredPolicy::never(), Some(&inactive));
        assert_eq!(authored_never.state, SecretState::Never);
        assert_eq!(authored_never.verification, None);
        let no_policy = secret("undeclared", AuthoredPolicy::undeclared(), Some(&inactive));
        assert_eq!(no_policy.state, SecretState::Undeclared);

        // The wire keeps the two explicit and distinct.
        let never_json = serde_json::to_string(&never).unwrap();
        assert!(never_json.contains("\"expiry\":{\"kind\":\"never\"}"));
        assert!(
            serde_json::to_string(&undeclared)
                .unwrap()
                .contains("\"expiry\":{\"kind\":\"unknown\"}")
        );

        // Neither contributes attention, even in a full snapshot.
        let status = fleet_with(Some(SecretInventory {
            scope: SCOPE.into(),
            entries: vec![never, undeclared],
            error: None,
        }));
        assert_eq!(status.attention(), 0);
    }

    #[test]
    fn provider_freshness_does_not_hide_known_deadlines() {
        let crossed = date("2026-11-01T00:00:00Z");
        let far = date("2027-03-01T00:00:00Z");
        let stale_at = "2026-11-12T23:00:00Z";

        let fresh = observed(ProviderStatus::Active, crossed.clone(), AS_OF);
        let fresh = secret("token", AuthoredPolicy::provider_reported(), Some(&fresh));
        assert_eq!(fresh.state, SecretState::Expired);
        assert_eq!(fresh.expiry, crossed);
        assert_eq!(fresh.verification, Some(Verification::Ok));

        // Stale evidence keeps a crossed deadline...
        let stale = observed(ProviderStatus::Active, crossed.clone(), stale_at);
        let stale = secret("token", AuthoredPolicy::provider_reported(), Some(&stale));
        assert_eq!(stale.state, SecretState::Expired);
        assert_eq!(stale.expiry, crossed);
        assert_eq!(stale.verification, Some(Verification::Stale));

        // ...but never turns a healthy or explicit non-expiring answer into
        // one, and keeps the last known expiry as context.
        let stale_far = observed(ProviderStatus::Active, far.clone(), stale_at);
        let stale_far = secret(
            "token",
            AuthoredPolicy::provider_reported(),
            Some(&stale_far),
        );
        assert_eq!(stale_far.state, SecretState::Unverifiable);
        assert_eq!(stale_far.expiry, far);
        assert_eq!(stale_far.verification, Some(Verification::Stale));

        let stale_never = observed(ProviderStatus::Active, ExpiryValue::Never, stale_at);
        let stale_never = secret(
            "token",
            AuthoredPolicy::provider_reported(),
            Some(&stale_never),
        );
        assert_eq!(stale_never.state, SecretState::Unverifiable);
        assert_eq!(stale_never.expiry, ExpiryValue::Never);

        // A failed attempt is not success evidence while the last success is
        // still recent: a healthy deadline becomes unverifiable, while one
        // inside the warning window keeps warning.
        let failed = ProviderObservation {
            status: ProviderStatus::Active,
            expiry: date("2027-03-01T00:00:00Z"),
            last_attempt_at: Some("2026-11-14T23:00:00Z".into()),
            last_success_at: Some("2026-11-14T22:00:00Z".into()),
        };
        let failed = secret("token", AuthoredPolicy::provider_reported(), Some(&failed));
        assert_eq!(failed.state, SecretState::Unverifiable);
        assert_eq!(failed.verification, Some(Verification::Failed));

        let failed_warning = ProviderObservation {
            status: ProviderStatus::Active,
            expiry: date("2026-11-20T00:00:00Z"),
            last_attempt_at: Some("2026-11-14T23:00:00Z".into()),
            last_success_at: Some("2026-11-14T22:00:00Z".into()),
        };
        let failed_warning = secret(
            "token",
            AuthoredPolicy::provider_reported(),
            Some(&failed_warning),
        );
        assert_eq!(failed_warning.state, SecretState::Expiring);
        assert_eq!(failed_warning.verification, Some(Verification::Failed));

        // A fresh explicit null expiry is the explicit non-expiring answer.
        let never = observed(ProviderStatus::Active, ExpiryValue::Never, AS_OF);
        let never = secret("token", AuthoredPolicy::provider_reported(), Some(&never));
        assert_eq!(never.state, SecretState::Never);
        assert_eq!(never.verification, Some(Verification::Ok));

        // Missing, unreadable and future evidence are all unverifiable: never
        // healthy, and never promoted to `never`.
        let missing = observed(ProviderStatus::Active, ExpiryValue::Unknown, AS_OF);
        let missing = secret("token", AuthoredPolicy::provider_reported(), Some(&missing));
        assert_eq!(missing.state, SecretState::Unverifiable);
        assert_eq!(missing.verification, Some(Verification::Missing));

        let malformed = observed(ProviderStatus::Active, date("2026-12-01"), AS_OF);
        let malformed = secret(
            "token",
            AuthoredPolicy::provider_reported(),
            Some(&malformed),
        );
        assert_eq!(malformed.state, SecretState::Unverifiable);
        assert_eq!(malformed.verification, Some(Verification::Malformed));
        assert_eq!(malformed.expiry, ExpiryValue::Unknown);

        let future = observed(ProviderStatus::Active, far, "2026-11-15T00:05:00Z");
        let future = secret("token", AuthoredPolicy::provider_reported(), Some(&future));
        assert_eq!(future.state, SecretState::Unverifiable);
        assert_eq!(future.verification, Some(Verification::Malformed));

        // Ordinary clock skew is not a verification problem.
        let skew = observed(
            ProviderStatus::Active,
            date("2027-03-01T00:00:00Z"),
            "2026-11-15T00:00:30Z",
        );
        assert_eq!(
            secret("token", AuthoredPolicy::provider_reported(), Some(&skew)).state,
            SecretState::Ok
        );

        // An explicit provider judgement of the credential outranks the
        // deadline it also reports.
        let inactive = observed(
            ProviderStatus::Inactive,
            date("2027-03-01T00:00:00Z"),
            AS_OF,
        );
        let inactive = secret(
            "token",
            AuthoredPolicy::provider_reported(),
            Some(&inactive),
        );
        assert_eq!(inactive.state, SecretState::Invalid);
        assert_eq!(inactive.verification, Some(Verification::Inactive));

        let pending = observed(
            ProviderStatus::NotBefore {
                not_before: "2026-12-01T00:00:00Z".into(),
            },
            date("2027-03-01T00:00:00Z"),
            AS_OF,
        );
        let pending = secret("token", AuthoredPolicy::provider_reported(), Some(&pending));
        assert_eq!(pending.state, SecretState::Invalid);
        assert_eq!(pending.verification, Some(Verification::NotBefore));

        let started = observed(
            ProviderStatus::NotBefore {
                not_before: "2026-11-01T00:00:00Z".into(),
            },
            date("2027-03-01T00:00:00Z"),
            AS_OF,
        );
        assert_eq!(
            secret("token", AuthoredPolicy::provider_reported(), Some(&started)).state,
            SecretState::Ok
        );

        // An unreadable or future attempt timestamp fails closed, even beside a
        // fresh success.
        for attempt in ["garbage", "2026-11-15T00:05:00Z"] {
            let bad_attempt = ProviderObservation {
                status: ProviderStatus::Active,
                expiry: ExpiryValue::Never,
                last_attempt_at: Some(attempt.into()),
                last_success_at: Some(AS_OF.into()),
            };
            let bad_attempt = secret(
                "token",
                AuthoredPolicy::provider_reported(),
                Some(&bad_attempt),
            );
            assert_eq!(bad_attempt.state, SecretState::Unverifiable, "{attempt}");
            assert_eq!(bad_attempt.verification, Some(Verification::Malformed));
        }

        // A crossed deadline survives only stale or failed checks of a trusted
        // earlier success; an untrustworthy success timestamp proves nothing.
        let future_crossed = observed(ProviderStatus::Active, crossed, "2026-11-15T00:05:00Z");
        let future_crossed = secret(
            "token",
            AuthoredPolicy::provider_reported(),
            Some(&future_crossed),
        );
        assert_eq!(future_crossed.state, SecretState::Unverifiable);
        assert_eq!(future_crossed.verification, Some(Verification::Malformed));
    }

    #[test]
    fn resolved_expiry_keeps_sub_second_precision() {
        // Half a second past the instant is still in the future: the stored
        // deadline must not round to one that has already crossed.
        let fraction = date("2026-11-15T00:00:00.500Z");
        let status = secret(
            "token",
            AuthoredPolicy::provider_reported(),
            Some(&observed(ProviderStatus::Active, fraction.clone(), AS_OF)),
        );
        assert_eq!(status.state, SecretState::Expiring);
        assert_eq!(status.expiry, fraction);
    }

    #[test]
    fn secret_attention_counts_each_concern_once() {
        let inventory = SecretInventory {
            scope: SCOPE.into(),
            entries: vec![
                secret(
                    "ok",
                    AuthoredPolicy::declared_date("2027-06-01T00:00:00Z"),
                    None,
                ),
                secret(
                    "expiring",
                    AuthoredPolicy::declared_date("2026-12-01T00:00:00Z"),
                    None,
                ),
                secret(
                    "expired",
                    AuthoredPolicy::declared_date("2026-11-01T00:00:00Z"),
                    None,
                ),
                secret("never", AuthoredPolicy::never(), None),
                secret("undeclared", AuthoredPolicy::undeclared(), None),
                secret("unverifiable", AuthoredPolicy::provider_reported(), None),
                secret(
                    "invalid-policy",
                    AuthoredPolicy::declared_date("2026-12-01T00:00:00"),
                    None,
                ),
                // Invalid outranks the crossed deadline it also reports, so this
                // entry stays one concern rather than two.
                secret(
                    "invalid-and-expired",
                    AuthoredPolicy::provider_reported(),
                    Some(&observed(
                        ProviderStatus::Inactive,
                        date("2026-11-01T00:00:00Z"),
                        AS_OF,
                    )),
                ),
            ],
            error: None,
        };
        assert_eq!(inventory.entries[7].state, SecretState::Invalid);

        // expiring, expired, unverifiable, invalid-policy, invalid-and-expired.
        let mut status = fleet_with(Some(inventory.clone()));
        assert_eq!(status.attention(), 5);

        // One scope-wide inventory error, not one per affected entry.
        status.secrets = Some(SecretInventory {
            error: Some("manifest_unreadable".into()),
            ..inventory
        });
        assert_eq!(status.attention(), 6);

        // The snapshot's existing concerns are unchanged.
        status.monitoring.error = Some("relay refused".into());
        assert_eq!(status.attention(), 7);
    }

    #[test]
    fn fleet_status_secret_wire_is_additive() {
        // A snapshot written before the secret inventory existed still parses.
        let legacy = r#"{
          "schemaVersion": 1,
          "generatedAt": "2026-11-10T03:10:00Z",
          "targetRevision": "new",
          "hosts": [{"name": "atlas", "health": "healthy", "revision": "new", "checkedAt": "2026-11-10T03:00:00Z"}],
          "monitoring": {"monitored": 2, "alerts": []},
          "pendingOps": [],
          "nightly": null
        }"#;
        let parsed: FleetStatus = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.schema_version, SCHEMA_VERSION);
        assert_eq!(parsed.secrets, None);
        assert_eq!(parsed.attention(), 0);

        // A new snapshot round-trips at the same schema version.
        let status = fleet_with(Some(SecretInventory {
            scope: SCOPE.into(),
            entries: vec![
                secret(
                    "syndb-cloudflare-pages-token",
                    AuthoredPolicy::never(),
                    None,
                ),
                secret(
                    "rotates",
                    AuthoredPolicy::declared_date("2027-06-01T00:00:00Z"),
                    None,
                ),
            ],
            error: None,
        }));
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains(&format!("\"schemaVersion\":{SCHEMA_VERSION}")));
        assert!(json.contains("\"policyKind\":\"never\""));
        assert_eq!(serde_json::from_str::<FleetStatus>(&json).unwrap(), status);

        // An old typed reader without the field ignores it instead of failing.
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        #[allow(dead_code)]
        struct LegacyFleetStatus {
            schema_version: u32,
            generated_at: String,
            hosts: Vec<HostStatus>,
            monitoring: Monitoring,
            #[serde(default)]
            pending_ops: Vec<PendingOp>,
            #[serde(default)]
            nightly: Option<NightlyRun>,
        }
        let old_reader: LegacyFleetStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(old_reader.schema_version, SCHEMA_VERSION);
        assert_eq!(old_reader.generated_at, status.generated_at);
        assert_eq!(old_reader.hosts, status.hosts);
    }
}
