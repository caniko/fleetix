use std::process::ExitCode;

#[cfg(feature = "cli")]
use clap::Parser;

#[cfg(feature = "cli")]
#[tokio::main]
async fn main() -> ExitCode {
    fleetix::cli::main_exit(fleetix::cli::Cli::parse()).await
}

/// `cli` is a default feature but this binary is not feature-gated, so a
/// `--no-default-features` build must still compile and fail clearly.
#[cfg(not(feature = "cli"))]
fn main() -> ExitCode {
    eprintln!("fleetix was built without the `cli` feature and has no interface");
    ExitCode::FAILURE
}
