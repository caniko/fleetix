//! Forge repository inventory: list, normalize and filter repositories on
//! Forgejo, GitHub and GitLab namespaces. Callers own credentials and policy.

use miette::Result;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// One forge namespace to inventory.
#[derive(Debug, Clone, Default)]
pub struct Source {
    /// `forgejo`, `github` or `gitlab`.
    pub provider: String,
    pub host: String,
    pub namespace: String,
    /// `user`, `organization` (Forgejo/GitHub) or `group` (GitLab).
    pub kind: String,
    pub token: Option<String>,
    pub include_subgroups: bool,
}

/// A repository normalized across providers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Repository {
    pub host: String,
    pub name: String,
    pub full_name: String,
    /// Git storage in bytes as reported by the forge; `None` when GitLab
    /// withholds statistics (below the Reporter role).
    pub size_bytes: Option<u64>,
    pub private: bool,
    pub fork: bool,
    pub mirror: bool,
    pub archived: bool,
    /// `None` for GitHub, which does not report emptiness.
    pub empty: Option<bool>,
    /// RFC 3339 timestamp of the last update/activity.
    pub updated_at: String,
    pub default_branch: Option<String>,
    /// `None` for GitHub, whose count also includes open pull requests.
    pub open_issues: Option<u64>,
    pub ssh_url: Option<String>,
    pub clone_url: Option<String>,
    /// Pull requests in any state; filled by [`pull_request_totals`].
    pub pull_requests_total: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct RawRepository {
    #[serde(default)]
    name: String,
    #[serde(default)]
    full_name: String,
    #[serde(default)]
    path_with_namespace: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    statistics: Option<RawStatistics>,
    #[serde(default)]
    private: bool,
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    fork: bool,
    #[serde(default)]
    forked_from_project: Option<serde_json::Value>,
    #[serde(default)]
    mirror: bool,
    #[serde(default)]
    mirror_url: Option<String>,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    empty: bool,
    #[serde(default)]
    empty_repo: bool,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    last_activity_at: Option<String>,
    #[serde(default)]
    default_branch: Option<String>,
    #[serde(default)]
    open_issues_count: u64,
    #[serde(default)]
    ssh_url: Option<String>,
    #[serde(default)]
    clone_url: Option<String>,
    #[serde(default)]
    ssh_url_to_repo: Option<String>,
    #[serde(default)]
    http_url_to_repo: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawStatistics {
    #[serde(default)]
    repository_size: u64,
}

impl RawRepository {
    fn normalize(self, source: &Source) -> Repository {
        let (gitlab, github) = (source.provider == "gitlab", source.provider == "github");
        Repository {
            host: source.host.clone(),
            full_name: if self.full_name.is_empty() {
                self.path_with_namespace
            } else {
                self.full_name
            },
            name: self.name,
            // Forgejo and GitHub report KiB; GitLab reports bytes in statistics.
            size_bytes: if gitlab {
                self.statistics.map(|statistics| statistics.repository_size)
            } else {
                Some(self.size * 1024)
            },
            private: self.private || self.visibility.is_some_and(|v| v != "public"),
            fork: self.fork || self.forked_from_project.is_some(),
            mirror: self.mirror || self.mirror_url.is_some(),
            archived: self.archived,
            empty: (!github).then_some(self.empty || self.empty_repo),
            updated_at: self
                .updated_at
                .or(self.last_activity_at)
                .unwrap_or_default(),
            default_branch: self.default_branch,
            open_issues: (!github).then_some(self.open_issues_count),
            ssh_url: self.ssh_url.or(self.ssh_url_to_repo),
            clone_url: self.clone_url.or(self.http_url_to_repo),
            pull_requests_total: None,
        }
    }
}

/// Repository selection criteria; `None` fields match anything.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub min_bytes: Option<u64>,
    pub private: Option<bool>,
    pub fork: Option<bool>,
    pub archived: Option<bool>,
    pub mirror: Option<bool>,
    /// `YYYY-MM-DD`; matches repositories last updated before this date.
    pub updated_before: Option<String>,
    pub name_contains: Option<String>,
    /// Match only repositories known to have no pull requests in any state.
    pub without_pull_requests: bool,
}

impl Filter {
    pub fn matches(&self, repository: &Repository) -> bool {
        let flag = |want: Option<bool>, have: bool| want.is_none_or(|want| want == have);
        self.min_bytes
            .is_none_or(|min| repository.size_bytes.is_some_and(|size| size >= min))
            && flag(self.private, repository.private)
            && flag(self.fork, repository.fork)
            && flag(self.archived, repository.archived)
            && flag(self.mirror, repository.mirror)
            // ponytail: date-prefix comparison ignores timezone offsets (<1 day error).
            && self
                .updated_before
                .as_deref()
                .is_none_or(|date| repository.updated_at.get(..10).is_some_and(|d| d < date))
            && self.name_contains.as_deref().is_none_or(|needle| {
                repository
                    .full_name
                    .to_lowercase()
                    .contains(&needle.to_lowercase())
            })
            && (!self.without_pull_requests || repository.pull_requests_total == Some(0))
    }
}

/// List every repository in a source namespace.
pub fn list(source: &Source) -> Result<Vec<Repository>> {
    let (endpoint, page_size) = match (source.provider.as_str(), source.kind.as_str()) {
        ("forgejo", "organization") => (
            format!(
                "https://{}/api/v1/orgs/{}/repos?limit=50&page=",
                source.host, source.namespace
            ),
            50,
        ),
        ("forgejo", "user") => (
            format!(
                "https://{}/api/v1/users/{}/repos?limit=50&page=",
                source.host, source.namespace
            ),
            50,
        ),
        ("github", "organization") => (
            format!(
                "https://api.github.com/orgs/{}/repos?per_page=100&page=",
                source.namespace
            ),
            100,
        ),
        // The public user endpoint hides private repositories even from their owner.
        ("github", "user") if github_login(source)?.eq_ignore_ascii_case(&source.namespace) => (
            "https://api.github.com/user/repos?affiliation=owner&per_page=100&page=".to_owned(),
            100,
        ),
        ("github", "user") => (
            format!(
                "https://api.github.com/users/{}/repos?per_page=100&page=",
                source.namespace
            ),
            100,
        ),
        ("gitlab", "group") => (
            format!(
                "https://{}/api/v4/groups/{}/projects?per_page=100&statistics=true&include_subgroups={}&page=",
                source.host,
                source.namespace.replace('/', "%2F"),
                source.include_subgroups
            ),
            100,
        ),
        ("gitlab", "user") => (
            format!(
                "https://{}/api/v4/users/{}/projects?per_page=100&statistics=true&page=",
                source.host,
                source.namespace.replace('/', "%2F")
            ),
            100,
        ),
        (provider, kind) => {
            return Err(miette::miette!(
                "unsupported forge source {provider:?} namespace kind {kind:?}"
            ));
        }
    };
    let mut repositories = Vec::new();
    for page in 1..=100 {
        let url = format!("{endpoint}{page}");
        let page_repositories: Vec<RawRepository> = get(&url, source.token.as_deref())?
            .body_mut()
            .read_json()
            .map_err(|error| miette::miette!("parse forge response from {url}: {error}"))?;
        let count = page_repositories.len();
        repositories.extend(
            page_repositories
                .into_iter()
                .map(|raw| raw.normalize(source)),
        );
        if count < page_size {
            return Ok(repositories);
        }
    }
    Err(miette::miette!(
        "{endpoint}: more than 100 pages; refusing to return a partial inventory"
    ))
}

/// The token owner's GitHub login, or an empty string without a token.
fn github_login(source: &Source) -> Result<String> {
    #[derive(Deserialize)]
    struct User {
        login: String,
    }
    let Some(token) = source.token.as_deref() else {
        return Ok(String::new());
    };
    let url = "https://api.github.com/user";
    get(url, Some(token))?
        .body_mut()
        .read_json::<User>()
        .map(|user| user.login)
        .map_err(|error| miette::miette!("parse forge response from {url}: {error}"))
}

/// Fill [`Repository::pull_requests_total`] (Forgejo only).
pub fn pull_request_totals(source: &Source, repositories: &mut [Repository]) -> Result<()> {
    if source.provider != "forgejo" {
        return Err(miette::miette!(
            "pull request totals are only supported for Forgejo sources"
        ));
    }
    for repository in repositories {
        let url = format!(
            "https://{}/api/v1/repos/{}/pulls?state=all&limit=1",
            source.host, repository.full_name
        );
        let response = get(&url, source.token.as_deref())?;
        let total = response
            .headers()
            .get("x-total-count")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| miette::miette!("{url} returned no X-Total-Count"))?;
        repository.pull_requests_total = Some(total);
    }
    Ok(())
}

/// The authenticated user's Forgejo quota usage and rules, as returned by the forge.
pub fn quota(source: &Source) -> Result<serde_json::Value> {
    if source.provider != "forgejo" {
        return Err(miette::miette!(
            "quota is only supported for Forgejo sources"
        ));
    }
    let url = format!("https://{}/api/v1/user/quota", source.host);
    get(&url, source.token.as_deref())?
        .body_mut()
        .read_json()
        .map_err(|error| miette::miette!("parse forge response from {url}: {error}"))
}

fn get(url: &str, token: Option<&str>) -> Result<ureq::http::Response<ureq::Body>> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .new_agent();
    let mut request = agent
        .get(url)
        .header("Accept", "application/json")
        .header("User-Agent", "fleetix-forge/1");
    if let Some(token) = token {
        request = request.header("Authorization", &format!("Bearer {token}"));
    }
    request
        .call()
        .map_err(|error| miette::miette!("GET {url} failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use miette::IntoDiagnostic;

    fn source(provider: &str) -> Source {
        Source {
            provider: provider.into(),
            host: "forge.example".into(),
            ..Source::default()
        }
    }

    fn parse(provider: &str, json: &str) -> Repository {
        serde_json::from_str::<RawRepository>(json)
            .into_diagnostic()
            .unwrap()
            .normalize(&source(provider))
    }

    #[test]
    fn providers_normalize_size_visibility_and_fork() {
        let forgejo = parse(
            "forgejo",
            r#"{"name":"a","full_name":"me/a","size":2,"private":true,"mirror":true,
                "updated_at":"2026-01-02T00:00:00Z","ssh_url":"ssh://a"}"#,
        );
        assert_eq!(forgejo.size_bytes, Some(2048));
        assert!(forgejo.private && forgejo.mirror && !forgejo.fork);
        assert_eq!((forgejo.empty, forgejo.open_issues), (Some(false), Some(0)));
        assert_eq!(forgejo.ssh_url.as_deref(), Some("ssh://a"));

        let github = parse(
            "github",
            r#"{"name":"b","full_name":"me/b","size":3,"fork":true,
                "mirror_url":"https://up/b.git","open_issues_count":4}"#,
        );
        assert_eq!(
            (
                github.size_bytes,
                github.fork,
                github.private,
                github.mirror
            ),
            (Some(3072), true, false, true)
        );
        assert_eq!((github.empty, github.open_issues), (None, None));

        let gitlab = parse(
            "gitlab",
            r#"{"name":"c","path_with_namespace":"grp/c","visibility":"internal",
                "statistics":{"repository_size":5000},"forked_from_project":{},
                "last_activity_at":"2025-05-05T00:00:00Z","empty_repo":true}"#,
        );
        assert_eq!(gitlab.full_name, "grp/c");
        assert_eq!(gitlab.size_bytes, Some(5000));
        assert!(gitlab.private && gitlab.fork && gitlab.empty == Some(true));
        assert_eq!(gitlab.updated_at, "2025-05-05T00:00:00Z");

        let restricted = parse("gitlab", r#"{"name":"d","path_with_namespace":"grp/d"}"#);
        assert_eq!(restricted.size_bytes, None);
    }

    #[test]
    fn filter_combines_criteria() {
        let repository = Repository {
            full_name: "me/Big-Repo".into(),
            size_bytes: Some(100 << 20),
            private: true,
            archived: true,
            updated_at: "2026-03-01T10:00:00+02:00".into(),
            pull_requests_total: Some(0),
            ..Repository::default()
        };
        let matches = |filter: Filter| filter.matches(&repository);
        assert!(matches(Filter::default()));
        assert!(matches(Filter {
            min_bytes: Some(50 << 20),
            private: Some(true),
            archived: Some(true),
            fork: Some(false),
            updated_before: Some("2026-04-01".into()),
            name_contains: Some("big".into()),
            without_pull_requests: true,
            ..Filter::default()
        }));
        assert!(!matches(Filter {
            min_bytes: Some(200 << 20),
            ..Filter::default()
        }));
        assert!(!matches(Filter {
            archived: Some(false),
            ..Filter::default()
        }));
        assert!(!matches(Filter {
            mirror: Some(true),
            ..Filter::default()
        }));
        assert!(!matches(Filter {
            updated_before: Some("2026-03-01".into()),
            ..Filter::default()
        }));
        assert!(!matches(Filter {
            name_contains: Some("small".into()),
            ..Filter::default()
        }));

        let unknown_size = Repository {
            size_bytes: None,
            ..repository.clone()
        };
        assert!(
            !Filter {
                min_bytes: Some(1),
                ..Filter::default()
            }
            .matches(&unknown_size)
        );

        let unknown_pulls = Repository {
            pull_requests_total: None,
            ..repository.clone()
        };
        assert!(
            !Filter {
                without_pull_requests: true,
                ..Filter::default()
            }
            .matches(&unknown_pulls)
        );
    }
}
