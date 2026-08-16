use serde_json::Value;

use crate::engine::UsageDelta;

use super::CodexProviderError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CodexRecord {
    ThreadStarted { thread_id: String },
    Progress(String),
    TurnCompleted(UsageDelta),
    Failed(String),
    Ignored,
}

pub(crate) fn first_record(
    bytes: &[u8],
) -> Result<Option<(CodexRecord, usize)>, CodexProviderError> {
    let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') else {
        return Ok(None);
    };
    let line = &bytes[..newline];
    if line.iter().all(u8::is_ascii_whitespace) {
        return Ok(Some((CodexRecord::Ignored, newline + 1)));
    }
    let value: Value = serde_json::from_slice(line)
        .map_err(|error| CodexProviderError::Protocol(error.to_string()))?;
    Ok(Some((decode(&value)?, newline + 1)))
}

fn decode(value: &Value) -> Result<CodexRecord, CodexProviderError> {
    match value.get("type").and_then(Value::as_str) {
        Some("thread.started") => Ok(CodexRecord::ThreadStarted {
            thread_id: required_string(value, "thread_id")?,
        }),
        Some("item.completed") => Ok(decode_item(value.get("item").unwrap_or(&Value::Null))),
        Some("turn.completed") => Ok(CodexRecord::TurnCompleted(decode_usage(value))),
        Some("turn.failed") => Ok(CodexRecord::Failed(error_message(
            value,
            "Codex turn failed",
        ))),
        Some("error") => Ok(CodexRecord::Failed(error_message(
            value,
            "Codex execution failed",
        ))),
        Some(_) | None => Ok(CodexRecord::Ignored),
    }
}

fn decode_item(item: &Value) -> CodexRecord {
    match item.get("type").and_then(Value::as_str) {
        Some("agent_message") => item
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map_or(CodexRecord::Ignored, |text| {
                CodexRecord::Progress(text.to_owned())
            }),
        Some("command_execution") => CodexRecord::Progress("Codex completed a command".to_owned()),
        Some("file_change") => CodexRecord::Progress("Codex modified files".to_owned()),
        Some("mcp_tool_call") => {
            CodexRecord::Progress("Codex completed an MCP tool call".to_owned())
        }
        Some("web_search") => CodexRecord::Progress("Codex completed a web search".to_owned()),
        Some("plan" | "todo_list") => CodexRecord::Progress("Codex updated its plan".to_owned()),
        Some(_) | None => CodexRecord::Ignored,
    }
}

fn decode_usage(value: &Value) -> UsageDelta {
    let usage = value.get("usage").unwrap_or(&Value::Null);
    UsageDelta {
        input_units: usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_units: usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

fn required_string(value: &Value, key: &'static str) -> Result<String, CodexProviderError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| CodexProviderError::Protocol(format!("missing {key}")))
}

fn error_message(value: &Value, fallback: &str) -> String {
    value
        .get("error")
        .and_then(|error| {
            error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
        })
        .or_else(|| value.get("message").and_then(Value::as_str))
        .filter(|message| !message.trim().is_empty())
        .unwrap_or(fallback)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_record_waits_without_consumption() {
        assert_eq!(first_record(br#"{"type":"thread.star"#).unwrap(), None);
    }

    #[test]
    fn thread_started_extracts_identity() {
        let raw = b"{\"type\":\"thread.started\",\"thread_id\":\"thread-A\",\"future\":1}\n";
        assert_eq!(
            first_record(raw).unwrap(),
            Some((
                CodexRecord::ThreadStarted {
                    thread_id: "thread-A".to_owned()
                },
                raw.len()
            ))
        );
    }

    #[test]
    fn agent_message_is_progress_but_reasoning_is_not_exposed() {
        let message = b"{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"safe result\"}}\n";
        assert!(matches!(
            first_record(message).unwrap(),
            Some((CodexRecord::Progress(text), _)) if text == "safe result"
        ));
        let reasoning = b"{\"type\":\"item.completed\",\"item\":{\"type\":\"reasoning\",\"text\":\"private chain\"}}\n";
        assert!(matches!(
            first_record(reasoning).unwrap(),
            Some((CodexRecord::Ignored, _))
        ));
    }

    #[test]
    fn turn_completed_keeps_stable_usage_only() {
        let raw = b"{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":100,\"cached_input_tokens\":50,\"output_tokens\":20,\"reasoning_output_tokens\":5}}\n";
        assert_eq!(
            first_record(raw).unwrap(),
            Some((
                CodexRecord::TurnCompleted(UsageDelta {
                    input_units: 100,
                    output_units: 20
                }),
                raw.len()
            ))
        );
    }

    #[test]
    fn unknown_is_checkpoint_and_invalid_complete_line_fails() {
        let unknown = b"{\"type\":\"future.codex.event\",\"something\":123}\n";
        assert!(matches!(
            first_record(unknown).unwrap(),
            Some((CodexRecord::Ignored, _))
        ));
        assert!(matches!(
            first_record(b"{\"type\":\"broken\"\n"),
            Err(CodexProviderError::Protocol(_))
        ));
    }

    #[test]
    fn failures_are_typed_without_strict_extra_fields() {
        let raw =
            b"{\"type\":\"turn.failed\",\"error\":{\"message\":\"sandbox denied\",\"code\":42}}\n";
        assert!(matches!(
            first_record(raw).unwrap(),
            Some((CodexRecord::Failed(message), _)) if message == "sandbox denied"
        ));
    }
}
