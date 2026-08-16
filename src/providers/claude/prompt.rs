use std::fmt::Write as _;

use crate::domain::{Role, StageKind};
use crate::engine::ProviderRequest;
use crate::providers::ArtifactRecord;

use super::ClaudeProviderError;

const MAX_DEPENDENCY_BYTES: u64 = 1024 * 1024;

pub(crate) fn compose(
    request: &ProviderRequest,
    artifacts: &[ArtifactRecord],
) -> Result<String, ClaudeProviderError> {
    let mut prompt = String::new();
    writeln!(prompt, "# Polycode stage").expect("String writes cannot fail");
    writeln!(prompt, "Task: {}", request.task()).expect("String writes cannot fail");
    writeln!(
        prompt,
        "Stage: {} ({:?})",
        request.stage_id(),
        request.stage_kind()
    )
    .expect("String writes cannot fail");
    writeln!(prompt, "Role: {:?}", request.role()).expect("String writes cannot fail");
    writeln!(
        prompt,
        "\n{}",
        role_instruction(request.role(), request.stage_kind())
    )
    .expect("String writes cannot fail");
    writeln!(
        prompt,
        "Work only inside current worktree. Respect repository instructions and native Claude Code configuration. Return concise Markdown describing result, evidence, and unresolved risks."
    )
    .expect("String writes cannot fail");

    let dependencies = artifacts
        .iter()
        .filter(|artifact| {
            request
                .dependency_stage_ids()
                .iter()
                .any(|id| id == artifact.metadata().stage_id())
        })
        .collect::<Vec<_>>();
    if !dependencies.is_empty() {
        prompt.push_str("\n# Direct dependency artifacts\n");
    }
    for artifact in dependencies {
        let metadata = std::fs::metadata(artifact.path())?;
        if metadata.len() > MAX_DEPENDENCY_BYTES {
            return Err(ClaudeProviderError::ArtifactTooLarge(
                usize::try_from(MAX_DEPENDENCY_BYTES).expect("constant fits usize"),
            ));
        }
        let content = std::fs::read_to_string(artifact.path())?;
        writeln!(
            prompt,
            "\n## {} ({:?})\n{}",
            artifact.metadata().stage_id(),
            artifact.metadata().kind(),
            content
        )
        .expect("String writes cannot fail");
    }
    Ok(prompt)
}

const fn role_instruction(role: Role, kind: StageKind) -> &'static str {
    match (role, kind) {
        (Role::Researcher, _) => "Inspect repository and gather evidence. Do not invent facts.",
        (Role::Architect, _) => "Design smallest coherent change. Name constraints and tradeoffs.",
        (Role::Implementer, _) => "Implement requested change and run proportionate verification.",
        (Role::Reviewer, _) => {
            "Review independently. Prioritize correctness, regressions, and missing tests."
        }
        (Role::EngineeringLead, StageKind::Decision) => {
            "Synthesize evidence and make explicit engineering decision."
        }
        (Role::EngineeringLead, _) => {
            "Integrate prior work into one actionable engineering result."
        }
    }
}
