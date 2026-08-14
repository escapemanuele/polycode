use thiserror::Error;

use crate::domain::{
    AttentionError, Role, RunAttentionError, RunId, RunProviderEventError, RunStageError,
    RunStatus, RunTransitionError, StageId,
};
use crate::store::StoreError;
use crate::workspace::{ApplyStatus, WorkspaceStatus};

use super::ProviderError;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    RunTransition(#[from] RunTransitionError),
    #[error(transparent)]
    StageTransition(#[from] RunStageError),
    #[error(transparent)]
    AttentionTransition(#[from] RunAttentionError),
    #[error(transparent)]
    Attention(#[from] AttentionError),
    #[error(transparent)]
    ProviderEvent(#[from] RunProviderEventError),
    #[error("run {0} has no persisted workspace")]
    MissingWorkspace(RunId),
    #[error("run {run_id} requires Ready workspace, found {status:?}")]
    WorkspaceNotReady {
        run_id: RunId,
        status: WorkspaceStatus,
    },
    #[error("run {run_id} execution is frozen by apply intent {status:?}")]
    ApplyInProgress { run_id: RunId, status: ApplyStatus },
    #[error("run execution cannot start from {0:?}")]
    RunNotPrepared(RunStatus),
    #[error("provider does not support role {0:?}")]
    UnsupportedRole(Role),
    #[error("provider changed during stage {stage_id}: {previous} -> {current}")]
    ProviderChanged {
        stage_id: StageId,
        previous: String,
        current: String,
    },
    #[error("provider protocol error for stage {stage_id}: {message}")]
    ProviderProtocol { stage_id: StageId, message: String },
    #[error("scheduler made no legal progress for run {0}")]
    NoProgress(RunId),
    #[error("scheduler exceeded {0} deterministic transitions")]
    DriveLimit(usize),
    #[error("provider checkpoint counter overflow")]
    CheckpointOverflow,
}
