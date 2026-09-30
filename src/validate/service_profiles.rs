use super::ValidationReport;
use crate::topology::{
    HealthProbe, HttpAccess, ServiceLifecycle, ServiceVisibility, Topology, valid_health_unit,
};

pub(super) fn validate(topology: &Topology, report: &mut ValidationReport) {
    for (name, profile) in &topology.services.catalog {
        let base = format!("services.catalog.{name}");
        let mut error = |code: &str, detail: String| {
            report.error(
                format!("service_profile.{code}"),
                Some(base.clone()),
                None,
                detail,
            );
        };
        if profile.display_name.trim().is_empty() || profile.category.trim().is_empty() {
            error(
                "missing_label",
                "Display name and category must be nonempty".into(),
            );
        }
        if name.is_empty() || profile.health.keys().any(|id| id.is_empty()) {
            error(
                "missing_id",
                "Service and check identifiers must be nonempty".into(),
            );
        }
        if profile
            .domain
            .as_ref()
            .is_some_and(|domain| !super::valid_hostname(domain))
        {
            error(
                "invalid_domain",
                "Domain affiliation must be a hostname".into(),
            );
        }
        if profile
            .exclusion_reason
            .as_ref()
            .is_some_and(|s| s.trim().is_empty())
        {
            error(
                "empty_exclusion",
                "An exclusion needs an explanation".into(),
            );
        }
        if profile.health.is_empty() && profile.exclusion_reason.is_none() {
            error(
                "missing_health_policy",
                "Every service needs health checks or an exclusion reason".into(),
            );
        }
        if profile.lifecycle != ServiceLifecycle::Active && profile.exclusion_reason.is_none() {
            error(
                "missing_lifecycle_reason",
                "Non-active services need an explanation".into(),
            );
        }
        for site in &profile.sites {
            if !topology.services.http_sites.contains_key(site) {
                error("unknown_site", format!("Unknown site '{site}'"));
            }
        }
        for endpoint in &profile.endpoints {
            if !topology.services.endpoints.contains_key(endpoint) {
                error("unknown_endpoint", format!("Unknown endpoint '{endpoint}'"));
            }
        }
        for (id, check) in &profile.health {
            let public =
                check.visibility.unwrap_or(profile.visibility) == ServiceVisibility::Public;
            if check
                .display_name
                .as_ref()
                .is_some_and(|label| label.trim().is_empty())
            {
                error(
                    "missing_label",
                    format!("Check '{id}' has an empty display name"),
                );
            }
            if check
                .category
                .as_ref()
                .is_some_and(|label| label.trim().is_empty())
            {
                error(
                    "missing_label",
                    format!("Check '{id}' has an empty category"),
                );
            }
            if profile.visibility == ServiceVisibility::Internal
                && check.visibility == Some(ServiceVisibility::Public)
            {
                error(
                    "visibility_escalation",
                    format!("Check '{id}' cannot publish an internal service"),
                );
            }
            if check.interval_seconds == 0
                || check.timeout_seconds == 0
                || check.timeout_seconds > check.interval_seconds
                || check.max_response_time_ms == 0
            {
                error(
                    "invalid_budget",
                    format!(
                        "Check '{id}' needs positive polling/timeout budgets, timeout <= interval"
                    ),
                );
            }
            match &check.probe {
                HealthProbe::Http {
                    site,
                    endpoint,
                    path,
                    accepted_status,
                    ..
                } => {
                    if site.is_some() == endpoint.is_some() {
                        error(
                            "ambiguous_http_target",
                            format!("Check '{id}' needs exactly one site or endpoint"),
                        );
                    }
                    if let Some(site) = site {
                        if !topology.services.http_sites.contains_key(site) {
                            error(
                                "unknown_site",
                                format!("Check '{id}' references unknown site '{site}'"),
                            );
                        } else if !profile.sites.contains(site) {
                            error(
                                "unowned_site",
                                format!("Check '{id}' must reference a site owned by this profile"),
                            );
                        }
                        if public
                            && topology
                                .services
                                .http_sites
                                .get(site)
                                .is_some_and(|target| {
                                    target.access == HttpAccess::Vpn
                                        || !topology
                                            .deployment
                                            .ingress_groups
                                            .get(&target.ingress)
                                            .is_some_and(|group| {
                                                group.scope == crate::topology::IngressScope::Public
                                            })
                                })
                        {
                            error(
                                "private_probe",
                                format!("Check '{id}' cannot publish a private site"),
                            );
                        }
                    }
                    if let Some(endpoint) = endpoint {
                        if public {
                            error(
                                "private_probe",
                                format!("Check '{id}' needs a public site for a public summary"),
                            );
                        }
                        if let Some(target) = topology.services.endpoints.get(endpoint) {
                            if !target.transport.is_http() || !profile.endpoints.contains(endpoint)
                            {
                                error(
                                    "invalid_http_endpoint",
                                    format!("Check '{id}' needs an owned HTTP endpoint"),
                                );
                            }
                        } else {
                            error("unknown_endpoint", format!("Unknown endpoint '{endpoint}'"));
                        }
                    }
                    if !path.starts_with('/')
                        || path.starts_with("//")
                        || path.contains(['\n', '\r'])
                    {
                        error(
                            "invalid_http_path",
                            format!("Check '{id}' needs an absolute origin path"),
                        );
                    }
                    if accepted_status.is_empty()
                        || accepted_status.iter().any(|s| !(100..=599).contains(s))
                    {
                        error(
                            "invalid_http_status",
                            format!("Check '{id}' needs valid expected HTTP statuses"),
                        );
                    }
                }
                HealthProbe::Tcp { endpoint } => {
                    if public {
                        error(
                            "private_probe",
                            format!("TCP check '{id}' must be internal"),
                        );
                    }
                    if !topology.services.endpoints.contains_key(endpoint) {
                        error("unknown_endpoint", format!("Unknown endpoint '{endpoint}'"));
                    } else if !profile.endpoints.contains(endpoint) {
                        error(
                            "unowned_endpoint",
                            format!("Check '{id}' needs an owned endpoint"),
                        );
                    } else if !topology.services.endpoints[endpoint].tcp_probe {
                        error(
                            "tcp_probe_disabled",
                            format!("Endpoint '{endpoint}' forbids TCP probes"),
                        );
                    }
                }
                HealthProbe::Dns { host, .. }
                | HealthProbe::Unit { host, .. }
                | HealthProbe::Job { host, .. }
                | HealthProbe::Contract { host, .. } => {
                    if !topology.hosts.contains_key(host) {
                        error(
                            "unknown_host",
                            format!("Check '{id}' references unknown host '{host}'"),
                        );
                    }
                    if public {
                        error(
                            "private_probe",
                            format!("Host-local check '{id}' must be internal"),
                        );
                    }
                    if let HealthProbe::Job {
                        max_age_seconds: 0, ..
                    } = check.probe
                    {
                        error(
                            "invalid_freshness",
                            format!("Job '{id}' needs a positive freshness budget"),
                        );
                    }
                    match &check.probe {
                        HealthProbe::Unit { unit, .. } | HealthProbe::Job { unit, .. }
                            if !valid_health_unit(unit)
                                || (matches!(check.probe, HealthProbe::Job { .. })
                                    && !unit.ends_with(".service")) =>
                        {
                            error(
                                "invalid_unit",
                                format!("Check '{id}' needs a single valid systemd unit"),
                            )
                        }
                        HealthProbe::Contract { contract, .. } if contract.trim().is_empty() => {
                            error(
                                "invalid_contract",
                                format!("Check '{id}' needs a contract ID"),
                            )
                        }
                        HealthProbe::Dns {
                            query, expected, ..
                        } if !super::valid_hostname(query)
                            || expected.parse::<std::net::Ipv4Addr>().is_err() =>
                        {
                            error(
                                "invalid_dns",
                                format!("Check '{id}' needs a DNS name and expected IPv4 address"),
                            )
                        }
                        _ => (),
                    }
                }
            }
        }
    }
}
