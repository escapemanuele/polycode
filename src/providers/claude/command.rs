use std::collections::BTreeSet;
use std::ffi::OsString;

use crate::domain::{ModelId, ProviderSessionId};

use super::{ClaudeProviderError, PermissionDenial};

pub(crate) struct ClaudeCommand {
    pub argv: Vec<OsString>,
    pub stdin: Vec<u8>,
}

pub(crate) fn initial(prompt: &str, model: Option<&ModelId>) -> ClaudeCommand {
    ClaudeCommand {
        argv: base(model),
        stdin: prompt.as_bytes().to_vec(),
    }
}

pub(crate) fn resume(
    session_id: &ProviderSessionId,
    denials: &[PermissionDenial],
    response: Option<&str>,
    model: Option<&ModelId>,
) -> Result<ClaudeCommand, ClaudeProviderError> {
    if denials
        .iter()
        .any(|denial| denial.tool_name == "AskUserQuestion")
        && response.is_none_or(|response| response.trim().is_empty())
    {
        return Err(ClaudeProviderError::QuestionResponseRequired);
    }
    let mut argv = base(model);
    argv.push(OsString::from("--resume"));
    argv.push(OsString::from(session_id.as_str()));
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
    if rules.is_empty()
        && !denials
            .iter()
            .any(|denial| denial.tool_name == "AskUserQuestion")
    {
        return Err(unsafe_permission.unwrap_or_else(|| {
            ClaudeProviderError::UnsafePermission("empty denial set".to_owned())
        }));
    }
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

fn base(model: Option<&ModelId>) -> Vec<OsString> {
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
    argv
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

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
            ),
            Err(ClaudeProviderError::QuestionResponseRequired)
        ));
        assert!(
            resume(
                &ProviderSessionId::new("session-1").unwrap(),
                &[denial],
                Some("Option A"),
                None,
            )
            .is_ok()
        );
    }
}
