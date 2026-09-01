use std::fmt::Write as _;

use crate::domain::StageKind;
use crate::engine::ProviderRequest;
use crate::providers::change_handoff::ChangeHandoff;
use crate::providers::{ArtifactRecord, change_handoff, stage_prompt};

use super::CodexProviderError;

const MAX_DEPENDENCY_BYTES: u64 = 1024 * 1024;

/// Codex rejects a turn whose input exceeds 1,048,576 characters
/// (`input_too_large`, code -32602). The margin leaves room for whatever the
/// runtime wraps around stdin. Bytes bound characters from above, so budgeting
/// in bytes can only undershoot the character limit.
const MAX_INPUT_BYTES: usize = 1024 * 1024 - 16 * 1024;

pub(crate) fn compose(
    request: &ProviderRequest,
    artifacts: &[ArtifactRecord],
    handoff: Option<&ChangeHandoff>,
    continue_instruction: Option<&str>,
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
    // See the Claude adapter: the operator's own instruction for a continue
    // cycle's follow-up stage, carried here through the same immutable
    // run-private stdin path an attention response uses.
    if let Some(instruction) = continue_instruction {
        writeln!(prompt, "\n# Operator instruction\n{instruction}")
            .expect("String writes cannot fail");
    }
    writeln!(
        prompt,
        "You are executing one Polycode stage. Work only inside current managed worktree. Respect repository instructions, AGENTS.md, rules, skills, MCP configuration, and native Codex configuration discovered normally. Do not apply changes to another checkout. Do not invoke Polycode apply. Do not commit or push. Return concise Markdown describing result, evidence, and unresolved risks."
    )
    .expect("String writes cannot fail");
    writeln!(prompt, "{}", stage_prompt::BOTTOM_LINE).expect("String writes cannot fail");
    match request.stage_kind() {
        StageKind::Implementation | StageKind::Fix | StageKind::FollowUp => writeln!(
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
    if let Some(handoff) = handoff {
        // The change map is the one part that may legitimately dwarf the
        // input limit, and it is navigation aid, not source of truth — so it
        // is the part that yields whatever room the rest of the prompt left.
        let room = MAX_INPUT_BYTES.saturating_sub(prompt.len());
        prompt.push_str(&change_handoff::render_within(handoff, room));
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::domain::{ProviderSessionId, Role, RunId, StageId, StageKind, StageStatus};
    use crate::git::{ChangeKind, ChangedFileRecord};

    use super::*;

    fn request(role: Role, kind: StageKind) -> ProviderRequest {
        ProviderRequest::new(
            RunId::from_u128(2),
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

    fn handoff() -> ChangeHandoff {
        ChangeHandoff::for_tests(
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
        )
    }

    #[test]
    fn every_stage_prompt_asks_for_the_bottom_line_section() {
        let prompt = compose(
            &request(Role::Researcher, StageKind::Research),
            &[],
            None,
            None,
        )
        .unwrap();

        assert!(prompt.contains(stage_prompt::BOTTOM_LINE));
    }

    #[test]
    fn reviewer_prompt_embeds_shared_change_handoff_verbatim_and_grows_by_it() {
        let handoff = handoff();
        let request = request(Role::SpecReviewer, StageKind::SpecReview);
        let without = compose(&request, &[], None, None).unwrap();
        let with = compose(&request, &[], Some(&handoff), None).unwrap();
        let section = change_handoff::render(&handoff);

        assert!(!without.contains("# Implementation change map"));
        assert!(with.contains(&section), "section must embed verbatim");
        assert_eq!(with.len(), without.len() + section.len());
        // Same shared render() as the Claude adapter: semantic identity is the
        // single provider-neutral section, byte-for-byte.
    }

    /// Codex rejects any turn over its input limit outright, so a change map
    /// bigger than the limit must arrive shortened, never verbatim — the run
    /// died on `input_too_large` in the wild.
    #[test]
    fn a_change_map_larger_than_the_input_limit_is_shortened_to_fit() {
        let diff_text = "+padding line of diff text to overflow the input\n".repeat(25_000);
        let total = diff_text.len() as u64;
        let giant = ChangeHandoff::for_tests(
            &"b".repeat(40),
            vec![ChangedFileRecord {
                kind: ChangeKind::Modified,
                path: "src/lib.rs".to_owned(),
                previous_path: None,
                binary: false,
            }],
            &diff_text,
            total,
            true,
        );
        assert!(change_handoff::render(&giant).len() > MAX_INPUT_BYTES);
        let request = request(Role::SpecReviewer, StageKind::SpecReview);
        let prompt = compose(&request, &[], Some(&giant), None).unwrap();

        assert!(prompt.len() <= MAX_INPUT_BYTES);
        assert!(prompt.contains("Completeness: INCOMPLETE"));
    }

    #[test]
    fn continuation_prompt_stays_compact_without_change_handoff() {
        let text = continuation(&request(
            Role::CodeQualityReviewer,
            StageKind::CodeQualityReview,
        ));
        assert!(!text.contains("# Implementation change map"));
    }

    /// See the Claude adapter's equivalent test: the operator's instruction
    /// must be embedded verbatim for a follow-up stage and absent otherwise.
    #[test]
    fn the_operators_continue_instruction_is_embedded_verbatim_when_present() {
        let request = request(Role::Implementer, StageKind::FollowUp);
        let without = compose(&request, &[], None, None).unwrap();
        let with = compose(&request, &[], None, Some("add integration tests too")).unwrap();

        assert!(!without.contains("# Operator instruction"));
        assert!(with.contains("# Operator instruction"));
        assert!(with.contains("add integration tests too"));
    }
}
