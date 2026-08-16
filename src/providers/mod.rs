//! Native coding-agent provider adapters and provider-owned persisted state.

mod artifact;
mod checkpoint;
pub mod claude;
mod session;

pub use artifact::{ArtifactRecord, ArtifactRecordError};
pub use checkpoint::{ProviderCommit, ProviderSessionMutation};
pub use session::{
    PendingProviderAttention, ProviderSessionRecord, ProviderSessionRecordId,
    ProviderSessionRevision, ProviderSessionStatus,
};
