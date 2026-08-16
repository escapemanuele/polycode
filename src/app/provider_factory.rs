use chrono::{DateTime, Utc};
use serde_json::json;

use crate::domain::{ConfigSnapshotId, ModelId, Role, RunId, WorkflowDefinition};
use crate::engine::{
    FakeProvider, FakeScenario, Provider, ProviderError, ProviderPoll, ProviderRequest,
};
use crate::providers::claude::{ClaudeInstallation, ClaudeProvider};
use crate::providers::codex::{CodexInstallation, CodexProvider};
use crate::store::SqliteStore;
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

pub enum RuntimeProvider {
    Fake(FakeProvider),
    Claude(ClaudeProvider),
    Codex(CodexProvider),
}

impl Provider for RuntimeProvider {
    fn id(&self) -> &crate::domain::ProviderId {
        match self {
            Self::Fake(provider) => provider.id(),
            Self::Claude(provider) => provider.id(),
            Self::Codex(provider) => provider.id(),
        }
    }

    fn supports_role(&self, role: Role) -> bool {
        match self {
            Self::Fake(provider) => provider.supports_role(role),
            Self::Claude(provider) => provider.supports_role(role),
            Self::Codex(provider) => provider.supports_role(role),
        }
    }

    fn keep_attached(&self) -> bool {
        match self {
            Self::Fake(provider) => provider.keep_attached(),
            Self::Claude(provider) => provider.keep_attached(),
            Self::Codex(provider) => provider.keep_attached(),
        }
    }

    fn stage_attention_response(
        &mut self,
        store: &mut SqliteStore,
        run_id: RunId,
        request_id: crate::domain::AttentionRequestId,
        response: Option<&str>,
    ) -> Result<(), ProviderError> {
        match self {
            Self::Fake(provider) => {
                provider.stage_attention_response(store, run_id, request_id, response)
            }
            Self::Claude(provider) => {
                provider.stage_attention_response(store, run_id, request_id, response)
            }
            Self::Codex(provider) => {
                provider.stage_attention_response(store, run_id, request_id, response)
            }
        }
    }

    fn poll(
        &mut self,
        store: &mut SqliteStore,
        request: &ProviderRequest,
    ) -> Result<ProviderPoll, ProviderError> {
        match self {
            Self::Fake(provider) => provider.poll(store, request),
            Self::Claude(provider) => provider.poll(store, request),
            Self::Codex(provider) => provider.poll(store, request),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RuntimeProviderFactory;

impl ProviderFactory for RuntimeProviderFactory {
    type Provider = RuntimeProvider;

    fn config_for_new_run(
        &self,
        provider: Option<&str>,
        id: ConfigSnapshotId,
        created_at: DateTime<Utc>,
    ) -> Result<ResolvedConfigSnapshot, AppError> {
        match provider {
            Some("fake") => {
                DevelopmentFakeProviderFactory.config_for_new_run(provider, id, created_at)
            }
            Some("claude") => {
                let installation = ClaudeInstallation::discover()?;
                if !installation.authenticated() {
                    return Err(
                        crate::providers::claude::ClaudeProviderError::NotAuthenticated.into(),
                    );
                }
                Ok(ResolvedConfigSnapshot::new(
                    id,
                    1,
                    json!({
                        "schema_version": 1,
                        "profile": "native_claude",
                        "provider": "claude",
                        "model": null,
                        "provider_options": {}
                    }),
                    created_at,
                )?)
            }
            Some("codex") => {
                let installation = CodexInstallation::discover()?;
                if !installation.authenticated() {
                    return Err(
                        crate::providers::codex::CodexProviderError::NotAuthenticated.into(),
                    );
                }
                Ok(ResolvedConfigSnapshot::new(
                    id,
                    1,
                    json!({
                        "schema_version": 1,
                        "profile": "native_codex",
                        "provider": "codex",
                        "model": null,
                        "provider_options": {
                            "execution_protocol": "exec_json_v1",
                            "sandbox_policy": "stage_kind_v1",
                            "approval_policy": "never"
                        }
                    }),
                    created_at,
                )?)
            }
            None => Err(AppError::NoProductionProvider),
            Some(other) => Err(AppError::UnsupportedProvider(other.to_owned())),
        }
    }

    fn for_run(
        &self,
        run_id: RunId,
        config: &ResolvedConfigSnapshot,
        workflow: &WorkflowDefinition,
        events: &[SequencedEvent],
    ) -> Result<Self::Provider, AppError> {
        let provider = config
            .payload()
            .get("provider")
            .and_then(serde_json::Value::as_str)
            .ok_or(AppError::LegacyExecutionConfig(run_id))?;
        match provider {
            "fake" => Ok(RuntimeProvider::Fake(
                DevelopmentFakeProviderFactory.for_run(run_id, config, workflow, events)?,
            )),
            "claude" if valid_claude_config(config) => {
                let model = config
                    .payload()
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .map(ModelId::new)
                    .transpose()?;
                Ok(RuntimeProvider::Claude(ClaudeProvider::from_environment(
                    model,
                )?))
            }
            "codex" if valid_codex_config(config) => {
                let model = config
                    .payload()
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .map(ModelId::new)
                    .transpose()?;
                Ok(RuntimeProvider::Codex(CodexProvider::from_environment(
                    model,
                )?))
            }
            "claude" | "codex" => Err(AppError::LegacyExecutionConfig(run_id)),
            other => Err(AppError::UnsupportedProvider(other.to_owned())),
        }
    }
}

fn valid_claude_config(config: &ResolvedConfigSnapshot) -> bool {
    config.schema_version() == 1
        && config
            .payload()
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            == Some(1)
        && config
            .payload()
            .get("profile")
            .and_then(serde_json::Value::as_str)
            == Some("native_claude")
        && config
            .payload()
            .get("provider")
            .and_then(serde_json::Value::as_str)
            == Some("claude")
}

fn valid_codex_config(config: &ResolvedConfigSnapshot) -> bool {
    let payload = config.payload();
    let options = payload.get("provider_options");
    config.schema_version() == 1
        && payload
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            == Some(1)
        && payload.get("profile").and_then(serde_json::Value::as_str) == Some("native_codex")
        && payload.get("provider").and_then(serde_json::Value::as_str) == Some("codex")
        && payload
            .get("model")
            .is_some_and(|model| model.is_null() || model.is_string())
        && options
            .and_then(|value| value.get("execution_protocol"))
            .and_then(serde_json::Value::as_str)
            == Some("exec_json_v1")
        && options
            .and_then(|value| value.get("sandbox_policy"))
            .and_then(serde_json::Value::as_str)
            == Some("stage_kind_v1")
        && options
            .and_then(|value| value.get("approval_policy"))
            .and_then(serde_json::Value::as_str)
            == Some("never")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_config_validation_requires_complete_supported_policy() {
        let now: DateTime<Utc> = std::time::SystemTime::now().into();
        let id = ConfigSnapshotId::new("codex-config-test").unwrap();
        let valid = ResolvedConfigSnapshot::new(
            id.clone(),
            1,
            json!({
                "schema_version":1,
                "profile":"native_codex",
                "provider":"codex",
                "model":null,
                "provider_options":{
                    "execution_protocol":"exec_json_v1",
                    "sandbox_policy":"stage_kind_v1",
                    "approval_policy":"never"
                }
            }),
            now,
        )
        .unwrap();
        assert!(valid_codex_config(&valid));

        let unsupported = ResolvedConfigSnapshot::new(
            id,
            1,
            json!({
                "schema_version":1,
                "profile":"native_codex",
                "provider":"codex",
                "model":42,
                "provider_options":{
                    "execution_protocol":"exec_json_v1",
                    "sandbox_policy":"stage_kind_v1",
                    "approval_policy":"never"
                }
            }),
            now,
        )
        .unwrap();
        assert!(!valid_codex_config(&unsupported));
    }
}
