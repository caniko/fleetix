use clap::Parser;

#[tokio::main]
async fn main() -> miette::Result<()> {
    let cli = fleetix::cli::Cli::parse();
    fleetix::cli::run(cli).await
}
