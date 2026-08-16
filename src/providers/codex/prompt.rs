use std::fmt::Write as _;

use crate::domain::StageKind;
use crate::engine::ProviderRequest;
use crate::providers::{ArtifactRecord, stage_prompt};

use super::CodexProviderError;

const MAX_DEPENDENCY_BYTES: u64 = 1024 * 1024;

pub(crate) fn compose(
    request: &ProviderRequest,
    artifacts: &[ArtifactRecord],
) -> Result<String, CodexProviderError> {
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
        stage_prompt::instruction(request.role(), request.stage_kind())
    )
    .expect("String writes cannot fail");
    writeln!(
        prompt,
        "You are executing one Polycode stage. Work only inside current managed worktree. Respect repository instructions, AGENTS.md, rules, skills, MCP configuration, and native Codex configuration discovered normally. Do not apply changes to another checkout. Do not invoke Polycode apply. Do not commit or push. Return concise Markdown describing result, evidence, and unresolved risks."
    )
    .expect("String writes cannot fail");
    match request.stage_kind() {
        StageKind::Implementation | StageKind::Fix => writeln!(
            prompt,
            "Make required changes in managed worktree and run proportionate local validation when safe."
        ),
        _ => writeln!(
            prompt,
            "Inspect, reason, and report only. Do not modify repository content."
        ),
    }
    .expect("String writes cannot fail");

    let dependencies = stage_prompt::direct_dependency_artifacts(request, artifacts);
    if !dependencies.is_empty() {
        prompt.push_str("\n# Direct dependency artifacts\n");
    }
    for artifact in dependencies {
        let metadata = std::fs::metadata(artifact.path())?;
        if metadata.len() > MAX_DEPENDENCY_BYTES {
            return Err(CodexProviderError::ArtifactTooLarge(
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

pub(crate) fn continuation(request: &ProviderRequest) -> String {
    format!(
        "Continue exact interrupted Polycode stage {} in same native Codex thread. Finish assigned {:?} work in current managed worktree. {} Do not commit, push, or apply changes to another checkout. Return final Markdown result.",
        request.stage_id(),
        request.stage_kind(),
        stage_prompt::instruction(request.role(), request.stage_kind())
    )
}
