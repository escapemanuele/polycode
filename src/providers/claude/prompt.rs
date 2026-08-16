use std::fmt::Write as _;

use crate::engine::ProviderRequest;
use crate::providers::{ArtifactRecord, stage_prompt};

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
        stage_prompt::instruction(request.role(), request.stage_kind())
    )
    .expect("String writes cannot fail");
    writeln!(
        prompt,
        "Work only inside current worktree. Respect repository instructions and native Claude Code configuration. Return concise Markdown describing result, evidence, and unresolved risks."
    )
    .expect("String writes cannot fail");

    let dependencies = stage_prompt::direct_dependency_artifacts(request, artifacts);
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::domain::{ProviderSessionId, Role, RunId, StageId, StageKind, StageStatus};

    use super::*;

    fn request(role: Role, kind: StageKind) -> ProviderRequest {
        ProviderRequest::new(
            RunId::from_u128(1),
            StageId::new("review").unwrap(),
            kind,
            StageStatus::Ready,
            role,
            "immutable task".to_owned(),
            PathBuf::from("/managed/worktree"),
            1,
            0,
            Option::<ProviderSessionId>::None,
            vec![],
        )
    }

    #[test]
    fn claude_uses_shared_specialized_reviewer_contracts() {
        let quality = compose(
            &request(Role::CodeQualityReviewer, StageKind::CodeQualityReview),
            &[],
        )
        .unwrap();
        let spec = compose(&request(Role::SpecReviewer, StageKind::SpecReview), &[]).unwrap();

        assert!(quality.contains("Judge HOW the implementation is engineered"));
        assert!(quality.contains("Do not edit files"));
        assert!(spec.contains("Judge WHAT behavior was delivered"));
        assert!(spec.contains("Missing, Wrong, or Unrequested"));
        assert!(spec.contains("Do not edit files"));
    }
}
