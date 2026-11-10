#[cfg(feature = "cli")]
use clap::{Args, Parser, Subcommand};
#[cfg(feature = "cli")]
use fleetix_sidecar::{CacheOptions, check, options_with_http, write_with_cache};
#[cfg(feature = "cli")]
use std::path::PathBuf;
use std::process::ExitCode;

#[cfg(feature = "cli")]
#[derive(Parser)]
#[command(
    version,
    about = "Generate Nix sidecars directly from Pkl, without Nix"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[cfg(feature = "cli")]
#[derive(Subcommand)]
enum Command {
    /// Evaluate Pkl and atomically write an importable Nix sidecar
    Generate(InputOutput),
    /// Reevaluate and compare bytes without writing output or cache (stale: exit 2)
    Check(InputOutput),
}

#[cfg(feature = "cli")]
#[derive(Args)]
struct InputOutput {
    input: PathBuf,
    output: PathBuf,
    /// Bypass the persistent cache (check always does so)
    #[arg(long)]
    no_cache: bool,
    /// HTTP URL rewrite rule in source_prefix=target_prefix format
    #[arg(long = "http-rewrite")]
    http_rewrite: Vec<String>,
    /// HTTP proxy URL
    #[arg(long = "http-proxy")]
    http_proxy: Option<String>,
}

#[cfg(feature = "cli")]
async fn run(cli: Cli) -> miette::Result<ExitCode> {
    let (args, checking) = match cli.command {
        Command::Generate(args) => (args, false),
        Command::Check(args) => (args, true),
    };
    let options = options_with_http(args.http_rewrite, args.http_proxy.as_deref())?;
    if checking {
        if check(&args.input, &args.output, options).await? {
            println!("Current {}", args.output.display());
            Ok(ExitCode::SUCCESS)
        } else {
            eprintln!("Stale {}", args.output.display());
            Ok(ExitCode::from(2))
        }
    } else {
        let outcome = write_with_cache(
            &args.input,
            &args.output,
            options,
            CacheOptions {
                enabled: !args.no_cache,
                directory: None,
            },
        )
        .await?;
        let status = if outcome.changed {
            "Wrote"
        } else {
            "Unchanged"
        };
        println!("{status} {}", args.output.display());
        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(feature = "cli")]
#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

/// `cli` is a default feature but this binary is not feature-gated, so a
/// `--no-default-features` build must still compile and fail clearly.
#[cfg(not(feature = "cli"))]
fn main() -> ExitCode {
    eprintln!("fleetix-sidecar was built without the `cli` feature and has no interface");
    ExitCode::FAILURE
}
