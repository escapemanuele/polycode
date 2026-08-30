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
    /// Internal release-pipeline gate: canonical tag matching this build.
    #[command(name = "__verify-release-tag", hide = true)]
    VerifyReleaseTag { tag: String },
    /// Internal bootstrap hook: record an installed executable as official.
    #[command(name = "__register-official-install", hide = true)]
    RegisterOfficialInstall {
        executable: PathBuf,
        /// Release asset the executable was installed from.
        #[arg(long)]
        asset: Option<String>,
    },
    /// Internal bootstrap hook: classify an executable without running it.
    #[command(name = "__install-source", hide = true)]
    InstallSourceOf { executable: Option<PathBuf> },
    /// Open interactive local control room.
    Tui,
    /// Experimental role-specific provider/model evaluation tools.
    Eval {
        #[command(subcommand)]
        command: EvalCommand,
    },
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
    /// Stop execution, keeping the run, workspace, and results resumable.
    Stop { run_id: RunId },
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
    /// Send a completed run back to fix its own result, then decide again.
    Fix { run_id: RunId },
    /// Apply completed workspace changes to source repository.
    Apply { run_id: RunId },
    /// Discard run and remove owned workspace resources.
    Discard { run_id: RunId },
    /// Check for a newer official Polycode release.
    Update(UpdateArgs),
    /// Check Polycode's local environment.
    Doctor,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Args)]
pub struct UpdateArgs {
    /// Report update status without installing anything.
    #[arg(long)]
    pub check: bool,
    /// Install without the interactive confirmation prompt.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Subcommand)]
pub enum EvalCommand {
    /// List source-controlled evaluation suites and cases.
    List,
    /// Execute one candidate target against an isolated suite.
    Run(EvalRunArgs),
    /// Aggregate one or more result files/directories without selecting a winner.
    Report {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Args)]
pub struct EvalRunArgs {
    /// Source-controlled suite version.
    #[arg(long, default_value = "role_core_v1")]
    pub suite: String,
    /// Candidate provider (`claude`, `codex`, or synthetic `fake`).
    #[arg(long)]
    pub provider: String,
    /// Explicit provider model; omission means native configured/default model.
    #[arg(long)]
    pub model: Option<String>,
    /// Fresh repetitions per case.
    #[arg(long, default_value_t = 1)]
    pub repeat: u32,
    /// Explicit acknowledgement that native evaluation may consume provider usage.
    #[arg(long)]
    pub allow_native_usage: bool,
    /// Evidence output directory. Defaults under ~/.polycode/evals.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Args)]
pub struct RunArgs {
    /// Task sent unchanged to each provider stage after outer trim normalization.
    pub task: String,
    /// Git repository; defaults to current directory.
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    /// Native provider (`claude` or `codex`) or deterministic development provider (`fake`).
    #[arg(long, conflicts_with = "profile")]
    pub provider: Option<String>,
    /// Versioned routing profile (`recommended`).
    #[arg(long, conflicts_with = "provider")]
    pub profile: Option<String>,
    /// Requested native-runtime effort (`native`, `low`, `medium`, `high`).
    /// Omitted or `native` preserves each runtime's own configured default.
    #[arg(long)]
    pub effort: Option<String>,
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

        let cli = Cli::try_parse_from(["polycode", "tui"]).unwrap();
        assert_eq!(cli.command, Some(Command::Tui));

        let cli = Cli::try_parse_from([
            "polycode",
            "eval",
            "run",
            "--provider",
            "codex",
            "--model",
            "fixture-model",
            "--repeat",
            "3",
            "--allow-native-usage",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Eval {
                command: EvalCommand::Run(EvalRunArgs { repeat: 3, .. })
            })
        ));
    }
}
