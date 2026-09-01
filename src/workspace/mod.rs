//! Crash-reconcilable Git workspace lifecycle orchestration.

mod error;
mod github;
mod manager;
mod model;

pub use error::WorkspaceError;
pub use manager::{PublishReceipt, PullRequestStatus, ReconciliationOutcome, WorkspaceManager};
pub use model::{
    ApplyStatus, RunApplyOperation, RunWorkspace, WorkspaceMode, WorkspaceRevision, WorkspaceStatus,
};
