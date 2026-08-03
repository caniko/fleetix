use crate::topology;
use crate::validate;
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use pklx::pklr::EvalOptions;

pub const EXIT_EVALUATION: u8 = 1;
pub const EXIT_VALIDATION: u8 = 2;
pub const EXIT_IO: u8 = 3;
pub const EXIT_NOT_FOUND: u8 = 4;

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
    let mut options = EvalOptions::default();
    if !args.http_rewrite.is_empty() {
        options.http_rewrites.clone_from(&args.http_rewrite);
    }
    if let Some(proxy_url) = &args.http_proxy {
        let proxy = pklx::pklr::reqwest::Proxy::all(proxy_url)
            .map_err(|error| miette::miette!("invalid proxy URL '{proxy_url}': {error}"))?;
        options.client = Some(
            pklx::pklr::reqwest::Client::builder()
                .proxy(proxy)
                .build()
                .map_err(|error| miette::miette!("failed to build HTTP client: {error}"))?,
        );
    }
    Ok(options)
}

async fn eval_pkl_for_nix(path: &Path, options: EvalOptions) -> miette::Result<String> {
    if path.file_name().and_then(|name| name.to_str()) != Some("Topology.aggregated.pkl") {
        return pklx::eval_pkl(path, options).await;
    }

    let flattened = topology::flatten_modular_topology(path)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| miette::miette!("create temporary topology: {error}"))?;
    temporary
        .write_all(flattened.as_bytes())
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|error| miette::miette!("write temporary topology: {error}"))?;
    let nix = pklx::eval_pkl(temporary.path(), options).await?;
    Ok(wrap_topology_nix(nix))
}

fn wrap_topology_nix(nix: String) -> String {
    format!(
        "let\n  scrub = value:\n    if builtins.isAttrs value then\n      builtins.listToAttrs (\n        builtins.filter (entry: entry.value != null) (\n          builtins.map (name: {{ inherit name; value = scrub value.${{name}}; }})\n            (builtins.attrNames (builtins.removeAttrs value [\"__pkl_class\"]))\n        )\n      )\n    else if builtins.isList value then builtins.map scrub value\n    else value;\nin\n  scrub (\n{nix}\n  )\n"
    )
}

#[derive(Debug, Parser)]
#[command(name = "fleetix", about = "Fleet topology toolkit")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
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
    /// Evaluate a topology and print Nix or an rkyv archive.
    Eval {
        path: PathBuf,
        #[arg(long)]
        rkyv: bool,
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
        #[command(flatten)]
        evaluator: EvaluatorArgs,
    },
    /// Observe trust stores against the declared topology trust.
    #[command(subcommand)]
    Trust(TrustCommand),
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
        Command::Trust(subcommand) => run_trust(subcommand).await?,
        Command::Validate {
            path,
            format,
            evaluator,
        } => {
            let topology =
                topology::load_topology_with_options(&path, build_options(&evaluator)?).await?;
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
                for error in &report.errors {
                    eprintln!("  ERROR: {error}");
                }
                for warning in &report.warnings {
                    eprintln!("  WARN:  {warning}");
                }
                if report.is_ok() {
                    println!("Validation passed with {} warnings.", report.warnings.len());
                }
            }
            if !report.is_ok() {
                return Err(CliError::new(
                    EXIT_VALIDATION,
                    format!(
                        "validation failed with {} errors and {} warnings",
                        report.errors.len(),
                        report.warnings.len()
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
                    println!("Host '{host_name}':\n  system: {}\n  deviceType: {:?}\n  lanIp: {:?}\n  links: {}\n  users: {}\n  rebuild.buildHost: {:?}", host_data.system, host_data.device_type, host_data.network.lan_ip, host_data.links.keys().cloned().collect::<Vec<_>>().join(", "), host_data.users.keys().cloned().collect::<Vec<_>>().join(", "), host_data.rebuild.build_host);
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
        Command::Eval {
            path,
            rkyv,
            evaluator,
        } => {
            let options = build_options(&evaluator)?;
            if rkyv {
                let topology = topology::load_topology_with_options(&path, options).await?;
                let bytes = topology::archive_topology(&topology)
                    .map_err(|error| miette::miette!("create archive: {error}"))?;
                std::io::stdout()
                    .write_all(&bytes)
                    .map_err(|error| CliError::new(EXIT_IO, format!("write archive: {error}")))?;
            } else {
                println!("{}", eval_pkl_for_nix(&path, options).await?);
            }
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
                        report.errors.len(),
                        report.warnings.len()
                    ),
                ));
            }
            let nix = eval_pkl_for_nix(&path, build_options(&evaluator)?).await?;
            atomic_write(
                &output,
                format!("# Generated by fleetix; do not edit by hand.\n{nix}"),
            )?;
            println!("Wrote {}", output.display());
        }
        Command::PklToNix {
            path,
            output,
            evaluator,
        } => {
            let nix = eval_pkl_for_nix(&path, build_options(&evaluator)?).await?;
            atomic_write(
                &output,
                format!("# Generated by fleetix; do not edit by hand.\n{nix}"),
            )?;
            println!("Wrote {}", output.display());
        }
    }
    Ok(())
}

async fn run_trust(subcommand: TrustCommand) -> Result<(), CliError> {
    use crate::trust::{self, notify, openssh, state, DeclaredTrust};

    struct ScanOutcome {
        actionable: Vec<openssh::Entry>,
        report: trust::Observation,
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
        let report = trust::scan(known_hosts, &declared).map_err(CliError::from)?;
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

    fn json_out<T: serde::Serialize>(value: &T) -> Result<(), CliError> {
        println!(
            "{}",
            serde_json::to_string_pretty(value).map_err(|error| {
                CliError::new(EXIT_EVALUATION, format!("serialize: {error}"))
            })?
        );
        Ok(())
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

/// Patch Trust.pkl and regenerate the sidecar, rolling back on failure.
async fn run_integrate(
    id: String,
    topology_path: &Path,
    known_hosts: &Path,
    sidecar: Option<&Path>,
) -> Result<(), CliError> {
    use crate::trust::{self, openssh, patch, DeclaredTrust};

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
    let report = trust::scan(known_hosts, &declared).map_err(CliError::from)?;
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
        if let Err(error) = export_topology(topology_path, sidecar).await {
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
/// trust integrate).
async fn export_topology(path: &Path, output: &Path) -> miette::Result<()> {
    let topology = topology::load_topology(path).await?;
    let report = validate::validate(&topology);
    if !report.is_ok() {
        return Err(miette::miette!(
            "refusing to export invalid topology: {} errors",
            report.errors.len()
        ));
    }
    let nix = eval_pkl_for_nix(path, EvalOptions::default()).await?;
    atomic_write(
        output,
        format!("# Generated by fleetix; do not edit by hand.\n{nix}"),
    )
}

fn atomic_write(path: &Path, contents: String) -> miette::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|error| miette::miette!("create {}: {error}", parent.display()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| miette::miette!("create temporary output: {error}"))?;
    if let Ok(metadata) = fs::metadata(path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())
            .map_err(|error| miette::miette!("preserve {} permissions: {error}", path.display()))?;
    }
    temporary
        .write_all(contents.as_bytes())
        .map_err(|error| miette::miette!("write {}: {error}", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| miette::miette!("sync {}: {error}", path.display()))?;
    temporary
        .persist(path)
        .map_err(|error| miette::miette!("replace {}: {}", path.display(), error.error))?;
    #[cfg(unix)]
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| miette::miette!("sync output directory {}: {error}", parent.display()))?;
    Ok(())
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
