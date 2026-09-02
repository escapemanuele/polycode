use std::collections::BTreeSet;
use std::ffi::OsString;

use crate::domain::{EffortLevel, EffortSetting, ModelId, ProviderSessionId};
use crate::image::{ImageToolServerCommand, MCP_SERVER_NAME, TOOL_NAME};

use super::{ClaudeProviderError, PermissionDenial};

/// The exact permission rule for the image tool under Claude Code's
/// `mcp__<server>__<tool>` naming.
pub(crate) fn image_tool_rule() -> String {
    format!("mcp__{MCP_SERVER_NAME}__{TOOL_NAME}")
}

/// The `--mcp-config` payload: one stdio server, the Polycode executable as
/// shim, the run's socket path. No environment block, so nothing here can
/// carry a credential.
pub(crate) fn mcp_config_json(image: &ImageToolServerCommand) -> String {
    serde_json::json!({
        "mcpServers": {
            MCP_SERVER_NAME: {
                "command": image.executable.to_string_lossy(),
                "args": image.args.iter().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>(),
            }
        }
    })
    .to_string()
}

#[derive(Debug)]
pub(crate) struct ClaudeCommand {
    pub argv: Vec<OsString>,
    pub stdin: Vec<u8>,
}

pub(crate) fn initial(
    prompt: &str,
    model: Option<&ModelId>,
    effort: EffortSetting,
    image: Option<&ImageToolServerCommand>,
) -> ClaudeCommand {
    let mut argv = base(model, effort, image);
    if image.is_some() {
        argv.push(OsString::from("--allowedTools"));
        argv.push(OsString::from(image_tool_rule()));
    }
    ClaudeCommand {
        argv,
        stdin: prompt.as_bytes().to_vec(),
    }
}

pub(crate) fn resume(
    session_id: &ProviderSessionId,
    denials: &[PermissionDenial],
    response: Option<&str>,
    model: Option<&ModelId>,
    effort: EffortSetting,
    image: Option<&ImageToolServerCommand>,
) -> Result<ClaudeCommand, ClaudeProviderError> {
    let mut rules = grant_rules(denials, response)?;
    // The image grant rides the same flag as approved denials: still one
    // exact rule, still only when the stage holds the grant.
    if image.is_some() {
        rules.insert(image_tool_rule());
    }
    let mut argv = base(model, effort, image);
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

fn base(
    model: Option<&ModelId>,
    effort: EffortSetting,
    image: Option<&ImageToolServerCommand>,
) -> Vec<OsString> {
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
    // Run-scoped MCP server: added to whatever the user configured, never
    // replacing it (`--strict-mcp-config` stays off), never written to
    // ~/.claude or the project.
    if let Some(image) = image {
        argv.push(OsString::from("--mcp-config"));
        argv.push(OsString::from(mcp_config_json(image)));
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
        let command = initial("prompt", None, EffortSetting::NativeDefault, None);
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
        let baseline = initial(prompt, None, EffortSetting::NativeDefault, None);
        for setting in [
            EffortSetting::LOW,
            EffortSetting::MEDIUM,
            EffortSetting::HIGH,
            EffortSetting::XHIGH,
        ] {
            let command = initial(prompt, None, setting, None);
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
            let command = initial("prompt", None, setting, None);
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
            None,
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
            None,
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
        let refused = resume(
            &session,
            &denials,
            None,
            None,
            EffortSetting::NativeDefault,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(refused.contains("yarn install"), "{refused}");
        let command = resume(
            &session,
            &denials,
            Some("Skip the install; the tests will run in CI."),
            None,
            EffortSetting::NativeDefault,
            None,
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
                EffortSetting::NativeDefault,
                None
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
                None,
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
                None,
            )
            .is_ok()
        );
    }

    fn image_command() -> ImageToolServerCommand {
        ImageToolServerCommand {
            executable: std::path::PathBuf::from("/opt/polycode/bin/polycode"),
            args: vec![
                OsString::from("__image-tool"),
                OsString::from("--socket"),
                OsString::from("/tmp/pcimg-run.sock"),
            ],
        }
    }

    #[test]
    fn image_grant_adds_run_scoped_mcp_server_and_exact_allow_rule_only() {
        let baseline = initial("prompt", None, EffortSetting::NativeDefault, None);
        let granted = initial(
            "prompt",
            None,
            EffortSetting::NativeDefault,
            Some(&image_command()),
        );
        assert_eq!(
            granted.stdin, baseline.stdin,
            "the grant never edits the prompt bytes here"
        );
        let args = granted
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let config_index = args.iter().position(|arg| arg == "--mcp-config").unwrap();
        let config: serde_json::Value = serde_json::from_str(&args[config_index + 1]).unwrap();
        assert_eq!(
            config,
            json!({"mcpServers": {"polycode_image": {
                "command": "/opt/polycode/bin/polycode",
                "args": ["__image-tool", "--socket", "/tmp/pcimg-run.sock"],
            }}})
        );
        assert!(
            config["mcpServers"]["polycode_image"].get("env").is_none(),
            "the MCP config must carry no environment block"
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--allowedTools", "mcp__polycode_image__image_generate"])
        );
        assert!(!args.iter().any(|arg| arg == "--strict-mcp-config"));
        assert!(
            !args
                .iter()
                .any(|arg| arg == "--dangerously-skip-permissions")
        );
        // Baseline argv is a strict prefix: nothing else moved.
        assert_eq!(&granted.argv[..baseline.argv.len()], &baseline.argv[..]);

        let resumed = resume(
            &ProviderSessionId::new("session-1").unwrap(),
            &[PermissionDenial {
                tool_name: "Write".to_owned(),
                tool_use_id: None,
                tool_input: json!({"file_path":"/tmp/result.txt"}),
            }],
            None,
            None,
            EffortSetting::NativeDefault,
            Some(&image_command()),
        )
        .unwrap();
        let args = resumed
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args.iter().filter(|arg| *arg == "--allowedTools").count(),
            1,
            "one flag carries both the approved rule and the image rule"
        );
        assert!(
            args.iter()
                .any(|arg| arg == "mcp__polycode_image__image_generate")
        );
        assert!(args.iter().any(|arg| arg == "--mcp-config"));
    }

    #[test]
    fn image_grant_carries_no_credential_anywhere_in_the_invocation() {
        // The invocation is built from the grant alone: the builder takes
        // no credential and reads no environment, so the only way a key could
        // appear is through the grant, which carries an executable path and
        // a socket path and nothing else. `env` keys and `OPENAI` anywhere
        // in argv are the two shapes a leak would take.
        let command = initial("prompt", None, EffortSetting::HIGH, Some(&image_command()));
        let joined = command
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!joined.contains("OPENAI"), "{joined}");
        assert!(!joined.contains("\"env\""), "{joined}");
        assert!(!joined.contains("sk-proj"), "{joined}");
        assert_eq!(command.stdin, b"prompt");
    }
}
