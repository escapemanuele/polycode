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
    RunTransition(#[from] crate::domain::RunTransitionError),
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
    #[error(
        "Repository has uncommitted changes.\n  Commit or stash them before starting a Polycode run."
    )]
    DirtySourceRepository,
    #[error("run {0} cannot be stopped from status {1:?}")]
    RunNotStoppable(RunId, crate::domain::RunStatus),
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

impl AppError {
    /// Whether this error is an optimistic-concurrency loss rather than a real
    /// failure. Callers that are safe to repeat — every step of a stop is
    /// idempotent — retry instead of surfacing a revision number to the user.
    #[must_use]
    pub fn is_concurrent_modification(&self) -> bool {
        matches!(
            self,
            Self::Store(StoreError::ConcurrentModification { .. })
                | Self::Process(
                    ProcessError::ConcurrentModification { .. }
                        | ProcessError::CursorConcurrentModification { .. }
                )
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::RunId;
    use crate::process::ManagedProcessId;

    // The stop path retries only what this predicate classifies as a lost
    // revision race. If an arm is dropped, stop silently goes back to
    // aborting mid-way and reporting a revision number to the user, so the
    // classification is pinned here rather than only through the racy
    // end-to-end fixture.
    #[test]
    fn only_lost_revision_races_are_retryable() {
        assert!(
            AppError::Store(StoreError::ConcurrentModification {
                run_id: RunId::new(),
                expected: 3,
            })
            .is_concurrent_modification()
        );
        assert!(
            AppError::Process(ProcessError::ConcurrentModification {
                process_id: ManagedProcessId::new(),
                expected: 3,
            })
            .is_concurrent_modification()
        );

        // A real refusal must never be retried into success.
        assert!(
            !AppError::RunNotStoppable(RunId::new(), crate::domain::RunStatus::Completed)
                .is_concurrent_modification()
        );
        assert!(!AppError::DirtySourceRepository.is_concurrent_modification());
    }
}
