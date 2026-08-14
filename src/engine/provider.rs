use std::path::{Path, PathBuf};

use crate::domain::{
    AttentionKind, ModelId, ProviderId, ProviderSessionId, Role, RunId, StageId, StageKind,
    StageStatus,
};

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
    },
    Usage(UsageDelta),
    Paused,
    Interrupted,
    Completed,
    Failed(String),
}

/// Result of one non-blocking synchronous provider poll.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderPoll {
    Pending,
    Signal(ProviderSignal),
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
}

/// Synchronous provider boundary. Implementations return at most one signal
/// per poll so scheduler tests and persistence checkpoints remain deterministic.
pub trait Provider {
    fn id(&self) -> &ProviderId;

    fn supports_role(&self, role: Role) -> bool;

    /// Polls provider without blocking scheduler indefinitely.
    ///
    /// # Errors
    /// Returns a typed adapter/protocol failure. A scripted work failure should
    /// instead be returned as [`ProviderSignal::Failed`].
    fn poll(&mut self, request: &ProviderRequest) -> Result<ProviderPoll, ProviderError>;
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
