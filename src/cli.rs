use crate::fsutil::atomic_write;
use crate::pkl_to_nix;
use crate::topology;
use crate::validate;
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use pklx::pklr::EvalOptions;

pub const EXIT_EVALUATION: u8 = 1;
pub const EXIT_VALIDATION: u8 = 2;
pub const EXIT_IO: u8 = 3;
pub const EXIT_NOT_FOUND: u8 = 4;
pub const EXIT_BUILD_TRAIN: u8 = 5;

#[derive(Debug)]
pub struct CliError {
    pub code: u8,
    pub report: miette::Report,
}

impl CliError {
    fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            report: miette::miette!("{}", message.into()),
        }
    }
}

impl From<miette::Report> for CliError {
    fn from(report: miette::Report) -> Self {
        Self {
            code: EXIT_EVALUATION,
            report,
        }
    }
}

#[derive(Debug, Clone, Args, Default)]
pub struct EvaluatorArgs {
    /// HTTP URL rewrite rules in `source_prefix=target_prefix` format.
    #[arg(long = "http-rewrite")]
    pub http_rewrite: Vec<String>,
    /// HTTP proxy URL.
    #[arg(long = "http-proxy")]
    pub http_proxy: Option<String>,
}

fn build_options(args: &EvaluatorArgs) -> miette::Result<EvalOptions> {
    pkl_to_nix::options_with_http(args.http_rewrite.clone(), args.http_proxy.as_deref())
}

async fn eval_pkl_for_nix(path: &Path, options: EvalOptions) -> miette::Result<String> {
    pkl_to_nix::render(path, options).await
}

#[derive(Debug, Parser)]
#[command(name = "fleetix", about = "Fleet topology toolkit")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Shared dependency-aware native construction and coordinator recovery.
    #[cfg(all(unix, feature = "build-train-cli"))]
    #[command(subcommand)]
    BuildTrain(crate::build_train::cli::TrainCommand),
    /// Reconcile managed MCP entries into harness configuration files.
    Mcp {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        state: PathBuf,
        /// Inspect changes without writing configuration or state.
        #[arg(long)]
        dry_run: bool,
    },
    /// Validate a topology file for cross-reference invariants.
    Validate {
        path: PathBuf,
        #[arg(long, value_enum, default_value = "human")]
        format: OutputFormat,
        #[command(flatten)]
        evaluator: EvaluatorArgs,
    },
    /// Show a fleet overview or one host.
    Show {
        path: PathBuf,
        #[arg(short, long)]
        host: Option<String>,
        #[arg(long, value_enum, default_value = "human")]
        format: OutputFormat,
        #[command(flatten)]
        evaluator: EvaluatorArgs,
    },
    /// List links with derived values.
    Links {
        path: PathBuf,
        #[arg(short, long)]
        name: Option<String>,
        #[arg(long, value_enum, default_value = "human")]
        format: OutputFormat,
        #[command(flatten)]
        evaluator: EvaluatorArgs,
    },
    /// Evaluate a topology and print Nix.
    Eval {
        path: PathBuf,
        #[command(flatten)]
        evaluator: EvaluatorArgs,
    },
    /// Validate and atomically export a topology sidecar.
    Export {
        path: PathBuf,
        output: PathBuf,
        #[command(flatten)]
        evaluator: EvaluatorArgs,
    },
    /// Convert any Pkl value to an importable Nix expression.
    PklToNix {
        path: PathBuf,
        output: PathBuf,
        /// Reevaluate instead of reading or writing the persistent render cache.
        #[arg(long)]
        no_cache: bool,
        #[command(flatten)]
        evaluator: EvaluatorArgs,
    },
    /// Observe trust stores against the declared topology trust.
    #[command(subcommand)]
    Trust(TrustCommand),
    /// Query merged supply facts (see SUPPLY.md).
    #[command(subcommand)]
    Supply(SupplyCommand),
}

#[derive(Debug, Args)]
pub struct SupplyInput {
    /// Facts document, or a JSON array of documents (repeatable).
    #[arg(long, required = true)]
    pub facts: Vec<PathBuf>,
    /// Topology whose `supply` section joins runner and cache nodes.
    #[arg(long)]
    pub topology: Option<PathBuf>,
    /// Warn about documents observed more than this many days ago.
    #[arg(long)]
    pub max_age_days: Option<i64>,
}

#[derive(Debug, Subcommand)]
pub enum SupplyCommand {
    /// Report usages that no upgrade path covers, and why.
    Gaps {
        #[command(flatten)]
        input: SupplyInput,
        #[arg(long, value_enum, default_value = "blocker")]
        by: GapGrouping,
        #[arg(long, value_enum, default_value = "human")]
        format: OutputFormat,
    },
    /// Walk dependencies from a node id or a repository coordinate.
    Path {
        node: String,
        #[command(flatten)]
        input: SupplyInput,
        /// Follow edges backward: who pins a revision, who uses a runner.
        #[arg(long)]
        reverse: bool,
        #[arg(long, default_value_t = 2)]
        depth: usize,
        #[arg(long, value_enum, default_value = "human")]
        format: OutputFormat,
    },
    /// Print the merged graph as JSON.
    Graph {
        #[command(flatten)]
        input: SupplyInput,
    },
    /// Forecast topology agent-minute budgets and rank reductions.
    Budget {
        #[command(flatten)]
        input: SupplyInput,
        /// Planned updates per month for the qualification-run forecast.
        #[arg(long, default_value_t = 1.0)]
        updates_per_month: f64,
        /// Orchestrator kind whose enrolled repositories qualify each update.
        #[arg(long, default_value = "simit-updater")]
        orchestrator: String,
        #[arg(long, value_enum, default_value = "human")]
        format: OutputFormat,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum GapGrouping {
    Blocker,
    Repo,
}

#[derive(Debug, Subcommand)]
pub enum TrustCommand {
    /// Scan a trust store and report proposals, conflicts, and skipped lines.
    Scan {
        /// Modular topology entrypoint (Topology.aggregated.pkl).
        #[arg(long)]
        topology: PathBuf,
        /// Trust store to observe (default: ~/.ssh/known_hosts).
        #[arg(long)]
        known_hosts: Option<PathBuf>,
        /// Observer decisions directory.
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Offer existing undeclared entries on the first scan.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        review_existing: bool,
        /// Prompt for each proposal via notify-send actions.
        #[arg(long)]
        notify: bool,
        /// Sidecar to regenerate after an interactive integrate.
        #[arg(long)]
        sidecar: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// List actionable proposals only.
    Pending {
        #[arg(long)]
        topology: PathBuf,
        #[arg(long)]
        known_hosts: Option<PathBuf>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        review_existing: bool,
        #[arg(long)]
        json: bool,
    },
    /// Declare an observed entry in Trust.pkl and regenerate the sidecar.
    Integrate {
        /// Proposal id (see `fleetix trust scan`).
        id: String,
        #[arg(long)]
        topology: PathBuf,
        #[arg(long)]
        known_hosts: Option<PathBuf>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Sidecar to regenerate; omitted or empty disables regeneration.
        #[arg(long)]
        sidecar: Option<PathBuf>,
    },
    /// Suppress a proposal so it is never prompted again.
    Ignore {
        /// Proposal id (see `fleetix trust scan`).
        id: String,
        #[arg(long)]
        known_hosts: Option<PathBuf>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Human,
    Json,
}

#[derive(Serialize)]
struct LinkView {
    name: String,
    subnet: String,
    port: u16,
    cidr: Option<String>,
    server: Option<String>,
    dial: Option<String>,
}

pub async fn run(cli: Cli) -> Result<(), CliError> {
    match cli.command {
        #[cfg(all(unix, feature = "build-train-cli"))]
        Command::BuildTrain(command) => crate::build_train::cli::run(command)
            .map_err(|error| CliError::new(EXIT_BUILD_TRAIN, error))?,
        Command::Mcp {
            manifest,
            state,
            dry_run,
        } => {
            let bytes = std::fs::read(&manifest)
                .map_err(|error| CliError::new(EXIT_IO, error.to_string()))?;
            let manifest = serde_json::from_slice(&bytes)
                .map_err(|error| CliError::new(EXIT_VALIDATION, error.to_string()))?;
            let report = crate::mcp::reconcile(&manifest, &state, dry_run)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&report)
                    .map_err(|error| CliError::new(EXIT_IO, error.to_string()))?
            );
        }
        Command::Trust(subcommand) => run_trust(subcommand).await?,
        Command::Supply(subcommand) => run_supply(subcommand).await?,
        Command::Validate {
            path,
            format,
            evaluator,
        } => {
            let topology =
                topology::evaluate_topology_with_options(&path, build_options(&evaluator)?).await?;
            let report = validate::validate(&topology);
            if format == OutputFormat::Json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|error| CliError::new(
                        EXIT_EVALUATION,
                        format!("serialize validation report: {error}")
                    ))?
                );
            } else {
                for issue in report.issues() {
                    match issue.severity {
                        validate::ValidationSeverity::Error => {
                            eprintln!("  ERROR: {}", issue.message);
                        }
                        validate::ValidationSeverity::Warning => {
                            eprintln!("  WARN:  {}", issue.message);
                        }
                    }
                }
                if report.is_ok() {
                    println!(
                        "Validation passed with {} warnings.",
                        report.warnings().count()
                    );
                }
            }
            if !report.is_ok() {
                return Err(CliError::new(
                    EXIT_VALIDATION,
                    format!(
                        "validation failed with {} errors and {} warnings",
                        report.errors().count(),
                        report.warnings().count()
                    ),
                ));
            }
        }
        Command::Show {
            path,
            host,
            format,
            evaluator,
        } => {
            let topology =
                topology::load_topology_with_options(&path, build_options(&evaluator)?).await?;
            if let Some(host_name) = host {
                let host_data = topology.hosts.get(&host_name).ok_or_else(|| {
                    CliError::new(EXIT_NOT_FOUND, format!("host '{host_name}' not found"))
                })?;
                if format == OutputFormat::Json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(host_data).map_err(|error| CliError::new(
                            EXIT_EVALUATION,
                            format!("serialize host: {error}")
                        ))?
                    );
                } else {
                    println!(
                        "Host '{host_name}':\n  system: {}\n  deviceType: {:?}\n  lanIp: {:?}\n  links: {}\n  users: {}\n  rebuild.buildHost: {:?}",
                        host_data.system,
                        host_data.device_type,
                        host_data.network.lan_ip,
                        host_data
                            .links
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", "),
                        host_data
                            .users
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", "),
                        host_data.rebuild.build_host
                    );
                }
            } else if format == OutputFormat::Json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&topology).map_err(|error| CliError::new(
                        EXIT_EVALUATION,
                        format!("serialize topology: {error}")
                    ))?
                );
            } else {
                println!("Fleet topology:");
                for (name, host_data) in &topology.hosts {
                    println!(
                        "  {name:<12} system: {:<15} deviceType: {:?}",
                        host_data.system, host_data.device_type
                    );
                }
            }
        }
        Command::Links {
            path,
            name,
            format,
            evaluator,
        } => {
            let topology =
                topology::load_topology_with_options(&path, build_options(&evaluator)?).await?;
            let names: Vec<String> = if let Some(name) = name {
                if !topology.links.contains_key(&name) {
                    return Err(CliError::new(
                        EXIT_NOT_FOUND,
                        format!("link '{name}' not found"),
                    ));
                }
                vec![name]
            } else {
                topology.links.keys().cloned().collect()
            };
            let views: Vec<LinkView> = names
                .iter()
                .map(|name| {
                    let link = &topology.links[name];
                    LinkView {
                        name: name.clone(),
                        subnet: link.subnet.clone(),
                        port: link.port,
                        cidr: topology.link_cidr(name).map(str::to_owned),
                        server: topology.link_server_address(name).map(str::to_owned),
                        dial: topology.link_dial(name),
                    }
                })
                .collect();
            if format == OutputFormat::Json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&views).map_err(|error| CliError::new(
                        EXIT_EVALUATION,
                        format!("serialize links: {error}")
                    ))?
                );
            } else {
                println!("Links:");
                for view in views {
                    println!(
                        "  {:<15} cidr: {:<15} server: {}",
                        view.name,
                        view.cidr.as_deref().unwrap_or(""),
                        view.server.as_deref().unwrap_or("(none)")
                    );
                }
            }
        }
        Command::Eval { path, evaluator } => {
            println!(
                "{}",
                eval_pkl_for_nix(&path, build_options(&evaluator)?).await?
            );
        }
        Command::Export {
            path,
            output,
            evaluator,
        } => {
            let options = build_options(&evaluator)?;
            let topology = topology::load_topology_with_options(&path, options).await?;
            let report = validate::validate(&topology);
            if !report.is_ok() {
                return Err(CliError::new(
                    EXIT_VALIDATION,
                    format!(
                        "refusing to export invalid topology: {} errors and {} warnings",
                        report.errors().count(),
                        report.warnings().count()
                    ),
                ));
            }
            export_topology(&path, &output, &evaluator).await?;
            println!("Wrote {}", output.display());
        }
        Command::PklToNix {
            path,
            output,
            no_cache,
            evaluator,
        } => {
            pkl_to_nix::write_with_cache(
                &path,
                &output,
                build_options(&evaluator)?,
                pkl_to_nix::CacheOptions {
                    enabled: !no_cache,
                    directory: None,
                },
            )
            .await?;
            println!("Wrote {}", output.display());
        }
    }
    Ok(())
}

async fn run_trust(subcommand: TrustCommand) -> Result<(), CliError> {
    use crate::trust::{DeclaredTrust, Observation, notify, openssh, state};

    struct ScanOutcome {
        actionable: Vec<openssh::Entry>,
        report: Observation,
    }

    fn resolve_known_hosts(path: Option<PathBuf>) -> miette::Result<PathBuf> {
        match path {
            Some(path) => Ok(path),
            None => {
                let home = std::env::var_os("HOME").ok_or_else(|| {
                    miette::miette!("HOME is unset; pass --known-hosts explicitly")
                })?;
                Ok(PathBuf::from(home).join(".ssh/known_hosts"))
            }
        }
    }

    fn resolve_state_dir(path: Option<PathBuf>) -> miette::Result<PathBuf> {
        Ok(path.unwrap_or_else(state::default_state_dir))
    }

    /// Re-run the scan and return the actionable (non-suppressed) proposals.
    /// When no decisions exist yet and reviewExisting is off, the current
    /// undeclared entries become the silent baseline.
    async fn actionable_proposals(
        topology_path: &Path,
        known_hosts: &Path,
        state_dir: &Path,
        review_existing: bool,
    ) -> Result<ScanOutcome, CliError> {
        let topology = topology::load_topology(topology_path)
            .await
            .map_err(CliError::from)?;
        let declared = DeclaredTrust::from_topology(&topology);
        let report = openssh::OpenSshKnownHosts
            .observe(known_hosts, &declared)
            .map_err(CliError::from)?;
        let existing = state::State::load(state_dir).map_err(CliError::from)?;

        if !review_existing && existing.is_none() {
            let mut baseline = state::State::new();
            for proposal in &report.proposals {
                baseline.suppress(&proposal.id);
            }
            baseline.save(state_dir).map_err(CliError::from)?;
            return Ok(ScanOutcome {
                actionable: Vec::new(),
                report,
            });
        }

        let actionable = report
            .proposals
            .iter()
            .filter(|entry| {
                existing
                    .as_ref()
                    .is_none_or(|state| !state.is_suppressed(&entry.id))
            })
            .cloned()
            .collect();
        Ok(ScanOutcome { actionable, report })
    }

    match subcommand {
        TrustCommand::Scan {
            topology,
            known_hosts,
            state_dir,
            review_existing,
            notify: notify_mode,
            sidecar,
            json,
        } => {
            let known_hosts = resolve_known_hosts(known_hosts)?;
            let state_dir = resolve_state_dir(state_dir)?;
            let ScanOutcome {
                actionable, report, ..
            } = actionable_proposals(&topology, &known_hosts, &state_dir, review_existing).await?;

            if json {
                #[derive(serde::Serialize)]
                struct ScanJson {
                    proposals: Vec<openssh::Entry>,
                    conflicts: Vec<openssh::Entry>,
                    skipped: Vec<String>,
                }
                return json_out(&ScanJson {
                    proposals: actionable,
                    conflicts: report.conflicts,
                    skipped: report.skipped,
                });
            }

            println!(
                "{} proposal(s), {} conflict(s), {} skipped line(s)",
                actionable.len(),
                report.conflicts.len(),
                report.skipped.len()
            );
            for entry in &report.conflicts {
                eprintln!(
                    "  CONFLICT: {} hostnames are declared with a different key",
                    entry.host_names.join(", ")
                );
            }
            for skip in &report.skipped {
                eprintln!("  SKIP: {skip}");
            }

            if !notify_mode {
                for entry in &actionable {
                    println!(
                        "  {}  {}  {}",
                        entry.id,
                        entry.host_names.join(","),
                        entry.key_text
                    );
                }
                return Ok(());
            }

            for entry in &actionable {
                match notify::prompt(entry) {
                    Ok(Some(notify::Action::Integrate)) => {
                        run_integrate(
                            entry.id.clone(),
                            &topology,
                            &known_hosts,
                            sidecar.as_deref(),
                        )
                        .await?;
                    }
                    Ok(Some(notify::Action::Ignore)) => {
                        let mut state = state::State::load(&state_dir)?.unwrap_or_default();
                        state.suppress(&entry.id);
                        state.save(&state_dir)?;
                        println!("Ignored {}", entry.id);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        eprintln!(
                            "cannot prompt for {}: {error}; run: fleetix trust integrate {} --topology {}",
                            entry.host_names.join(","),
                            entry.id,
                            topology.display()
                        );
                    }
                }
            }
            Ok(())
        }
        TrustCommand::Pending {
            topology,
            known_hosts,
            state_dir,
            review_existing,
            json,
        } => {
            let known_hosts = resolve_known_hosts(known_hosts)?;
            let state_dir = resolve_state_dir(state_dir)?;
            let ScanOutcome {
                actionable, report, ..
            } = actionable_proposals(&topology, &known_hosts, &state_dir, review_existing).await?;
            if json {
                json_out(&actionable)?;
            } else {
                for entry in &actionable {
                    println!(
                        "{}  {}  {}{}",
                        entry.id,
                        entry.host_names.join(","),
                        entry.key_text,
                        entry
                            .fingerprint
                            .as_ref()
                            .map(|fingerprint| format!("  {fingerprint}"))
                            .unwrap_or_default()
                    );
                }
                if report.conflicts.is_empty() && report.skipped.is_empty() {
                    println!("No pending trust proposals.");
                }
            }
            Ok(())
        }
        TrustCommand::Integrate {
            id,
            topology,
            known_hosts,
            sidecar,
            ..
        } => {
            let known_hosts = resolve_known_hosts(known_hosts)?;
            run_integrate(id, &topology, &known_hosts, sidecar.as_deref()).await
        }
        TrustCommand::Ignore {
            id,
            known_hosts,
            state_dir,
        } => {
            let known_hosts = resolve_known_hosts(known_hosts)?;
            let state_dir = resolve_state_dir(state_dir)?;
            let (entries, _) = openssh::parse_entries(&known_hosts).map_err(CliError::from)?;
            if !entries.iter().any(|entry| entry.id == id) {
                return Err(CliError::new(
                    EXIT_NOT_FOUND,
                    format!("no known_hosts entry with id '{id}'"),
                ));
            }
            let mut state = state::State::load(&state_dir)?.unwrap_or_default();
            state.suppress(&id);
            state.save(&state_dir)?;
            println!("Ignored {id}");
            Ok(())
        }
    }
}

fn json_out<T: Serialize>(value: &T) -> Result<(), CliError> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|error| { CliError::new(EXIT_EVALUATION, format!("serialize: {error}")) })?
    );
    Ok(())
}

/// Print a Markdown table, escaping cell pipes and newlines.
fn markdown_table(headers: &[&str], rows: impl IntoIterator<Item = Vec<String>>) {
    println!("| {} |", headers.join(" | "));
    println!("|{}", " --- |".repeat(headers.len()));
    for row in rows {
        let cells: Vec<_> = row
            .iter()
            .map(|cell| cell.replace('|', "\\|").replace('\n', " "))
            .collect();
        println!("| {} |", cells.join(" | "));
    }
}

async fn run_supply(subcommand: SupplyCommand) -> Result<(), CliError> {
    use crate::supply;

    async fn load(input: &SupplyInput) -> Result<supply::Graph, CliError> {
        let mut docs = Vec::new();
        for path in &input.facts {
            let text = std::fs::read_to_string(path).map_err(|error| {
                CliError::new(EXIT_IO, format!("read {}: {error}", path.display()))
            })?;
            docs.extend(supply::parse(&text).map_err(|error| {
                CliError::new(EXIT_VALIDATION, format!("{}: {error}", path.display()))
            })?);
        }
        if let Some(limit) = input.max_age_days {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs());
            for doc in &docs {
                match supply::age_days(&doc.observed_at, now) {
                    Some(age) if age <= limit => {}
                    Some(age) => eprintln!(
                        "  WARN:  {} facts observed at {} are {age} days old (limit {limit})",
                        doc.producer.name, doc.observed_at
                    ),
                    None => eprintln!(
                        "  WARN:  {} facts have an unreadable observedAt '{}'",
                        doc.producer.name, doc.observed_at
                    ),
                }
            }
        }
        let topology = match &input.topology {
            Some(path) => Some(topology::load_topology(path).await?),
            None => None,
        };
        Ok(supply::build(&docs, topology.as_ref()))
    }

    match subcommand {
        SupplyCommand::Gaps { input, by, format } => {
            let report = supply::gaps(&load(&input).await?);
            if format == OutputFormat::Json {
                return json_out(&report);
            }
            let summary = &report.summary;
            println!(
                "{} repositories, {} usages, {} covered; drift: {} covered, {} uncovered.\n",
                summary.repositories,
                summary.usages,
                summary.covered,
                summary.drift.covered,
                summary.drift.uncovered
            );
            let mut gaps: Vec<_> = report.gaps.iter().collect();
            match by {
                GapGrouping::Blocker => {
                    markdown_table(
                        &["Blocker", "Gaps", "Repositories", "Usages"],
                        summary.by_blocker.iter().map(|count| {
                            vec![
                                count.blocker.clone(),
                                count.gaps.to_string(),
                                count.repositories.to_string(),
                                count.usages.to_string(),
                            ]
                        }),
                    );
                    gaps.sort_by(|a, b| a.blocker.cmp(&b.blocker));
                }
                GapGrouping::Repo => markdown_table(
                    &["Repository", "Gaps", "Blockers"],
                    summary.by_repository.iter().map(|count| {
                        vec![
                            count.repository.clone(),
                            count.gaps.to_string(),
                            count.blockers.join(", "),
                        ]
                    }),
                ),
            }
            println!();
            markdown_table(
                &["Repository", "Node", "Blocker", "Reason", "Owner"],
                gaps.iter().map(|gap| {
                    vec![
                        gap.repository.clone().unwrap_or_default(),
                        gap.node.clone(),
                        match &gap.subcode {
                            Some(code) => format!("{}({code})", gap.blocker),
                            None => gap.blocker.clone(),
                        },
                        gap.reason.clone(),
                        gap.owner.clone().unwrap_or_default(),
                    ]
                }),
            );
            if !report.errors.is_empty() {
                println!();
                markdown_table(
                    &["Producer", "Subject", "Code", "Message"],
                    report.errors.iter().map(|error| {
                        vec![
                            error.producer.clone(),
                            error.value.subject.clone(),
                            error.value.code.clone(),
                            error.value.message.clone(),
                        ]
                    }),
                );
            }
            Ok(())
        }
        SupplyCommand::Path {
            node,
            input,
            reverse,
            depth,
            format,
        } => {
            let report = supply::walk(&load(&input).await?, &node, reverse, depth)
                .map_err(|error| CliError::new(EXIT_NOT_FOUND, error.to_string()))?;
            if format == OutputFormat::Json {
                return json_out(&report);
            }
            markdown_table(
                &["Depth", "Node", "Kind"],
                report.nodes.iter().map(|node| {
                    vec![
                        node.depth.to_string(),
                        node.id.clone(),
                        node.kind.clone().unwrap_or_default(),
                    ]
                }),
            );
            println!();
            markdown_table(
                &["From", "Edge", "To"],
                report
                    .edges
                    .iter()
                    .map(|edge| vec![edge.from.clone(), edge.kind.clone(), edge.to.clone()]),
            );
            Ok(())
        }
        SupplyCommand::Graph { input } => {
            let mut graph = serde_json::to_value(load(&input).await?)
                .map_err(|error| CliError::new(EXIT_EVALUATION, format!("serialize: {error}")))?;
            graph["contract"] = supply::GRAPH_CONTRACT.into();
            json_out(&graph)
        }
        SupplyCommand::Budget {
            input,
            updates_per_month,
            orchestrator,
            format,
        } => {
            let report = supply::budget(&load(&input).await?, &orchestrator, updates_per_month)
                .map_err(|error| CliError::new(EXIT_VALIDATION, error.to_string()))?;
            if format == OutputFormat::Json {
                return json_out(&report);
            }
            for forecast in &report.budgets {
                println!(
                    "{}: forecast {} of {} minutes ({}x, {}); observed {}, updater qualification {}.\n",
                    forecast.name,
                    forecast.forecast_minutes,
                    forecast.minutes_per_month,
                    forecast.utilization,
                    if forecast.within_budget {
                        "within budget"
                    } else {
                        "over budget"
                    },
                    forecast.observed_minutes,
                    forecast.updater_minutes
                );
                markdown_table(
                    &["Rule", "Node", "Savings", "Reason"],
                    forecast.candidates.iter().map(|candidate| {
                        vec![
                            candidate.rule.to_string(),
                            candidate.node.clone(),
                            candidate.estimated_savings.to_string(),
                            candidate.reason.clone(),
                        ]
                    }),
                );
                println!();
            }
            Ok(())
        }
    }
}

/// Patch Trust.pkl and regenerate the sidecar, rolling back on failure.
async fn run_integrate(
    id: String,
    topology_path: &Path,
    known_hosts: &Path,
    sidecar: Option<&Path>,
) -> Result<(), CliError> {
    use crate::trust::{DeclaredTrust, openssh, patch};

    let (entries, _) = openssh::parse_entries(known_hosts).map_err(CliError::from)?;
    let entry = entries
        .iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| {
            CliError::new(
                EXIT_NOT_FOUND,
                format!("no known_hosts entry with id '{id}'"),
            )
        })?
        .clone();

    let topology = topology::load_topology(topology_path)
        .await
        .map_err(CliError::from)?;
    let declared = DeclaredTrust::from_topology(&topology);
    let report = openssh::OpenSshKnownHosts
        .observe(known_hosts, &declared)
        .map_err(CliError::from)?;
    if !report
        .proposals
        .iter()
        .any(|proposal| proposal.id == entry.id)
    {
        return Err(CliError::new(
            EXIT_EVALUATION,
            format!("entry '{id}' is not an actionable proposal (already declared or conflicting)"),
        ));
    }

    let trust_pkl = topology_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("Trust.pkl");
    let previous = patch::patch_trust_pkl(&trust_pkl, &entry).map_err(CliError::from)?;

    if let Some(sidecar) = sidecar {
        if let Err(error) = export_topology(topology_path, sidecar, &EvaluatorArgs::default()).await
        {
            patch::restore_trust_pkl(&trust_pkl, previous.as_deref()).map_err(CliError::from)?;
            return Err(CliError::new(
                EXIT_EVALUATION,
                format!("sidecar regeneration failed, rolled back Trust.pkl: {error}"),
            ));
        }
    }
    println!(
        "Declared {} for {} in {}",
        entry.key_text,
        entry.host_names.join(","),
        trust_pkl.display()
    );
    if sidecar.is_none() {
        println!("No --sidecar given; regenerate the sidecar manually if this repo has one.");
    }
    Ok(())
}

/// Validate and atomically export a topology sidecar (shared by Export and
/// trust integrate). `evaluator` are the raw CLI args; options are built
/// inside so the non-cloneable [`EvalOptions`] can be used twice.
async fn export_topology(
    path: &Path,
    output: &Path,
    evaluator: &EvaluatorArgs,
) -> miette::Result<()> {
    let topology = topology::load_topology_with_options(path, build_options(evaluator)?).await?;
    let report = validate::validate(&topology);
    if !report.is_ok() {
        return Err(miette::miette!(
            "refusing to export invalid topology: {} errors",
            report.errors().count()
        ));
    }
    let nix = eval_pkl_for_nix(path, build_options(evaluator)?).await?;
    atomic_write(
        output,
        format!("# Generated by fleetix; do not edit by hand.\n{nix}").as_bytes(),
    )
}

pub async fn main_exit(cli: Cli) -> ExitCode {
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{:?}", error.report);
            ExitCode::from(error.code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn supply(args: &[&str]) -> Result<(), CliError> {
        let argv = ["fleetix", "supply"].iter().chain(args);
        run(Cli::try_parse_from(argv).unwrap()).await
    }

    #[tokio::test]
    async fn supply_commands_map_failures_to_exit_codes() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/supply");
        let fixture = |name: &str| fixtures.join(name).display().to_string();
        let (simit, overlay) = (
            fixture("simit-usage-facts.v1.json"),
            fixture("canix-overlay.v1.json"),
        );
        for by in ["blocker", "repo"] {
            let args = ["gaps", "--facts", &simit, "--facts", &overlay, "--by", by];
            supply(&args).await.unwrap();
            supply(&[&args[..], &["--max-age-days", "1"]].concat())
                .await
                .unwrap();
        }
        supply(&["path", "github.com/example/drifted", "--facts", &simit])
            .await
            .unwrap();
        supply(&["graph", "--facts", &simit]).await.unwrap();
        let budget_facts = fixture("budget-facts.v1.json");
        let budget_topology = fixture("budget-topology.pkl");
        let budget = ["budget", "--facts", &budget_facts];
        supply(&[&budget[..], &["--topology", &budget_topology]].concat())
            .await
            .unwrap();
        assert_eq!(supply(&budget).await.unwrap_err().code, EXIT_VALIDATION);

        let code = |result: Result<(), CliError>| result.unwrap_err().code;
        let missing = supply(&["path", "repo:missing", "--facts", &simit]).await;
        assert_eq!(code(missing), EXIT_NOT_FOUND);
        let unreadable = supply(&["graph", "--facts", "/nonexistent/facts.json"]).await;
        assert_eq!(code(unreadable), EXIT_IO);
        let not_facts = supply(&["gaps", "--facts", &fixture("expected-gaps.json")]).await;
        assert_eq!(code(not_facts), EXIT_VALIDATION);
        assert!(Cli::try_parse_from(["fleetix", "supply", "gaps"]).is_err());
    }

    #[test]
    fn trust_commands_accept_explicit_review_existing_values() {
        let scan = Cli::try_parse_from([
            "fleetix",
            "trust",
            "scan",
            "--topology",
            "Topology.pkl",
            "--review-existing",
            "false",
        ])
        .unwrap();
        assert!(matches!(
            scan.command,
            Command::Trust(TrustCommand::Scan {
                review_existing: false,
                ..
            })
        ));

        let pending = Cli::try_parse_from([
            "fleetix",
            "trust",
            "pending",
            "--topology",
            "Topology.pkl",
            "--review-existing",
            "true",
        ])
        .unwrap();
        assert!(matches!(
            pending.command,
            Command::Trust(TrustCommand::Pending {
                review_existing: true,
                ..
            })
        ));
    }
}
