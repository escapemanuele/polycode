use std::ffi::OsString;
use std::io::{Read, Write};
use std::time::Duration;

fn main() -> std::io::Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let mode = arguments.next().unwrap_or_default();
    match mode.to_string_lossy().as_ref() {
        "success" => {
            std::io::stdout().write_all(b"quick-success\n")?;
        }
        "slow" => {
            let milliseconds = parse_u64(arguments.next())?;
            std::thread::sleep(Duration::from_millis(milliseconds));
            std::io::stdout().write_all(b"slow-success\n")?;
        }
        "stderr" => {
            std::io::stderr().write_all(b"separate-stderr\n")?;
        }
        "fail-42" => {
            std::io::stderr().write_all(b"expected-failure\n")?;
            std::process::exit(42);
        }
        "partial" => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(b"{\"message\":\"par")?;
            stdout.flush()?;
            std::thread::sleep(Duration::from_millis(250));
            stdout.write_all(b"tial\"}\n")?;
        }
        "wait-interrupt" => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(b"ready-for-interrupt\n")?;
            stdout.flush()?;
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        "large" => {
            let length = parse_u64(arguments.next())?;
            let mut remaining = length;
            let block = vec![b'x'; 64 * 1024];
            let mut stdout = std::io::stdout().lock();
            while remaining > 0 {
                let count = usize::try_from(remaining.min(block.len() as u64)).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid length")
                })?;
                stdout.write_all(&block[..count])?;
                remaining -= count as u64;
            }
        }
        "inspect" => {
            let cwd = std::env::current_dir()?;
            let inherited = std::env::var_os("HOME").unwrap_or_default();
            let overridden = std::env::var_os("POLYCODE_TEST_OVERRIDE").unwrap_or_default();
            let remaining: Vec<OsString> = arguments.collect();
            writeln!(
                std::io::stdout(),
                "cwd={}\ninherited={}\noverride={}\nargs={}",
                cwd.display(),
                inherited.to_string_lossy(),
                overridden.to_string_lossy(),
                remaining
                    .iter()
                    .map(|value| value.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("|")
            )?;
        }
        "stdin" => {
            let mut bytes = Vec::new();
            std::io::stdin().read_to_end(&mut bytes)?;
            std::io::stdout().write_all(&bytes)?;
        }
        "codex" => codex_fixture(&arguments.collect::<Vec<_>>())?,
        "claude" => claude_fixture(&arguments.collect::<Vec<_>>())?,
        _ => {
            std::io::stderr().write_all(b"unknown fixture mode\n")?;
            std::process::exit(64);
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "single native fixture keeps emitted Claude protocol sequence inspectable"
)]
fn claude_fixture(arguments: &[OsString]) -> std::io::Result<()> {
    let args = arguments
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    match args.as_slice() {
        [version] if version == "--version" => {
            writeln!(std::io::stdout(), "claude-code fixture-1")?;
            return Ok(());
        }
        [auth, status, json] if auth == "auth" && status == "status" && json == "--json" => {
            if std::env::var_os("POLYCODE_FAKE_CLAUDE_PROBE_FAILURE").is_some() {
                writeln!(std::io::stdout(), "not-json")?;
                return Ok(());
            }
            writeln!(
                std::io::stdout(),
                "{}",
                serde_json::json!({"loggedIn":true,"authMethod":"fixture"})
            )?;
            return Ok(());
        }
        _ => {}
    }
    let resumed_session = args
        .iter()
        .position(|argument| argument == "--resume")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let mut stdin = String::new();
    std::io::stdin().read_to_string(&mut stdin)?;
    let stage = resumed_session
        .as_deref()
        .and_then(|session| session.strip_prefix("claude-session-"))
        .map(ToOwned::to_owned)
        .or_else(|| {
            stdin
                .lines()
                .find_map(|line| line.strip_prefix("Stage: "))
                .and_then(|line| line.split_once(' ').map(|(stage, _)| stage.to_owned()))
        })
        .unwrap_or_else(|| "resumed".to_owned());
    if let Some(capture) = std::env::var_os("POLYCODE_FAKE_CLAUDE_CAPTURE_DIR") {
        let capture = std::path::PathBuf::from(capture);
        std::fs::create_dir_all(&capture)?;
        std::fs::write(
            capture.join(format!("{stage}.claude.argv")),
            args.join("\n"),
        )?;
        std::fs::write(capture.join(format!("{stage}.claude.stdin")), &stdin)?;
    }
    let session = resumed_session.unwrap_or_else(|| format!("claude-session-{stage}"));
    writeln!(
        std::io::stdout(),
        "{}",
        serde_json::json!({
            "type":"system",
            "subtype":"init",
            "session_id":session,
            "model":"claude-fixture"
        })
    )?;
    let question_stage = std::env::var("POLYCODE_FAKE_CLAUDE_QUESTION_STAGE").ok();
    if (std::env::var_os("POLYCODE_FAKE_CLAUDE_QUESTION").is_some()
        || question_stage.as_deref() == Some(stage.as_str()))
        && !args.iter().any(|argument| argument == "--resume")
    {
        writeln!(
            std::io::stdout(),
            "{}",
            serde_json::json!({
                "type":"assistant",
                "message":{"content":[{
                    "type":"tool_use",
                    "name":"AskUserQuestion",
                    "input":{"questions":[{"question":"Choose fixture option"}]}
                }]}
            })
        )?;
        return Ok(());
    }
    writeln!(
        std::io::stdout(),
        "{}",
        serde_json::json!({
            "type":"assistant",
            "message":{"content":[{"type":"text","text":"Fake Claude progress"}]}
        })
    )?;
    writeln!(
        std::io::stdout(),
        "{}",
        serde_json::json!({
            "type":"result",
            "subtype":"success",
            "is_error":false,
            "session_id":session,
            "result":format!("# {stage} result\nFake Claude completed.\n")
        })
    )?;
    if stage == "architecture"
        && let Some(path) = std::env::var_os("POLYCODE_FAKE_CLAUDE_REMOVE_CODEX")
    {
        std::fs::remove_file(path)?;
    }
    if stage == "quality_review"
        && let Some(path) = std::env::var_os("POLYCODE_FAKE_CLAUDE_REMOVE_COMPLETED_CODEX")
    {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "single fixture command keeps native CLI protocol behavior inspectable"
)]
fn codex_fixture(arguments: &[OsString]) -> std::io::Result<()> {
    let args = arguments
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    match args.as_slice() {
        [version] if version == "--version" => {
            writeln!(std::io::stdout(), "codex-cli fixture-1")?;
            return Ok(());
        }
        [login, status] if login == "login" && status == "status" => {
            if std::env::var_os("POLYCODE_FAKE_CODEX_UNAUTHENTICATED").is_some() {
                writeln!(std::io::stdout(), "Not logged in")?;
                std::process::exit(1);
            }
            writeln!(std::io::stdout(), "Logged in using ChatGPT fixture-secret")?;
            return Ok(());
        }
        [exec, help] if exec == "exec" && help == "--help" => {
            writeln!(std::io::stdout(), "--json --output-last-message")?;
            return Ok(());
        }
        [exec, resume, help] if exec == "exec" && resume == "resume" && help == "--help" => {
            writeln!(std::io::stdout(), "SESSION_ID")?;
            return Ok(());
        }
        _ => {}
    }

    if !args.iter().any(|argument| argument == "exec") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "fixture expected codex exec",
        ));
    }
    let output_index = args
        .iter()
        .position(|argument| argument == "--output-last-message")
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "fixture missing final-message path",
            )
        })?;
    let output_path = args.get(output_index + 1).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "fixture missing final-message value",
        )
    })?;
    let mut stdin = String::new();
    std::io::stdin().read_to_string(&mut stdin)?;
    let stage = stdin
        .lines()
        .find_map(|line| line.strip_prefix("Stage: "))
        .and_then(|line| line.split_once(' ').map(|(stage, _)| stage))
        .unwrap_or("resumed")
        .to_owned();
    if let Some(capture) = std::env::var_os("POLYCODE_FAKE_CODEX_CAPTURE_DIR") {
        let capture = std::path::PathBuf::from(capture);
        std::fs::create_dir_all(&capture)?;
        std::fs::write(capture.join(format!("{stage}.argv")), args.join("\n"))?;
        std::fs::write(capture.join(format!("{stage}.stdin")), &stdin)?;
    }
    if std::env::var_os("POLYCODE_FAKE_CODEX_WRITE").is_some() {
        std::fs::write("hello.txt", "created by fake Codex\n")?;
        std::fs::write("README.md", "fixture changed by fake Codex\n")?;
    }
    let fail_once = if let Some(directory) = std::env::var_os("POLYCODE_FAKE_CODEX_FAIL_ONCE_DIR") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory)?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(format!("{stage}.failed-once")))
            .is_ok()
    } else {
        false
    };
    if !fail_once {
        std::fs::write(
            output_path,
            format!("# {stage} result\nFake Codex completed.\n"),
        )?;
    }
    let thread_id = args
        .iter()
        .position(|argument| argument == "resume")
        .and_then(|index| args.get(index + 1))
        .cloned()
        .unwrap_or_else(|| {
            if fail_once {
                format!("codex-thread-{stage}-attempt-1")
            } else if std::env::var_os("POLYCODE_FAKE_CODEX_FAIL_ONCE_DIR").is_some() {
                format!("codex-thread-{stage}-attempt-2")
            } else {
                format!("codex-thread-{stage}")
            }
        });
    writeln!(
        std::io::stdout(),
        "{}",
        serde_json::json!({"type":"thread.started","thread_id":thread_id})
    )?;
    writeln!(
        std::io::stdout(),
        "{}",
        serde_json::json!({"type":"turn.started"})
    )?;
    if fail_once {
        writeln!(
            std::io::stdout(),
            "{}",
            serde_json::json!({"type":"turn.failed","error":{"message":"fixture failure"}})
        )?;
        return Ok(());
    }
    writeln!(
        std::io::stdout(),
        "{}",
        serde_json::json!({
            "type":"item.completed",
            "item":{"id":"message-1","type":"agent_message","text":"Fake Codex progress"}
        })
    )?;
    writeln!(
        std::io::stdout(),
        "{}",
        serde_json::json!({
            "type":"turn.completed",
            "usage":{"input_tokens":11,"cached_input_tokens":3,"output_tokens":7,"reasoning_output_tokens":2}
        })
    )?;
    Ok(())
}

fn parse_u64(value: Option<OsString>) -> std::io::Result<u64> {
    value
        .and_then(|value| value.into_string().ok())
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing integer"))
}
