//! Operator CLI for the paired service-surface runtime.

use super::native::Native;
use super::runtime::{self, BootReport, OperationReport, StatusReport};
use super::{DEFAULT_POLICY_PATH, Surface};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(
    name = "fleetix-surfaces",
    version,
    about = "Exact-output dev/prod NixOS service-surface lifecycle"
)]
struct Args {
    #[arg(long, global = true, default_value = DEFAULT_POLICY_PATH)]
    policy: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Read-only committed state, pending journal and live identities.
    Status,
    /// Deploy one exact realized guest system output to the dev container.
    DeployDev {
        /// Exact /nix/store/<hash>-<name> guest system output.
        system: String,
    },
    /// Promote the exact durably verified dev output to prod.
    Promote {
        /// Required expected dev release (compare-and-swap).
        #[arg(long)]
        expected_dev: String,
    },
    /// Restore prod to its recorded previous release after operator admission.
    Rollback {
        /// Optional compare-and-swap against the recorded current prod release.
        #[arg(long)]
        expected_prod: Option<String>,
    },
    /// Explicitly recover the pending operation's previous release.
    Recover {
        /// Optional exact pending operation id.
        #[arg(long)]
        operation: Option<String>,
    },
    /// Forward-complete one exact pending candidate under the host lock.
    Resume {
        /// Optional exact pending operation id.
        #[arg(long)]
        operation: Option<String>,
    },
    /// Boot only durably verified releases. Refuses unattended boot on a
    /// pending transaction; never selects or builds a new release.
    Boot,
}

pub fn run() -> ExitCode {
    let args = Args::parse();
    match dispatch(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fleetix-surfaces: {error}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(args: Args) -> Result<(), String> {
    let policy = runtime::load_policy(&args.policy)?;
    let backend = Native::new(&policy)?;
    match args.command {
        Command::Status => {
            let status = runtime::status(&policy, &backend)?;
            print_status(&status)
        }
        Command::DeployDev { system } => {
            print_operation(
                "deployed dev",
                runtime::deploy_dev(&policy, &backend, &system)?,
            );
            Ok(())
        }
        Command::Promote { expected_dev } => {
            print_operation(
                "promoted to prod",
                runtime::promote(&policy, &backend, &expected_dev)?,
            );
            Ok(())
        }
        Command::Rollback { expected_prod } => {
            print_operation(
                "rolled back prod",
                runtime::rollback(&policy, &backend, expected_prod.as_deref())?,
            );
            Ok(())
        }
        Command::Recover { operation } => {
            print_operation(
                "recovered",
                runtime::recover(&policy, &backend, operation.as_deref())?,
            );
            Ok(())
        }
        Command::Resume { operation } => {
            print_operation(
                "resumed",
                runtime::resume(&policy, &backend, operation.as_deref())?,
            );
            Ok(())
        }
        Command::Boot => {
            let report = runtime::boot(&policy, &backend)?;
            print_boot(&report)?;
            if report.ok() {
                Ok(())
            } else {
                Err(format!("{} surface(s) failed to boot", report.errors.len()))
            }
        }
    }
}

fn print_status(status: &StatusReport) -> Result<(), String> {
    println!("host policy: {}", status.policy_digest);
    println!("generation: {}", status.state.generation);
    for surface in [Surface::Dev, Surface::Prod] {
        let record = status.state.record(surface);
        println!(
            "{surface}: release={} previous={}",
            value(record.artifact.as_deref()),
            value(record.previous.as_deref())
        );
        let live = status
            .live
            .iter()
            .find(|live| live.surface == surface)
            .ok_or("status is missing a surface observation")?;
        match (&live.current, &live.error) {
            (_, Some(error)) => println!(
                "{surface}: container={} live=unavailable ({error})",
                live.container
            ),
            (Some(current), None) => println!(
                "{surface}: container={} live={current} expected={}",
                live.container,
                value(live.expected.as_deref())
            ),
            (None, None) => println!("{surface}: container={} live=unknown", live.container),
        }
    }
    if let Some(pending) = &status.pending {
        println!(
            "pending: operation={} id={} surface={} phase={} committed_journal={}",
            pending.operation.label(),
            pending.id,
            pending.surface(),
            pending.phase,
            status.pending_committed
        );
        if let Some(failure) = &pending.failure {
            println!(
                "pending failure: phase={} {}",
                failure.phase, failure.message
            );
        }
    } else {
        println!("pending: none");
    }
    Ok(())
}

fn print_operation(action: &str, report: OperationReport) {
    if report.aborted {
        println!(
            "aborted: surface={} candidate={} operation={}",
            report.surface, report.artifact, report.id
        );
        return;
    }
    let recovery = if report.recovered { " recovered" } else { "" };
    println!(
        "{action}{recovery}: surface={} artifact={} operation={}",
        report.surface, report.artifact, report.id
    );
}

fn print_boot(report: &BootReport) -> Result<(), String> {
    if report.started.is_empty() {
        println!("boot: no durably verified surfaces");
    } else {
        println!(
            "boot: started {}",
            report
                .started
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    for error in &report.errors {
        eprintln!("boot: {error}");
    }
    Ok(())
}

fn value(value: Option<&str>) -> &str {
    value.unwrap_or("none")
}
