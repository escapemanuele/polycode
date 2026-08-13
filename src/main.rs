mod cli;
mod config;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    init_tracing()?;

    let cli = cli::Cli::parse();
    cli::commands::execute(cli.command.as_ref())
}

fn init_tracing() -> Result<()> {
    let filter = match std::env::var("RUST_LOG") {
        Ok(value) => EnvFilter::try_new(value).context("RUST_LOG contains an invalid filter")?,
        Err(std::env::VarError::NotPresent) => EnvFilter::new("polycode=info"),
        Err(error) => return Err(error).context("RUST_LOG is not valid Unicode"),
    };

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .context("failed to initialize tracing subscriber")
}
