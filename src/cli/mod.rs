pub mod commands;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::domain::{AttentionRequestId, RunId, StageId};

/// Orchestrate native coding agents as a specialized engineering team.
#[derive(Debug, Parser)]
#[command(name = "polycode", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Internal managed-process runner.
    #[command(name = "__run-process", hide = true)]
    RunProcess { manifest: PathBuf },
    /// Internal signal-normalizing exec bridge.
    #[command(name = "__exec-process", hide = true)]
    ExecProcess { manifest: PathBuf },
    /// Run implementation-only workflow.
    Fast(RunArgs),
    /// Run architecture, implementation, review, and decision workflow.
    Standard(RunArgs),
    /// Run full research-to-decision workflow.
    Deep(RunArgs),
    /// Run read-only parallel review workflow.
    Review(RunArgs),
    /// List known runs.
    Runs,
    /// Inspect one run.
    Status { run_id: RunId },
    /// Continue one prepared or suspended run.
    Resume { run_id: RunId },
    /// Retry one failed stage explicitly.
    Retry { run_id: RunId, stage_id: StageId },
    /// Resolve one pending attention request and continue.
    Resolve {
        run_id: RunId,
        attention_id: AttentionRequestId,
        /// Answer for a provider question. Omit when approving a permission request.
        #[arg(long)]
        response: Option<String>,
    },
    /// Apply completed workspace changes to source repository.
    Apply { run_id: RunId },
    /// Discard run and remove owned workspace resources.
    Discard { run_id: RunId },
    /// Check Polycode's local environment.
    Doctor,
}

#[derive(Clone, Debug, PartialEq, Eq, Args)]
pub struct RunArgs {
    /// Task sent unchanged to each provider stage after outer trim normalization.
    pub task: String,
    /// Git repository; defaults to current directory.
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    /// Native provider (`claude`) or deterministic development provider (`fake`).
    #[arg(long)]
    pub provider: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_workflow_and_control_commands() {
        let cli = Cli::try_parse_from([
            "polycode",
            "deep",
            "inspect repository",
            "--repo",
            "/tmp/repo",
            "--provider",
            "fake",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Deep(RunArgs { provider: Some(ref provider), .. })) if provider == "fake"
        ));

        let run = RunId::from_u128(1);
        let cli = Cli::try_parse_from(["polycode", "status", &run.to_string()]).unwrap();
        assert_eq!(cli.command, Some(Command::Status { run_id: run }));

        let cli = Cli::try_parse_from(["polycode", "__run-process", "/tmp/spec.json"]).unwrap();
        assert_eq!(
            cli.command,
            Some(Command::RunProcess {
                manifest: PathBuf::from("/tmp/spec.json")
            })
        );

        let cli = Cli::try_parse_from(["polycode", "__exec-process", "/tmp/spec.json"]).unwrap();
        assert_eq!(
            cli.command,
            Some(Command::ExecProcess {
                manifest: PathBuf::from("/tmp/spec.json")
            })
        );
    }
}
