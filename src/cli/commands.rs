use anyhow::Result;
use clap::CommandFactory;

use super::{Cli, Command};

pub fn execute(command: Option<&Command>) -> Result<()> {
    match command {
        Some(Command::Doctor) => doctor(),
        Some(Command::Runs) => runs(),
        None => {
            Cli::command().print_help()?;
            println!();
            Ok(())
        }
    }
}

fn doctor() -> Result<()> {
    let config_file = crate::config::config_file()?;
    let database_file = crate::store::database_file()?;

    println!("Polycode doctor");
    println!("  status: Milestone 2 persistence");
    println!("  config: {}", config_file.display());
    println!("  database: {}", database_file.display());
    if database_file.exists() {
        let store = crate::store::SqliteStore::open(&database_file)?;
        println!("  database schema: {}", store.schema_version()?);
    } else {
        println!("  database schema: not initialized");
    }
    println!("  provider checks: not implemented");

    Ok(())
}

fn runs() -> Result<()> {
    let database_file = crate::store::database_file()?;
    let store = crate::store::SqliteStore::open(&database_file)?;
    let runs = store.list_runs()?;
    if runs.is_empty() {
        println!("No runs.");
        return Ok(());
    }
    for run in runs {
        println!(
            "{}  {:?}  {:?}  rev={}  {}",
            run.id,
            run.status,
            run.workflow,
            run.revision.value(),
            run.updated_at.to_rfc3339()
        );
    }
    Ok(())
}
