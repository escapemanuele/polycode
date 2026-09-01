use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::domain::{ConfigSnapshotId, EffortSetting, ProviderId, Role, RunId, WorkflowDefinition};
use crate::engine::{
    FakeProvider, FakeScenario, Provider, ProviderAttentionContext, ProviderError, ProviderPoll,
    ProviderRequest,
};
use crate::providers::claude::{ClaudeInstallation, ClaudeProvider, ClaudeProviderError};
use crate::providers::codex::{CodexInstallation, CodexProvider, CodexProviderError};
use crate::providers::verify::VerifyProvider;
use crate::store::{ResolvedConfigSnapshot, SequencedEvent, SqliteStore};

use super::AppError;
use super::routing::{
    ExecutionSelection, ExecutionTarget, RecommendedAvailability, ResourcePlan, RoutingPlan,
    UniformProvider, VERIFY_PROVIDER_ID, resolve_config,
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
        effort: EffortSetting,
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

pub trait ProviderResolver {
    type Provider: Provider;

    /// Reconstructs provider behavior from one validated immutable configuration.
    ///
    /// # Errors
    /// Rejects invalid configuration or unavailable routed provider infrastructure.
    fn resolve_for_run(
        &self,
        run_id: RunId,
        config: &ResolvedConfigSnapshot,
        workflow: &WorkflowDefinition,
        events: &[SequencedEvent],
    ) -> Result<Self::Provider, AppError>;
}

impl<T: ProviderFactory> ProviderResolver for T {
    type Provider = T::Provider;

    fn resolve_for_run(
        &self,
        run_id: RunId,
        config: &ResolvedConfigSnapshot,
        workflow: &WorkflowDefinition,
        events: &[SequencedEvent],
    ) -> Result<Self::Provider, AppError> {
        ProviderFactory::for_run(self, run_id, config, workflow, events)
    }
}

/// Fakes every agent role and runs verification for real, writing its
/// artifacts under an explicit root rather than the developer's data
/// directory — every in-process test would otherwise leave a `verify.md`
/// in `~/.polycode/runs`.
#[derive(Clone, Debug)]
pub struct DevelopmentFakeProviderFactory {
    artifact_root: PathBuf,
}

impl DevelopmentFakeProviderFactory {
    #[must_use]
    pub fn new(artifact_root: PathBuf) -> Self {
        Self { artifact_root }
    }
}

impl ProviderFactory for DevelopmentFakeProviderFactory {
    /// Routed rather than a bare `FakeProvider`: every agent role is faked,
    /// but the verifier is never an agent, so the run's verify stages go
    /// to the real `verify` provider here exactly as they do in production.
    type Provider = RoutedProvider;

    fn config_for_new_run(
        &self,
        selection: ExecutionSelection,
        effort: EffortSetting,
        workflow: &WorkflowDefinition,
        id: ConfigSnapshotId,
        created_at: DateTime<Utc>,
    ) -> Result<ResolvedConfigSnapshot, AppError> {
        if selection != ExecutionSelection::Uniform(UniformProvider::Fake) {
            return Err(AppError::UnsupportedProvider(format!("{selection:?}")));
        }
        Ok(resolve_config(
            selection,
            effort,
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
        let resource_plan = ResourcePlan::from_snapshot(config, workflow)?;
        Ok(RoutedProvider::new(plan, resource_plan, workflow.clone())
            .with_artifact_root(self.artifact_root.clone()))
    }
}

pub enum RuntimeProvider {
    Fake(FakeProvider),
    Claude(ClaudeProvider),
    Codex(CodexProvider),
    Verify(VerifyProvider),
}

impl Provider for RuntimeProvider {
    fn provider_id_for(&self, request: &ProviderRequest) -> Result<ProviderId, ProviderError> {
        match self {
            Self::Fake(provider) => provider.provider_id_for(request),
            Self::Claude(provider) => provider.provider_id_for(request),
            Self::Codex(provider) => provider.provider_id_for(request),
            Self::Verify(provider) => provider.provider_id_for(request),
        }
    }

    fn supports_role(&self, role: crate::domain::Role) -> bool {
        match self {
            Self::Fake(provider) => provider.supports_role(role),
            Self::Claude(provider) => provider.supports_role(role),
            Self::Codex(provider) => provider.supports_role(role),
            Self::Verify(provider) => provider.supports_role(role),
        }
    }

    fn keep_attached_for(&self, request: &ProviderRequest) -> Result<bool, ProviderError> {
        match self {
            Self::Fake(provider) => provider.keep_attached_for(request),
            Self::Claude(provider) => provider.keep_attached_for(request),
            Self::Codex(provider) => provider.keep_attached_for(request),
            Self::Verify(provider) => provider.keep_attached_for(request),
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
            Self::Verify(provider) => provider.stage_attention_response(store, context, response),
        }
    }

    fn can_auto_resolve_attention(
        &mut self,
        store: &mut SqliteStore,
        context: &ProviderAttentionContext,
    ) -> Result<bool, ProviderError> {
        match self {
            Self::Fake(provider) => provider.can_auto_resolve_attention(store, context),
            Self::Claude(provider) => provider.can_auto_resolve_attention(store, context),
            Self::Codex(provider) => provider.can_auto_resolve_attention(store, context),
            Self::Verify(provider) => provider.can_auto_resolve_attention(store, context),
        }
    }

    fn stage_continue_instruction(
        &mut self,
        store: &mut SqliteStore,
        run_id: crate::domain::RunId,
        stage_id: &crate::domain::StageId,
        role: crate::domain::Role,
        instruction: &str,
    ) -> Result<(), ProviderError> {
        match self {
            Self::Fake(provider) => {
                provider.stage_continue_instruction(store, run_id, stage_id, role, instruction)
            }
            Self::Claude(provider) => {
                provider.stage_continue_instruction(store, run_id, stage_id, role, instruction)
            }
            Self::Codex(provider) => {
                provider.stage_continue_instruction(store, run_id, stage_id, role, instruction)
            }
            Self::Verify(provider) => {
                provider.stage_continue_instruction(store, run_id, stage_id, role, instruction)
            }
        }
    }

    fn discard_continue_instruction(
        &mut self,
        store: &mut SqliteStore,
        run_id: crate::domain::RunId,
        stage_id: &crate::domain::StageId,
    ) -> Result<(), ProviderError> {
        match self {
            Self::Fake(provider) => provider.discard_continue_instruction(store, run_id, stage_id),
            Self::Claude(provider) => {
                provider.discard_continue_instruction(store, run_id, stage_id)
            }
            Self::Codex(provider) => provider.discard_continue_instruction(store, run_id, stage_id),
            Self::Verify(provider) => {
                provider.discard_continue_instruction(store, run_id, stage_id)
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
            Self::Verify(provider) => provider.poll(store, request),
        }
    }
}

/// Request-aware provider composition with lazy native adapter construction.
pub struct RoutedProvider {
    plan: RoutingPlan,
    resource_plan: ResourcePlan,
    workflow: WorkflowDefinition,
    runtimes: HashMap<(ExecutionTarget, EffortSetting), RuntimeProvider>,
    isolated_runtime: Option<(PathBuf, PathBuf)>,
    /// Where the verify provider writes its artifacts when not the
    /// configured data directory: the eval runtime root, or a test fixture.
    artifact_root: Option<PathBuf>,
    eval_auto_approve: bool,
}

impl RoutedProvider {
    #[must_use]
    pub fn new(
        plan: RoutingPlan,
        resource_plan: ResourcePlan,
        workflow: WorkflowDefinition,
    ) -> Self {
        Self {
            plan,
            resource_plan,
            workflow,
            runtimes: HashMap::new(),
            isolated_runtime: None,
            artifact_root: None,
            eval_auto_approve: false,
        }
    }

    /// The same provider with verification artifacts redirected under
    /// `root`, the tree the native adapters would use under the data
    /// directory.
    #[must_use]
    pub fn with_artifact_root(mut self, root: PathBuf) -> Self {
        self.artifact_root = Some(root);
        self
    }

    /// A provider whose *runtime* is redirected: process data root and runner
    /// executable both point somewhere the evaluation harness owns, so an
    /// eval never touches the user's real process data or their installed
    /// CLI. It also auto-approves eval permission prompts, which no caller
    /// should be surprised by after reading the name.
    ///
    /// What it does not isolate, and what the name must not be read as
    /// claiming: the git checkout, the store, or anything else a run touches.
    /// Those are shared with whatever else is running.
    #[must_use]
    pub(crate) fn isolated(
        plan: RoutingPlan,
        resource_plan: ResourcePlan,
        workflow: WorkflowDefinition,
        process_root: PathBuf,
        runner_executable: PathBuf,
    ) -> Self {
        Self {
            plan,
            resource_plan,
            workflow,
            runtimes: HashMap::new(),
            artifact_root: Some(process_root.clone()),
            isolated_runtime: Some((process_root, runner_executable)),
            eval_auto_approve: true,
        }
    }

    #[must_use]
    pub const fn plan(&self) -> &RoutingPlan {
        &self.plan
    }

    fn target_for_role(&self, role: Role) -> Result<ExecutionTarget, ProviderError> {
        self.plan
            .route(role)
            .map(|route| route.target().clone())
            .ok_or_else(|| ProviderError::new(format!("configured route missing for {role:?}")))
    }

    /// Requested effort resolved once from the immutable resource plan.
    fn effort_for_role(&self, role: Role) -> Result<EffortSetting, ProviderError> {
        self.resource_plan
            .effort(role)
            .ok_or_else(|| ProviderError::new(format!("configured effort missing for {role:?}")))
    }

    fn runtime_for(
        &mut self,
        target: &ExecutionTarget,
        effort: EffortSetting,
    ) -> Result<&mut RuntimeProvider, ProviderError> {
        let key = (target.clone(), effort);
        if !self.runtimes.contains_key(&key) {
            let runtime = match target.provider_id().as_str() {
                "fake" => RuntimeProvider::Fake(
                    FakeProvider::new(FakeScenario::successful(&self.workflow))
                        .map_err(|error| ProviderError::new(error.to_string()))?,
                ),
                "claude" => RuntimeProvider::Claude(
                    self.claude_provider(target.model_id().cloned())
                        .map(|provider| provider.with_effort(effort))
                        .map_err(|error| {
                            ProviderError::new(format!(
                                "configured provider unavailable for claude target: {error}"
                            ))
                        })?,
                ),
                "codex" => RuntimeProvider::Codex(
                    self.codex_provider(target.model_id().cloned())
                        .map(|provider| provider.with_effort(effort))
                        .map_err(|error| {
                            ProviderError::new(format!(
                                "configured provider unavailable for codex target: {error}"
                            ))
                        })?,
                ),
                VERIFY_PROVIDER_ID => RuntimeProvider::Verify(self.verify_provider()?),
                other => {
                    return Err(ProviderError::new(format!(
                        "unsupported configured provider {other:?}"
                    )));
                }
            };
            self.runtimes.insert(key.clone(), runtime);
        }
        self.runtimes
            .get_mut(&key)
            .ok_or_else(|| ProviderError::new("lazy provider cache insertion failed"))
    }

    fn claude_provider(
        &self,
        model: Option<crate::domain::ModelId>,
    ) -> Result<ClaudeProvider, ClaudeProviderError> {
        match &self.isolated_runtime {
            Some((root, runner)) if self.eval_auto_approve => {
                ClaudeProvider::from_runtime_with_eval_policy(
                    model,
                    root.clone(),
                    runner.clone(),
                    true,
                )
            }
            Some((root, runner)) => {
                ClaudeProvider::from_runtime(model, root.clone(), runner.clone())
            }
            None => ClaudeProvider::from_environment(model),
        }
    }

    /// The verifier writes artifacts where the native adapters do: under
    /// the explicit root when one was given (eval runtime, test fixture),
    /// else the configured data directory.
    fn verify_provider(&self) -> Result<VerifyProvider, ProviderError> {
        match &self.artifact_root {
            Some(root) => Ok(VerifyProvider::new(root.clone())),
            None => VerifyProvider::from_environment().map_err(|error| {
                ProviderError::new(format!(
                    "configured provider unavailable for verify target: {error}"
                ))
            }),
        }
    }

    fn codex_provider(
        &self,
        model: Option<crate::domain::ModelId>,
    ) -> Result<CodexProvider, CodexProviderError> {
        match &self.isolated_runtime {
            Some((root, runner)) => {
                CodexProvider::from_runtime(model, root.clone(), runner.clone())
            }
            None => CodexProvider::from_environment(model),
        }
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
        let effort = self.effort_for_role(request.role())?;
        self.runtimes
            .get(&(target, effort))
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
        let effort = self.effort_for_role(context.role())?;
        self.runtime_for(&target, effort)?
            .stage_attention_response(store, context, response)
    }

    fn can_auto_resolve_attention(
        &mut self,
        store: &mut SqliteStore,
        context: &ProviderAttentionContext,
    ) -> Result<bool, ProviderError> {
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
            return Ok(false);
        }
        let effort = self.effort_for_role(context.role())?;
        self.runtime_for(&target, effort)?
            .can_auto_resolve_attention(store, context)
    }

    /// No provider session exists yet for the stage this instruction is
    /// staged for, so — unlike attention, which resolves its route from an
    /// existing session — this resolves the route from `role` directly. Every
    /// continue cycle's follow-up stage routes through `Role::Implementer`,
    /// so the caller supplies exactly the role that stage will use.
    fn stage_continue_instruction(
        &mut self,
        store: &mut SqliteStore,
        run_id: crate::domain::RunId,
        stage_id: &crate::domain::StageId,
        role: crate::domain::Role,
        instruction: &str,
    ) -> Result<(), ProviderError> {
        let target = self.target_for_role(role)?;
        let effort = self.effort_for_role(role)?;
        self.runtime_for(&target, effort)?
            .stage_continue_instruction(store, run_id, stage_id, role, instruction)
    }

    /// Same route resolution as [`Self::stage_continue_instruction`]: the
    /// stage still has no provider session, so the leaf runtime is resolved
    /// from `Role::Implementer` directly, not from session lookup.
    fn discard_continue_instruction(
        &mut self,
        store: &mut SqliteStore,
        run_id: crate::domain::RunId,
        stage_id: &crate::domain::StageId,
    ) -> Result<(), ProviderError> {
        let target = self.target_for_role(Role::Implementer)?;
        let effort = self.effort_for_role(Role::Implementer)?;
        self.runtime_for(&target, effort)?
            .discard_continue_instruction(store, run_id, stage_id)
    }

    fn poll(
        &mut self,
        store: &mut SqliteStore,
        request: &ProviderRequest,
    ) -> Result<ProviderPoll, ProviderError> {
        let target = self.target_for_role(request.role())?;
        let effort = self.effort_for_role(request.role())?;
        let runtime = self.runtime_for(&target, effort)?;
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
        effort: EffortSetting,
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
            effort,
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
        let resource_plan = ResourcePlan::from_snapshot(config, workflow)?;
        Ok(RoutedProvider::new(plan, resource_plan, workflow.clone()))
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
    use super::*;
    use crate::domain::{StageId, StageKind, StageStatus, WorkflowKind};

    fn native_default_resource_plan(workflow: &WorkflowDefinition) -> ResourcePlan {
        let snapshot = resolve_config(
            ExecutionSelection::Uniform(UniformProvider::Fake),
            EffortSetting::NativeDefault,
            workflow,
            RecommendedAvailability::default(),
            ConfigSnapshotId::new("effort-test").unwrap(),
            std::time::SystemTime::now().into(),
        )
        .unwrap();
        ResourcePlan::from_snapshot(&snapshot, workflow).unwrap()
    }

    #[test]
    fn reconstructing_routed_provider_does_not_instantiate_native_adapters() {
        let workflow = WorkflowDefinition::built_in(WorkflowKind::Standard);
        let snapshot = resolve_config(
            ExecutionSelection::Recommended,
            EffortSetting::NativeDefault,
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
        let mut provider = RoutedProvider::new(
            RoutingPlan::test_plan(routes),
            native_default_resource_plan(&workflow),
            workflow.clone(),
        );
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
