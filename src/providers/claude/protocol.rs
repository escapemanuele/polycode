use serde_json::Value;

use crate::engine::UsageDelta;

use super::ClaudeProviderError;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PermissionDenial {
    pub tool_name: String,
    pub tool_input: Value,
}

impl PermissionDenial {
    pub(crate) fn is_mutating_tool(&self) -> bool {
        matches!(self.tool_name.as_str(), "Edit" | "Write")
    }

    /// Whether denial can be treated as historical after a successful terminal result.
    ///
    /// This is deliberately stricter than `exact_rule`: an exact Bash rule is
    /// still not enough to grant execution authority. Only commands proven to
    /// be read-only diagnostics may be ignored as recovered history.
    pub(crate) fn is_recovered_diagnostic(&self) -> bool {
        match self.tool_name.as_str() {
            "Read" | "WebFetch" => true,
            "Bash" => self
                .tool_input
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(safe_diagnostic_shell),
            _ => false,
        }
    }

    pub(crate) fn requires_terminal_attention(&self) -> bool {
        !self.is_recovered_diagnostic()
    }

    /// Exact Edit/Write continuation allowed only by disposable native eval.
    pub(crate) fn is_safe_eval_edit(&self, workspace_path: &std::path::Path) -> bool {
        if !self.is_mutating_tool() {
            return false;
        }
        let Some(path) = self.tool_input.get("file_path").and_then(Value::as_str) else {
            return false;
        };
        if !safe_rule_value(path) || !std::path::Path::new(path).is_absolute() {
            return false;
        }
        let requested = std::path::Path::new(path);
        if requested
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return false;
        }
        if std::fs::symlink_metadata(requested)
            .ok()
            .is_some_and(|metadata| metadata.file_type().is_symlink())
        {
            return false;
        }
        if std::fs::metadata(requested)
            .ok()
            .is_some_and(|metadata| metadata.is_dir())
        {
            return false;
        }
        let Ok(root) = std::fs::canonicalize(workspace_path) else {
            return false;
        };
        let canonical = if requested.exists() {
            std::fs::canonicalize(requested).ok()
        } else {
            requested.parent().and_then(|parent| {
                std::fs::canonicalize(parent)
                    .ok()
                    .map(|parent| parent.join(requested.file_name().unwrap_or_default()))
            })
        };
        canonical.is_some_and(|path| path.starts_with(&root) && path != root)
    }

    pub(crate) fn exact_rule(&self) -> Result<String, ClaudeProviderError> {
        let target = match self.tool_name.as_str() {
            "Write" | "Edit" => self
                .tool_input
                .get("file_path")
                .and_then(Value::as_str)
                .filter(|path| safe_rule_value(path))
                .map(|path| format!("Edit({})", path_rule(path))),
            "Read" => self
                .tool_input
                .get("file_path")
                .and_then(Value::as_str)
                .filter(|path| safe_rule_value(path))
                .map(|path| format!("Read({})", path_rule(path))),
            "Bash" => self
                .tool_input
                .get("command")
                .and_then(Value::as_str)
                .filter(|command| safe_rule_value(command) && !compound_shell(command))
                .map(|command| format!("Bash({command})")),
            "WebFetch" => self
                .tool_input
                .get("url")
                .and_then(Value::as_str)
                .and_then(|url| url.split('/').nth(2))
                .map(|domain| format!("WebFetch(domain:{domain})")),
            _ => None,
        };
        target.ok_or_else(|| ClaudeProviderError::UnsafePermission(self.tool_name.clone()))
    }
}

fn path_rule(path: &str) -> String {
    if path.starts_with('/') {
        format!("/{path}")
    } else {
        path.to_owned()
    }
}

fn compound_shell(command: &str) -> bool {
    command.contains("&&")
        || command.contains("||")
        || command
            .chars()
            .any(|character| matches!(character, ';' | '|' | '&' | '\n' | '\r'))
}

fn safe_diagnostic_shell(command: &str) -> bool {
    if command.trim().is_empty()
        || command.contains('>')
        || command.contains('<')
        || command.contains("$(")
        || command.contains('`')
    {
        return false;
    }
    let mut segment = String::new();
    let mut segments = Vec::new();
    let bytes = command.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let separator = match bytes[index] {
            b'&' if bytes.get(index + 1) == Some(&b'&') => Some(2),
            b'|' if bytes.get(index + 1) == Some(&b'|') => Some(2),
            b';' | b'|' | b'&' | b'\n' | b'\r' => Some(1),
            _ => None,
        };
        if let Some(width) = separator {
            segments.push(std::mem::take(&mut segment));
            index += width;
        } else {
            segment.push(bytes[index] as char);
            index += 1;
        }
    }
    segments.push(segment);
    segments
        .into_iter()
        .map(|segment| segment.trim().to_owned())
        .filter(|segment| !segment.is_empty())
        .all(|segment| safe_diagnostic_command(&segment))
}

fn safe_diagnostic_command(command: &str) -> bool {
    let mut words = command.split_whitespace();
    if command.split_whitespace().any(|word| {
        matches!(
            word,
            "-exec" | "-execdir" | "-delete" | "--delete" | "--in-place"
        )
    }) {
        return false;
    }
    let Some(executable) = words.next() else {
        return false;
    };
    let executable = std::path::Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(executable);
    match executable {
        "git" => matches!(
            words.next(),
            Some("status" | "diff" | "log" | "ls-files" | "show" | "rev-parse")
        ),
        "cargo" => matches!(
            words.next(),
            Some("test" | "check" | "build" | "clippy" | "fmt" | "metadata")
        ),
        "grep" | "rg" | "find" | "cat" | "head" | "tail" | "ls" | "pwd" | "wc" | "file"
        | "sort" => true,
        _ => false,
    }
}

fn safe_rule_value(value: &str) -> bool {
    !value.is_empty()
        && !value
            .chars()
            .any(|character| matches!(character, '*' | '?' | ')' | '\n' | '\r'))
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ClaudeRecord {
    Initialized {
        session_id: String,
        model: Option<String>,
    },
    Progress(String),
    Usage(UsageDelta),
    NeedsUser {
        summary: String,
        denials: Vec<PermissionDenial>,
        question: bool,
    },
    Result {
        session_id: Option<String>,
        content: String,
        success: bool,
        error: Option<String>,
        denials: Vec<PermissionDenial>,
    },
    Ignored,
}

pub(crate) fn first_record(
    bytes: &[u8],
) -> Result<Option<(ClaudeRecord, usize)>, ClaudeProviderError> {
    let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') else {
        return Ok(None);
    };
    let line = &bytes[..newline];
    if line.iter().all(u8::is_ascii_whitespace) {
        return Ok(Some((ClaudeRecord::Ignored, newline + 1)));
    }
    let value: Value = serde_json::from_slice(line)
        .map_err(|error| ClaudeProviderError::Protocol(error.to_string()))?;
    Ok(Some((decode(&value)?, newline + 1)))
}

fn decode(value: &Value) -> Result<ClaudeRecord, ClaudeProviderError> {
    match value.get("type").and_then(Value::as_str) {
        Some("system") if value.get("subtype").and_then(Value::as_str) == Some("init") => {
            let session_id = required_string(value, "session_id")?;
            Ok(ClaudeRecord::Initialized {
                session_id,
                model: value
                    .get("model")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            })
        }
        Some("assistant") => Ok(decode_assistant(value)),
        Some("result") => Ok(decode_result(value)),
        Some(_) | None => Ok(ClaudeRecord::Ignored),
    }
}

fn decode_assistant(value: &Value) -> ClaudeRecord {
    let message = value.get("message").unwrap_or(value);
    let content = message.get("content").and_then(Value::as_array);
    if let Some(tool) = content.and_then(|items| {
        items.iter().find(|item| {
            item.get("type").and_then(Value::as_str) == Some("tool_use")
                && item.get("name").and_then(Value::as_str) == Some("AskUserQuestion")
        })
    }) {
        return ClaudeRecord::NeedsUser {
            summary: question_summary(tool.get("input").unwrap_or(&Value::Null)),
            denials: vec![PermissionDenial {
                tool_name: "AskUserQuestion".to_owned(),
                tool_input: tool.get("input").cloned().unwrap_or(Value::Null),
            }],
            question: true,
        };
    }
    if let Some(usage) = message.get("usage") {
        let input_units = usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let output_units = usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if input_units != 0 || output_units != 0 {
            return ClaudeRecord::Usage(UsageDelta {
                input_units,
                output_units,
            });
        }
    }
    let text = content
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        ClaudeRecord::Ignored
    } else {
        ClaudeRecord::Progress(text)
    }
}

fn decode_result(value: &Value) -> ClaudeRecord {
    let denials = value
        .get("permission_denials")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|denial| {
            Some(PermissionDenial {
                tool_name: denial.get("tool_name")?.as_str()?.to_owned(),
                tool_input: denial.get("tool_input").cloned().unwrap_or(Value::Null),
            })
        })
        .collect::<Vec<_>>();
    // `permission_denials` is cumulative history, not proof of an unresolved
    // request. Provider maps mutation/unsafe denials separately.
    let success = !value
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && value.get("subtype").and_then(Value::as_str) == Some("success");
    let content = value
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let error = (!success && denials.is_empty()).then(|| {
        value
            .get("error")
            .and_then(Value::as_str)
            .or_else(|| value.get("subtype").and_then(Value::as_str))
            .unwrap_or("Claude Code execution failed")
            .to_owned()
    });
    ClaudeRecord::Result {
        session_id: value
            .get("session_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        content,
        success,
        error,
        denials,
    }
}

fn required_string(value: &Value, key: &'static str) -> Result<String, ClaudeProviderError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| ClaudeProviderError::Protocol(format!("missing {key}")))
}

fn question_summary(input: &Value) -> String {
    input
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|question| question.get("question").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
        .pipe(|summary| {
            if summary.is_empty() {
                "Claude Code needs user input".to_owned()
            } else {
                summary
            }
        })
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl<T> Pipe for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_record_waits_for_newline() {
        assert_eq!(first_record(br#"{"type":"system"}"#).unwrap(), None);
    }

    #[test]
    fn init_extracts_native_identity() {
        let raw = b"{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"abc\",\"model\":\"claude-x\"}\n";
        assert_eq!(
            first_record(raw).unwrap(),
            Some((
                ClaudeRecord::Initialized {
                    session_id: "abc".to_owned(),
                    model: Some("claude-x".to_owned())
                },
                raw.len()
            ))
        );
    }

    #[test]
    fn structured_denial_stays_structured() {
        let raw = b"{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"\",\"permission_denials\":[{\"tool_name\":\"Write\",\"tool_input\":{\"file_path\":\"/tmp/a\"}}]}\n";
        let Some((
            ClaudeRecord::Result {
                success, denials, ..
            },
            _,
        )) = first_record(raw).unwrap()
        else {
            panic!()
        };
        assert!(success);
        assert_eq!(denials[0].exact_rule().unwrap(), "Edit(//tmp/a)");
    }

    #[test]
    fn recovered_diagnostic_denial_is_not_terminal_attention() {
        let denial = PermissionDenial {
            tool_name: "Bash".to_owned(),
            tool_input: serde_json::json!({"command":"cargo test && cargo clippy"}),
        };
        assert!(denial.is_recovered_diagnostic());
        assert!(!denial.requires_terminal_attention());
        assert!(denial.exact_rule().is_err());
    }

    #[test]
    fn mutating_or_ambiguous_bash_denial_stays_terminal_attention() {
        let denial = PermissionDenial {
            tool_name: "Bash".to_owned(),
            tool_input: serde_json::json!({"command":"rm -rf /tmp/output && git status"}),
        };
        assert!(!denial.is_recovered_diagnostic());
        assert!(denial.requires_terminal_attention());
        assert!(denial.exact_rule().is_err());
    }

    #[test]
    fn eval_edit_requires_canonical_path_inside_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("worktree");
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(workspace.join("src/lib.rs"), "fn main() {}\n").unwrap();
        let denial = PermissionDenial {
            tool_name: "Edit".to_owned(),
            tool_input: serde_json::json!({
                "file_path": workspace.join("src/lib.rs").to_string_lossy()
            }),
        };
        assert!(denial.is_safe_eval_edit(&workspace));
        let escape = PermissionDenial {
            tool_name: "Edit".to_owned(),
            tool_input: serde_json::json!({"file_path": "/tmp/other-repo/file"}),
        };
        assert!(!escape.is_safe_eval_edit(&workspace));
        let outside = temp.path().join("outside.txt");
        std::fs::write(&outside, "outside\n").unwrap();
        let link = workspace.join("src/link.txt");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let symlink = PermissionDenial {
            tool_name: "Write".to_owned(),
            tool_input: serde_json::json!({"file_path": link}),
        };
        assert!(!symlink.is_safe_eval_edit(&workspace));
    }

    #[test]
    fn ask_user_question_always_decodes_as_question_attention() {
        let raw = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"AskUserQuestion","input":{"questions":[{"question":"Choose?"}]}}]}}
"#;
        let Some((ClaudeRecord::NeedsUser { question, .. }, _)) = first_record(raw).unwrap() else {
            panic!()
        };
        assert!(question);
    }

    #[test]
    fn compound_bash_is_not_misrepresented_as_one_exact_rule() {
        let denial = PermissionDenial {
            tool_name: "Bash".to_owned(),
            tool_input: serde_json::json!({"command":"printf x >> file && git diff"}),
        };
        assert!(matches!(
            denial.exact_rule(),
            Err(ClaudeProviderError::UnsafePermission(_))
        ));
    }
}
