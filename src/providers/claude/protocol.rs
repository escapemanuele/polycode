use serde_json::Value;

use crate::engine::UsageDelta;

use super::ClaudeProviderError;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PermissionDenial {
    pub tool_name: String,
    pub tool_input: Value,
}

impl PermissionDenial {
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
    let success = !value
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && value.get("subtype").and_then(Value::as_str) == Some("success")
        && denials.is_empty();
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
        assert!(!success);
        assert_eq!(denials[0].exact_rule().unwrap(), "Edit(//tmp/a)");
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
