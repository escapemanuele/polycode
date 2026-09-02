//! Crash-reconcilable Git workspace lifecycle orchestration.

mod branch_name;
mod error;
mod github;
mod manager;
mod model;
mod pull_request;

pub use error::WorkspaceError;
pub use manager::{PublishReceipt, PullRequestStatus, ReconciliationOutcome, WorkspaceManager};
pub use model::{
    ApplyStatus, RunApplyOperation, RunWorkspace, WorkspaceMode, WorkspaceRevision, WorkspaceStatus,
};
pub use pull_request::{PullRequestDraft, extract as extract_pull_request_draft};
