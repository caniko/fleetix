use crate::topology;
use crate::validate;
use clap::Parser;
use std::io::Write;
use std::path::{Path, PathBuf};

use pklx::pklr::EvalOptions;

fn build_options(
    http_rewrite: Vec<String>,
    http_proxy: Option<String>,
) -> miette::Result<EvalOptions> {
    let mut options = EvalOptions::default();
    if !http_rewrite.is_empty() {
        options.http_rewrites = http_rewrite;
    }
    if let Some(proxy_url) = http_proxy {
        let proxy = pklx::pklr::reqwest::Proxy::all(&proxy_url)
            .map_err(|e| miette::miette!("Invalid proxy URL '{}': {}", proxy_url, e))?;
        let client = pklx::pklr::reqwest::Client::builder()
            .proxy(proxy)
            .build()
            .map_err(|e| miette::miette!("Failed to build HTTP client: {}", e))?;
        options.client = Some(client);
    }
    Ok(options)
}

async fn eval_pkl_for_nix(path: &Path, options: EvalOptions) -> miette::Result<String> {
    if path.file_name().and_then(|name| name.to_str()) != Some("Topology.aggregated.pkl") {
        return pklx::eval_pkl(path, options).await;
    }

    let flattened = topology::flatten_modular_topology(path)?;
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| miette::miette!("build temporary topology filename: {e}"))?
        .as_nanos();
    let tmp_path = std::env::temp_dir().join(format!(
        "fleetix-topology-{}-{unique}.pkl",
        std::process::id()
    ));
    std::fs::write(&tmp_path, flattened)
        .map_err(|e| miette::miette!("write temporary topology {}: {e}", tmp_path.display()))?;

    let result = pklx::eval_pkl(&tmp_path, options).await;
    let _ = std::fs::remove_file(&tmp_path);
    result.map(wrap_topology_nix)
}

fn wrap_topology_nix(nix: String) -> String {
    format!(
        r#"let
  scrub = value:
    if builtins.isAttrs value then
      builtins.listToAttrs (
        builtins.filter (entry: entry.value != null) (
          builtins.map (name: {{
            inherit name;
            value = scrub value.${{name}};
          }}) (builtins.attrNames (builtins.removeAttrs value ["__pkl_class"]))
        )
      )
    else if builtins.isList value then
      builtins.map scrub value
    else
      value;
in
  scrub (
{nix}
  )
"#
    )
}

#[derive(Parser)]
#[command(name = "fleetix", about = "Fleet topology toolkit")]
pub enum Cli {
    /// Validate a topology file for cross-reference invariants
    Validate {
        /// Path to the .pkl topology file
        path: PathBuf,
        /// HTTP URL rewrite rules in "source_prefix=target_prefix" format
        #[arg(long = "http-rewrite")]
        http_rewrite: Vec<String>,
        /// HTTP proxy URL (e.g. http://proxy:8080)
        #[arg(long = "http-proxy")]
        http_proxy: Option<String>,
    },
    /// Show a human-readable fleet overview
    Show {
        /// Path to the .pkl topology file
        path: PathBuf,
        /// Filter to a single host
        #[arg(short, long)]
        host: Option<String>,
        /// HTTP URL rewrite rules in "source_prefix=target_prefix" format
        #[arg(long = "http-rewrite")]
        http_rewrite: Vec<String>,
        /// HTTP proxy URL (e.g. http://proxy:8080)
        #[arg(long = "http-proxy")]
        http_proxy: Option<String>,
    },
    /// List links with derived values
    Links {
        /// Path to the .pkl topology file
        path: PathBuf,
        /// Filter to a single link
        #[arg(short, long)]
        name: Option<String>,
        /// HTTP URL rewrite rules in "source_prefix=target_prefix" format
        #[arg(long = "http-rewrite")]
        http_rewrite: Vec<String>,
        /// HTTP proxy URL (e.g. http://proxy:8080)
        #[arg(long = "http-proxy")]
        http_proxy: Option<String>,
    },
    /// Evaluate topology and output a Nix expression
    Eval {
        /// Path to the .pkl topology file
        path: PathBuf,
        /// Output an rkyv archive for zero-copy access instead of Nix
        #[arg(long)]
        rkyv: bool,
        /// HTTP URL rewrite rules in "source_prefix=target_prefix" format
        #[arg(long = "http-rewrite")]
        http_rewrite: Vec<String>,
        /// HTTP proxy URL (e.g. http://proxy:8080)
        #[arg(long = "http-proxy")]
        http_proxy: Option<String>,
    },
    /// Export topology sidecars from a topology file
    Export {
        /// Path to the .pkl topology file
        path: PathBuf,
        /// Output Nix sidecar path
        output: PathBuf,
        /// HTTP URL rewrite rules in "source_prefix=target_prefix" format
        #[arg(long = "http-rewrite")]
        http_rewrite: Vec<String>,
        /// HTTP proxy URL (e.g. http://proxy:8080)
        #[arg(long = "http-proxy")]
        http_proxy: Option<String>,
    },
}

pub async fn run(cli: Cli) -> miette::Result<()> {
    match cli {
        Cli::Validate {
            path,
            http_rewrite,
            http_proxy,
        } => {
            let options = build_options(http_rewrite, http_proxy)?;
            let topo = topology::load_topology_with_options(&path, options).await?;
            let report = validate::validate(&topo);
            for err in &report.errors {
                eprintln!("  ERROR: {err}");
            }
            for warn in &report.warnings {
                eprintln!("  WARN:  {warn}");
            }
            if report.is_ok() {
                println!("Validation passed with {} warnings.", report.warnings.len());
            } else {
                eprintln!(
                    "Validation FAILED with {} errors and {} warnings.",
                    report.errors.len(),
                    report.warnings.len()
                );
                std::process::exit(1);
            }
        }

        Cli::Show {
            path,
            host,
            http_rewrite,
            http_proxy,
        } => {
            let options = build_options(http_rewrite, http_proxy)?;
            let topo = topology::load_topology_with_options(&path, options).await?;
            if let Some(h) = host {
                match topo.hosts.get(&h) {
                    Some(hdata) => println!(
                        "Host '{}':\n  system: {}\n  deviceType: {:?}\n  lanIp: {:?}\n  links: {}\n  users: {}\n  rebuild.buildHost: {:?}",
                        h,
                        hdata.system,
                        hdata.device_type,
                        hdata.network.lan_ip,
                        hdata.links.keys().cloned().collect::<Vec<_>>().join(", "),
                        hdata.users.keys().cloned().collect::<Vec<_>>().join(", "),
                        hdata.rebuild.build_host,
                    ),
                    None => println!("Host '{}' not found.", h),
                }
            } else {
                println!("Fleet topology:");
                for (name, hdata) in &topo.hosts {
                    println!(
                        "  {:<12} system: {:<15} deviceType: {:?}",
                        name, hdata.system, hdata.device_type
                    );
                }
            }
        }

        Cli::Links {
            path,
            name,
            http_rewrite,
            http_proxy,
        } => {
            let options = build_options(http_rewrite, http_proxy)?;
            let topo = topology::load_topology_with_options(&path, options).await?;
            if let Some(n) = name {
                match topo.links.get(&n) {
                    Some(link) => {
                        let cidr = topo.link_cidr(&n).unwrap_or("");
                        let server = topo.link_server_address(&n).unwrap_or("(none)");
                        let dial = topo.link_dial(&n).unwrap_or_else(|| "(no dial)".into());
                        println!(
                            "Link '{}':\n  subnet: {}\n  port: {}\n  cidr: {}\n  server: {}\n  dial: {}",
                            n, link.subnet, link.port, cidr, server, dial
                        );
                    }
                    None => println!("Link '{}' not found.", n),
                }
            } else {
                println!("Links:");
                for (name, _link) in &topo.links {
                    let cidr = topo.link_cidr(name).unwrap_or("");
                    let server = topo.link_server_address(name).unwrap_or("(none)");
                    println!("  {:<15} cidr: {:<15} server: {}", name, cidr, server);
                }
            }
        }

        Cli::Eval {
            path,
            rkyv,
            http_rewrite,
            http_proxy,
        } => {
            let options = build_options(http_rewrite, http_proxy)?;
            if rkyv {
                let topo = topology::load_topology_with_options(&path, options).await?;
                let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&topo)
                    .map_err(|e| miette::miette!("Failed to create rkyv archive: {e}"))?;
                std::io::stdout()
                    .write_all(&bytes)
                    .map_err(|e| miette::miette!("Failed to write output: {e}"))?;
            } else {
                let nix = eval_pkl_for_nix(&path, options).await?;
                println!("{nix}");
            }
        }

        Cli::Export {
            path,
            output,
            http_rewrite,
            http_proxy,
        } => {
            let options = build_options(http_rewrite, http_proxy)?;
            let nix = eval_pkl_for_nix(&path, options).await?;
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| miette::miette!("create {}: {e}", parent.display()))?;
            }
            std::fs::write(
                &output,
                format!(
                    "# Generated from {}; do not edit by hand.\n{nix}",
                    path.display()
                ),
            )
            .map_err(|e| miette::miette!("write {}: {e}", output.display()))?;
            println!("Wrote {}", output.display());
        }
    }

    Ok(())
}
