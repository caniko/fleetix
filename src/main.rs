use clap::Parser;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    fleetix::cli::main_exit(fleetix::cli::Cli::parse()).await
}
