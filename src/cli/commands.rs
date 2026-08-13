use anyhow::Result;
use clap::CommandFactory;

use super::{Cli, Command};

pub fn execute(command: Option<&Command>) -> Result<()> {
    match command {
        Some(Command::Doctor) => doctor(),
        Some(Command::Runs) => {
            runs();
            Ok(())
        }
        None => {
            Cli::command().print_help()?;
            println!();
            Ok(())
        }
    }
}

fn doctor() -> Result<()> {
    let config_file = crate::config::config_file()?;

    println!("Polycode doctor");
    println!("  status: Milestone 0 bootstrap");
    println!("  config: {}", config_file.display());
    println!("  provider checks: not implemented");

    Ok(())
}

fn runs() {
    println!("Run persistence is not implemented (planned for Milestone 2).");
}
