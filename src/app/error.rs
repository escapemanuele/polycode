use thiserror::Error;

use crate::domain::{IdError, RunId, StageId};
use crate::engine::{EngineError, FakeScenarioError};
use crate::git::GitError;
use crate::process::ProcessError;
use crate::providers::claude::ClaudeProviderError;
use crate::providers::codex::CodexProviderError;
use crate::store::{RunInputError, StoreError};
use crate::workspace::WorkspaceError;

use super::RoutingError;

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    RunInput(#[from] RunInputError),
    #[error(transparent)]
    Identifier(#[from] IdError),
    #[error(transparent)]
    FakeScenario(#[from] FakeScenarioError),
    #[error(transparent)]
    Claude(#[from] ClaudeProviderError),
    #[error(transparent)]
    Codex(#[from] CodexProviderError),
    #[error(transparent)]
    Process(#[from] ProcessError),
    #[error(transparent)]
    Routing(#[from] RoutingError),
    #[error(
        "execution selection is required; use --provider claude|codex|fake or --profile recommended"
    )]
    NoProductionProvider,
    #[error("unsupported provider {0:?}; supported providers: claude, codex, fake")]
    UnsupportedProvider(String),
    #[error("run {0} cannot be resumed because its input predates the executable schema")]
    LegacyRunInput(RunId),
    #[error(
        "run {0} cannot be resumed because its execution configuration predates the executable schema"
    )]
    LegacyExecutionConfig(RunId),
    #[error("run {0} was discarded and cannot continue")]
    DiscardedRun(RunId),
    #[error("run {run_id} stage {stage_id} has no verified artifact")]
    ArtifactNotFound { run_id: RunId, stage_id: StageId },
    #[error("run {run_id} stage {stage_id} has no managed process log")]
    ProcessLogNotFound { run_id: RunId, stage_id: StageId },
    #[error("run {run_id} has no stage {stage_id}")]
    StageNotFound { run_id: RunId, stage_id: StageId },
}
