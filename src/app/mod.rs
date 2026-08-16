//! Application use cases coordinating domain, persistence, Git, and providers.

mod error;
mod provider_factory;
mod query;
mod run_service;

pub use error::AppError;
pub use provider_factory::{
    DevelopmentFakeProviderFactory, ProviderFactory, RuntimeProvider, RuntimeProviderFactory,
};
pub use query::{
    AttentionSummary, CommittedEvent, RunDetails, RunListItem, StageSummary, UsageSummary,
};
pub use run_service::{ApplyOutcome, ExecutionReport, QuiescentState, RunService};
