use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::domain::{ConfigSnapshotId, ModelId, ProviderId, Role, WorkflowDefinition};
use crate::store::ResolvedConfigSnapshot;

pub const RECOMMENDED_PROFILE_VERSION: &str = "recommended_v1";
const UNIFORM_PROFILE_VERSION: &str = "uniform_v1";
pub(crate) const EVAL_PROFILE_VERSION: &str = "eval_v1";

/// One immutable provider/model destination selected for a role.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExecutionTarget {
    provider_id: ProviderId,
    model_id: Option<ModelId>,
}

impl ExecutionTarget {
    #[must_use]
    pub const fn new(provider_id: ProviderId, model_id: Option<ModelId>) -> Self {
        Self {
            provider_id,
            model_id,
        }
    }

    #[must_use]
    pub const fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    #[must_use]
    pub const fn model_id(&self) -> Option<&ModelId> {
        self.model_id.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleRoute {
    target: ExecutionTarget,
    reason: String,
}

impl RoleRoute {
    #[must_use]
    pub const fn target(&self) -> &ExecutionTarget {
        &self.target
    }

    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Validated immutable role routing reconstructed from one configuration snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutingPlan {
    profile: String,
    profile_version: String,
    role_routes: HashMap<Role, RoleRoute>,
    provider_configs: HashMap<String, ProviderConfig>,
}

impl RoutingPlan {
    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    #[must_use]
    pub fn profile_version(&self) -> &str {
        &self.profile_version
    }

    #[must_use]
    pub fn route(&self, role: Role) -> Option<&RoleRoute> {
        self.role_routes.get(&role)
    }

    pub fn routes(&self) -> impl Iterator<Item = (Role, &RoleRoute)> {
        self.role_routes.iter().map(|(role, route)| (*role, route))
    }

    #[must_use]
    pub fn provider_config(&self, provider_id: &ProviderId) -> Option<&ProviderConfig> {
        self.provider_configs.get(provider_id.as_str())
    }

    #[cfg(test)]
    pub(crate) fn test_plan(routes: HashMap<Role, ExecutionTarget>) -> Self {
        let provider_configs = routes
            .values()
            .map(|target| target.provider_id().as_str())
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|provider| {
                let dto = provider_config_dto(provider).unwrap();
                (
                    provider.to_owned(),
                    ProviderConfig {
                        profile: dto.profile,
                        schema_version: dto.schema_version,
                        provider_options: dto.provider_options,
                        scenario: dto.scenario,
                    },
                )
            })
            .collect();
        Self {
            profile: "test".to_owned(),
            profile_version: "test_v1".to_owned(),
            role_routes: routes
                .into_iter()
                .map(|(role, target)| {
                    (
                        role,
                        RoleRoute {
                            target,
                            reason: "test_route".to_owned(),
                        },
                    )
                })
                .collect(),
            provider_configs,
        }
    }

    /// Decodes config v2 or normalizes a legacy uniform v1 config in memory.
    ///
    /// # Errors
    /// Rejects malformed, unsafe, unsupported, or incomplete routing configuration.
    pub fn from_snapshot(
        snapshot: &ResolvedConfigSnapshot,
        workflow: &WorkflowDefinition,
    ) -> Result<Self, RoutingError> {
        match snapshot.schema_version() {
            1 => decode_legacy(snapshot, workflow),
            2 => decode_v2(snapshot, workflow),
            version => Err(RoutingError::UnsupportedSchema(version)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderConfig {
    profile: String,
    schema_version: u32,
    provider_options: Value,
    scenario: Option<String>,
}

impl ProviderConfig {
    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    #[must_use]
    pub const fn provider_options(&self) -> &Value {
        &self.provider_options
    }

    #[must_use]
    pub fn scenario(&self) -> Option<&str> {
        self.scenario.as_deref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UniformProvider {
    Claude,
    Codex,
    Fake,
}

impl UniformProvider {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Fake => "fake",
        }
    }
}

impl TryFrom<&str> for UniformProvider {
    type Error = RoutingError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "fake" => Ok(Self::Fake),
            other => Err(RoutingError::UnsupportedProvider(other.to_owned())),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionSelection {
    Uniform(UniformProvider),
    Recommended,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecommendedAvailability {
    pub claude: bool,
    pub codex: bool,
}

/// Resolves a selection to explicit routes and encodes immutable config schema v2.
///
/// # Errors
/// Rejects unavailable Recommended or invalid identifiers/configuration.
pub fn resolve_config(
    selection: ExecutionSelection,
    workflow: &WorkflowDefinition,
    availability: RecommendedAvailability,
    id: ConfigSnapshotId,
    created_at: DateTime<Utc>,
) -> Result<ResolvedConfigSnapshot, RoutingError> {
    let roles = required_roles(workflow);
    let (profile, profile_version, routes) = match selection {
        ExecutionSelection::Uniform(provider) => {
            let routes = roles
                .into_iter()
                .map(|role| {
                    (
                        role,
                        RouteDto {
                            provider: provider.as_str().to_owned(),
                            model: None,
                            reason: "explicit_provider".to_owned(),
                        },
                    )
                })
                .collect();
            ("uniform", UNIFORM_PROFILE_VERSION, routes)
        }
        ExecutionSelection::Recommended => {
            let routes = recommended_routes(roles, availability)?;
            ("recommended", RECOMMENDED_PROFILE_VERSION, routes)
        }
    };
    let used = routes
        .values()
        .map(|route| route.provider.as_str())
        .collect::<HashSet<_>>();
    let providers = used
        .into_iter()
        .map(|provider| Ok((provider.to_owned(), provider_config_dto(provider)?)))
        .collect::<Result<HashMap<_, _>, RoutingError>>()?;
    let payload = RoutingPayloadV2 {
        schema_version: 2,
        profile: profile.to_owned(),
        profile_version: profile_version.to_owned(),
        routes,
        providers,
    };
    let snapshot = ResolvedConfigSnapshot::new(id, 2, serde_json::to_value(payload)?, created_at)
        .map_err(|error| RoutingError::InvalidConfig(error.to_string()))?;
    RoutingPlan::from_snapshot(&snapshot, workflow)?;
    Ok(snapshot)
}

/// Encodes one isolated evaluation route while keeping support roles synthetic.
///
/// This profile is not exposed by normal run creation or Recommended routing.
pub(crate) fn resolve_eval_config(
    target_role: Role,
    target: &ExecutionTarget,
    workflow: &WorkflowDefinition,
    id: ConfigSnapshotId,
    created_at: DateTime<Utc>,
) -> Result<ResolvedConfigSnapshot, RoutingError> {
    let roles = required_roles(workflow);
    if !roles.contains(&target_role) {
        return Err(RoutingError::EvaluationRoleAbsent(target_role));
    }
    let routes = roles
        .into_iter()
        .map(|role| {
            let (target, reason) = if role == target_role {
                (target.clone(), "eval_candidate")
            } else {
                (
                    ExecutionTarget::new(
                        ProviderId::new("fake").expect("static provider ID is valid"),
                        None,
                    ),
                    "eval_support",
                )
            };
            (
                role,
                RouteDto {
                    provider: target.provider_id().to_string(),
                    model: target.model_id().map(ToString::to_string),
                    reason: reason.to_owned(),
                },
            )
        })
        .collect::<HashMap<_, _>>();
    let providers = routes
        .values()
        .map(|route| route.provider.as_str())
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|provider| Ok((provider.to_owned(), provider_config_dto(provider)?)))
        .collect::<Result<HashMap<_, _>, RoutingError>>()?;
    let payload = RoutingPayloadV2 {
        schema_version: 2,
        profile: "eval".to_owned(),
        profile_version: EVAL_PROFILE_VERSION.to_owned(),
        routes,
        providers,
    };
    let snapshot = ResolvedConfigSnapshot::new(id, 2, serde_json::to_value(payload)?, created_at)
        .map_err(|error| RoutingError::InvalidConfig(error.to_string()))?;
    RoutingPlan::from_snapshot(&snapshot, workflow)?;
    Ok(snapshot)
}

fn recommended_routes(
    roles: HashSet<Role>,
    availability: RecommendedAvailability,
) -> Result<HashMap<Role, RouteDto>, RoutingError> {
    let fallback = match availability {
        RecommendedAvailability {
            claude: true,
            codex: false,
        } => Some("claude"),
        RecommendedAvailability {
            claude: false,
            codex: true,
        } => Some("codex"),
        RecommendedAvailability {
            claude: false,
            codex: false,
        } => return Err(RoutingError::RecommendedUnavailable),
        RecommendedAvailability {
            claude: true,
            codex: true,
        } => None,
    };
    Ok(roles
        .into_iter()
        .map(|role| {
            let (provider, reason) = if let Some(provider) = fallback {
                (
                    provider,
                    "fallback_preferred_provider_unavailable_at_run_creation",
                )
            } else {
                match role {
                    Role::Implementer => ("codex", "implementation_specialist"),
                    Role::CodeQualityReviewer | Role::SpecReviewer => {
                        ("claude", "independent_from_implementer")
                    }
                    Role::Researcher | Role::Architect | Role::Reviewer | Role::EngineeringLead => {
                        ("claude", "recommended_default")
                    }
                }
            };
            (
                role,
                RouteDto {
                    provider: provider.to_owned(),
                    model: None,
                    reason: reason.to_owned(),
                },
            )
        })
        .collect())
}

fn required_roles(workflow: &WorkflowDefinition) -> HashSet<Role> {
    workflow
        .stages()
        .iter()
        .map(crate::domain::StageDefinition::role)
        .collect()
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutingPayloadV2 {
    schema_version: u32,
    profile: String,
    profile_version: String,
    routes: HashMap<Role, RouteDto>,
    providers: HashMap<String, ProviderConfigDto>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteDto {
    provider: String,
    model: Option<String>,
    reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderConfigDto {
    profile: String,
    schema_version: u32,
    provider_options: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scenario: Option<String>,
}

fn decode_v2(
    snapshot: &ResolvedConfigSnapshot,
    workflow: &WorkflowDefinition,
) -> Result<RoutingPlan, RoutingError> {
    let payload: RoutingPayloadV2 = serde_json::from_value(snapshot.payload().clone())?;
    if payload.schema_version != 2 {
        return Err(RoutingError::InvalidConfig(
            "payload schema_version must be 2".to_owned(),
        ));
    }
    match (payload.profile.as_str(), payload.profile_version.as_str()) {
        ("uniform", UNIFORM_PROFILE_VERSION)
        | ("recommended", RECOMMENDED_PROFILE_VERSION)
        | ("eval", EVAL_PROFILE_VERSION) => {}
        _ => return Err(RoutingError::InvalidProfileMetadata),
    }
    let mut provider_configs = HashMap::new();
    for (provider, config) in payload.providers {
        validate_provider_config(&provider, &config)?;
        provider_configs.insert(
            provider,
            ProviderConfig {
                profile: config.profile,
                schema_version: config.schema_version,
                provider_options: config.provider_options,
                scenario: config.scenario,
            },
        );
    }
    let mut role_routes = HashMap::new();
    for (role, route) in payload.routes {
        let provider_id = ProviderId::new(route.provider.clone())
            .map_err(|error| RoutingError::InvalidConfig(error.to_string()))?;
        if !matches!(provider_id.as_str(), "claude" | "codex" | "fake") {
            return Err(RoutingError::UnsupportedProvider(route.provider));
        }
        if payload.profile == "recommended" && provider_id.as_str() == "fake" {
            return Err(RoutingError::FakeInRecommended);
        }
        if !provider_configs.contains_key(provider_id.as_str()) {
            return Err(RoutingError::MissingProviderConfig(provider_id.to_string()));
        }
        let model_id = route
            .model
            .map(ModelId::new)
            .transpose()
            .map_err(|error| RoutingError::InvalidConfig(error.to_string()))?;
        if route.reason.is_empty() || route.reason.chars().any(char::is_whitespace) {
            return Err(RoutingError::InvalidConfig(
                "route reason must be non-empty machine text".to_owned(),
            ));
        }
        role_routes.insert(
            role,
            RoleRoute {
                target: ExecutionTarget::new(provider_id, model_id),
                reason: route.reason,
            },
        );
    }
    validate_required_routes(workflow, &role_routes)?;
    if payload.profile == "uniform"
        && role_routes
            .values()
            .map(RoleRoute::target)
            .collect::<HashSet<_>>()
            .len()
            != 1
    {
        return Err(RoutingError::InvalidConfig(
            "uniform profile must use one execution target".to_owned(),
        ));
    }
    if payload.profile == "eval" {
        let candidate_count = role_routes
            .values()
            .filter(|route| route.reason() == "eval_candidate")
            .count();
        let support_routes_valid = role_routes.values().all(|route| {
            route.reason() == "eval_candidate"
                || (route.reason() == "eval_support"
                    && route.target().provider_id().as_str() == "fake"
                    && route.target().model_id().is_none())
        });
        if candidate_count != 1
            || !support_routes_valid
            || role_routes.len() != required_roles(workflow).len()
        {
            return Err(RoutingError::InvalidConfig(
                "eval profile requires one candidate route and Fake support routes".to_owned(),
            ));
        }
    }
    Ok(RoutingPlan {
        profile: payload.profile,
        profile_version: payload.profile_version,
        role_routes,
        provider_configs,
    })
}

fn decode_legacy(
    snapshot: &ResolvedConfigSnapshot,
    workflow: &WorkflowDefinition,
) -> Result<RoutingPlan, RoutingError> {
    let payload = snapshot.payload();
    if payload.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(RoutingError::InvalidConfig(
            "legacy payload schema_version must be 1".to_owned(),
        ));
    }
    let provider = payload
        .get("provider")
        .and_then(Value::as_str)
        .ok_or_else(|| RoutingError::InvalidConfig("legacy provider missing".to_owned()))?;
    if matches!(provider, "claude" | "codex")
        && !payload
            .get("model")
            .is_some_and(|model| model.is_null() || model.is_string())
    {
        return Err(RoutingError::InvalidConfig(
            "legacy native model must be null or string".to_owned(),
        ));
    }
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(ModelId::new)
        .transpose()
        .map_err(|error| RoutingError::InvalidConfig(error.to_string()))?;
    let config = legacy_provider_config(provider, payload)?;
    let provider_id = ProviderId::new(provider)
        .map_err(|error| RoutingError::InvalidConfig(error.to_string()))?;
    let target = ExecutionTarget::new(provider_id, model);
    let role_routes = required_roles(workflow)
        .into_iter()
        .map(|role| {
            (
                role,
                RoleRoute {
                    target: target.clone(),
                    reason: "legacy_uniform_config".to_owned(),
                },
            )
        })
        .collect();
    Ok(RoutingPlan {
        profile: payload
            .get("profile")
            .and_then(Value::as_str)
            .unwrap_or("legacy_uniform")
            .to_owned(),
        profile_version: "legacy_schema_v1".to_owned(),
        role_routes,
        provider_configs: HashMap::from([(provider.to_owned(), config)]),
    })
}

fn legacy_provider_config(provider: &str, payload: &Value) -> Result<ProviderConfig, RoutingError> {
    let dto = match provider {
        "fake"
            if payload.get("profile").and_then(Value::as_str) == Some("development_fake")
                && payload.get("scenario").and_then(Value::as_str)
                    == Some("default_success_v1") =>
        {
            provider_config_dto("fake")?
        }
        "claude"
            if payload.get("profile").and_then(Value::as_str) == Some("native_claude")
                && payload.get("provider_options") == Some(&json!({})) =>
        {
            provider_config_dto("claude")?
        }
        "codex"
            if payload.get("profile").and_then(Value::as_str) == Some("native_codex")
                && payload.get("provider_options") == Some(&codex_options()) =>
        {
            provider_config_dto("codex")?
        }
        "claude" | "codex" | "fake" => {
            return Err(RoutingError::InvalidConfig(
                "unsupported legacy provider configuration".to_owned(),
            ));
        }
        other => return Err(RoutingError::UnsupportedProvider(other.to_owned())),
    };
    Ok(ProviderConfig {
        profile: dto.profile,
        schema_version: dto.schema_version,
        provider_options: dto.provider_options,
        scenario: dto.scenario,
    })
}

fn validate_required_routes(
    workflow: &WorkflowDefinition,
    routes: &HashMap<Role, RoleRoute>,
) -> Result<(), RoutingError> {
    for role in required_roles(workflow) {
        if !routes.contains_key(&role) {
            return Err(RoutingError::MissingRoleRoute(role));
        }
    }
    Ok(())
}

fn provider_config_dto(provider: &str) -> Result<ProviderConfigDto, RoutingError> {
    match provider {
        "claude" => Ok(ProviderConfigDto {
            profile: "native_claude".to_owned(),
            schema_version: 1,
            provider_options: json!({}),
            scenario: None,
        }),
        "codex" => Ok(ProviderConfigDto {
            profile: "native_codex".to_owned(),
            schema_version: 1,
            provider_options: codex_options(),
            scenario: None,
        }),
        "fake" => Ok(ProviderConfigDto {
            profile: "development_fake".to_owned(),
            schema_version: 1,
            provider_options: json!({}),
            scenario: Some("default_success_v1".to_owned()),
        }),
        other => Err(RoutingError::UnsupportedProvider(other.to_owned())),
    }
}

fn validate_provider_config(
    provider: &str,
    config: &ProviderConfigDto,
) -> Result<(), RoutingError> {
    let expected = provider_config_dto(provider)?;
    if config.profile != expected.profile
        || config.schema_version != expected.schema_version
        || config.provider_options != expected.provider_options
        || config.scenario != expected.scenario
    {
        return Err(RoutingError::InvalidProviderConfig(provider.to_owned()));
    }
    Ok(())
}

fn codex_options() -> Value {
    json!({
        "execution_protocol": "exec_json_v1",
        "sandbox_policy": "stage_kind_v1",
        "approval_policy": "never"
    })
}

#[derive(Debug, Error)]
pub enum RoutingError {
    #[error("unsupported execution configuration schema {0}")]
    UnsupportedSchema(u32),
    #[error("invalid routing configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid routing profile metadata")]
    InvalidProfileMetadata,
    #[error("workflow role {0:?} has no configured route")]
    MissingRoleRoute(Role),
    #[error("unsupported route provider {0:?}")]
    UnsupportedProvider(String),
    #[error("route provider {0:?} has no native provider configuration")]
    MissingProviderConfig(String),
    #[error("provider {0:?} has unsupported native configuration")]
    InvalidProviderConfig(String),
    #[error("recommended profile cannot route to development FakeProvider")]
    FakeInRecommended,
    #[error("recommended profile requires authenticated Claude Code or Codex CLI")]
    RecommendedUnavailable,
    #[error("evaluation target role {0:?} is absent from workflow")]
    EvaluationRoleAbsent(Role),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::WorkflowKind;

    type CorruptConfig = (&'static str, fn(&mut Value));

    fn snapshot(selection: ExecutionSelection, available: RecommendedAvailability) -> RoutingPlan {
        let workflow = WorkflowDefinition::built_in(WorkflowKind::Standard);
        let snapshot = resolve_config(
            selection,
            &workflow,
            available,
            ConfigSnapshotId::new("routing-test").unwrap(),
            std::time::SystemTime::now().into(),
        )
        .unwrap();
        RoutingPlan::from_snapshot(&snapshot, &workflow).unwrap()
    }

    #[test]
    fn recommended_v1_is_mixed_when_both_providers_are_available() {
        let plan = snapshot(
            ExecutionSelection::Recommended,
            RecommendedAvailability {
                claude: true,
                codex: true,
            },
        );
        assert_eq!(plan.profile_version(), RECOMMENDED_PROFILE_VERSION);
        assert_eq!(
            plan.route(Role::Implementer)
                .unwrap()
                .target()
                .provider_id()
                .as_str(),
            "codex"
        );
        for role in [
            Role::Architect,
            Role::CodeQualityReviewer,
            Role::SpecReviewer,
            Role::EngineeringLead,
        ] {
            assert_eq!(
                plan.route(role).unwrap().target().provider_id().as_str(),
                "claude"
            );
        }
    }

    #[test]
    fn recommended_persists_only_roles_required_by_workflow() {
        let plan = snapshot(
            ExecutionSelection::Recommended,
            RecommendedAvailability {
                claude: true,
                codex: true,
            },
        );
        assert_eq!(plan.routes().count(), 5);

        let workflow = WorkflowDefinition::built_in(WorkflowKind::Fast);
        let config = resolve_config(
            ExecutionSelection::Recommended,
            &workflow,
            RecommendedAvailability {
                claude: true,
                codex: true,
            },
            ConfigSnapshotId::new("fast-used-roles").unwrap(),
            std::time::SystemTime::now().into(),
        )
        .unwrap();
        let fast = RoutingPlan::from_snapshot(&config, &workflow).unwrap();
        assert_eq!(fast.routes().count(), 1);
        assert!(fast.route(Role::Implementer).is_some());
    }

    #[test]
    fn recommended_falls_back_only_during_resolution() {
        for (availability, provider) in [
            (
                RecommendedAvailability {
                    claude: true,
                    codex: false,
                },
                "claude",
            ),
            (
                RecommendedAvailability {
                    claude: false,
                    codex: true,
                },
                "codex",
            ),
        ] {
            let plan = snapshot(ExecutionSelection::Recommended, availability);
            assert!(plan.routes().all(|(_, route)| {
                route.target().provider_id().as_str() == provider
                    && route.reason().starts_with("fallback_")
            }));
        }
        let workflow = WorkflowDefinition::built_in(WorkflowKind::Fast);
        assert!(matches!(
            resolve_config(
                ExecutionSelection::Recommended,
                &workflow,
                RecommendedAvailability::default(),
                ConfigSnapshotId::new("none").unwrap(),
                std::time::SystemTime::now().into(),
            ),
            Err(RoutingError::RecommendedUnavailable)
        ));
    }

    #[test]
    fn uniform_routes_every_required_role_and_model_is_target_identity() {
        let routing = snapshot(
            ExecutionSelection::Uniform(UniformProvider::Codex),
            RecommendedAvailability::default(),
        );
        assert!(
            routing
                .routes()
                .all(|(_, route)| route.target().provider_id().as_str() == "codex")
        );
        let plain = ExecutionTarget::new(ProviderId::new("codex").unwrap(), None);
        let modeled = ExecutionTarget::new(
            ProviderId::new("codex").unwrap(),
            Some(ModelId::new("model-a").unwrap()),
        );
        assert_ne!(plain, modeled);
        assert_eq!(HashSet::from([plain, modeled]).len(), 2);
    }

    #[test]
    fn persisted_routes_are_independent_from_current_policy() {
        let workflow = WorkflowDefinition::built_in(WorkflowKind::Fast);
        let persisted = resolve_config(
            ExecutionSelection::Recommended,
            &workflow,
            RecommendedAvailability {
                claude: true,
                codex: true,
            },
            ConfigSnapshotId::new("stable").unwrap(),
            std::time::SystemTime::now().into(),
        )
        .unwrap();
        let loaded = RoutingPlan::from_snapshot(&persisted, &workflow).unwrap();
        assert_eq!(
            loaded
                .route(Role::Implementer)
                .unwrap()
                .target()
                .provider_id()
                .as_str(),
            "codex"
        );
    }

    #[test]
    fn legacy_schema_v1_normalizes_without_rewriting_snapshot() {
        let workflow = WorkflowDefinition::built_in(WorkflowKind::Standard);
        let payload = json!({
            "schema_version":1,
            "profile":"native_codex",
            "provider":"codex",
            "model":null,
            "provider_options":codex_options()
        });
        let snapshot = ResolvedConfigSnapshot::new(
            ConfigSnapshotId::new("legacy").unwrap(),
            1,
            payload.clone(),
            std::time::SystemTime::now().into(),
        )
        .unwrap();
        let plan = RoutingPlan::from_snapshot(&snapshot, &workflow).unwrap();
        assert!(
            plan.routes()
                .all(|(_, route)| route.target().provider_id().as_str() == "codex")
        );
        assert_eq!(snapshot.payload(), &payload);
        assert_eq!(plan.profile_version(), "legacy_schema_v1");
    }

    #[test]
    fn config_v2_validation_fails_closed_for_malformed_routes_and_native_options() {
        let workflow = WorkflowDefinition::built_in(WorkflowKind::Standard);
        let valid = resolve_config(
            ExecutionSelection::Recommended,
            &workflow,
            RecommendedAvailability {
                claude: true,
                codex: true,
            },
            ConfigSnapshotId::new("valid").unwrap(),
            std::time::SystemTime::now().into(),
        )
        .unwrap();
        let cases: [CorruptConfig; 7] = [
            (
                "missing-route",
                (|value: &mut Value| {
                    value["routes"].as_object_mut().unwrap().remove("architect");
                }) as fn(&mut Value),
            ),
            ("unsupported-provider", |value: &mut Value| {
                value["routes"]["architect"]["provider"] = json!("gemini");
            }),
            ("invalid-model", |value: &mut Value| {
                value["routes"]["architect"]["model"] = json!(42);
            }),
            ("missing-provider-config", |value: &mut Value| {
                value["providers"].as_object_mut().unwrap().remove("codex");
            }),
            ("malformed-profile", |value: &mut Value| {
                value["profile_version"] = json!("recommended_v999");
            }),
            ("unsafe-options", |value: &mut Value| {
                value["providers"]["codex"]["provider_options"]["approval_policy"] =
                    json!("always");
            }),
            ("fake-recommended", |value: &mut Value| {
                value["routes"]["architect"]["provider"] = json!("fake");
                value["providers"]["fake"] =
                    serde_json::to_value(provider_config_dto("fake").unwrap()).unwrap();
            }),
        ];
        for (name, mutate) in cases {
            let mut payload = valid.payload().clone();
            mutate(&mut payload);
            let snapshot = ResolvedConfigSnapshot::new(
                ConfigSnapshotId::new(name).unwrap(),
                2,
                payload,
                std::time::SystemTime::now().into(),
            )
            .unwrap();
            assert!(
                RoutingPlan::from_snapshot(&snapshot, &workflow).is_err(),
                "{name} unexpectedly passed"
            );
        }
    }

    #[test]
    fn eval_profile_routes_exactly_one_role_to_modeled_candidate_and_support_to_fake() {
        let workflow = WorkflowDefinition::built_in(WorkflowKind::Review);
        let target = ExecutionTarget::new(
            ProviderId::new("codex").unwrap(),
            Some(ModelId::new("candidate-model").unwrap()),
        );
        let snapshot = resolve_eval_config(
            Role::SpecReviewer,
            &target,
            &workflow,
            ConfigSnapshotId::new("eval-routing").unwrap(),
            std::time::SystemTime::now().into(),
        )
        .unwrap();
        let plan = RoutingPlan::from_snapshot(&snapshot, &workflow).unwrap();
        assert_eq!(plan.profile(), "eval");
        assert_eq!(plan.profile_version(), EVAL_PROFILE_VERSION);
        assert_eq!(plan.route(Role::SpecReviewer).unwrap().target(), &target);
        assert_eq!(
            plan.routes()
                .filter(|(_, route)| route.reason() == "eval_candidate")
                .count(),
            1
        );
        assert!(plan.routes().all(|(role, route)| {
            role == Role::SpecReviewer
                || (route.target().provider_id().as_str() == "fake"
                    && route.target().model_id().is_none()
                    && route.reason() == "eval_support")
        }));
        assert_eq!(RECOMMENDED_PROFILE_VERSION, "recommended_v1");
    }
}
