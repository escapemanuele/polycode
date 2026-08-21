use std::fmt::Write as _;

use crate::engine::ProviderRequest;
use crate::providers::change_handoff::ChangeHandoff;
use crate::providers::{ArtifactRecord, change_handoff, stage_prompt};

use super::ClaudeProviderError;

const MAX_DEPENDENCY_BYTES: u64 = 1024 * 1024;

pub(crate) fn compose(
    request: &ProviderRequest,
    artifacts: &[ArtifactRecord],
    handoff: Option<&ChangeHandoff>,
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
    if let Some(handoff) = handoff {
        prompt.push_str(&change_handoff::render(handoff));
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
    fn reviewer_prompt_embeds_shared_change_handoff_verbatim_and_grows_by_it() {
        use crate::git::{ChangeKind, ChangedFileRecord};

        let handoff = ChangeHandoff::for_tests(
            &"b".repeat(40),
            vec![ChangedFileRecord {
                kind: ChangeKind::Modified,
                path: "src/lib.rs".to_owned(),
                previous_path: None,
                binary: false,
            }],
            "diff --git a/src/lib.rs b/src/lib.rs\n+one line\n",
            46,
            true,
        );
        let request = request(Role::CodeQualityReviewer, StageKind::CodeQualityReview);
        let without = compose(&request, &[], None).unwrap();
        let with = compose(&request, &[], Some(&handoff)).unwrap();
        let section = change_handoff::render(&handoff);

        assert!(!without.contains("# Implementation change map"));
        assert!(with.contains(&section), "section must embed verbatim");
        assert_eq!(with.len(), without.len() + section.len());
    }

    #[test]
    fn resume_continuation_stays_compact_without_change_handoff() {
        use crate::domain::ProviderSessionId;

        let session = ProviderSessionId::new("claude-session").unwrap();
        let denial = super::super::protocol::PermissionDenial {
            tool_name: "Edit".to_owned(),
            tool_use_id: None,
            tool_input: serde_json::json!({"file_path": "/managed/worktree/a.rs"}),
        };
        let command = super::super::command::resume(&session, &[denial], None, None).unwrap();
        let stdin = String::from_utf8(command.stdin).unwrap();
        assert!(!stdin.contains("# Implementation change map"));
        assert!(stdin.len() < 256, "resume stdin must stay compact");
    }

    #[test]
    fn claude_uses_shared_specialized_reviewer_contracts() {
        let quality = compose(
            &request(Role::CodeQualityReviewer, StageKind::CodeQualityReview),
            &[],
            None,
        )
        .unwrap();
        let spec = compose(
            &request(Role::SpecReviewer, StageKind::SpecReview),
            &[],
            None,
        )
        .unwrap();

        assert!(quality.contains("Judge HOW the implementation is engineered"));
        assert!(quality.contains("Do not edit files"));
        assert!(spec.contains("Judge WHAT behavior was delivered"));
        assert!(spec.contains("Missing, Wrong, or Unrequested"));
        assert!(spec.contains("Do not edit files"));
    }
}
