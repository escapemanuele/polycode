//! Application use cases coordinating domain, persistence, Git, and providers.

mod error;
mod provider_factory;
mod query;
mod routing;
mod run_service;

pub use error::AppError;
pub use provider_factory::{
    DevelopmentFakeProviderFactory, ProviderFactory, RoutedProvider, RuntimeProvider,
    RuntimeProviderFactory,
};
pub use query::{
    ArtifactSummary, ArtifactView, AttentionSummary, ChangedFileSummary, CommittedEvent,
    ProcessLogStream, ProcessLogView, RouteSummary, RunDetails, RunDiffPreview, RunListItem,
    StageSummary, UsageSummary,
};
pub use routing::{
    ExecutionSelection, ExecutionTarget, RECOMMENDED_PROFILE_VERSION, RecommendedAvailability,
    RoleRoute, RoutingError, RoutingPlan, UniformProvider,
};
pub use run_service::{ApplyOutcome, ExecutionReport, QuiescentState, RunService};
