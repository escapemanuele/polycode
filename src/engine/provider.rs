use std::path::{Path, PathBuf};

use crate::domain::{
    AttentionKind, AttentionRequestId, ModelId, ProviderId, ProviderSessionId, Role, RunId,
    StageId, StageKind, StageStatus,
};
use crate::providers::ProviderCommit;
use crate::store::SqliteStore;

/// Provider-neutral usage added by one provider signal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsageDelta {
    pub input_units: u64,
    pub output_units: u64,
}

/// One atomic provider output consumed by workflow execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderSignal {
    Started {
        model_id: Option<ModelId>,
        session_id: Option<ProviderSessionId>,
    },
    Progress(String),
    NeedsUser {
        kind: AttentionKind,
        summary: String,
        request_id: Option<AttentionRequestId>,
    },
    Usage(UsageDelta),
    Paused,
    Interrupted,
    Resumed,
    Completed,
    Failed(String),
}

/// Result of one non-blocking synchronous provider poll.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderPoll {
    Pending,
    Signal(ProviderSignal),
    Checkpoint(ProviderCommit),
    Emission {
        signals: Vec<ProviderSignal>,
        commit: ProviderCommit,
    },
}

/// Complete context needed to produce one provider signal.
#[derive(Clone, Debug)]
pub struct ProviderRequest {
    run_id: RunId,
    stage_id: StageId,
    stage_kind: StageKind,
    stage_status: StageStatus,
    role: Role,
    task: String,
    workspace_path: PathBuf,
    attempt: u32,
    signal_index: usize,
    session_id: Option<ProviderSessionId>,
    dependency_stage_ids: Vec<StageId>,
}

impl ProviderRequest {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        run_id: RunId,
        stage_id: StageId,
        stage_kind: StageKind,
        stage_status: StageStatus,
        role: Role,
        task: String,
        workspace_path: PathBuf,
        attempt: u32,
        signal_index: usize,
        session_id: Option<ProviderSessionId>,
        dependency_stage_ids: Vec<StageId>,
    ) -> Self {
        Self {
            run_id,
            stage_id,
            stage_kind,
            stage_status,
            role,
            task,
            workspace_path,
            attempt,
            signal_index,
            session_id,
            dependency_stage_ids,
        }
    }

    #[must_use]
    pub const fn run_id(&self) -> RunId {
        self.run_id
    }

    #[must_use]
    pub const fn stage_id(&self) -> &StageId {
        &self.stage_id
    }

    #[must_use]
    pub const fn stage_kind(&self) -> StageKind {
        self.stage_kind
    }

    #[must_use]
    pub const fn stage_status(&self) -> StageStatus {
        self.stage_status
    }

    #[must_use]
    pub const fn role(&self) -> Role {
        self.role
    }

    #[must_use]
    pub fn task(&self) -> &str {
        &self.task
    }

    #[must_use]
    pub fn workspace_path(&self) -> &Path {
        &self.workspace_path
    }

    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    #[must_use]
    pub const fn signal_index(&self) -> usize {
        self.signal_index
    }

    #[must_use]
    pub const fn session_id(&self) -> Option<&ProviderSessionId> {
        self.session_id.as_ref()
    }

    #[must_use]
    pub fn dependency_stage_ids(&self) -> &[StageId] {
        &self.dependency_stage_ids
    }
}

/// Synchronous provider boundary. Each poll accepts at most one native record;
/// one record may yield an ordered atomic signal batch.
pub trait Provider {
    fn id(&self) -> &ProviderId;

    fn supports_role(&self, role: Role) -> bool;

    /// Whether synchronous CLI should keep polling while process remains live.
    fn keep_attached(&self) -> bool {
        false
    }

    /// Stages optional human response before domain attention resolution commits.
    /// Default providers need no out-of-band response material.
    ///
    /// # Errors
    /// Returns provider-specific validation or durable input failures.
    fn stage_attention_response(
        &mut self,
        _store: &mut SqliteStore,
        _run_id: RunId,
        _request_id: AttentionRequestId,
        _response: Option<&str>,
    ) -> Result<(), ProviderError> {
        Ok(())
    }

    /// Polls provider without blocking scheduler indefinitely.
    ///
    /// # Errors
    /// Returns a typed adapter/protocol failure. A scripted work failure should
    /// instead be returned as [`ProviderSignal::Failed`].
    fn poll(
        &mut self,
        store: &mut SqliteStore,
        request: &ProviderRequest,
    ) -> Result<ProviderPoll, ProviderError>;
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("provider error: {message}")]
pub struct ProviderError {
    message: String,
}

impl ProviderError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}
