use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    AttentionError, AttentionRequest, AttentionRequestId, AttentionStatus, ConfigSnapshotId,
    DependencyKind, DomainEvent, DomainEventKind, EventMetadata, RunId, Stage, StageId,
    StageStatus, StageTransition, StageTransitionError, WorkflowDefinition, WorkflowKind,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Created,
    Preparing,
    Ready,
    Running,
    NeedsUser,
    Paused,
    Interrupted,
    Completed,
    Applied,
    Discarded,
    Failed,
}

impl RunStatus {
    #[must_use]
    pub const fn is_execution_finished(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Applied | Self::Discarded | Self::Failed
        )
    }

    #[must_use]
    pub const fn is_lifecycle_closed(self) -> bool {
        matches!(self, Self::Applied | Self::Discarded)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunTransition {
    BeginPreparation,
    FinishPreparation,
    Start,
    Pause,
    Interrupt,
    Resume,
    Recover,
    Complete,
    Fail,
    Apply,
    Discard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
enum ResumableRunStatus {
    Running,
    NeedsUser,
}

impl ResumableRunStatus {
    const fn into_status(self) -> RunStatus {
        match self {
            Self::Running => RunStatus::Running,
            Self::NeedsUser => RunStatus::NeedsUser,
        }
    }
}

impl TryFrom<RunStatus> for ResumableRunStatus {
    type Error = ();

    fn try_from(status: RunStatus) -> Result<Self, Self::Error> {
        match status {
            RunStatus::Running => Ok(Self::Running),
            RunStatus::NeedsUser => Ok(Self::NeedsUser),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Run {
    id: RunId,
    task: String,
    workflow: WorkflowDefinition,
    config_snapshot_id: ConfigSnapshotId,
    status: RunStatus,
    suspended_from: Option<ResumableRunStatus>,
    stages: Vec<Stage>,
    attention_requests: Vec<AttentionRequest>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Run {
    /// Creates one run from a validated workflow definition.
    ///
    /// # Errors
    /// Returns [`RunCreationError::EmptyTask`] when task has no content.
    pub fn new(
        id: RunId,
        task: impl Into<String>,
        workflow: WorkflowDefinition,
        config_snapshot_id: ConfigSnapshotId,
        created_at: DateTime<Utc>,
    ) -> Result<Self, RunCreationError> {
        let task = task.into();
        if task.trim().is_empty() {
            return Err(RunCreationError::EmptyTask);
        }
        let stages = workflow
            .stages()
            .iter()
            .map(|definition| Stage::from_definition(id, definition))
            .collect();
        Ok(Self {
            id,
            task,
            workflow,
            config_snapshot_id,
            status: RunStatus::Created,
            suspended_from: None,
            stages,
            attention_requests: Vec::new(),
            created_at,
            updated_at: created_at,
        })
    }

    #[must_use]
    pub const fn id(&self) -> RunId {
        self.id
    }

    #[must_use]
    pub fn task(&self) -> &str {
        &self.task
    }

    #[must_use]
    pub const fn workflow_kind(&self) -> WorkflowKind {
        self.workflow.kind()
    }

    #[must_use]
    pub const fn workflow(&self) -> &WorkflowDefinition {
        &self.workflow
    }

    #[must_use]
    pub const fn config_snapshot_id(&self) -> &ConfigSnapshotId {
        &self.config_snapshot_id
    }

    #[must_use]
    pub const fn status(&self) -> RunStatus {
        self.status
    }

    #[must_use]
    pub fn stages(&self) -> &[Stage] {
        &self.stages
    }

    #[must_use]
    pub fn stage(&self, stage_id: &StageId) -> Option<&Stage> {
        self.stages.iter().find(|stage| stage.id() == stage_id)
    }

    #[must_use]
    pub fn attention_requests(&self) -> &[AttentionRequest] {
        &self.attention_requests
    }

    #[must_use]
    pub const fn created_at(&self) -> &DateTime<Utc> {
        &self.created_at
    }

    #[must_use]
    pub const fn updated_at(&self) -> &DateTime<Utc> {
        &self.updated_at
    }

    #[must_use]
    pub fn created_event(&self, metadata: EventMetadata) -> DomainEvent {
        DomainEvent::new(
            metadata,
            self.id,
            None,
            DomainEventKind::RunCreated {
                workflow: self.workflow.kind(),
            },
        )
    }

    /// Applies one legal run lifecycle transition and returns its semantic event.
    ///
    /// # Errors
    /// Returns [`RunTransitionError`] without mutating run when transition or
    /// completion invariants are invalid.
    pub fn transition(
        &mut self,
        transition: RunTransition,
        metadata: EventMetadata,
    ) -> Result<DomainEvent, RunTransitionError> {
        let from = self.status;
        let kind = match (from, transition) {
            (RunStatus::Created, RunTransition::BeginPreparation) => {
                self.status = RunStatus::Preparing;
                DomainEventKind::RunPreparationStarted
            }
            (RunStatus::Preparing, RunTransition::FinishPreparation) => {
                self.status = RunStatus::Ready;
                DomainEventKind::RunPrepared
            }
            (RunStatus::Ready, RunTransition::Start) => {
                self.status = RunStatus::Running;
                DomainEventKind::RunStarted
            }
            (RunStatus::Running | RunStatus::NeedsUser, RunTransition::Pause) => {
                self.suspend_run(RunStatus::Paused)?;
                for stage in &mut self.stages {
                    stage.pause_for_run();
                }
                DomainEventKind::RunPaused
            }
            (RunStatus::Running | RunStatus::NeedsUser, RunTransition::Interrupt) => {
                self.suspend_run(RunStatus::Interrupted)?;
                for stage in &mut self.stages {
                    stage.interrupt_for_run();
                }
                DomainEventKind::RunInterrupted
            }
            (RunStatus::Paused, RunTransition::Resume) => {
                self.restore_run()?;
                for stage in &mut self.stages {
                    stage.resume_for_run();
                }
                DomainEventKind::RunResumed
            }
            (RunStatus::Interrupted, RunTransition::Recover) => {
                self.restore_run()?;
                for stage in &mut self.stages {
                    stage.recover_for_run();
                }
                DomainEventKind::RunRecovered
            }
            (RunStatus::Running, RunTransition::Complete) => {
                self.ensure_completion_allowed()?;
                self.status = RunStatus::Completed;
                DomainEventKind::RunCompleted
            }
            (
                RunStatus::Preparing
                | RunStatus::Ready
                | RunStatus::Running
                | RunStatus::NeedsUser
                | RunStatus::Paused
                | RunStatus::Interrupted,
                RunTransition::Fail,
            ) => {
                self.cancel_all_pending(metadata.occurred_at())?;
                self.status = RunStatus::Failed;
                self.suspended_from = None;
                DomainEventKind::RunFailed
            }
            (RunStatus::Completed, RunTransition::Apply) => {
                self.status = RunStatus::Applied;
                DomainEventKind::RunApplied
            }
            (
                RunStatus::Created
                | RunStatus::Preparing
                | RunStatus::Ready
                | RunStatus::Running
                | RunStatus::NeedsUser
                | RunStatus::Paused
                | RunStatus::Interrupted
                | RunStatus::Completed
                | RunStatus::Failed,
                RunTransition::Discard,
            ) => {
                self.cancel_all_pending(metadata.occurred_at())?;
                self.status = RunStatus::Discarded;
                self.suspended_from = None;
                DomainEventKind::RunDiscarded
            }
            _ => {
                return Err(RunTransitionError::InvalidTransition { from, transition });
            }
        };

        self.updated_at = metadata.occurred_at();
        Ok(DomainEvent::new(metadata, self.id, None, kind))
    }

    /// Applies one legal stage transition owned by this active run.
    ///
    /// # Errors
    /// Rejects inactive runs, unknown stages, unsatisfied dependencies, retry
    /// invalidation, and invalid stage lifecycle changes.
    pub fn transition_stage(
        &mut self,
        stage_id: &StageId,
        transition: StageTransition,
        metadata: EventMetadata,
    ) -> Result<DomainEvent, RunStageError> {
        if !matches!(self.status, RunStatus::Running | RunStatus::NeedsUser) {
            return Err(RunStageError::RunNotActive(self.status));
        }
        let index = self.stage_index(stage_id)?;
        let degraded = if transition == StageTransition::MarkReady {
            self.ensure_stage_ready(index)?
        } else {
            Vec::new()
        };
        if transition == StageTransition::Retry {
            self.ensure_retry_safe(stage_id)?;
        }

        self.stages[index].transition(transition)?;
        self.updated_at = metadata.occurred_at();
        let kind = match transition {
            StageTransition::MarkReady => DomainEventKind::StageReady {
                degraded: !degraded.is_empty(),
            },
            StageTransition::Start => DomainEventKind::StageStarted,
            StageTransition::Pause => DomainEventKind::StagePaused,
            StageTransition::Interrupt => DomainEventKind::StageInterrupted,
            StageTransition::Resume => DomainEventKind::StageResumed,
            StageTransition::Recover => DomainEventKind::StageRecovered,
            StageTransition::Complete => DomainEventKind::StageCompleted,
            StageTransition::Skip => DomainEventKind::StageSkipped,
            StageTransition::Fail => DomainEventKind::StageFailed,
            StageTransition::Retry => DomainEventKind::StageRetryScheduled,
        };
        Ok(DomainEvent::new(
            metadata,
            self.id,
            Some(stage_id.clone()),
            kind,
        ))
    }

    /// Adds one pending attention request and moves stage/run to `NeedsUser`.
    ///
    /// # Errors
    /// Rejects inactive runs, mismatched identities, duplicate request IDs, or
    /// stages that are not currently executing.
    pub fn request_attention(
        &mut self,
        request: AttentionRequest,
        metadata: EventMetadata,
    ) -> Result<DomainEvent, RunAttentionError> {
        if !matches!(self.status, RunStatus::Running | RunStatus::NeedsUser) {
            return Err(RunAttentionError::RunNotActive(self.status));
        }
        if request.run_id() != self.id {
            return Err(RunAttentionError::WrongRun {
                expected: self.id,
                actual: request.run_id(),
            });
        }
        if !request.status().is_pending() {
            return Err(RunAttentionError::RequestNotPending(request.id()));
        }
        if self
            .attention_requests
            .iter()
            .any(|existing| existing.id() == request.id())
        {
            return Err(RunAttentionError::DuplicateRequest(request.id()));
        }
        let stage_index = self.stage_index(request.stage_id())?;
        self.stages[stage_index].request_attention()?;

        let request_id = request.id();
        let kind = request.kind();
        let stage_id = request.stage_id().clone();
        self.attention_requests.push(request);
        self.status = RunStatus::NeedsUser;
        self.updated_at = metadata.occurred_at();
        Ok(DomainEvent::new(
            metadata,
            self.id,
            Some(stage_id),
            DomainEventKind::NeedsUser {
                attention_request_id: request_id,
                kind,
            },
        ))
    }

    /// Resolves one pending request. Last resolution restores affected stage/run.
    ///
    /// # Errors
    /// Rejects unknown or already-closed requests and inconsistent ownership.
    pub fn resolve_attention(
        &mut self,
        request_id: AttentionRequestId,
        metadata: EventMetadata,
    ) -> Result<DomainEvent, RunAttentionError> {
        self.close_attention(request_id, metadata, false)
    }

    /// Cancels one pending request without pretending a human answered it.
    ///
    /// # Errors
    /// Rejects unknown or already-closed requests and inconsistent ownership.
    pub fn cancel_attention(
        &mut self,
        request_id: AttentionRequestId,
        metadata: EventMetadata,
    ) -> Result<DomainEvent, RunAttentionError> {
        self.close_attention(request_id, metadata, true)
    }

    /// Checks cross-aggregate invariants used by future validated rehydration.
    ///
    /// # Errors
    /// Returns a typed invariant violation when owned stages, attention, or
    /// lifecycle aggregates contradict one another.
    pub fn validate_invariants(&self) -> Result<(), RunInvariantError> {
        let mut stage_ids = HashSet::new();
        for stage in &self.stages {
            if stage.run_id() != self.id {
                return Err(RunInvariantError::StageOwnedByDifferentRun(
                    stage.id().clone(),
                ));
            }
            if !stage_ids.insert(stage.id().clone()) {
                return Err(RunInvariantError::DuplicateStage(stage.id().clone()));
            }
        }
        let expected_stage_ids = self
            .workflow
            .stages()
            .iter()
            .map(|stage| stage.id().clone())
            .collect::<HashSet<_>>();
        if expected_stage_ids != stage_ids {
            return Err(RunInvariantError::WorkflowStageSetMismatch);
        }

        let mut attention_ids = HashSet::new();
        for request in &self.attention_requests {
            if !attention_ids.insert(request.id()) {
                return Err(RunInvariantError::DuplicateAttention(request.id()));
            }
            if request.run_id() != self.id || self.stage(request.stage_id()).is_none() {
                return Err(RunInvariantError::InvalidAttentionOwner(request.id()));
            }
        }

        let pending = self.pending_attention_count();
        let run_is_suspended = matches!(self.status, RunStatus::Paused | RunStatus::Interrupted);
        if run_is_suspended != self.suspended_from.is_some() {
            return Err(RunInvariantError::RunSuspensionMismatch);
        }
        let run_expects_attention = match self.status {
            RunStatus::NeedsUser => true,
            RunStatus::Paused | RunStatus::Interrupted => {
                self.suspended_from == Some(ResumableRunStatus::NeedsUser)
            }
            _ => false,
        };
        if run_expects_attention != (pending > 0) {
            return Err(RunInvariantError::RunAttentionMismatch);
        }
        for stage in &self.stages {
            let has_pending = self.pending_for_stage(stage.id());
            if stage.expects_attention() != has_pending {
                return Err(RunInvariantError::StageAttentionMismatch(
                    stage.id().clone(),
                ));
            }
        }
        if matches!(self.status, RunStatus::Completed | RunStatus::Applied) {
            self.ensure_completion_allowed()
                .map_err(|_| RunInvariantError::InvalidCompletedRun)?;
        }
        if self.status.is_lifecycle_closed() && pending > 0 {
            return Err(RunInvariantError::ClosedRunHasPendingAttention);
        }
        Ok(())
    }

    fn stage_index(&self, stage_id: &StageId) -> Result<usize, RunStageError> {
        self.stages
            .iter()
            .position(|stage| stage.id() == stage_id)
            .ok_or_else(|| RunStageError::UnknownStage(stage_id.clone()))
    }

    fn ensure_stage_ready(&self, index: usize) -> Result<Vec<StageId>, RunStageError> {
        let stage = &self.stages[index];
        let mut waiting = Vec::new();
        let mut blocked = Vec::new();
        let mut degraded = Vec::new();
        for dependency in stage.dependencies() {
            let dependency_stage = self
                .stage(dependency.stage_id())
                .ok_or_else(|| RunStageError::UnknownStage(dependency.stage_id().clone()))?;
            match (dependency.kind(), dependency_stage.status()) {
                (_, StageStatus::Completed) => {}
                (DependencyKind::Required, StageStatus::Failed | StageStatus::Skipped) => {
                    blocked.push(dependency.stage_id().clone());
                }
                (DependencyKind::Optional, StageStatus::Failed | StageStatus::Skipped) => {
                    degraded.push(dependency.stage_id().clone());
                }
                _ => waiting.push(dependency.stage_id().clone()),
            }
        }
        if !blocked.is_empty() {
            return Err(RunStageError::RequiredDependenciesBlocked {
                stage_id: stage.id().clone(),
                dependencies: blocked,
            });
        }
        if !waiting.is_empty() {
            return Err(RunStageError::DependenciesNotFinished {
                stage_id: stage.id().clone(),
                dependencies: waiting,
            });
        }
        Ok(degraded)
    }

    fn ensure_retry_safe(&self, stage_id: &StageId) -> Result<(), RunStageError> {
        let advanced = self
            .stages
            .iter()
            .filter(|stage| {
                stage
                    .dependencies()
                    .iter()
                    .any(|dependency| dependency.stage_id() == stage_id)
                    && !matches!(stage.status(), StageStatus::Pending | StageStatus::Ready)
            })
            .map(|stage| stage.id().clone())
            .collect::<Vec<_>>();
        if advanced.is_empty() {
            Ok(())
        } else {
            Err(RunStageError::RetryWouldInvalidate {
                stage_id: stage_id.clone(),
                advanced_dependents: advanced,
            })
        }
    }

    fn ensure_completion_allowed(&self) -> Result<(), RunTransitionError> {
        let mut blockers = Vec::new();
        for stage in &self.stages {
            if !stage.status().is_terminal_outcome() {
                blockers.push(CompletionBlocker {
                    stage_id: stage.id().clone(),
                    status: stage.status(),
                    reason: CompletionBlockerReason::StageNotTerminal,
                });
                continue;
            }
            if stage.status() != StageStatus::Failed {
                continue;
            }

            let dependent_edges = self
                .stages
                .iter()
                .flat_map(|dependent| {
                    dependent
                        .dependencies()
                        .iter()
                        .filter(move |dependency| dependency.stage_id() == stage.id())
                })
                .collect::<Vec<_>>();
            let reason = if dependent_edges.is_empty() {
                Some(CompletionBlockerReason::FailedLeafStage)
            } else if dependent_edges
                .iter()
                .any(|dependency| dependency.kind() == DependencyKind::Required)
            {
                Some(CompletionBlockerReason::RequiredStageFailed)
            } else {
                None
            };
            if let Some(reason) = reason {
                blockers.push(CompletionBlocker {
                    stage_id: stage.id().clone(),
                    status: stage.status(),
                    reason,
                });
            }
        }
        if self.pending_attention_count() > 0 {
            return Err(RunTransitionError::PendingAttention);
        }
        if blockers.is_empty() {
            Ok(())
        } else {
            Err(RunTransitionError::CompletionBlocked(blockers))
        }
    }

    fn suspend_run(&mut self, suspended_status: RunStatus) -> Result<(), RunTransitionError> {
        let resume_to = ResumableRunStatus::try_from(self.status)
            .map_err(|()| RunTransitionError::InvalidSuspensionSource { from: self.status })?;
        self.suspended_from = Some(resume_to);
        self.status = suspended_status;
        Ok(())
    }

    fn restore_run(&mut self) -> Result<(), RunTransitionError> {
        let resume_to = self
            .suspended_from
            .ok_or(RunTransitionError::MissingSuspensionContext)?;
        self.status = resume_to.into_status();
        self.suspended_from = None;
        Ok(())
    }

    fn close_attention(
        &mut self,
        request_id: AttentionRequestId,
        metadata: EventMetadata,
        cancel: bool,
    ) -> Result<DomainEvent, RunAttentionError> {
        let index = self
            .attention_requests
            .iter()
            .position(|request| request.id() == request_id)
            .ok_or(RunAttentionError::UnknownRequest(request_id))?;
        if !self.attention_requests[index].status().is_pending() {
            return Err(RunAttentionError::Attention(AttentionError::AlreadyClosed(
                request_id,
            )));
        }
        let stage_id = self.attention_requests[index].stage_id().clone();
        let stage_index = self.stage_index(&stage_id)?;
        if cancel {
            self.attention_requests[index].cancel(metadata.occurred_at())?;
        } else {
            self.attention_requests[index].resolve(metadata.occurred_at())?;
        }

        let stage_pending = self.pending_for_stage(&stage_id);
        self.stages[stage_index].attention_resolved(stage_pending);
        if self.pending_attention_count() == 0 {
            match self.status {
                RunStatus::NeedsUser => self.status = RunStatus::Running,
                RunStatus::Paused | RunStatus::Interrupted
                    if self.suspended_from == Some(ResumableRunStatus::NeedsUser) =>
                {
                    self.suspended_from = Some(ResumableRunStatus::Running);
                }
                _ => {}
            }
        }
        self.updated_at = metadata.occurred_at();
        let kind = if cancel {
            DomainEventKind::AttentionCancelled {
                attention_request_id: request_id,
            }
        } else {
            DomainEventKind::AttentionResolved {
                attention_request_id: request_id,
            }
        };
        Ok(DomainEvent::new(metadata, self.id, Some(stage_id), kind))
    }

    fn pending_for_stage(&self, stage_id: &StageId) -> bool {
        self.attention_requests.iter().any(|request| {
            request.stage_id() == stage_id && request.status() == &AttentionStatus::Pending
        })
    }

    fn pending_attention_count(&self) -> usize {
        self.attention_requests
            .iter()
            .filter(|request| request.status().is_pending())
            .count()
    }

    fn cancel_all_pending(&mut self, at: DateTime<Utc>) -> Result<(), AttentionError> {
        if let Some(request) = self
            .attention_requests
            .iter()
            .find(|request| request.status().is_pending() && request.created_at() > &at)
        {
            return Err(AttentionError::ClosedBeforeCreation {
                id: request.id(),
                created_at: *request.created_at(),
                closed_at: at,
            });
        }
        for request in &mut self.attention_requests {
            if request.status().is_pending() {
                request.cancel(at)?;
            }
        }
        for stage in &mut self.stages {
            stage.attention_resolved(false);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RunCreationError {
    #[error("run task must not be empty")]
    EmptyTask,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionBlockerReason {
    StageNotTerminal,
    FailedLeafStage,
    RequiredStageFailed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionBlocker {
    pub stage_id: StageId,
    pub status: StageStatus,
    pub reason: CompletionBlockerReason,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RunTransitionError {
    #[error("run transition {transition:?} is invalid from {from:?}")]
    InvalidTransition {
        from: RunStatus,
        transition: RunTransition,
    },
    #[error("run cannot suspend from {from:?}")]
    InvalidSuspensionSource { from: RunStatus },
    #[error("run suspension context is missing")]
    MissingSuspensionContext,
    #[error("run completion is blocked: {0:?}")]
    CompletionBlocked(Vec<CompletionBlocker>),
    #[error("run completion is blocked by unresolved attention")]
    PendingAttention,
    #[error(transparent)]
    Attention(#[from] AttentionError),
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RunStageError {
    #[error("run is not active: {0:?}")]
    RunNotActive(RunStatus),
    #[error("unknown stage: {0}")]
    UnknownStage(StageId),
    #[error("stage {stage_id} is waiting for dependencies: {dependencies:?}")]
    DependenciesNotFinished {
        stage_id: StageId,
        dependencies: Vec<StageId>,
    },
    #[error("stage {stage_id} has blocked required dependencies: {dependencies:?}")]
    RequiredDependenciesBlocked {
        stage_id: StageId,
        dependencies: Vec<StageId>,
    },
    #[error(
        "retrying stage {stage_id} would invalidate advanced dependents: {advanced_dependents:?}"
    )]
    RetryWouldInvalidate {
        stage_id: StageId,
        advanced_dependents: Vec<StageId>,
    },
    #[error(transparent)]
    Stage(#[from] StageTransitionError),
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RunAttentionError {
    #[error("run is not active for new attention: {0:?}")]
    RunNotActive(RunStatus),
    #[error("attention request belongs to run {actual}, expected {expected}")]
    WrongRun { expected: RunId, actual: RunId },
    #[error("duplicate attention request: {0}")]
    DuplicateRequest(AttentionRequestId),
    #[error("attention request is already resolved or cancelled: {0}")]
    RequestNotPending(AttentionRequestId),
    #[error("unknown attention request: {0}")]
    UnknownRequest(AttentionRequestId),
    #[error(transparent)]
    Stage(#[from] RunStageError),
    #[error(transparent)]
    StageLifecycle(#[from] StageTransitionError),
    #[error(transparent)]
    Attention(#[from] AttentionError),
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RunInvariantError {
    #[error("stage {0} belongs to another run")]
    StageOwnedByDifferentRun(StageId),
    #[error("duplicate stage in run: {0}")]
    DuplicateStage(StageId),
    #[error("run stage set does not match its workflow definition")]
    WorkflowStageSetMismatch,
    #[error("duplicate attention request in run: {0}")]
    DuplicateAttention(AttentionRequestId),
    #[error("attention request has invalid run or stage ownership: {0}")]
    InvalidAttentionOwner(AttentionRequestId),
    #[error("run status and pending attention disagree")]
    RunAttentionMismatch,
    #[error("run status and suspension context disagree")]
    RunSuspensionMismatch,
    #[error("stage {0} status and pending attention disagree")]
    StageAttentionMismatch(StageId),
    #[error("completed/applied run contains invalid stage outcomes")]
    InvalidCompletedRun,
    #[error("closed run contains pending attention")]
    ClosedRunHasPendingAttention,
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::domain::{
        AttentionKind, AttentionRequestId, Dependency, EventId, Role, StageDefinition, StageKind,
        WorkflowDefinitionError,
    };

    fn at(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 14, 8, 0, second)
            .single()
            .unwrap()
    }

    fn metadata(value: u128, second: u32) -> EventMetadata {
        EventMetadata::new(EventId::from_u128(value), at(second))
    }

    fn id(value: &str) -> StageId {
        StageId::new(value).unwrap()
    }

    fn fast_workflow() -> WorkflowDefinition {
        WorkflowDefinition::new(
            WorkflowKind::Fast,
            vec![StageDefinition::new(
                id("implementation"),
                StageKind::Implementation,
                Role::Implementer,
                vec![],
            )],
        )
        .unwrap()
    }

    fn new_run(workflow: WorkflowDefinition) -> Run {
        Run::new(
            RunId::from_u128(10),
            "test task",
            workflow,
            ConfigSnapshotId::new("recommended-2026.08").unwrap(),
            at(0),
        )
        .unwrap()
    }

    fn start_run(run: &mut Run) {
        for (transition, event, second) in [
            (RunTransition::BeginPreparation, 1, 1),
            (RunTransition::FinishPreparation, 2, 2),
            (RunTransition::Start, 3, 3),
        ] {
            run.transition(transition, metadata(event, second)).unwrap();
        }
    }

    fn complete_stage(run: &mut Run, stage_id: &StageId, event: u128) {
        run.transition_stage(stage_id, StageTransition::MarkReady, metadata(event, 4))
            .unwrap();
        run.transition_stage(stage_id, StageTransition::Start, metadata(event + 1, 5))
            .unwrap();
        run.transition_stage(stage_id, StageTransition::Complete, metadata(event + 2, 6))
            .unwrap();
    }

    #[test]
    fn run_happy_path_reaches_applied_through_atomic_ready_boundary() {
        let mut run = new_run(fast_workflow());
        assert_eq!(run.status(), RunStatus::Created);
        start_run(&mut run);
        assert_eq!(run.status(), RunStatus::Running);
        complete_stage(&mut run, &id("implementation"), 10);
        run.transition(RunTransition::Complete, metadata(20, 7))
            .unwrap();
        run.transition(RunTransition::Apply, metadata(21, 8))
            .unwrap();

        assert_eq!(run.status(), RunStatus::Applied);
        assert!(run.status().is_lifecycle_closed());
        assert!(run.validate_invariants().is_ok());
    }

    #[test]
    fn completed_and_discarded_runs_reject_execution() {
        let mut completed = new_run(fast_workflow());
        start_run(&mut completed);
        complete_stage(&mut completed, &id("implementation"), 30);
        completed
            .transition(RunTransition::Complete, metadata(40, 7))
            .unwrap();
        let before = completed.clone();
        assert!(
            completed
                .transition(RunTransition::Start, metadata(41, 8))
                .is_err()
        );
        assert_eq!(completed, before);

        completed
            .transition(RunTransition::Discard, metadata(42, 9))
            .unwrap();
        assert_eq!(completed.status(), RunStatus::Discarded);
        assert!(
            completed
                .transition(RunTransition::Start, metadata(43, 10))
                .is_err()
        );
    }

    #[test]
    fn run_pause_and_interruption_restore_distinctly() {
        let mut paused = new_run(fast_workflow());
        start_run(&mut paused);
        paused
            .transition(RunTransition::Pause, metadata(50, 4))
            .unwrap();
        assert_eq!(paused.status(), RunStatus::Paused);
        assert!(
            paused
                .transition(RunTransition::Recover, metadata(51, 5))
                .is_err()
        );
        paused
            .transition(RunTransition::Resume, metadata(52, 6))
            .unwrap();
        assert_eq!(paused.status(), RunStatus::Running);

        let mut interrupted = new_run(fast_workflow());
        start_run(&mut interrupted);
        interrupted
            .transition(RunTransition::Interrupt, metadata(53, 4))
            .unwrap();
        assert_eq!(interrupted.status(), RunStatus::Interrupted);
        assert!(
            interrupted
                .transition(RunTransition::Resume, metadata(54, 5))
                .is_err()
        );
        interrupted
            .transition(RunTransition::Recover, metadata(55, 6))
            .unwrap();
        assert_eq!(interrupted.status(), RunStatus::Running);
    }

    #[test]
    fn multiple_attention_requests_resolve_in_order_without_losing_state() {
        let mut run = new_run(fast_workflow());
        start_run(&mut run);
        let stage_id = id("implementation");
        run.transition_stage(&stage_id, StageTransition::MarkReady, metadata(60, 4))
            .unwrap();
        run.transition_stage(&stage_id, StageTransition::Start, metadata(61, 5))
            .unwrap();
        let first = AttentionRequest::new(
            AttentionRequestId::from_u128(1),
            run.id(),
            stage_id.clone(),
            AttentionKind::Permission,
            "Run database reset",
            at(6),
        )
        .unwrap();
        let second = AttentionRequest::new(
            AttentionRequestId::from_u128(2),
            run.id(),
            stage_id.clone(),
            AttentionKind::Question,
            "Choose fallback",
            at(7),
        )
        .unwrap();
        run.request_attention(first, metadata(62, 6)).unwrap();
        run.request_attention(second, metadata(63, 7)).unwrap();
        assert_eq!(run.status(), RunStatus::NeedsUser);
        assert_eq!(
            run.stage(&stage_id).unwrap().status(),
            StageStatus::NeedsUser
        );

        run.resolve_attention(AttentionRequestId::from_u128(1), metadata(64, 8))
            .unwrap();
        assert_eq!(run.status(), RunStatus::NeedsUser);
        run.resolve_attention(AttentionRequestId::from_u128(2), metadata(65, 9))
            .unwrap();
        assert_eq!(run.status(), RunStatus::Running);
        assert_eq!(run.stage(&stage_id).unwrap().status(), StageStatus::Running);
        assert!(run.validate_invariants().is_ok());
        assert!(
            run.resolve_attention(AttentionRequestId::from_u128(2), metadata(66, 10))
                .is_err()
        );
    }

    #[test]
    fn closed_attention_request_cannot_enter_active_queue() {
        let mut run = new_run(fast_workflow());
        start_run(&mut run);
        let stage_id = id("implementation");
        run.transition_stage(&stage_id, StageTransition::MarkReady, metadata(67, 4))
            .unwrap();
        run.transition_stage(&stage_id, StageTransition::Start, metadata(68, 5))
            .unwrap();
        let mut request = AttentionRequest::new(
            AttentionRequestId::from_u128(3),
            run.id(),
            stage_id,
            AttentionKind::Question,
            "Already answered",
            at(6),
        )
        .unwrap();
        request.resolve(at(7)).unwrap();
        let before = run.clone();

        assert_eq!(
            run.request_attention(request, metadata(69, 8)),
            Err(RunAttentionError::RequestNotPending(
                AttentionRequestId::from_u128(3)
            ))
        );
        assert_eq!(run, before);
    }

    #[test]
    fn closing_run_cannot_backdate_attention_or_partially_mutate() {
        let mut run = new_run(fast_workflow());
        start_run(&mut run);
        let stage_id = id("implementation");
        run.transition_stage(&stage_id, StageTransition::MarkReady, metadata(76, 4))
            .unwrap();
        run.transition_stage(&stage_id, StageTransition::Start, metadata(77, 5))
            .unwrap();
        run.request_attention(
            AttentionRequest::new(
                AttentionRequestId::from_u128(8),
                run.id(),
                stage_id,
                AttentionKind::Permission,
                "Approve change",
                at(8),
            )
            .unwrap(),
            metadata(78, 8),
        )
        .unwrap();
        let before = run.clone();

        assert!(matches!(
            run.transition(RunTransition::Discard, metadata(79, 7)),
            Err(RunTransitionError::Attention(
                AttentionError::ClosedBeforeCreation { .. }
            ))
        ));
        assert_eq!(run, before);
    }

    #[test]
    fn attention_resolution_while_paused_changes_resume_target() {
        let mut run = new_run(fast_workflow());
        start_run(&mut run);
        let stage_id = id("implementation");
        run.transition_stage(&stage_id, StageTransition::MarkReady, metadata(70, 4))
            .unwrap();
        run.transition_stage(&stage_id, StageTransition::Start, metadata(71, 5))
            .unwrap();
        let request_id = AttentionRequestId::from_u128(7);
        run.request_attention(
            AttentionRequest::new(
                request_id,
                run.id(),
                stage_id.clone(),
                AttentionKind::Decision,
                "Proceed?",
                at(6),
            )
            .unwrap(),
            metadata(72, 6),
        )
        .unwrap();
        run.transition(RunTransition::Pause, metadata(73, 7))
            .unwrap();
        run.resolve_attention(request_id, metadata(74, 8)).unwrap();
        run.transition(RunTransition::Resume, metadata(75, 9))
            .unwrap();

        assert_eq!(run.status(), RunStatus::Running);
        assert_eq!(run.stage(&stage_id).unwrap().status(), StageStatus::Running);
        assert!(run.validate_invariants().is_ok());
    }

    #[test]
    fn required_dependencies_block_while_optional_failure_allows_degraded_ready() {
        let research = id("research");
        let independent = id("independent");
        let synthesis = id("synthesis");
        let workflow = WorkflowDefinition::new(
            WorkflowKind::Review,
            vec![
                StageDefinition::new(
                    research.clone(),
                    StageKind::DeepAnalysis,
                    Role::Reviewer,
                    vec![],
                ),
                StageDefinition::new(
                    independent.clone(),
                    StageKind::IndependentReview,
                    Role::Reviewer,
                    vec![],
                ),
                StageDefinition::new(
                    synthesis.clone(),
                    StageKind::Synthesis,
                    Role::Reviewer,
                    vec![
                        Dependency::required(research.clone()),
                        Dependency::optional(independent.clone()),
                    ],
                ),
            ],
        )
        .unwrap();
        let mut run = new_run(workflow);
        start_run(&mut run);
        assert!(matches!(
            run.transition_stage(&synthesis, StageTransition::MarkReady, metadata(80, 4)),
            Err(RunStageError::DependenciesNotFinished { .. })
        ));
        complete_stage(&mut run, &research, 81);
        run.transition_stage(&independent, StageTransition::MarkReady, metadata(84, 7))
            .unwrap();
        run.transition_stage(&independent, StageTransition::Start, metadata(85, 8))
            .unwrap();
        run.transition_stage(&independent, StageTransition::Fail, metadata(86, 9))
            .unwrap();
        let event = run
            .transition_stage(&synthesis, StageTransition::MarkReady, metadata(87, 10))
            .unwrap();

        assert_eq!(
            event.kind(),
            &DomainEventKind::StageReady { degraded: true }
        );
    }

    #[test]
    fn optional_failed_branch_can_complete_but_required_or_leaf_failure_cannot() {
        let optional = id("optional_review");
        let synthesis = id("synthesis");
        let workflow = WorkflowDefinition::new(
            WorkflowKind::Review,
            vec![
                StageDefinition::new(
                    optional.clone(),
                    StageKind::IndependentReview,
                    Role::Reviewer,
                    vec![],
                ),
                StageDefinition::new(
                    synthesis.clone(),
                    StageKind::Synthesis,
                    Role::Reviewer,
                    vec![Dependency::optional(optional.clone())],
                ),
            ],
        )
        .unwrap();
        let mut run = new_run(workflow);
        start_run(&mut run);
        run.transition_stage(&optional, StageTransition::MarkReady, metadata(90, 4))
            .unwrap();
        run.transition_stage(&optional, StageTransition::Start, metadata(91, 5))
            .unwrap();
        run.transition_stage(&optional, StageTransition::Fail, metadata(92, 6))
            .unwrap();
        complete_stage(&mut run, &synthesis, 93);
        run.transition(RunTransition::Complete, metadata(96, 10))
            .unwrap();
        assert_eq!(run.status(), RunStatus::Completed);

        let mut failed_leaf = new_run(fast_workflow());
        start_run(&mut failed_leaf);
        let implementation = id("implementation");
        failed_leaf
            .transition_stage(&implementation, StageTransition::MarkReady, metadata(97, 4))
            .unwrap();
        failed_leaf
            .transition_stage(&implementation, StageTransition::Start, metadata(98, 5))
            .unwrap();
        failed_leaf
            .transition_stage(&implementation, StageTransition::Fail, metadata(99, 6))
            .unwrap();
        assert!(matches!(
            failed_leaf.transition(RunTransition::Complete, metadata(100, 7)),
            Err(RunTransitionError::CompletionBlocked(_))
        ));
    }

    #[test]
    fn unknown_stage_and_retry_execution_boundary_are_enforced() {
        let first = id("first");
        let second = id("second");
        let workflow = WorkflowDefinition::new(
            WorkflowKind::Standard,
            vec![
                StageDefinition::new(
                    first.clone(),
                    StageKind::Architecture,
                    Role::Architect,
                    vec![],
                ),
                StageDefinition::new(
                    second.clone(),
                    StageKind::Implementation,
                    Role::Implementer,
                    vec![Dependency::optional(first.clone())],
                ),
            ],
        )
        .unwrap();
        let mut run = new_run(workflow);
        start_run(&mut run);
        assert_eq!(
            run.transition_stage(&id("missing"), StageTransition::MarkReady, metadata(110, 4)),
            Err(RunStageError::UnknownStage(id("missing")))
        );
        run.transition_stage(&first, StageTransition::MarkReady, metadata(111, 4))
            .unwrap();
        run.transition_stage(&first, StageTransition::Start, metadata(112, 5))
            .unwrap();
        run.transition_stage(&first, StageTransition::Fail, metadata(113, 6))
            .unwrap();
        run.transition_stage(&second, StageTransition::MarkReady, metadata(114, 7))
            .unwrap();

        let mut ready_dependent = run.clone();
        ready_dependent
            .transition_stage(&first, StageTransition::Retry, metadata(115, 8))
            .unwrap();
        assert_eq!(
            ready_dependent.stage(&first).unwrap().status(),
            StageStatus::Pending
        );
        assert_eq!(
            ready_dependent.stage(&second).unwrap().status(),
            StageStatus::Ready
        );

        run.transition_stage(&second, StageTransition::Start, metadata(116, 8))
            .unwrap();
        assert!(matches!(
            run.transition_stage(&first, StageTransition::Retry, metadata(117, 9)),
            Err(RunStageError::RetryWouldInvalidate { .. })
        ));
    }

    #[test]
    fn status_and_transition_enums_use_stable_snake_case_serialization() {
        assert_eq!(
            serde_json::to_string(&RunStatus::NeedsUser).unwrap(),
            "\"needs_user\""
        );
        let decoded: RunStatus = serde_json::from_str("\"interrupted\"").unwrap();
        assert_eq!(decoded, RunStatus::Interrupted);
    }

    #[test]
    fn invariant_validation_rejects_orphaned_suspension_context() {
        let mut run = new_run(fast_workflow());
        run.suspended_from = Some(ResumableRunStatus::Running);

        assert_eq!(
            run.validate_invariants(),
            Err(RunInvariantError::RunSuspensionMismatch)
        );
    }

    #[test]
    fn workflow_constructor_error_type_remains_distinct() {
        let error = WorkflowDefinition::new(WorkflowKind::Fast, vec![]).unwrap_err();
        assert_eq!(error, WorkflowDefinitionError::NoStages);
    }

    fn candidate_for_status(status: RunStatus) -> Run {
        let mut run = new_run(fast_workflow());
        match status {
            RunStatus::Created => {}
            RunStatus::Preparing => {
                run.transition(RunTransition::BeginPreparation, metadata(200, 1))
                    .unwrap();
            }
            RunStatus::Ready => {
                run.transition(RunTransition::BeginPreparation, metadata(201, 1))
                    .unwrap();
                run.transition(RunTransition::FinishPreparation, metadata(202, 2))
                    .unwrap();
            }
            RunStatus::Running => {
                start_run(&mut run);
                complete_stage(&mut run, &id("implementation"), 203);
            }
            RunStatus::NeedsUser => {
                start_run(&mut run);
                let stage_id = id("implementation");
                run.transition_stage(&stage_id, StageTransition::MarkReady, metadata(206, 4))
                    .unwrap();
                run.transition_stage(&stage_id, StageTransition::Start, metadata(207, 5))
                    .unwrap();
                run.request_attention(
                    AttentionRequest::new(
                        AttentionRequestId::from_u128(200),
                        run.id(),
                        stage_id,
                        AttentionKind::Question,
                        "Need input",
                        at(6),
                    )
                    .unwrap(),
                    metadata(208, 6),
                )
                .unwrap();
            }
            RunStatus::Paused => {
                start_run(&mut run);
                complete_stage(&mut run, &id("implementation"), 209);
                run.transition(RunTransition::Pause, metadata(212, 7))
                    .unwrap();
            }
            RunStatus::Interrupted => {
                start_run(&mut run);
                complete_stage(&mut run, &id("implementation"), 213);
                run.transition(RunTransition::Interrupt, metadata(216, 7))
                    .unwrap();
            }
            RunStatus::Completed => {
                start_run(&mut run);
                complete_stage(&mut run, &id("implementation"), 217);
                run.transition(RunTransition::Complete, metadata(220, 7))
                    .unwrap();
            }
            RunStatus::Applied => {
                run = candidate_for_status(RunStatus::Completed);
                run.transition(RunTransition::Apply, metadata(221, 8))
                    .unwrap();
            }
            RunStatus::Discarded => {
                run.transition(RunTransition::Discard, metadata(222, 1))
                    .unwrap();
            }
            RunStatus::Failed => {
                run.transition(RunTransition::BeginPreparation, metadata(223, 1))
                    .unwrap();
                run.transition(RunTransition::Fail, metadata(224, 2))
                    .unwrap();
            }
        }
        run
    }

    #[test]
    fn run_transition_table_rejects_every_unspecified_pair_without_mutation() {
        let statuses = [
            RunStatus::Created,
            RunStatus::Preparing,
            RunStatus::Ready,
            RunStatus::Running,
            RunStatus::NeedsUser,
            RunStatus::Paused,
            RunStatus::Interrupted,
            RunStatus::Completed,
            RunStatus::Applied,
            RunStatus::Discarded,
            RunStatus::Failed,
        ];
        let transitions = [
            RunTransition::BeginPreparation,
            RunTransition::FinishPreparation,
            RunTransition::Start,
            RunTransition::Pause,
            RunTransition::Interrupt,
            RunTransition::Resume,
            RunTransition::Recover,
            RunTransition::Complete,
            RunTransition::Fail,
            RunTransition::Apply,
            RunTransition::Discard,
        ];

        for status in statuses {
            for transition in transitions {
                let mut run = candidate_for_status(status);
                let before = run.clone();
                let result = run.transition(transition, metadata(300, 30));
                let expected = match (status, transition) {
                    (RunStatus::Created, RunTransition::BeginPreparation) => {
                        Some(RunStatus::Preparing)
                    }
                    (RunStatus::Preparing, RunTransition::FinishPreparation) => {
                        Some(RunStatus::Ready)
                    }
                    (RunStatus::Ready, RunTransition::Start)
                    | (RunStatus::Paused, RunTransition::Resume)
                    | (RunStatus::Interrupted, RunTransition::Recover) => Some(RunStatus::Running),
                    (RunStatus::Running | RunStatus::NeedsUser, RunTransition::Pause) => {
                        Some(RunStatus::Paused)
                    }
                    (RunStatus::Running | RunStatus::NeedsUser, RunTransition::Interrupt) => {
                        Some(RunStatus::Interrupted)
                    }
                    (RunStatus::Running, RunTransition::Complete) => Some(RunStatus::Completed),
                    (
                        RunStatus::Preparing
                        | RunStatus::Ready
                        | RunStatus::Running
                        | RunStatus::NeedsUser
                        | RunStatus::Paused
                        | RunStatus::Interrupted,
                        RunTransition::Fail,
                    ) => Some(RunStatus::Failed),
                    (RunStatus::Completed, RunTransition::Apply) => Some(RunStatus::Applied),
                    (
                        RunStatus::Created
                        | RunStatus::Preparing
                        | RunStatus::Ready
                        | RunStatus::Running
                        | RunStatus::NeedsUser
                        | RunStatus::Paused
                        | RunStatus::Interrupted
                        | RunStatus::Completed
                        | RunStatus::Failed,
                        RunTransition::Discard,
                    ) => Some(RunStatus::Discarded),
                    _ => None,
                };

                if let Some(expected) = expected {
                    assert!(result.is_ok(), "expected {status:?} × {transition:?}");
                    assert_eq!(run.status(), expected);
                } else {
                    assert!(result.is_err(), "unexpected {status:?} × {transition:?}");
                    assert_eq!(run, before, "invalid run transition mutated state");
                }
            }
        }
    }
}
