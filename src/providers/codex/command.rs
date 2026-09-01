use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::domain::{EffortLevel, EffortSetting, ModelId, ProviderSessionId, StageKind};

pub(crate) struct CodexCommand {
    pub argv: Vec<OsString>,
    pub stdin: Vec<u8>,
    pub final_message_path: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CodexSandbox {
    ReadOnly,
    WorkspaceWrite,
}

impl CodexSandbox {
    pub(crate) const fn for_stage(kind: StageKind) -> Self {
        match kind {
            StageKind::Implementation
            | StageKind::Simplification
            | StageKind::Fix
            | StageKind::FollowUp => Self::WorkspaceWrite,
            StageKind::Research
            | StageKind::Architecture
            | StageKind::CodeQualityReview
            | StageKind::SpecReview
            | StageKind::Review
            | StageKind::IndependentReview
            | StageKind::DeepAnalysis
            | StageKind::Synthesis
            | StageKind::Decision
            | StageKind::Verify => Self::ReadOnly,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
        }
    }
}

pub(crate) fn initial(
    prompt: &str,
    stage_kind: StageKind,
    model: Option<&ModelId>,
    effort: EffortSetting,
    final_message_path: &Path,
) -> CodexCommand {
    let mut argv = base(stage_kind, model, effort, final_message_path);
    argv.push(OsString::from("-"));
    CodexCommand {
        argv,
        stdin: prompt.as_bytes().to_vec(),
        final_message_path: final_message_path.to_path_buf(),
    }
}

pub(crate) fn resume(
    session_id: &ProviderSessionId,
    prompt: &str,
    stage_kind: StageKind,
    model: Option<&ModelId>,
    effort: EffortSetting,
    final_message_path: &Path,
) -> CodexCommand {
    let mut argv = base(stage_kind, model, effort, final_message_path);
    argv.push(OsString::from("resume"));
    argv.push(OsString::from(session_id.as_str()));
    argv.push(OsString::from("-"));
    CodexCommand {
        argv,
        stdin: prompt.as_bytes().to_vec(),
        final_message_path: final_message_path.to_path_buf(),
    }
}

fn base(
    stage_kind: StageKind,
    model: Option<&ModelId>,
    effort: EffortSetting,
    final_message_path: &Path,
) -> Vec<OsString> {
    let mut argv = Vec::new();
    if let Some(model) = model {
        argv.push(OsString::from("--model"));
        argv.push(OsString::from(model.as_str()));
    }
    // Adapter-owned mapping onto the native supported `model_reasoning_effort`
    // configuration override. NativeDefault omits the override entirely so
    // ~/.codex/config.toml keeps deciding.
    if let EffortSetting::Level(level) = effort {
        argv.push(OsString::from("-c"));
        argv.push(OsString::from(format!(
            "model_reasoning_effort=\"{}\"",
            native_effort_value(level)
        )));
    }
    argv.extend([
        OsString::from("--sandbox"),
        OsString::from(CodexSandbox::for_stage(stage_kind).as_str()),
        OsString::from("--ask-for-approval"),
        OsString::from("never"),
        OsString::from("exec"),
        OsString::from("--json"),
        OsString::from("--color"),
        OsString::from("never"),
        OsString::from("--output-last-message"),
        final_message_path.as_os_str().to_owned(),
    ]);
    argv
}

/// Native Codex value for one explicit requested level.
pub(crate) const fn native_effort_value(level: EffortLevel) -> &'static str {
    match level {
        EffortLevel::Low => "low",
        EffortLevel::Medium => "medium",
        EffortLevel::High => "high",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_default_omits_reasoning_override_byte_identical() {
        let command = initial(
            "prompt",
            StageKind::Review,
            None,
            EffortSetting::NativeDefault,
            Path::new("/private/final.md"),
        );
        assert!(
            !command
                .argv
                .iter()
                .any(|arg| arg.to_string_lossy().contains("model_reasoning_effort"))
        );
        assert!(!command.argv.iter().any(|arg| arg == "-c"));
    }

    #[test]
    fn explicit_effort_maps_onto_model_reasoning_effort_override() {
        for (setting, expected) in [
            (EffortSetting::LOW, "model_reasoning_effort=\"low\""),
            (EffortSetting::MEDIUM, "model_reasoning_effort=\"medium\""),
            (EffortSetting::HIGH, "model_reasoning_effort=\"high\""),
        ] {
            for command in [
                initial(
                    "prompt",
                    StageKind::Review,
                    None,
                    setting,
                    Path::new("/private/final.md"),
                ),
                resume(
                    &ProviderSessionId::new("thread-1").unwrap(),
                    "continue",
                    StageKind::Review,
                    None,
                    setting,
                    Path::new("/private/final.md"),
                ),
            ] {
                let args = strings(&command.argv);
                assert!(
                    args.windows(2)
                        .any(|pair| pair[0] == "-c" && pair[1] == expected),
                    "{setting:?} must produce -c {expected}"
                );
                // Root-level override must precede the exec subcommand.
                let c_index = args.iter().position(|arg| arg == "-c").unwrap();
                let exec_index = args.iter().position(|arg| arg == "exec").unwrap();
                assert!(c_index < exec_index);
            }
        }
    }

    #[test]
    fn initial_prompt_is_stdin_and_policy_is_explicit() {
        let marker = "SUPER_SECRET_TASK_MARKER";
        let command = initial(
            marker,
            StageKind::Review,
            None,
            EffortSetting::NativeDefault,
            Path::new("/private/final.md"),
        );
        let args = strings(&command.argv);
        assert_eq!(args.last().map(String::as_str), Some("-"));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--sandbox", "read-only"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--ask-for-approval", "never"])
        );
        assert!(args.iter().any(|arg| arg == "--json"));
        assert!(!args.iter().any(|arg| arg.contains(marker)));
        assert_eq!(command.stdin, marker.as_bytes());
        assert_safe(&args);
    }

    #[test]
    fn implementation_uses_workspace_write_and_explicit_model() {
        let command = initial(
            "task",
            StageKind::Implementation,
            Some(&ModelId::new("configured-model").unwrap()),
            EffortSetting::NativeDefault,
            Path::new("/private/final.md"),
        );
        let args = strings(&command.argv);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--sandbox", "workspace-write"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--model", "configured-model"])
        );
        assert_safe(&args);
    }

    #[test]
    fn specialized_review_stages_are_read_only() {
        for kind in [StageKind::CodeQualityReview, StageKind::SpecReview] {
            let command = initial(
                "review",
                kind,
                None,
                EffortSetting::NativeDefault,
                Path::new("/private/final.md"),
            );
            let args = strings(&command.argv);
            assert!(
                args.windows(2)
                    .any(|pair| pair == ["--sandbox", "read-only"])
            );
            assert_safe(&args);
        }
    }

    #[test]
    fn resume_targets_exact_native_session_without_last() {
        let command = resume(
            &ProviderSessionId::new("thread-A").unwrap(),
            "continue",
            StageKind::Fix,
            None,
            EffortSetting::NativeDefault,
            Path::new("/private/final.md"),
        );
        let args = strings(&command.argv);
        assert!(
            args.windows(3)
                .any(|part| part == ["resume", "thread-A", "-"])
        );
        assert!(!args.iter().any(|arg| arg == "--last"));
        assert!(!args.iter().any(|arg| arg == "--model"));
        assert_safe(&args);
    }

    fn strings(argv: &[OsString]) -> Vec<String> {
        argv.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn assert_safe(args: &[String]) {
        for forbidden in [
            "--yolo",
            "--dangerously-bypass-approvals-and-sandbox",
            "danger-full-access",
            "--ephemeral",
            "--skip-git-repo-check",
            "--ignore-user-config",
            "--ignore-rules",
            "--dangerously-bypass-hook-trust",
        ] {
            assert!(!args.iter().any(|arg| arg == forbidden));
        }
    }
}
