use std::collections::BTreeSet;
use std::ffi::OsString;

use crate::domain::{EffortLevel, EffortSetting, ModelId, ProviderSessionId};

use super::{ClaudeProviderError, PermissionDenial};

#[derive(Debug)]
pub(crate) struct ClaudeCommand {
    pub argv: Vec<OsString>,
    pub stdin: Vec<u8>,
}

pub(crate) fn initial(
    prompt: &str,
    model: Option<&ModelId>,
    effort: EffortSetting,
) -> ClaudeCommand {
    ClaudeCommand {
        argv: base(model, effort),
        stdin: prompt.as_bytes().to_vec(),
    }
}

pub(crate) fn resume(
    session_id: &ProviderSessionId,
    denials: &[PermissionDenial],
    response: Option<&str>,
    model: Option<&ModelId>,
    effort: EffortSetting,
) -> Result<ClaudeCommand, ClaudeProviderError> {
    let rules = grant_rules(denials, response)?;
    let mut argv = base(model, effort);
    argv.push(OsString::from("--resume"));
    argv.push(OsString::from(session_id.as_str()));
    if !rules.is_empty() {
        argv.push(OsString::from("--allowedTools"));
        argv.extend(rules.into_iter().map(OsString::from));
    }
    let stdin = response.unwrap_or(
        "User approved the exact pending permission request. Continue the same task and session.",
    );
    Ok(ClaudeCommand {
        argv,
        stdin: stdin.as_bytes().to_vec(),
    })
}

/// Exact `--allowedTools` rules that replay one approved attention, or the
/// reason it cannot be replayed at all.
///
/// This is the single gate for "can this approval be honoured": resolution
/// must run it *before* committing, so an unreplayable denial (compound Bash)
/// is refused to the user instead of committing a resolution every later
/// drive fails on.
///
/// An operator response is also an answer to a permission request: when no
/// denial can be granted exactly, a non-empty response still resumes the
/// session with that text and grants nothing, so the operator can say
/// "skip it, do X instead" rather than only stop the run.
///
/// # Errors
/// `QuestionResponseRequired` when a pending question has no response;
/// `UnsafePermission` when no denial can be granted as an exact rule and
/// there is no response to continue on.
pub(crate) fn grant_rules(
    denials: &[PermissionDenial],
    response: Option<&str>,
) -> Result<BTreeSet<String>, ClaudeProviderError> {
    let has_question = denials
        .iter()
        .any(|denial| denial.tool_name == "AskUserQuestion");
    if has_question && response.is_none_or(|response| response.trim().is_empty()) {
        return Err(ClaudeProviderError::QuestionResponseRequired);
    }
    let mut rules = BTreeSet::new();
    let mut unsafe_permission = None;
    for denial in denials
        .iter()
        .filter(|denial| denial.tool_name != "AskUserQuestion")
    {
        match denial.exact_rule() {
            Ok(rule) => {
                rules.insert(rule);
            }
            Err(error) => unsafe_permission = Some(error),
        }
    }
    let has_response = response.is_some_and(|response| !response.trim().is_empty());
    if rules.is_empty() && !has_question && !has_response {
        return Err(unsafe_permission.unwrap_or_else(|| {
            ClaudeProviderError::UnsafePermission("empty denial set".to_owned())
        }));
    }
    // With nothing grantable but an operator answer present, the session
    // resumes on that answer alone: "continue without it". Nothing is granted.
    Ok(rules)
}

fn base(model: Option<&ModelId>, effort: EffortSetting) -> Vec<OsString> {
    let mut argv = vec![
        OsString::from("-p"),
        OsString::from("--verbose"),
        OsString::from("--output-format"),
        OsString::from("stream-json"),
        OsString::from("--permission-mode"),
        OsString::from("dontAsk"),
    ];
    if let Some(model) = model {
        argv.push(OsString::from("--model"));
        argv.push(OsString::from(model.as_str()));
    }
    // Adapter-owned mapping onto the native supported `--effort` session flag
    // (Claude Code 2.x: low|medium|high|xhigh|max). NativeDefault omits the
    // flag entirely so native configuration keeps deciding.
    if let EffortSetting::Level(level) = effort {
        argv.push(OsString::from("--effort"));
        argv.push(OsString::from(native_effort_value(level)));
    }
    argv
}

/// Native Claude Code value for one explicit requested level.
pub(crate) const fn native_effort_value(level: EffortLevel) -> &'static str {
    match level {
        EffortLevel::Low => "low",
        EffortLevel::Medium => "medium",
        EffortLevel::High => "high",
        EffortLevel::XHigh => "xhigh",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn native_default_argv_is_byte_identical_to_pre_effort_invocation() {
        let command = initial("prompt", None, EffortSetting::NativeDefault);
        let args = command
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                "-p",
                "--verbose",
                "--output-format",
                "stream-json",
                "--permission-mode",
                "dontAsk",
            ]
        );
    }

    #[test]
    fn effort_changes_only_argv_never_the_injected_prompt() {
        let prompt = "identical stage prompt with change map";
        let baseline = initial(prompt, None, EffortSetting::NativeDefault);
        for setting in [
            EffortSetting::LOW,
            EffortSetting::MEDIUM,
            EffortSetting::HIGH,
            EffortSetting::XHIGH,
        ] {
            let command = initial(prompt, None, setting);
            assert_eq!(command.stdin, baseline.stdin);
            assert_ne!(command.argv, baseline.argv);
        }
    }

    #[test]
    fn explicit_effort_maps_onto_native_effort_flag_never_silently() {
        for (setting, native) in [
            (EffortSetting::LOW, "low"),
            (EffortSetting::MEDIUM, "medium"),
            (EffortSetting::HIGH, "high"),
            (EffortSetting::XHIGH, "xhigh"),
        ] {
            let command = initial("prompt", None, setting);
            let args = command
                .argv
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            assert!(
                args.windows(2).any(|pair| pair == ["--effort", native]),
                "{setting:?} must produce --effort {native}"
            );
        }
        let resume_command = resume(
            &ProviderSessionId::new("session-9").unwrap(),
            &[PermissionDenial {
                tool_name: "Write".to_owned(),
                tool_use_id: None,
                tool_input: json!({"file_path":"/tmp/result.txt"}),
            }],
            None,
            None,
            EffortSetting::HIGH,
        )
        .unwrap();
        let args = resume_command
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.windows(2).any(|pair| pair == ["--effort", "high"]));
    }

    #[test]
    fn resume_uses_same_session_and_exact_write_target() {
        let command = resume(
            &ProviderSessionId::new("session-1").unwrap(),
            &[PermissionDenial {
                tool_name: "Write".to_owned(),
                tool_use_id: None,
                tool_input: json!({"file_path":"/tmp/result.txt"}),
            }],
            None,
            None,
            EffortSetting::NativeDefault,
        )
        .unwrap();
        let args = command
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--resume", "session-1"])
        );
        assert!(args.iter().any(|arg| arg == "Edit(//tmp/result.txt)"));
        assert_eq!(
            args.iter().filter(|arg| *arg == "--allowedTools").count(),
            1
        );
        assert!(
            !args
                .iter()
                .any(|arg| arg == "--dangerously-skip-permissions")
        );
    }

    #[test]
    fn unreplayable_permission_resumes_on_operator_response_without_granting() {
        let denials = vec![PermissionDenial {
            tool_name: "Bash".to_owned(),
            tool_input: json!({"command": "yarn install 2>&1 | tail -20"}),
            tool_use_id: None,
        }];
        let session = ProviderSessionId::new("s").unwrap();
        let refused = resume(&session, &denials, None, None, EffortSetting::NativeDefault)
            .unwrap_err()
            .to_string();
        assert!(refused.contains("yarn install"), "{refused}");
        let command = resume(
            &session,
            &denials,
            Some("Skip the install; the tests will run in CI."),
            None,
            EffortSetting::NativeDefault,
        )
        .unwrap();
        assert!(
            !command.argv.iter().any(|arg| arg == "--allowedTools"),
            "nothing granted: {:?}",
            command.argv
        );
        assert_eq!(
            command.stdin,
            b"Skip the install; the tests will run in CI."
        );
        assert!(
            resume(
                &session,
                &denials,
                Some("  "),
                None,
                EffortSetting::NativeDefault
            )
            .is_err(),
            "blank response is not an answer"
        );
    }

    #[test]
    fn question_requires_response() {
        let denial = PermissionDenial {
            tool_name: "AskUserQuestion".to_owned(),
            tool_use_id: None,
            tool_input: json!({"questions":[{"question":"Which option?"}]}),
        };
        assert!(matches!(
            resume(
                &ProviderSessionId::new("session-1").unwrap(),
                std::slice::from_ref(&denial),
                None,
                None,
                EffortSetting::NativeDefault,
            ),
            Err(ClaudeProviderError::QuestionResponseRequired)
        ));
        assert!(
            resume(
                &ProviderSessionId::new("session-1").unwrap(),
                &[denial],
                Some("Option A"),
                None,
                EffortSetting::NativeDefault,
            )
            .is_ok()
        );
    }
}
