use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{Role, StageId};

/// Identity of one opinionated built-in workflow family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowKind {
    Fast,
    Standard,
    Deep,
    Review,
}

/// Semantic work performed by a stage, independent from assigned role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageKind {
    Research,
    Architecture,
    Implementation,
    CodeQualityReview,
    SpecReview,
    /// Legacy/general review stage retained for persisted runs.
    Review,
    IndependentReview,
    DeepAnalysis,
    Synthesis,
    Decision,
    Fix,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    Required,
    Optional,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Dependency {
    stage_id: StageId,
    kind: DependencyKind,
}

impl Dependency {
    #[must_use]
    pub const fn new(stage_id: StageId, kind: DependencyKind) -> Self {
        Self { stage_id, kind }
    }

    #[must_use]
    pub fn required(stage_id: StageId) -> Self {
        Self::new(stage_id, DependencyKind::Required)
    }

    #[must_use]
    pub fn optional(stage_id: StageId) -> Self {
        Self::new(stage_id, DependencyKind::Optional)
    }

    #[must_use]
    pub const fn stage_id(&self) -> &StageId {
        &self.stage_id
    }

    #[must_use]
    pub const fn kind(&self) -> DependencyKind {
        self.kind
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageDefinition {
    id: StageId,
    kind: StageKind,
    role: Role,
    dependencies: Vec<Dependency>,
}

impl StageDefinition {
    #[must_use]
    pub const fn new(
        id: StageId,
        kind: StageKind,
        role: Role,
        dependencies: Vec<Dependency>,
    ) -> Self {
        Self {
            id,
            kind,
            role,
            dependencies,
        }
    }

    #[must_use]
    pub const fn id(&self) -> &StageId {
        &self.id
    }

    #[must_use]
    pub const fn kind(&self) -> StageKind {
        self.kind
    }

    #[must_use]
    pub const fn role(&self) -> Role {
        self.role
    }

    #[must_use]
    pub fn dependencies(&self) -> &[Dependency] {
        &self.dependencies
    }
}

/// Validated dependency DAG for one workflow kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkflowDefinition {
    kind: WorkflowKind,
    stages: Vec<StageDefinition>,
}

impl WorkflowDefinition {
    /// Creates and validates a workflow dependency graph.
    ///
    /// # Errors
    /// Rejects empty workflows, duplicate stages/dependencies, self references,
    /// unknown dependencies, and cycles.
    pub fn new(
        kind: WorkflowKind,
        stages: Vec<StageDefinition>,
    ) -> Result<Self, WorkflowDefinitionError> {
        validate_stages(&stages)?;
        Ok(Self { kind, stages })
    }

    /// Returns one validated built-in workflow definition.
    ///
    /// Built-ins are ordinary DAG data. The scheduler never branches on
    /// [`WorkflowKind`] and therefore treats user-defined definitions with the
    /// same graph shape identically.
    ///
    /// # Panics
    /// Panics only when a source-controlled built-in definition violates DAG
    /// invariants. Tests cover every built-in definition.
    #[must_use]
    pub fn built_in(kind: WorkflowKind) -> Self {
        let stages = match kind {
            WorkflowKind::Fast => fast_stages(),
            WorkflowKind::Standard => standard_stages(),
            WorkflowKind::Deep => deep_stages(),
            WorkflowKind::Review => review_stages(),
        };
        Self::new(kind, stages).expect("built-in workflow must remain a valid DAG")
    }

    /// Returns this workflow with further stages appended, revalidated whole.
    ///
    /// A workflow is data, and the scheduler never branches on
    /// [`WorkflowKind`], so a run that grows a remediation cycle is still an
    /// ordinary DAG. Every invariant is rechecked against the combined set —
    /// duplicate IDs, unknown dependencies and cycles are rejected here rather
    /// than discovered by the scheduler later.
    ///
    /// # Errors
    /// Rejects an empty extension and any extension that would break the DAG.
    pub fn extended(
        &self,
        additional: Vec<StageDefinition>,
    ) -> Result<Self, WorkflowDefinitionError> {
        if additional.is_empty() {
            return Err(WorkflowDefinitionError::NoStages);
        }
        let mut stages = self.stages.clone();
        stages.extend(additional);
        Self::new(self.kind, stages)
    }

    #[must_use]
    pub const fn kind(&self) -> WorkflowKind {
        self.kind
    }

    #[must_use]
    pub fn stages(&self) -> &[StageDefinition] {
        &self.stages
    }

    #[must_use]
    pub fn stage(&self, stage_id: &StageId) -> Option<&StageDefinition> {
        self.stages.iter().find(|stage| stage.id == *stage_id)
    }

    /// Whether any stage can modify repository content.
    #[must_use]
    pub fn requires_writable_workspace(&self) -> bool {
        self.stages
            .iter()
            .any(|stage| matches!(stage.kind(), StageKind::Implementation | StageKind::Fix))
    }
}

fn fast_stages() -> Vec<StageDefinition> {
    vec![stage(
        "implementation",
        StageKind::Implementation,
        Role::Implementer,
        vec![],
    )]
}

fn standard_stages() -> Vec<StageDefinition> {
    vec![
        stage(
            "architecture",
            StageKind::Architecture,
            Role::Architect,
            vec![],
        ),
        stage(
            "implementation",
            StageKind::Implementation,
            Role::Implementer,
            vec![required("architecture")],
        ),
        stage(
            "quality_review",
            StageKind::CodeQualityReview,
            Role::CodeQualityReviewer,
            vec![required("implementation")],
        ),
        stage(
            "spec_review",
            StageKind::SpecReview,
            Role::SpecReviewer,
            vec![required("implementation"), required("architecture")],
        ),
        stage(
            "decision",
            StageKind::Decision,
            Role::EngineeringLead,
            vec![required("quality_review"), required("spec_review")],
        ),
    ]
}

fn deep_stages() -> Vec<StageDefinition> {
    vec![
        stage("research", StageKind::Research, Role::Researcher, vec![]),
        stage(
            "architecture",
            StageKind::Architecture,
            Role::Architect,
            vec![required("research")],
        ),
        stage(
            "implementation",
            StageKind::Implementation,
            Role::Implementer,
            vec![required("architecture")],
        ),
        stage(
            "quality_review",
            StageKind::CodeQualityReview,
            Role::CodeQualityReviewer,
            vec![required("implementation")],
        ),
        stage(
            "spec_review",
            StageKind::SpecReview,
            Role::SpecReviewer,
            vec![required("implementation"), required("architecture")],
        ),
        stage(
            "decision",
            StageKind::Decision,
            Role::EngineeringLead,
            vec![required("quality_review"), required("spec_review")],
        ),
    ]
}

fn review_stages() -> Vec<StageDefinition> {
    vec![
        stage("research", StageKind::Research, Role::Researcher, vec![]),
        stage(
            "quality_review",
            StageKind::CodeQualityReview,
            Role::CodeQualityReviewer,
            vec![required("research")],
        ),
        stage(
            "spec_review",
            StageKind::SpecReview,
            Role::SpecReviewer,
            vec![required("research")],
        ),
        stage(
            "synthesis",
            StageKind::Synthesis,
            Role::EngineeringLead,
            vec![optional("quality_review"), optional("spec_review")],
        ),
        stage(
            "decision",
            StageKind::Decision,
            Role::EngineeringLead,
            vec![required("synthesis")],
        ),
    ]
}

/// The stages of one remediation cycle: an implementer fix, then a fresh
/// decision over it.
///
/// The fix depends on the decision it is answering, so it cannot start before
/// that verdict exists. The new decision depends on the fix, so no verdict is
/// ever recorded over work that has not happened. Nothing here re-runs the
/// reviews: the lead reads the new diff against findings it already has, and
/// an operator who wants the full review back can say so by starting a review
/// run over the result.
///
/// # Panics
/// Panics only if `index` could produce an invalid stage identity, which it
/// cannot: the identities are built from a decimal integer.
#[must_use]
pub fn fix_cycle_stages(index: u32, after_decision: &StageId) -> Vec<StageDefinition> {
    let fix_id = StageId::new(format!("fix_{index}")).expect("fix stage ID must remain valid");
    let decision_id =
        StageId::new(format!("decision_{index}")).expect("decision stage ID must remain valid");
    vec![
        StageDefinition::new(
            fix_id.clone(),
            StageKind::Fix,
            Role::Implementer,
            vec![Dependency::required(after_decision.clone())],
        ),
        StageDefinition::new(
            decision_id,
            StageKind::Decision,
            Role::EngineeringLead,
            // Both, and required. The fix alone does not say what it was
            // meant to resolve, and the verdict alone does not say whether it
            // was. A decision over one without the other is not a decision.
            vec![
                Dependency::required(fix_id),
                Dependency::required(after_decision.clone()),
            ],
        ),
    ]
}

fn stage(id: &str, kind: StageKind, role: Role, dependencies: Vec<Dependency>) -> StageDefinition {
    StageDefinition::new(
        StageId::new(id).expect("built-in stage ID must remain valid"),
        kind,
        role,
        dependencies,
    )
}

fn required(id: &str) -> Dependency {
    Dependency::required(StageId::new(id).expect("built-in dependency ID must remain valid"))
}

fn optional(id: &str) -> Dependency {
    Dependency::optional(StageId::new(id).expect("built-in dependency ID must remain valid"))
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkflowDefinitionError {
    #[error("workflow must contain at least one stage")]
    NoStages,
    #[error("duplicate stage ID: {0}")]
    DuplicateStageId(StageId),
    #[error("stage {stage_id} depends on itself")]
    SelfDependency { stage_id: StageId },
    #[error("stage {stage_id} repeats dependency {dependency_id}")]
    DuplicateDependency {
        stage_id: StageId,
        dependency_id: StageId,
    },
    #[error("stage {stage_id} references unknown dependency {dependency_id}")]
    UnknownDependency {
        stage_id: StageId,
        dependency_id: StageId,
    },
    #[error("workflow contains a dependency cycle")]
    DependencyCycle,
    #[error("validated dependency graph lost stage {0}")]
    InconsistentGraph(StageId),
}

fn validate_stages(stages: &[StageDefinition]) -> Result<(), WorkflowDefinitionError> {
    if stages.is_empty() {
        return Err(WorkflowDefinitionError::NoStages);
    }

    let mut known = HashSet::new();
    for stage in stages {
        if !known.insert(stage.id.clone()) {
            return Err(WorkflowDefinitionError::DuplicateStageId(stage.id.clone()));
        }
    }

    for stage in stages {
        let mut dependencies = HashSet::new();
        for dependency in &stage.dependencies {
            if dependency.stage_id == stage.id {
                return Err(WorkflowDefinitionError::SelfDependency {
                    stage_id: stage.id.clone(),
                });
            }
            if !dependencies.insert(dependency.stage_id.clone()) {
                return Err(WorkflowDefinitionError::DuplicateDependency {
                    stage_id: stage.id.clone(),
                    dependency_id: dependency.stage_id.clone(),
                });
            }
            if !known.contains(&dependency.stage_id) {
                return Err(WorkflowDefinitionError::UnknownDependency {
                    stage_id: stage.id.clone(),
                    dependency_id: dependency.stage_id.clone(),
                });
            }
        }
    }

    validate_acyclic(stages)
}

fn validate_acyclic(stages: &[StageDefinition]) -> Result<(), WorkflowDefinitionError> {
    let mut indegree = stages
        .iter()
        .map(|stage| (stage.id.clone(), stage.dependencies.len()))
        .collect::<HashMap<_, _>>();
    let mut dependents = HashMap::<StageId, Vec<StageId>>::new();
    for stage in stages {
        for dependency in &stage.dependencies {
            dependents
                .entry(dependency.stage_id.clone())
                .or_default()
                .push(stage.id.clone());
        }
    }

    let mut ready = indegree
        .iter()
        .filter_map(|(stage_id, count)| (*count == 0).then_some(stage_id.clone()))
        .collect::<VecDeque<_>>();
    let mut visited = 0;
    while let Some(stage_id) = ready.pop_front() {
        visited += 1;
        if let Some(children) = dependents.get(&stage_id) {
            for child in children {
                let count = indegree
                    .get_mut(child)
                    .ok_or_else(|| WorkflowDefinitionError::InconsistentGraph(child.clone()))?;
                *count -= 1;
                if *count == 0 {
                    ready.push_back(child.clone());
                }
            }
        }
    }

    if visited == stages.len() {
        Ok(())
    } else {
        Err(WorkflowDefinitionError::DependencyCycle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> StageId {
        StageId::new(value).unwrap()
    }

    fn stage(value: &str, dependencies: Vec<Dependency>) -> StageDefinition {
        StageDefinition::new(id(value), StageKind::Review, Role::Reviewer, dependencies)
    }

    #[test]
    fn required_and_optional_dependencies_are_preserved() {
        let definition = WorkflowDefinition::new(
            WorkflowKind::Review,
            vec![
                stage("analysis", vec![]),
                stage("independent", vec![]),
                stage(
                    "synthesis",
                    vec![
                        Dependency::required(id("analysis")),
                        Dependency::optional(id("independent")),
                    ],
                ),
            ],
        )
        .unwrap();

        assert_eq!(definition.stages()[2].dependencies().len(), 2);
        assert_eq!(
            definition.stages()[2].dependencies()[1].kind(),
            DependencyKind::Optional
        );
    }

    #[test]
    fn invalid_workflow_definitions_are_rejected() {
        assert_eq!(
            WorkflowDefinition::new(WorkflowKind::Fast, vec![]),
            Err(WorkflowDefinitionError::NoStages)
        );

        let duplicate = stage("same", vec![]);
        assert_eq!(
            WorkflowDefinition::new(WorkflowKind::Fast, vec![duplicate.clone(), duplicate]),
            Err(WorkflowDefinitionError::DuplicateStageId(id("same")))
        );

        assert_eq!(
            WorkflowDefinition::new(
                WorkflowKind::Fast,
                vec![stage("self", vec![Dependency::required(id("self"))])]
            ),
            Err(WorkflowDefinitionError::SelfDependency {
                stage_id: id("self")
            })
        );

        assert_eq!(
            WorkflowDefinition::new(
                WorkflowKind::Fast,
                vec![stage("known", vec![Dependency::required(id("missing"))])]
            ),
            Err(WorkflowDefinitionError::UnknownDependency {
                stage_id: id("known"),
                dependency_id: id("missing")
            })
        );
    }

    #[test]
    fn duplicate_dependencies_and_cycles_are_rejected() {
        assert_eq!(
            WorkflowDefinition::new(
                WorkflowKind::Review,
                vec![
                    stage("source", vec![]),
                    stage(
                        "consumer",
                        vec![
                            Dependency::required(id("source")),
                            Dependency::optional(id("source")),
                        ],
                    ),
                ],
            ),
            Err(WorkflowDefinitionError::DuplicateDependency {
                stage_id: id("consumer"),
                dependency_id: id("source")
            })
        );

        assert_eq!(
            WorkflowDefinition::new(
                WorkflowKind::Review,
                vec![
                    stage("one", vec![Dependency::required(id("two"))]),
                    stage("two", vec![Dependency::required(id("one"))]),
                ],
            ),
            Err(WorkflowDefinitionError::DependencyCycle)
        );
    }

    #[test]
    fn built_in_review_is_a_data_driven_fan_out_and_join() {
        let workflow = WorkflowDefinition::built_in(WorkflowKind::Review);
        let quality = workflow.stage(&id("quality_review")).unwrap();
        let spec = workflow.stage(&id("spec_review")).unwrap();
        let synthesis = workflow.stage(&id("synthesis")).unwrap();

        assert_eq!(quality.kind(), StageKind::CodeQualityReview);
        assert_eq!(quality.role(), Role::CodeQualityReviewer);
        assert_eq!(
            quality.dependencies(),
            &[Dependency::required(id("research"))]
        );
        assert_eq!(spec.kind(), StageKind::SpecReview);
        assert_eq!(spec.role(), Role::SpecReviewer);
        assert_eq!(spec.dependencies(), &[Dependency::required(id("research"))]);
        assert_eq!(
            synthesis.dependencies(),
            &[
                Dependency::optional(id("quality_review")),
                Dependency::optional(id("spec_review")),
            ]
        );
    }

    #[test]
    fn new_built_ins_specialize_reviewers_and_direct_artifact_dependencies() {
        let fast = WorkflowDefinition::built_in(WorkflowKind::Fast);
        assert_eq!(fast.stages().len(), 1);
        assert_eq!(fast.stages()[0].id(), &id("implementation"));

        let standard = WorkflowDefinition::built_in(WorkflowKind::Standard);
        assert!(
            standard
                .stage(&id("architecture"))
                .unwrap()
                .dependencies()
                .is_empty()
        );
        let deep = WorkflowDefinition::built_in(WorkflowKind::Deep);
        assert!(
            deep.stage(&id("research"))
                .unwrap()
                .dependencies()
                .is_empty()
        );
        assert_eq!(
            deep.stage(&id("architecture")).unwrap().dependencies(),
            &[Dependency::required(id("research"))]
        );

        for kind in [WorkflowKind::Standard, WorkflowKind::Deep] {
            let workflow = WorkflowDefinition::built_in(kind);
            assert!(workflow.stage(&id("review")).is_none());
            let quality = workflow.stage(&id("quality_review")).unwrap();
            let spec = workflow.stage(&id("spec_review")).unwrap();
            let decision = workflow.stage(&id("decision")).unwrap();
            assert_eq!(quality.kind(), StageKind::CodeQualityReview);
            assert_eq!(quality.role(), Role::CodeQualityReviewer);
            assert_eq!(
                quality.dependencies(),
                &[Dependency::required(id("implementation"))]
            );
            assert_eq!(spec.kind(), StageKind::SpecReview);
            assert_eq!(spec.role(), Role::SpecReviewer);
            assert_eq!(
                spec.dependencies(),
                &[
                    Dependency::required(id("implementation")),
                    Dependency::required(id("architecture")),
                ]
            );
            assert_eq!(
                decision.dependencies(),
                &[
                    Dependency::required(id("quality_review")),
                    Dependency::required(id("spec_review")),
                ]
            );
        }
    }

    #[test]
    fn stage_kind_serialization_preserves_legacy_and_specialized_values() {
        assert_eq!(
            serde_json::from_str::<StageKind>("\"review\"").unwrap(),
            StageKind::Review
        );
        assert_eq!(
            serde_json::to_string(&StageKind::CodeQualityReview).unwrap(),
            "\"code_quality_review\""
        );
        assert_eq!(
            serde_json::to_string(&StageKind::SpecReview).unwrap(),
            "\"spec_review\""
        );
    }

    #[test]
    fn every_built_in_is_a_valid_nonempty_dag() {
        for kind in [
            WorkflowKind::Fast,
            WorkflowKind::Standard,
            WorkflowKind::Deep,
            WorkflowKind::Review,
        ] {
            let workflow = WorkflowDefinition::built_in(kind);
            assert_eq!(workflow.kind(), kind);
            assert!(!workflow.stages().is_empty());
        }
    }

    #[test]
    fn writable_workspace_capability_depends_on_stage_semantics_not_workflow_name() {
        let read_only =
            WorkflowDefinition::new(WorkflowKind::Standard, vec![stage("review", vec![])]).unwrap();
        let mutating = WorkflowDefinition::new(
            WorkflowKind::Review,
            vec![StageDefinition::new(
                id("fix"),
                StageKind::Fix,
                Role::Implementer,
                vec![],
            )],
        )
        .unwrap();

        assert!(!read_only.requires_writable_workspace());
        assert!(mutating.requires_writable_workspace());
    }
}
