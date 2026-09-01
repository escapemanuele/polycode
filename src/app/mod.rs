//! Application use cases coordinating domain, persistence, Git, and providers.

mod error;
mod provider_factory;
pub(crate) mod query;
mod routing;
mod run_service;

pub use error::AppError;
pub use provider_factory::{
    DevelopmentFakeProviderFactory, ProviderFactory, ProviderResolver, RoutedProvider,
    RuntimeProvider, RuntimeProviderFactory,
};
pub use query::{
    ArtifactSummary, ArtifactView, AttentionSummary, BlockedDependencyRef, ChangedFileSummary,
    CommittedEvent, ProcessLogStream, ProcessLogView, ProviderUsage, RouteSummary, RunDetails,
    RunDiffPreview, RunListItem, RunUsage, StageDependencyRef, StageExecutionEvidence,
    StageSummary, StageWaitingSummary, UsageSummary,
};
pub(crate) use routing::resolve_eval_config;
pub use routing::{
    DecisionBasis, DecisionConfidence, ExecutionSelection, ExecutionTarget,
    RECOMMENDED_PROFILE_VERSION, RECOMMENDED_PROFILE_VERSION_V1,
    RECOMMENDED_V2_EVIDENCE_FINGERPRINT, RECOMMENDED_V2_EVIDENCE_SUITE, RecommendedAvailability,
    RecommendedDecision, RecommendedProvenance, ResourcePlan, RoleRoute, RoutingError, RoutingPlan,
    UniformProvider, recommended_provenance, unroutable_fix_role,
};
pub use run_service::{ApplyOutcome, ExecutionReport, QuiescentState, RunService};
