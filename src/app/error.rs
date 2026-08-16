use thiserror::Error;

use crate::domain::{IdError, RunId};
use crate::engine::{EngineError, FakeScenarioError};
use crate::git::GitError;
use crate::process::ProcessError;
use crate::providers::claude::ClaudeProviderError;
use crate::store::{RunInputError, StoreError};
use crate::workspace::WorkspaceError;

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
    Process(#[from] ProcessError),
    #[error("provider is required; use --provider claude or --provider fake")]
    NoProductionProvider,
    #[error("unsupported provider {0:?}; supported providers: claude, fake")]
    UnsupportedProvider(String),
    #[error("run {0} cannot be resumed because its input predates the executable schema")]
    LegacyRunInput(RunId),
    #[error(
        "run {0} cannot be resumed because its execution configuration predates the executable schema"
    )]
    LegacyExecutionConfig(RunId),
    #[error("run {0} was discarded and cannot continue")]
    DiscardedRun(RunId),
}
