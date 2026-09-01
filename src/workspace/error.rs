use std::path::PathBuf;

use thiserror::Error;

use crate::domain::{RunId, RunStatus, RunTransitionError};
use crate::git::GitError;
use crate::store::StoreError;

use super::{WorkspaceMode, WorkspaceStatus};

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("workspace filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("run lifecycle transition failed: {0}")]
    RunTransition(#[from] RunTransitionError),
    #[error("run {run_id} status {status:?} is not valid for {operation}")]
    InvalidRunStatus {
        run_id: RunId,
        status: RunStatus,
        operation: &'static str,
    },
    #[error("run {0} already has a workspace")]
    WorkspaceAlreadyExists(RunId),
    #[error("run {0} has no workspace")]
    WorkspaceMissing(RunId),
    #[error("workspace for run {run_id} is {status:?}; expected {expected}")]
    InvalidWorkspaceStatus {
        run_id: RunId,
        status: WorkspaceStatus,
        expected: &'static str,
    },
    #[error("workspace path already exists: {0}")]
    WorkspacePathConflict(PathBuf),
    #[error("workspace branch already exists: {0}")]
    BranchConflict(String),
    #[error("workspace ownership mismatch for run {run_id}: {reason}")]
    WorkspaceOwnershipMismatch { run_id: RunId, reason: String },
    #[error("workspace for run {run_id} is broken: {reason}")]
    WorkspaceBroken { run_id: RunId, reason: String },
    #[error("source checkout contains local changes: {0}")]
    SourceCheckoutDirty(PathBuf),
    #[error("review/detached workspace cannot be applied")]
    ReviewWorkspaceNotApplicable,
    /// The run's latest verify stage did not pass. A failed verification
    /// still completes the run — the decision only optionally depends on
    /// it — so this gate, not the run status, is what keeps unverified
    /// changes out of the source checkout and off the remote.
    #[error("verification did not pass: stage {stage_id} is {status}")]
    VerificationNotPassed {
        stage_id: crate::domain::StageId,
        status: String,
    },
    #[error("workspace has no changes to apply")]
    EmptyPatch,
    #[error("workspace has no changes to publish")]
    NothingToPublish,
    #[error("repository at {0} has no 'origin' remote to publish to")]
    NoRemote(PathBuf),
    #[error("patch cannot be applied cleanly")]
    PatchCheckFailed,
    #[error("apply state is ambiguous; manual recovery required")]
    AmbiguousApplyState,
    #[error("run {0} was already applied")]
    ApplyAlreadyPerformed(RunId),
    #[error("run {0} has an apply operation in progress")]
    ApplyInProgress(RunId),
    #[error("persisted apply patch hash differs from regenerated workspace patch")]
    PatchHashMismatch,
    #[error("stored workspace is invalid: {0}")]
    InvalidStoredWorkspace(&'static str),
    #[error("workspace mode {0:?} has no branch")]
    MissingBranch(WorkspaceMode),
    #[cfg(test)]
    #[error("injected crash at {0}")]
    InjectedCrash(&'static str),
}
