//! Backend-neutral service identity, disclosure, and health contracts.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceProfile {
    pub display_name: String,
    pub category: String,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub visibility: ServiceVisibility,
    #[serde(default)]
    pub lifecycle: ServiceLifecycle,
    #[serde(default)]
    pub sites: Vec<String>,
    #[serde(default)]
    pub endpoints: Vec<String>,
    #[serde(default)]
    pub health: IndexMap<String, ServiceHealthCheck>,
    #[serde(default)]
    pub exclusion_reason: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceVisibility {
    Public,
    #[default]
    Internal,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceLifecycle {
    #[default]
    Active,
    OnDemand,
    Planned,
    Retired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceHealthCheck {
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub visibility: Option<ServiceVisibility>,
    pub probe: HealthProbe,
    #[serde(default = "interval")]
    pub interval_seconds: u32,
    #[serde(default = "timeout")]
    pub timeout_seconds: u32,
    #[serde(default = "response_time")]
    pub max_response_time_ms: u32,
}

fn interval() -> u32 {
    60
}
fn timeout() -> u32 {
    10
}
fn response_time() -> u32 {
    5000
}
fn health_path() -> String {
    "/".into()
}
fn statuses() -> Vec<u16> {
    vec![200]
}
fn true_value() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum HealthProbe {
    Http {
        #[serde(default)]
        site: Option<String>,
        #[serde(default)]
        endpoint: Option<String>,
        #[serde(default = "health_path")]
        path: String,
        #[serde(default = "statuses")]
        accepted_status: Vec<u16>,
        #[serde(default = "true_value")]
        follow_redirects: bool,
        #[serde(default)]
        body: Vec<BodyAssertion>,
    },
    Tcp {
        endpoint: String,
    },
    Dns {
        host: String,
        query: String,
        expected: String,
    },
    Unit {
        host: String,
        unit: String,
    },
    Job {
        host: String,
        unit: String,
        max_age_seconds: u32,
    },
    Contract {
        host: String,
        contract: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BodyAssertion {
    #[serde(default)]
    pub path: String,
    pub operator: BodyOperator,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BodyOperator {
    Equals,
    Contains,
    Nonempty,
}

/// A single systemd unit name, never a pattern or a command-line option.
pub(crate) fn valid_health_unit(unit: &str) -> bool {
    !unit.starts_with('-')
        && unit.len() <= 255
        && unit
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_@:.\\-".contains(&b))
        && [".service", ".socket", ".timer", ".target", ".mount"]
            .iter()
            .any(|suffix| {
                unit.strip_suffix(suffix)
                    .is_some_and(|base| !base.is_empty())
            })
}
