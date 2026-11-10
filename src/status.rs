//! Fleet operations snapshot: one serialized contract between the tool that
//! observes a fleet and every surface that shows it.
//!
//! This module owns the shape and the judgements derived from it (is a host
//! outdated, what needs the operator). It performs no I/O: the caller probes
//! hosts, fetches the Gatus body and reads its own approval queue.

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
}
