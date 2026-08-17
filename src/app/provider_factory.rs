use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::domain::{ConfigSnapshotId, ProviderId, RunId, WorkflowDefinition};
use crate::engine::{
    FakeProvider, FakeScenario, Provider, ProviderAttentionContext, ProviderError, ProviderPoll,
    ProviderRequest,
};
use crate::providers::claude::{ClaudeInstallation, ClaudeProvider, ClaudeProviderError};
use crate::providers::codex::{CodexInstallation, CodexProvider, CodexProviderError};
use crate::store::{ResolvedConfigSnapshot, SequencedEvent, SqliteStore};

use super::AppError;
use super::routing::{
    ExecutionSelection, ExecutionTarget, RecommendedAvailability, RoutingPlan, UniformProvider,
    resolve_config,
};

pub trait ProviderFactory {
    type Provider: Provider;

    /// Resolves immutable execution configuration for a new run.
    ///
    /// # Errors
    /// Rejects unavailable providers, failed probes, or invalid routing.
    fn config_for_new_run(
        &self,
        selection: ExecutionSelection,
        workflow: &WorkflowDefinition,
        id: ConfigSnapshotId,
        created_at: DateTime<Utc>,
    ) -> Result<ResolvedConfigSnapshot, AppError>;

    /// Reconstructs provider behavior from durable configuration and events.
    ///
    /// # Errors
    /// Rejects unsupported or structurally invalid configuration.
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
        selection: ExecutionSelection,
        workflow: &WorkflowDefinition,
        id: ConfigSnapshotId,
        created_at: DateTime<Utc>,
    ) -> Result<ResolvedConfigSnapshot, AppError> {
        if selection != ExecutionSelection::Uniform(UniformProvider::Fake) {
            return Err(AppError::UnsupportedProvider(format!("{selection:?}")));
        }
        Ok(resolve_config(
            selection,
            workflow,
            RecommendedAvailability::default(),
            id,
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
        let plan = RoutingPlan::from_snapshot(config, workflow).map_err(|error| {
            if config.schema_version() == 1 {
                AppError::LegacyExecutionConfig(run_id)
            } else {
                error.into()
            }
        })?;
        if plan
            .routes()
            .any(|(_, route)| route.target().provider_id().as_str() != "fake")
        {
            return Err(AppError::UnsupportedProvider(
                "development factory only supports fake routes".to_owned(),
            ));
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
    fn provider_id_for(&self, request: &ProviderRequest) -> Result<ProviderId, ProviderError> {
        match self {
            Self::Fake(provider) => provider.provider_id_for(request),
            Self::Claude(provider) => provider.provider_id_for(request),
            Self::Codex(provider) => provider.provider_id_for(request),
        }
    }

    fn supports_role(&self, role: crate::domain::Role) -> bool {
        match self {
            Self::Fake(provider) => provider.supports_role(role),
            Self::Claude(provider) => provider.supports_role(role),
            Self::Codex(provider) => provider.supports_role(role),
        }
    }

    fn keep_attached_for(&self, request: &ProviderRequest) -> Result<bool, ProviderError> {
        match self {
            Self::Fake(provider) => provider.keep_attached_for(request),
            Self::Claude(provider) => provider.keep_attached_for(request),
            Self::Codex(provider) => provider.keep_attached_for(request),
        }
    }

    fn stage_attention_response(
        &mut self,
        store: &mut SqliteStore,
        context: &ProviderAttentionContext,
        response: Option<&str>,
    ) -> Result<(), ProviderError> {
        match self {
            Self::Fake(provider) => provider.stage_attention_response(store, context, response),
            Self::Claude(provider) => provider.stage_attention_response(store, context, response),
            Self::Codex(provider) => provider.stage_attention_response(store, context, response),
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

/// Request-aware provider composition with lazy native adapter construction.
pub struct RoutedProvider {
    plan: RoutingPlan,
    workflow: WorkflowDefinition,
    runtimes: HashMap<ExecutionTarget, RuntimeProvider>,
}

impl RoutedProvider {
    #[must_use]
    pub fn new(plan: RoutingPlan, workflow: WorkflowDefinition) -> Self {
        Self {
            plan,
            workflow,
            runtimes: HashMap::new(),
        }
    }

    #[must_use]
    pub const fn plan(&self) -> &RoutingPlan {
        &self.plan
    }

    fn target_for_role(&self, role: crate::domain::Role) -> Result<ExecutionTarget, ProviderError> {
        self.plan
            .route(role)
            .map(|route| route.target().clone())
            .ok_or_else(|| ProviderError::new(format!("configured route missing for {role:?}")))
    }

    fn runtime_for(
        &mut self,
        target: &ExecutionTarget,
    ) -> Result<&mut RuntimeProvider, ProviderError> {
        if !self.runtimes.contains_key(target) {
            let runtime = match target.provider_id().as_str() {
                "fake" => RuntimeProvider::Fake(
                    FakeProvider::new(FakeScenario::successful(&self.workflow))
                        .map_err(|error| ProviderError::new(error.to_string()))?,
                ),
                "claude" => RuntimeProvider::Claude(
                    ClaudeProvider::from_environment(target.model_id().cloned()).map_err(
                        |error| {
                            ProviderError::new(format!(
                                "configured provider unavailable for claude target: {error}"
                            ))
                        },
                    )?,
                ),
                "codex" => RuntimeProvider::Codex(
                    CodexProvider::from_environment(target.model_id().cloned()).map_err(
                        |error| {
                            ProviderError::new(format!(
                                "configured provider unavailable for codex target: {error}"
                            ))
                        },
                    )?,
                ),
                other => {
                    return Err(ProviderError::new(format!(
                        "unsupported configured provider {other:?}"
                    )));
                }
            };
            self.runtimes.insert(target.clone(), runtime);
        }
        self.runtimes
            .get_mut(target)
            .ok_or_else(|| ProviderError::new("lazy provider cache insertion failed"))
    }
}

impl Provider for RoutedProvider {
    fn provider_id_for(&self, request: &ProviderRequest) -> Result<ProviderId, ProviderError> {
        Ok(self.target_for_role(request.role())?.provider_id().clone())
    }

    fn supports_role(&self, role: crate::domain::Role) -> bool {
        self.plan.route(role).is_some()
    }

    fn keep_attached_for(&self, request: &ProviderRequest) -> Result<bool, ProviderError> {
        let target = self.target_for_role(request.role())?;
        self.runtimes
            .get(&target)
            .ok_or_else(|| ProviderError::new("waiting provider was not instantiated"))?
            .keep_attached_for(request)
    }

    fn stage_attention_response(
        &mut self,
        store: &mut SqliteStore,
        context: &ProviderAttentionContext,
        response: Option<&str>,
    ) -> Result<(), ProviderError> {
        let target = self.target_for_role(context.role())?;
        let session = store
            .list_provider_sessions(context.run_id())
            .map_err(|error| ProviderError::new(error.to_string()))?
            .into_iter()
            .find(|session| {
                session.stage_id() == context.stage_id()
                    && session
                        .pending_attention()
                        .is_some_and(|pending| pending.attention_id() == context.request_id())
            })
            .ok_or_else(|| ProviderError::new("attention has no matching provider session"))?;
        if session.provider_id() != target.provider_id() {
            return Err(ProviderError::new(format!(
                "attention route provider mismatch: route={}, session={}",
                target.provider_id(),
                session.provider_id()
            )));
        }
        self.runtime_for(&target)?
            .stage_attention_response(store, context, response)
    }

    fn poll(
        &mut self,
        store: &mut SqliteStore,
        request: &ProviderRequest,
    ) -> Result<ProviderPoll, ProviderError> {
        let target = self.target_for_role(request.role())?;
        let runtime = self.runtime_for(&target)?;
        if runtime.provider_id_for(request)? != *target.provider_id() {
            return Err(ProviderError::new(
                "resolved leaf provider identity mismatch",
            ));
        }
        if !runtime.supports_request(request)? {
            return Err(ProviderError::new(format!(
                "provider {} does not support role {:?}",
                target.provider_id(),
                request.role()
            )));
        }
        runtime.poll(store, request)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RuntimeProviderFactory;

impl ProviderFactory for RuntimeProviderFactory {
    type Provider = RoutedProvider;

    fn config_for_new_run(
        &self,
        selection: ExecutionSelection,
        workflow: &WorkflowDefinition,
        id: ConfigSnapshotId,
        created_at: DateTime<Utc>,
    ) -> Result<ResolvedConfigSnapshot, AppError> {
        let availability = match selection {
            ExecutionSelection::Uniform(provider) => {
                require_explicit_provider(provider)?;
                RecommendedAvailability::default()
            }
            ExecutionSelection::Recommended => probe_recommended_availability()?,
        };
        Ok(resolve_config(
            selection,
            workflow,
            availability,
            id,
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
        let plan = RoutingPlan::from_snapshot(config, workflow).map_err(|error| {
            if config.schema_version() == 1 {
                AppError::LegacyExecutionConfig(run_id)
            } else {
                error.into()
            }
        })?;
        Ok(RoutedProvider::new(plan, workflow.clone()))
    }
}

fn require_explicit_provider(provider: UniformProvider) -> Result<(), AppError> {
    match provider {
        UniformProvider::Fake => Ok(()),
        UniformProvider::Claude => {
            let installation = ClaudeInstallation::discover()?;
            installation
                .authenticated()
                .then_some(())
                .ok_or(ClaudeProviderError::NotAuthenticated.into())
        }
        UniformProvider::Codex => {
            let installation = CodexInstallation::discover()?;
            installation
                .authenticated()
                .then_some(())
                .ok_or(CodexProviderError::NotAuthenticated.into())
        }
    }
}

fn probe_recommended_availability() -> Result<RecommendedAvailability, AppError> {
    let claude = match ClaudeInstallation::discover() {
        Ok(installation) => installation.authenticated(),
        Err(ClaudeProviderError::NotFound | ClaudeProviderError::NotAuthenticated) => false,
        Err(error) => return Err(error.into()),
    };
    let codex = match CodexInstallation::discover() {
        Ok(installation) => installation.authenticated(),
        Err(CodexProviderError::NotFound | CodexProviderError::NotAuthenticated) => false,
        Err(error) => return Err(error.into()),
    };
    Ok(RecommendedAvailability { claude, codex })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::domain::{Role, StageId, StageKind, StageStatus, WorkflowKind};

    #[test]
    fn reconstructing_routed_provider_does_not_instantiate_native_adapters() {
        let workflow = WorkflowDefinition::built_in(WorkflowKind::Standard);
        let snapshot = resolve_config(
            ExecutionSelection::Recommended,
            &workflow,
            RecommendedAvailability {
                claude: true,
                codex: true,
            },
            ConfigSnapshotId::new("lazy-test").unwrap(),
            std::time::SystemTime::now().into(),
        )
        .unwrap();
        let provider = RuntimeProviderFactory
            .for_run(RunId::from_u128(1), &snapshot, &workflow, &[])
            .unwrap();
        assert!(provider.runtimes.is_empty());
    }

    #[test]
    fn attachment_policy_comes_from_current_stage_target_not_any_configured_provider() {
        let workflow = WorkflowDefinition::built_in(WorkflowKind::Standard);
        let fake = ExecutionTarget::new(ProviderId::new("fake").unwrap(), None);
        let codex = ExecutionTarget::new(ProviderId::new("codex").unwrap(), None);
        let routes = workflow
            .stages()
            .iter()
            .map(|stage| {
                (
                    stage.role(),
                    if stage.role() == Role::Implementer {
                        codex.clone()
                    } else {
                        fake.clone()
                    },
                )
            })
            .collect();
        let mut provider = RoutedProvider::new(RoutingPlan::test_plan(routes), workflow.clone());
        assert!(
            provider
                .plan
                .routes()
                .any(|(_, route)| { route.target().provider_id().as_str() == "codex" })
        );
        let stage = workflow.stages().first().unwrap();
        let request = ProviderRequest::new(
            RunId::from_u128(1),
            StageId::new(stage.id().as_str()).unwrap(),
            StageKind::Architecture,
            StageStatus::Running,
            stage.role(),
            "task".to_owned(),
            PathBuf::from("/tmp/polycode-routing-test"),
            1,
            0,
            None,
            Vec::new(),
        );
        let mut store = SqliteStore::open_in_memory().unwrap();
        let _ = provider.poll(&mut store, &request).unwrap();
        assert!(!provider.keep_attached_for(&request).unwrap());
    }
}
