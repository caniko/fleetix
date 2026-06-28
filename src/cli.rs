use crate::topology;
use crate::validate;
use clap::Parser;
use miette::IntoDiagnostic;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "fleetix", about = "Fleet topology toolkit")]
pub enum Cli {
    /// Validate a topology file for cross-reference invariants
    Validate {
        /// Path to the .pkl topology file
        path: PathBuf,
    },
    /// Show a human-readable fleet overview
    Show {
        /// Path to the .pkl topology file
        path: PathBuf,
        /// Filter to a single host
        #[arg(short, long)]
        host: Option<String>,
    },
    /// List links with derived values
    Links {
        /// Path to the .pkl topology file
        path: PathBuf,
        /// Filter to a single link
        #[arg(short, long)]
        name: Option<String>,
    },
    /// Evaluate topology and output a Nix expression
    Eval {
        /// Path to the .pkl topology file
        path: PathBuf,
    },
    /// Evaluate topology and output JSON
    EvalJson {
        /// Path to the .pkl topology file
        path: PathBuf,
    },
}

pub async fn run(cli: Cli) -> miette::Result<()> {
    match cli {
        Cli::Validate { path } => {
            let topo = topology::load_topology(&path).await?;
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

        Cli::Show { path, host } => {
            let topo = topology::load_topology(&path).await?;
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

        Cli::Links { path, name } => {
            let topo = topology::load_topology(&path).await?;
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

        Cli::Eval { path } => {
            let topo = topology::load_topology(&path).await?;
            let json = serde_json::to_string_pretty(&topo).into_diagnostic()?;
            println!("{json}");
        }

        Cli::EvalJson { path } => {
            let topo = topology::load_topology(&path).await?;
            let json = serde_json::to_string_pretty(&topo).into_diagnostic()?;
            println!("{json}");
        }
    }

    Ok(())
}
