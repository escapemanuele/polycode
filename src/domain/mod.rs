//! Explicit, provider-neutral state for recoverable orchestration.
//!
//! Domain types contain no persistence, process, network, Git, or UI behavior.

mod artifact;
mod attention;
mod effort;
mod event;
mod ids;
mod rehydration;
mod role;
mod run;
mod stage;
mod workflow;

pub use artifact::{ArtifactKind, ArtifactMetadata, ArtifactStatus};
pub use attention::{AttentionError, AttentionKind, AttentionRequest, AttentionStatus};
pub use effort::{EffortLevel, EffortParseError, EffortSetting};
pub use event::{DomainEvent, DomainEventKind, EventMetadata, NativeModelUsage};
pub use ids::{
    ArtifactId, AttentionRequestId, ConfigSnapshotId, EventId, IdError, ModelId, ProviderId,
    ProviderSessionId, RunId, StageId,
};
pub use rehydration::{
    RunRehydrationData, RunResumeStatus, StageRehydrationData, StageResumeStatus,
    StageSuspensionOwner,
};
pub use role::Role;
pub use run::{
    CompletionBlocker, CompletionBlockerReason, Run, RunAttentionError, RunFixError,
    RunInvariantError, RunProviderEventError, RunRehydrationError, RunStageError, RunStatus,
    RunTransition, RunTransitionError, StageDependencyReport,
};
pub use stage::{Stage, StageRehydrationError, StageStatus, StageTransition, StageTransitionError};
pub use workflow::{
    Dependency, DependencyKind, StageDefinition, StageKind, WorkflowDefinition,
    WorkflowDefinitionError, WorkflowKind, continue_cycle_stages, fix_cycle_stages,
    next_follow_up_stage_id,
};
