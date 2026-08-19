use std::path::{Path, PathBuf};

use crate::domain::{
    AttentionKind, AttentionRequestId, ModelId, NativeModelUsage, ProviderId, ProviderSessionId,
    Role, RunId, StageId, StageKind, StageStatus,
};
use crate::providers::ProviderCommit;
use crate::store::SqliteStore;

/// Provider-neutral usage added by one provider signal.
///
/// All unit values are provider-native (each runtime's own accounting) and
/// are never normalized across providers. `None` means the runtime did not
/// report the dimension for this signal; `Some(0)` means it explicitly
/// reported zero. The optional `native_models` breakdown is a parallel
/// provider-defined view that overlaps the aggregate units and must never be
/// summed with them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageDelta {
    pub input_units: u64,
    pub output_units: u64,
    pub cache_read_units: Option<u64>,
    pub cache_write_units: Option<u64>,
    pub reasoning_output_units: Option<u64>,
    pub native_models: Option<Vec<NativeModelUsage>>,
}

impl UsageDelta {
    /// Usage carrying only the stable input/output dimensions.
    #[must_use]
    pub const fn stable(input_units: u64, output_units: u64) -> Self {
        Self {
            input_units,
            output_units,
            cache_read_units: None,
            cache_write_units: None,
            reasoning_output_units: None,
            native_models: None,
        }
    }
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

/// Persisted stage context used to route one human-attention continuation.
#[derive(Clone, Debug)]
pub struct ProviderAttentionContext {
    run_id: RunId,
    stage_id: StageId,
    stage_kind: StageKind,
    role: Role,
    request_id: AttentionRequestId,
}

impl ProviderAttentionContext {
    #[must_use]
    pub const fn new(
        run_id: RunId,
        stage_id: StageId,
        stage_kind: StageKind,
        role: Role,
        request_id: AttentionRequestId,
    ) -> Self {
        Self {
            run_id,
            stage_id,
            stage_kind,
            role,
            request_id,
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
    pub const fn role(&self) -> Role {
        self.role
    }
    #[must_use]
    pub const fn request_id(&self) -> AttentionRequestId {
        self.request_id
    }
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
    /// Returns actual provider that will serve one request.
    ///
    /// # Errors
    /// Returns invalid/missing route failures.
    fn provider_id_for(&self, request: &ProviderRequest) -> Result<ProviderId, ProviderError>;

    fn supports_role(&self, role: Role) -> bool;

    /// Checks request support after route resolution.
    ///
    /// # Errors
    /// Returns invalid/missing route failures.
    fn supports_request(&self, request: &ProviderRequest) -> Result<bool, ProviderError> {
        Ok(self.supports_role(request.role()))
    }

    /// Whether synchronous CLI should keep polling while process remains live.
    ///
    /// # Errors
    /// Returns invalid/missing route failures.
    fn keep_attached_for(&self, _request: &ProviderRequest) -> Result<bool, ProviderError> {
        Ok(false)
    }

    /// Stages optional human response before domain attention resolution commits.
    /// Default providers need no out-of-band response material.
    ///
    /// # Errors
    /// Returns provider-specific validation or durable input failures.
    fn stage_attention_response(
        &mut self,
        _store: &mut SqliteStore,
        _context: &ProviderAttentionContext,
        _response: Option<&str>,
    ) -> Result<(), ProviderError> {
        Ok(())
    }

    /// Reports whether explicit eval policy may resolve this attention request
    /// without human input. Production providers default to false.
    ///
    /// # Errors
    /// Returns provider-specific persistence or safety failures.
    fn can_auto_resolve_attention(
        &mut self,
        _store: &mut SqliteStore,
        _context: &ProviderAttentionContext,
    ) -> Result<bool, ProviderError> {
        Ok(false)
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
