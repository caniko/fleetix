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
        Self { code: EXIT_EVALUATION, report }
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
        Command::Validate { path, format, evaluator } => {
            let topology = topology::load_topology_with_options(&path, build_options(&evaluator)?).await?;
            let report = validate::validate(&topology);
            if format == OutputFormat::Json {
                println!("{}", serde_json::to_string_pretty(&report).map_err(|error| CliError::new(EXIT_EVALUATION, format!("serialize validation report: {error}")))?);
            } else {
                for error in &report.errors { eprintln!("  ERROR: {error}"); }
                for warning in &report.warnings { eprintln!("  WARN:  {warning}"); }
                if report.is_ok() { println!("Validation passed with {} warnings.", report.warnings.len()); }
            }
            if !report.is_ok() {
                return Err(CliError::new(EXIT_VALIDATION, format!("validation failed with {} errors and {} warnings", report.errors.len(), report.warnings.len())));
            }
        }
        Command::Show { path, host, format, evaluator } => {
            let topology = topology::load_topology_with_options(&path, build_options(&evaluator)?).await?;
            if let Some(host_name) = host {
                let host_data = topology.hosts.get(&host_name).ok_or_else(|| CliError::new(EXIT_NOT_FOUND, format!("host '{host_name}' not found")))?;
                if format == OutputFormat::Json {
                    println!("{}", serde_json::to_string_pretty(host_data).map_err(|error| CliError::new(EXIT_EVALUATION, format!("serialize host: {error}")))?);
                } else {
                    println!("Host '{host_name}':\n  system: {}\n  deviceType: {:?}\n  lanIp: {:?}\n  links: {}\n  users: {}\n  rebuild.buildHost: {:?}", host_data.system, host_data.device_type, host_data.network.lan_ip, host_data.links.keys().cloned().collect::<Vec<_>>().join(", "), host_data.users.keys().cloned().collect::<Vec<_>>().join(", "), host_data.rebuild.build_host);
                }
            } else if format == OutputFormat::Json {
                println!("{}", serde_json::to_string_pretty(&topology).map_err(|error| CliError::new(EXIT_EVALUATION, format!("serialize topology: {error}")))?);
            } else {
                println!("Fleet topology:");
                for (name, host_data) in &topology.hosts { println!("  {name:<12} system: {:<15} deviceType: {:?}", host_data.system, host_data.device_type); }
            }
        }
        Command::Links { path, name, format, evaluator } => {
            let topology = topology::load_topology_with_options(&path, build_options(&evaluator)?).await?;
            let names: Vec<String> = if let Some(name) = name {
                if !topology.links.contains_key(&name) { return Err(CliError::new(EXIT_NOT_FOUND, format!("link '{name}' not found"))); }
                vec![name]
            } else { topology.links.keys().cloned().collect() };
            let views: Vec<LinkView> = names.iter().map(|name| {
                let link = &topology.links[name];
                LinkView { name: name.clone(), subnet: link.subnet.clone(), port: link.port, cidr: topology.link_cidr(name).map(str::to_owned), server: topology.link_server_address(name).map(str::to_owned), dial: topology.link_dial(name) }
            }).collect();
            if format == OutputFormat::Json {
                println!("{}", serde_json::to_string_pretty(&views).map_err(|error| CliError::new(EXIT_EVALUATION, format!("serialize links: {error}")))?);
            } else {
                println!("Links:");
                for view in views { println!("  {:<15} cidr: {:<15} server: {}", view.name, view.cidr.as_deref().unwrap_or(""), view.server.as_deref().unwrap_or("(none)")); }
            }
        }
        Command::Eval { path, rkyv, evaluator } => {
            let options = build_options(&evaluator)?;
            if rkyv {
                let topology = topology::load_topology_with_options(&path, options).await?;
                let bytes = topology::archive_topology(&topology).map_err(|error| miette::miette!("create archive: {error}"))?;
                std::io::stdout().write_all(&bytes).map_err(|error| CliError::new(EXIT_IO, format!("write archive: {error}")))?;
            } else { println!("{}", eval_pkl_for_nix(&path, options).await?); }
        }
        Command::Export { path, output, evaluator } => {
            let options = build_options(&evaluator)?;
            let topology = topology::load_topology_with_options(&path, options.clone()).await?;
            let report = validate::validate(&topology);
            if !report.is_ok() { return Err(CliError::new(EXIT_VALIDATION, format!("refusing to export invalid topology: {} errors and {} warnings", report.errors.len(), report.warnings.len()))); }
            let nix = eval_pkl_for_nix(&path, options).await?;
            atomic_write(&output, format!("# Generated by fleetix; do not edit by hand.\n{nix}"))?;
            println!("Wrote {}", output.display());
        }
        Command::PklToNix { path, output, evaluator } => {
            let nix = eval_pkl_for_nix(&path, build_options(&evaluator)?).await?;
            atomic_write(&output, format!("# Generated by fleetix; do not edit by hand.\n{nix}"))?;
            println!("Wrote {}", output.display());
        }
    }
    Ok(())
}

fn atomic_write(path: &Path, contents: String) -> miette::Result<()> {
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| miette::miette!("create {}: {error}", parent.display()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| miette::miette!("create temporary output: {error}"))?;
    if let Ok(metadata) = fs::metadata(path) { temporary.as_file().set_permissions(metadata.permissions()).map_err(|error| miette::miette!("preserve {} permissions: {error}", path.display()))?; }
    temporary.write_all(contents.as_bytes()).map_err(|error| miette::miette!("write {}: {error}", path.display()))?;
    temporary.as_file().sync_all().map_err(|error| miette::miette!("sync {}: {error}", path.display()))?;
    temporary.persist(path).map_err(|error| miette::miette!("replace {}: {}", path.display(), error.error))?;
    #[cfg(unix)]
    fs::File::open(parent).and_then(|directory| directory.sync_all()).map_err(|error| miette::miette!("sync output directory {}: {error}", parent.display()))?;
    Ok(())
}

pub async fn main_exit(cli: Cli) -> ExitCode {
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => { eprintln!("{:?}", error.report); ExitCode::from(error.code) }
    }
}
