//! Standalone build-train frontend. Operations return typed values before the
//! frontend serializes them; requests carry data rather than executable commands.
use super::{
    Fence, Outcome, Request,
    native::{self, Connection, Service},
    runtime::{Command, Reply},
};
use clap::Subcommand;
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;

#[derive(Debug, Subcommand)]
pub enum TrainCommand {
    /// Start the private builder-local coordinator.
    Serve {
        #[arg(long)]
        config: PathBuf,
    },
    /// Durably join with frozen request data, held until explicit live admission.
    Register {
        #[arg(long)]
        connection: PathBuf,
        /// JSON Request with exact derivation/named-output roots and immutable source.
        #[arg(long)]
        request: PathBuf,
        /// Bound registration delivery, planner queueing and preparation; expiry only detaches.
        #[arg(long, default_value_t = 86400, value_parser = clap::value_parser!(u64).range(1..=86400))]
        wait_seconds: u64,
    },
    /// Admit a held request after the caller's live safety and resource checks pass.
    Admit {
        #[arg(long)]
        connection: PathBuf,
        #[arg(long)]
        attempt: String,
    },
    /// Wait for this request only; timeout or interruption detaches without cancellation.
    Wait {
        #[arg(long)]
        connection: PathBuf,
        #[arg(long)]
        attempt: String,
        #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..=86400))]
        wait_seconds: u64,
    },
    /// Inspect durable request outcome, running workers and activation fence.
    Status {
        #[arg(long)]
        connection: PathBuf,
        #[arg(long)]
        attempt: Option<String>,
    },
    /// Cancel this request's interests; already-running shared work finishes.
    Cancel {
        #[arg(long)]
        connection: PathBuf,
        #[arg(long)]
        attempt: String,
    },
    /// Retry this terminal request in held admission; rerun live checks before admit.
    Retry {
        #[arg(long)]
        connection: PathBuf,
        #[arg(long)]
        attempt: String,
    },
    /// Archive terminal evidence before releasing this request's train-owned roots.
    Retire {
        #[arg(long)]
        connection: PathBuf,
        #[arg(long)]
        attempt: String,
    },
    /// Close dispatch and drain under a bounded deadline; failed drain retains its fence.
    Drain {
        #[arg(long)]
        connection: PathBuf,
        #[arg(long)]
        attempt: String,
        #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(0..=86400))]
        wait_seconds: u64,
    },
    /// Verify construction-side activation readiness; caller still owns the host lease.
    AuthorizeActivation {
        #[arg(long)]
        connection: PathBuf,
        #[arg(long)]
        attempt: String,
    },
    /// Release the exact drained fence after verifying its owner's activation outcome.
    ReleaseFence {
        #[arg(long)]
        connection: PathBuf,
        #[arg(long)]
        token: String,
    },
    /// Complete offline policy handover with both immutable contracts and retained fence.
    Rollover {
        #[arg(long)]
        previous_config: PathBuf,
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        token: String,
    },
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum Execution {
    Reply(Reply),
    Completion {
        outcome: Outcome,
    },
    Drained {
        fence: Fence,
    },
    Rollover {
        #[serde(rename = "rolloverReceipt")]
        receipt: PathBuf,
    },
    Served,
}

fn signals() -> Result<Arc<AtomicBool>, String> {
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, Arc::clone(&stop)).map_err(|e| e.to_string())?;
    }
    Ok(stop)
}

fn service(path: &Path) -> Result<Service, String> {
    serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

/// Execute typed operations; consumers can provide their own presentation.
pub fn execute(command: TrainCommand) -> Result<Execution, String> {
    let call = |path: &Path, command| {
        Connection::load(path)?
            .client()
            .call(command)
            .map(Execution::Reply)
    };
    match command {
        TrainCommand::Serve { config } => {
            native::serve(service(&config)?, signals()?)?;
            Ok(Execution::Served)
        }
        TrainCommand::Register {
            connection,
            request,
            wait_seconds,
        } => {
            let request: Request =
                serde_json::from_slice(&std::fs::read(request).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            let stop = signals()?;
            Connection::load(&connection)?
                .client()
                .register_for(request, Duration::from_secs(wait_seconds), &stop)
                .map(Execution::Reply)
        }
        TrainCommand::Admit {
            connection,
            attempt,
        } => call(&connection, Command::Admit(attempt)),
        TrainCommand::Wait {
            connection,
            attempt,
            wait_seconds,
        } => {
            let stop = signals()?;
            let outcome = Connection::load(&connection)?.client().wait_for(
                &attempt,
                Duration::from_secs(wait_seconds),
                &stop,
            )?;
            Ok(Execution::Completion { outcome })
        }
        TrainCommand::Status {
            connection,
            attempt,
        } => call(
            &connection,
            attempt.map_or(Command::Inspect, Command::Status),
        ),
        TrainCommand::Cancel {
            connection,
            attempt,
        } => call(&connection, Command::Cancel(attempt)),
        TrainCommand::Retry {
            connection,
            attempt,
        } => call(&connection, Command::Retry(attempt)),
        TrainCommand::Retire {
            connection,
            attempt,
        } => call(&connection, Command::Retire(attempt)),
        TrainCommand::Drain {
            connection,
            attempt,
            wait_seconds,
        } => {
            let fence = Connection::load(&connection)?
                .client()
                .drain(&attempt, Duration::from_secs(wait_seconds))?;
            Ok(Execution::Drained { fence })
        }
        TrainCommand::AuthorizeActivation {
            connection,
            attempt,
        } => call(&connection, Command::AuthorizeActivation(attempt)),
        TrainCommand::ReleaseFence { connection, token } => {
            call(&connection, Command::ReleaseFence(token))
        }
        TrainCommand::Rollover {
            previous_config,
            config,
            token,
        } => {
            let receipt = native::rollover(service(&previous_config)?, service(&config)?, &token)?;
            Ok(Execution::Rollover { receipt })
        }
    }
}

/// Serialize the standalone CLI's result. Status reports terminal evidence;
/// completion waits fail the process for unsuccessful terminal outcomes.
pub fn run(command: TrainCommand) -> Result<(), String> {
    let result = execute(command)?;
    if matches!(result, Execution::Served) {
        return Ok(());
    }
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, &result).map_err(|e| e.to_string())?;
    writeln!(stdout).map_err(|e| e.to_string())?;
    match result {
        Execution::Completion {
            outcome: Outcome::Failed(error),
        } => Err(format!("construction failed: {error}")),
        Execution::Completion {
            outcome: Outcome::Cancelled,
        } => Err("construction cancelled".into()),
        _ => Ok(()),
    }
}
