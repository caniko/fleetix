//! Target-local worker. Install fixed policy arguments in a service; never
//! authorize an unprivileged caller to select a privileged policy or command.
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(about = "Recoverable target-local NixOS and Home Manager deployment")]
struct Cli {
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Admit and deploy an untrusted request using an installed policy.
    Run {
        #[arg(long)]
        policy: PathBuf,
        #[arg(long)]
        request: PathBuf,
    },
    /// Restore unsettled transactions after worker failure or reboot.
    Recover {
        #[arg(long)]
        policy: PathBuf,
        /// Maintenance retry after inspecting a failed restoration. Does not
        /// bypass the unrelated-generation or retained-identity checks.
        #[arg(long)]
        retry_failed: bool,
    },
    /// Inspect durable transaction states and their journal paths; no mutation.
    Status {
        #[arg(long)]
        policy: PathBuf,
    },
}

#[cfg(unix)]
fn main() -> ExitCode {
    use fleetix::deployment::runtime;
    let cli = Cli::parse();
    if let Err(error) = runtime::install_signal_handlers() {
        eprintln!("cannot install deployment signal handling: {error}");
        return ExitCode::FAILURE;
    }
    let result = match cli.command {
        Action::Run { policy, request } => runtime::run(&policy, &request),
        Action::Recover {
            policy,
            retry_failed,
        } => {
            if retry_failed {
                runtime::retry_recovery(&policy)
            } else {
                runtime::recover(&policy)
            }
        }
        Action::Status { policy } => runtime::status(&policy).map_err(runtime::WorkerError::from),
    };
    match result {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            if error.retryable {
                eprintln!("deployment deferred: {error}");
                // The installed unit forces a restart only for this exit code,
                // so lock contention re-runs admission instead of needing an
                // operator; every refused or unsafe state stays final.
                ExitCode::from(75)
            } else {
                eprintln!("deployment refused or failed: {error}");
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("fleetix-deploy requires Unix process and filesystem semantics");
    ExitCode::FAILURE
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn help_needs_no_privilege_or_deployment_state() {
        let error = match Cli::try_parse_from(["fleetix-deploy", "--help"]) {
            Ok(_) => panic!("--help must not parse as a command"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
    }
}
