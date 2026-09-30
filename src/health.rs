//! Host-local health collection and authenticated Gatus publication.
//!
//! The caller supplies paths, selected checks, credentials, and any architecture-
//! specific contract evaluator. No fleet inventory or global configuration is read.

use crate::topology::valid_health_unit;
use crate::topology::{BodyAssertion, BodyOperator, HealthProbe, ServiceHealthCheck};
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, process::Command, time::Duration};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublisherConfig {
    pub host: String,
    pub url: String,
    pub systemctl: PathBuf,
    pub timeout: PathBuf,
    pub checks: Vec<LocalCheck>,
}

#[derive(Debug, Deserialize)]
pub struct LocalCheck {
    pub key: String,
    pub source: ServiceHealthCheck,
    /// Resolved literal-loopback target for an endpoint owned by this publisher.
    #[serde(default)]
    pub target: Option<LocalNetworkTarget>,
}

#[derive(Debug, Deserialize)]
pub struct LocalNetworkTarget {
    pub host: String,
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct Observation {
    pub key: String,
    pub healthy: bool,
    pub detail: String,
}

/// Bound process and request concurrency. Deployment glue budgets the same batches.
pub const PUBLISHER_CONCURRENCY: usize = 8;

fn unit_healthy(output: &str, max_age: Option<u32>, uptime_us: u64) -> bool {
    let fields: std::collections::HashMap<_, _> = output
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();
    if fields.get("LoadState") != Some(&"loaded") {
        return false;
    }
    match max_age {
        None => fields.get("ActiveState") == Some(&"active"),
        Some(age) => {
            let exited = fields
                .get("ExecMainExitTimestampMonotonic")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            fields.get("ActiveState") == Some(&"inactive")
                && fields.get("Result") == Some(&"success")
                && fields.get("ExecMainCode") == Some(&"1")
                && fields.get("ExecMainStatus") == Some(&"0")
                && exited > 0
                && exited <= uptime_us
                && uptime_us - exited <= u64::from(age) * 1_000_000
        }
    }
}

fn body_matches(body: &str, assertion: &BodyAssertion) -> bool {
    let json = serde_json::from_str::<serde_json::Value>(body).ok();
    let mut value = json.as_ref();
    for segment in assertion.path.split('.').filter(|s| !s.is_empty()) {
        value = value.and_then(|v| {
            v.get(segment)
                .or_else(|| segment.parse::<usize>().ok().and_then(|i| v.get(i)))
        });
    }
    if !assertion.path.is_empty() && value.is_none() {
        return false;
    }
    let text = value
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        })
        .unwrap_or_else(|| body.to_owned());
    match assertion.operator {
        BodyOperator::Equals => text == assertion.value,
        BodyOperator::Contains => text.contains(&assertion.value),
        BodyOperator::Nonempty => value.map_or(!text.is_empty(), |v| match v {
            serde_json::Value::String(s) => !s.is_empty(),
            serde_json::Value::Array(a) => !a.is_empty(),
            serde_json::Value::Object(o) => !o.is_empty(),
            _ => false,
        }),
    }
}

fn network_healthy(check: &LocalCheck) -> bool {
    use pklx::pklr::reqwest;
    let Some(target) = &check.target else {
        return false;
    };
    let started = std::time::Instant::now();
    let result = (|| -> Option<bool> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        runtime.block_on(async {
            let timeout = Duration::from_secs(u64::from(check.source.timeout_seconds));
            tokio::time::timeout(timeout, async {
                match &check.source.probe {
                    HealthProbe::Tcp { .. } => {
                        let url = reqwest::Url::parse(&target.url).ok()?;
                        tokio::net::TcpStream::connect((url.host_str()?, url.port()?))
                            .await
                            .ok()?;
                        Some(true)
                    }
                    HealthProbe::Http {
                        accepted_status,
                        follow_redirects,
                        body,
                        ..
                    } => {
                        let client = reqwest::Client::builder()
                            .no_proxy()
                            .timeout(timeout)
                            .redirect(if *follow_redirects {
                                reqwest::redirect::Policy::limited(10)
                            } else {
                                reqwest::redirect::Policy::none()
                            })
                            .build()
                            .ok()?;
                        let mut response = client.get(&target.url).send().await.ok()?;
                        if !accepted_status.contains(&response.status().as_u16()) {
                            return Some(false);
                        }
                        let mut bytes = Vec::new();
                        while let Some(chunk) = response.chunk().await.ok()? {
                            // Readiness evidence is bounded even for an unhealthy streaming server.
                            if bytes.len() + chunk.len() > 1_048_576 {
                                return Some(false);
                            }
                            bytes.extend_from_slice(&chunk);
                        }
                        let text = String::from_utf8(bytes).ok()?;
                        Some(body.iter().all(|assertion| body_matches(&text, assertion)))
                    }
                    _ => None,
                }
            })
            .await
            .ok()
            .flatten()
        })
    })()
    .unwrap_or(false);
    result && started.elapsed().as_millis() < u128::from(check.source.max_response_time_ms)
}

pub fn collect(
    config: &PublisherConfig,
    local_host: &str,
    uptime_us: u64,
    contract: impl Fn(&str, u32) -> bool + Sync,
) -> Result<Vec<Observation>> {
    if config.host != local_host {
        return Err(miette::miette!(
            "health publisher belongs to {}, not {local_host}",
            config.host
        ));
    }
    let mut keys = std::collections::HashSet::new();
    // Reject a bad selection before running any probes or publishing results.
    for check in &config.checks {
        let host = match &check.source.probe {
            HealthProbe::Unit { host, unit } | HealthProbe::Job { host, unit, .. } => {
                if !valid_health_unit(unit) {
                    return Err(miette::miette!("invalid health unit"));
                }
                host
            }
            HealthProbe::Contract { host, contract } if !contract.is_empty() => host,
            HealthProbe::Http {
                endpoint: Some(_),
                site: None,
                ..
            }
            | HealthProbe::Tcp { .. } => {
                let target = check
                    .target
                    .as_ref()
                    .ok_or_else(|| miette::miette!("missing local network target"))?;
                let url = pklx::pklr::reqwest::Url::parse(&target.url).into_diagnostic()?;
                let scheme = if matches!(check.source.probe, HealthProbe::Tcp { .. }) {
                    "tcp"
                } else {
                    "http"
                };
                if url.scheme() != scheme
                    || url.host_str() != Some("127.0.0.1")
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.fragment().is_some()
                    || (scheme == "tcp" && url.port().is_none())
                {
                    return Err(miette::miette!("invalid local network target"));
                }
                &target.host
            }
            _ => return Err(miette::miette!("{} is not a host-local check", check.key)),
        };
        if host != local_host {
            return Err(miette::miette!("{} targets a different host", check.key));
        }
        if check.key.is_empty() || !keys.insert(&check.key) || check.source.timeout_seconds == 0 {
            return Err(miette::miette!("invalid or duplicate health check"));
        }
    }
    let observe = |check: &LocalCheck| {
        let healthy = if let HealthProbe::Unit { unit, .. } | HealthProbe::Job { unit, .. } =
            &check.source.probe
        {
            // Pass the unit after `--`; never interpret profile strings as shell.
            let output = Command::new(&config.timeout)
                .arg("--kill-after=1s")
                .arg(format!("{}s", check.source.timeout_seconds))
                .arg(&config.systemctl)
                .args(["show", "--property=LoadState,ActiveState,Result,ExecMainCode,ExecMainStatus,ExecMainExitTimestampMonotonic", "--", unit])
                .output();
            let max_age = match check.source.probe {
                HealthProbe::Job {
                    max_age_seconds, ..
                } => Some(max_age_seconds),
                _ => None,
            };
            output.is_ok_and(|out| {
                out.status.success()
                    && unit_healthy(&String::from_utf8_lossy(&out.stdout), max_age, uptime_us)
            })
        } else if let HealthProbe::Contract { contract: id, .. } = &check.source.probe {
            contract(id, check.source.timeout_seconds)
        } else {
            network_healthy(check)
        };
        Observation {
            key: check.key.clone(),
            healthy,
            // Publish a bounded verdict, never command output or credential paths.
            detail: if healthy {
                "healthy"
            } else {
                "health contract failed or evidence expired"
            }
            .into(),
        }
    };
    let mut results = Vec::with_capacity(config.checks.len());
    for batch in config.checks.chunks(PUBLISHER_CONCURRENCY) {
        std::thread::scope(|scope| -> Result<()> {
            let handles: Vec<_> = batch
                .iter()
                .map(|check| scope.spawn(|| observe(check)))
                .collect();
            for handle in handles {
                results.push(
                    handle
                        .join()
                        .map_err(|_| miette::miette!("health collector panicked"))?,
                );
            }
            Ok(())
        })?;
    }
    Ok(results)
}

pub async fn publish(
    config: &PublisherConfig,
    token: &str,
    observations: &[Observation],
) -> Result<()> {
    use pklx::pklr::reqwest;
    if token.trim().is_empty() {
        return Err(miette::miette!("empty health publication credential"));
    }
    let base = reqwest::Url::parse(&config.url).into_diagnostic()?;
    if !matches!(base.scheme(), "http" | "https")
        || !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
        || base.path() != "/"
    {
        return Err(miette::miette!("invalid health publication URL"));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .into_diagnostic()?;
    let mut failed = Vec::new();
    let mut keys = std::collections::HashSet::new();
    // Preflight the entire result set so an invalid tail cannot partially publish.
    for observation in observations {
        if !keys.insert(&observation.key) || !config.checks.iter().any(|c| c.key == observation.key)
        {
            return Err(miette::miette!("unselected health publication key"));
        }
    }
    for batch in observations.chunks(PUBLISHER_CONCURRENCY) {
        let mut tasks = tokio::task::JoinSet::new();
        for observation in batch {
            let mut url = base.clone();
            url.path_segments_mut()
                .map_err(|_| miette::miette!("invalid health publication URL"))?
                .clear()
                .extend(["api", "v1", "endpoints", &observation.key, "external"]);
            url.query_pairs_mut().append_pair(
                "success",
                if observation.healthy { "true" } else { "false" },
            );
            if !observation.healthy {
                url.query_pairs_mut()
                    .append_pair("error", &observation.detail);
            }
            let request = client.post(url).bearer_auth(token.trim());
            let key = observation.key.clone();
            tasks.spawn(async move {
                (
                    key,
                    request.send().await.is_ok_and(|r| r.status().is_success()),
                )
            });
        }
        while let Some(result) = tasks.join_next().await {
            let (key, healthy) = result.into_diagnostic()?;
            if !healthy {
                failed.push(key);
            }
        }
    }
    if !failed.is_empty() {
        return Err(miette::miette!(
            "health publication failed for {}",
            failed.join(", ")
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_jobs_must_have_recent_successful_evidence() {
        let good = "LoadState=loaded\nActiveState=inactive\nResult=success\nExecMainCode=1\nExecMainStatus=0\nExecMainExitTimestampMonotonic=1000000\n";
        assert!(unit_healthy(good, Some(60), 60_000_000));
        assert!(!unit_healthy(good, None, 60_000_000));
        assert!(!unit_healthy(good, Some(60), 62_000_000));
        assert!(!unit_healthy(good, Some(60), 500_000));
        assert!(!unit_healthy(
            &good.replace("Status=0", "Status=1"),
            Some(60),
            2_000_000
        ));
        assert!(!unit_healthy(
            &good.replace("1000000", "0"),
            Some(60),
            2_000_000
        ));
        assert!(!unit_healthy(
            &good.replace("Code=1", "Code=2"),
            Some(60),
            2_000_000
        ));
        assert!(!unit_healthy(
            &good.replace("inactive", "activating"),
            Some(60),
            2_000_000
        ));
    }

    #[test]
    fn missing_and_failed_units_cannot_appear_healthy() {
        assert!(!unit_healthy(
            "LoadState=not-found\nActiveState=active",
            None,
            0
        ));
        assert!(!unit_healthy(
            "LoadState=loaded\nActiveState=failed",
            None,
            0
        ));
        assert!(unit_healthy(
            "LoadState=loaded\nActiveState=active",
            None,
            0
        ));
    }
}
