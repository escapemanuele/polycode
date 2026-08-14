use chrono::{DateTime, Utc};
use serde_json::json;

use crate::domain::{ConfigSnapshotId, RunId, WorkflowDefinition};
use crate::engine::{FakeProvider, FakeScenario, Provider};
use crate::store::{ResolvedConfigSnapshot, SequencedEvent};

use super::AppError;

pub trait ProviderFactory {
    type Provider: Provider;

    /// Resolves immutable execution configuration for a new run.
    ///
    /// # Errors
    /// Rejects missing or unsupported provider selection.
    fn config_for_new_run(
        &self,
        provider: Option<&str>,
        id: ConfigSnapshotId,
        created_at: DateTime<Utc>,
    ) -> Result<ResolvedConfigSnapshot, AppError>;

    /// Reconstructs provider behavior from durable configuration and events.
    ///
    /// # Errors
    /// Rejects legacy/unsupported configuration or invalid provider scripts.
    fn for_run(
        &self,
        run_id: RunId,
        config: &ResolvedConfigSnapshot,
        workflow: &WorkflowDefinition,
        events: &[SequencedEvent],
    ) -> Result<Self::Provider, AppError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DevelopmentFakeProviderFactory;

impl ProviderFactory for DevelopmentFakeProviderFactory {
    type Provider = FakeProvider;

    fn config_for_new_run(
        &self,
        provider: Option<&str>,
        id: ConfigSnapshotId,
        created_at: DateTime<Utc>,
    ) -> Result<ResolvedConfigSnapshot, AppError> {
        match provider {
            None => return Err(AppError::NoProductionProvider),
            Some("fake") => {}
            Some(other) => return Err(AppError::UnsupportedProvider(other.to_owned())),
        }
        Ok(ResolvedConfigSnapshot::new(
            id,
            1,
            json!({
                "schema_version": 1,
                "profile": "development_fake",
                "provider": "fake",
                "scenario": "default_success_v1"
            }),
            created_at,
        )?)
    }

    fn for_run(
        &self,
        run_id: RunId,
        config: &ResolvedConfigSnapshot,
        workflow: &WorkflowDefinition,
        _events: &[SequencedEvent],
    ) -> Result<Self::Provider, AppError> {
        let payload = config.payload();
        let supported = config.schema_version() == 1
            && payload
                .get("schema_version")
                .and_then(serde_json::Value::as_u64)
                == Some(1)
            && payload.get("profile").and_then(serde_json::Value::as_str)
                == Some("development_fake")
            && payload.get("provider").and_then(serde_json::Value::as_str) == Some("fake")
            && payload.get("scenario").and_then(serde_json::Value::as_str)
                == Some("default_success_v1");
        if !supported {
            return Err(AppError::LegacyExecutionConfig(run_id));
        }
        Ok(FakeProvider::new(FakeScenario::successful(workflow))?)
    }
}
