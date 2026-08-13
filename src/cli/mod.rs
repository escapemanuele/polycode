pub mod commands;

use clap::{Parser, Subcommand};

/// Orchestrate native coding agents as a specialized engineering team.
#[derive(Debug, Parser)]
#[command(name = "polycode", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Check Polycode's local environment.
    Doctor,
    /// List known runs.
    Runs,
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command};
    use clap::Parser;

    #[test]
    fn parses_doctor_command() {
        let cli = Cli::try_parse_from(["polycode", "doctor"]).expect("doctor should parse");

        assert_eq!(cli.command, Some(Command::Doctor));
    }

    #[test]
    fn parses_runs_command() {
        let cli = Cli::try_parse_from(["polycode", "runs"]).expect("runs should parse");

        assert_eq!(cli.command, Some(Command::Runs));
    }
}
