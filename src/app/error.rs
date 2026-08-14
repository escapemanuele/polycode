use thiserror::Error;

use crate::domain::{IdError, RunId};
use crate::engine::{EngineError, FakeScenarioError};
use crate::git::GitError;
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
    #[error("no production provider is available yet; use --provider fake")]
    NoProductionProvider,
    #[error("unsupported provider {0:?}; Milestone 5 supports only --provider fake")]
    UnsupportedProvider(String),
    #[error(
        "run {0} cannot be resumed through the Milestone 5 CLI because its input predates this schema"
    )]
    LegacyRunInput(RunId),
    #[error(
        "run {0} cannot be resumed through the Milestone 5 CLI because its execution configuration predates this schema"
    )]
    LegacyExecutionConfig(RunId),
    #[error("run {0} was discarded and cannot continue")]
    DiscardedRun(RunId),
}
