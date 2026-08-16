use std::path::PathBuf;

use thiserror::Error;

use crate::domain::{RunId, StageId};
use crate::store::StoreError;

use super::{BackendSessionId, ManagedProcessId, ManagedProcessStatus, OutputStream};

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("SQLite process operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("process filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("process manifest JSON operation failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("managed processes require macOS or Linux")]
    UnsupportedPlatform,
    #[error("invalid {0}")]
    InvalidIdentifier(&'static str),
    #[error("invalid process specification: {0}")]
    InvalidSpec(&'static str),
    #[error("stored managed-process record is invalid: {0}")]
    InvalidStoredProcess(&'static str),
    #[error("managed process {0} does not exist")]
    ProcessNotFound(ManagedProcessId),
    #[error("managed process {0} already exists with different immutable identity")]
    ProcessConflict(ManagedProcessId),
    #[error(
        "run {run_id} stage {stage_id} attempt {attempt} invocation {invocation} already has another process"
    )]
    AttemptConflict {
        run_id: RunId,
        stage_id: StageId,
        attempt: u32,
        invocation: u32,
    },
    #[error("managed process {process_id} changed since revision {expected}")]
    ConcurrentModification {
        process_id: ManagedProcessId,
        expected: u64,
    },
    #[error("managed process {process_id} {stream:?} cursor changed since revision {expected}")]
    CursorConcurrentModification {
        process_id: ManagedProcessId,
        stream: OutputStream,
        expected: u64,
    },
    #[error("invalid managed-process transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: ManagedProcessStatus,
        to: ManagedProcessStatus,
    },
    #[error("run {run_id} has no stage {stage_id}")]
    UnknownStage { run_id: RunId, stage_id: StageId },
    #[error("managed process working directory must equal ready run workspace: {0}")]
    WorkspaceMismatch(PathBuf),
    #[error("tmux executable was not found")]
    TmuxNotFound,
    #[error("tmux command failed during {operation}: {message}")]
    TmuxCommand {
        operation: &'static str,
        message: String,
    },
    #[error("tmux session {session_id} is owned by another process")]
    ForeignSession { session_id: BackendSessionId },
    #[error("managed process {process_id} has invalid ownership evidence")]
    OwnershipMismatch { process_id: ManagedProcessId },
    #[error("managed process {process_id} has corrupt exit evidence: {reason}")]
    InvalidExitEvidence {
        process_id: ManagedProcessId,
        reason: &'static str,
    },
    #[error("managed process {0} output was truncated below acknowledged offset")]
    OutputTruncated(ManagedProcessId),
    #[error("output read size must be between 1 and {0} bytes")]
    InvalidReadSize(usize),
    #[error("output acknowledgement is outside delivered chunk")]
    InvalidAcknowledgement,
    #[error("managed process {0} did not stop before interrupt timeout")]
    InterruptTimeout(ManagedProcessId),
    #[error("managed process {0} has no valid live runtime evidence")]
    MissingRuntimeEvidence(ManagedProcessId),
    #[error("managed process signal command failed: {0}")]
    SignalCommand(String),
    #[error("managed process runner failed: {0}")]
    Runner(String),
}
