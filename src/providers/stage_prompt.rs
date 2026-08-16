use crate::domain::{Role, StageKind};
use crate::engine::ProviderRequest;

use super::ArtifactRecord;

pub(crate) fn direct_dependency_artifacts<'a>(
    request: &ProviderRequest,
    artifacts: &'a [ArtifactRecord],
) -> Vec<&'a ArtifactRecord> {
    artifacts
        .iter()
        .filter(|artifact| {
            request
                .dependency_stage_ids()
                .iter()
                .any(|id| id == artifact.metadata().stage_id())
        })
        .collect()
}

/// Provider-neutral semantic contract for one engineering responsibility.
/// Native adapters add transport and safety framing around this text.
pub(crate) const fn instruction(role: Role, kind: StageKind) -> &'static str {
    match (role, kind) {
        (Role::CodeQualityReviewer, StageKind::CodeQualityReview) => {
            "Inspect the actual repository code and relevant diff, not only an Implementation artifact when one exists. Judge HOW the implementation is engineered: simplicity, readability, maintainability, naming, module boundaries, error handling, hidden side effects, coupling, avoidable nesting and control-flow complexity, unnecessary abstraction, speculative generality, brittle tests, tests that pass by construction, dead code, duplicated concepts, convoluted comments, and implementation-level regressions. Do not repeat a complete requirement or specification review. Do not edit files. Return Markdown with # Code Quality Review and optional ## Must fix, ## Minor, and ## Good sections; do not invent findings."
        }
        (Role::SpecReviewer, StageKind::SpecReview) => {
            "Inspect the actual repository code and relevant diff against the immutable user task and available architecture or design evidence. Judge WHAT behavior was delivered. Classify findings as Missing, Wrong, or Unrequested. Treat RunInput as primary intent; architecture refines implementation but cannot silently override it. Treat code and repository state as observed result, Implementation artifacts as supporting context, repository-native authoritative specifications as additional evidence, and tests as evidence rather than product truth. Useful unrequested behavior remains a scope issue. Do not spend the review on style unless it causes a requirement failure. Do not edit files. Return Markdown with # Specification Review and optional ## Missing, ## Wrong, ## Unrequested, and ## Good sections; do not invent findings."
        }
        (Role::Researcher, _) => "Inspect repository and gather evidence. Do not invent facts.",
        (Role::Architect, _) => "Design smallest coherent change. Name constraints and tradeoffs.",
        (Role::Implementer, _) => "Implement requested change and run proportionate verification.",
        (Role::CodeQualityReviewer, _) => {
            "Assess implementation quality independently. Inspect actual repository state and do not edit files."
        }
        (Role::SpecReviewer, _) => {
            "Assess delivered behavior against immutable user intent independently. Inspect actual repository state and do not edit files."
        }
        (Role::Reviewer, _) => {
            "Perform legacy/general independent review. Prioritize correctness, regressions, and missing tests. Do not edit files."
        }
        (Role::EngineeringLead, StageKind::Decision) => {
            "Synthesize direct review evidence across two distinct axes: implementation quality and specification compliance. Do not count findings mechanically or infer approval from reviewer completion. Surface disagreements between reviewers and make an explicit engineering decision."
        }
        (Role::EngineeringLead, _) => {
            "Integrate direct dependency evidence into one actionable engineering result."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specialized_reviews_have_distinct_contracts() {
        let quality = instruction(Role::CodeQualityReviewer, StageKind::CodeQualityReview);
        let spec = instruction(Role::SpecReviewer, StageKind::SpecReview);

        assert!(quality.contains("HOW"));
        assert!(quality.contains("actual repository code and relevant diff"));
        assert!(quality.contains("Do not repeat a complete requirement"));
        assert!(!quality.contains("Classify findings as Missing, Wrong, or Unrequested"));
        assert!(spec.contains("WHAT"));
        assert!(spec.contains("Missing, Wrong, or Unrequested"));
        assert!(spec.contains("tests as evidence rather than product truth"));
        assert_ne!(quality, spec);
    }

    #[test]
    fn decision_contract_synthesizes_both_review_axes() {
        let decision = instruction(Role::EngineeringLead, StageKind::Decision);
        assert!(decision.contains("implementation quality"));
        assert!(decision.contains("specification compliance"));
        assert!(decision.contains("Surface disagreements"));
    }
}
