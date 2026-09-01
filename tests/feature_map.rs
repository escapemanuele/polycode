//! Guards `docs/features/` against mechanical drift.
//!
//! The Feature Map promises that every path it cites exists, every command
//! and flag in its `Driving it` blocks is accepted by the binary, and every
//! key it names is one the TUI actually maps. These tests hold that promise
//! so a rename or a removed flag turns the map red instead of stale.
//! Semantic drift (a gotcha that no longer applies, a missing sub-feature)
//! is still a reading job; see `docs/features/README.md`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn feature_files() -> Vec<(PathBuf, String)> {
    let dir = repo_root().join("docs/features");
    let mut files: Vec<_> = fs::read_dir(&dir)
        .expect("docs/features exists")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .map(|path| {
            let text = fs::read_to_string(&path).expect("feature file is readable");
            (path, text)
        })
        .collect();
    files.sort();
    assert!(files.len() > 1, "the Feature Map has no feature files");
    files
}

/// Every `` `token` `` in the text.
fn backtick_tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split('`').skip(1).step_by(2)
}

/// Lines inside fenced bash code blocks.
fn bash_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with("```") {
            inside = !inside && line.trim_start_matches('`').trim() == "bash";
            continue;
        }
        if inside {
            lines.push(line);
        }
    }
    lines
}

#[test]
fn every_cited_path_exists() {
    const PREFIXES: [&str; 5] = ["src/", "tests/", "evals/", ".github/", "docs/"];
    let root = repo_root();
    let mut missing = Vec::new();
    for (file, text) in feature_files() {
        for token in backtick_tokens(&text) {
            let candidate = token.split_whitespace().next().unwrap_or("");
            if !PREFIXES.iter().any(|prefix| candidate.starts_with(prefix)) {
                continue;
            }
            let cited = candidate.trim_end_matches(':');
            if !root.join(cited).exists() {
                missing.push(format!("{}: `{cited}`", file.display()));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "paths cited by the Feature Map do not exist:\n{}",
        missing.join("\n")
    );
}

struct Invocation {
    subcommands: Vec<String>,
    flags: BTreeSet<String>,
}

/// Parses one `polycode ...` line into the subcommand path and the long
/// flags it mentions. Comments, placeholders and bracketed alternatives are
/// ignored; only what the binary must recognise survives.
fn parse_invocation(line: &str) -> Option<Invocation> {
    let line = line.split('#').next()?.trim();
    let mut tokens = line.split_whitespace();
    if tokens.next()? != "polycode" {
        return None;
    }
    let mut subcommands = Vec::new();
    let mut flags = BTreeSet::new();
    let mut reading_subcommands = true;
    for token in tokens {
        for piece in token.split(['[', ']', '|', '=']) {
            if piece.starts_with("--") {
                flags.insert(piece.to_owned());
                reading_subcommands = false;
            }
        }
        let is_word = token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if reading_subcommands && is_word && !token.starts_with('-') && subcommands.len() < 2 {
            subcommands.push(token.to_owned());
        } else {
            reading_subcommands = false;
        }
    }
    Some(Invocation { subcommands, flags })
}

fn help_text(subcommands: &[String]) -> Option<String> {
    let output = Command::new(env!("CARGO_BIN_EXE_polycode"))
        .args(subcommands)
        .arg("--help")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[test]
fn every_driving_command_and_flag_is_accepted_by_the_binary() {
    let mut failures = Vec::new();
    for (file, text) in feature_files() {
        for line in bash_lines(&text) {
            let Some(invocation) = parse_invocation(line) else {
                continue;
            };
            let mut subcommands = invocation.subcommands.clone();
            let mut help = help_text(&subcommands);
            while help.is_none() && subcommands.pop().is_some() {
                help = help_text(&subcommands);
            }
            if subcommands != invocation.subcommands {
                failures.push(format!(
                    "{}: `{line}` — `polycode {}` is not a command",
                    file.display(),
                    invocation.subcommands.join(" ")
                ));
                continue;
            }
            let help = help.expect("`polycode --help` itself must succeed");
            for flag in &invocation.flags {
                if !help.contains(flag.as_str()) {
                    failures.push(format!(
                        "{}: `{line}` — `{flag}` is not a flag of `polycode {}`",
                        file.display(),
                        subcommands.join(" ")
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "Feature Map commands the binary rejects:\n{}",
        failures.join("\n")
    );
}

#[test]
fn every_key_in_the_control_room_map_is_bound() {
    let root = repo_root();
    let doc = fs::read_to_string(root.join("docs/features/control-room.md"))
        .expect("control-room.md exists");
    let input = fs::read_to_string(root.join("src/tui/input.rs")).expect("input.rs exists");
    let mut unbound = BTreeSet::new();
    for token in backtick_tokens(&doc) {
        let mut chars = token.chars();
        let (Some(key), None) = (chars.next(), chars.next()) else {
            continue;
        };
        if !key.is_ascii_graphic() {
            continue;
        }
        if !input.contains(&format!("KeyCode::Char('{key}')")) {
            unbound.insert(key);
        }
    }
    assert!(
        unbound.is_empty(),
        "control-room.md names keys src/tui/input.rs does not bind: {unbound:?}"
    );
}

#[test]
fn feature_map_parser_reads_the_documented_shapes() {
    let inv = parse_invocation(
        r#"polycode fast "<task>" [--repo <path>] [--provider claude|codex|fake | --profile recommended] [--effort native|low|medium|high]"#,
    )
    .unwrap();
    assert_eq!(inv.subcommands, ["fast"]);
    assert_eq!(
        inv.flags.iter().collect::<Vec<_>>(),
        ["--effort", "--profile", "--provider", "--repo"]
    );

    let inv = parse_invocation("polycode eval run --provider fake   # suite defaults").unwrap();
    assert_eq!(inv.subcommands, ["eval", "run"]);
    assert_eq!(inv.flags.iter().collect::<Vec<_>>(), ["--provider"]);

    let inv =
        parse_invocation("polycode resolve <run-id> <attention-id> --response \"<x>\"").unwrap();
    assert_eq!(inv.subcommands, ["resolve"]);
    assert_eq!(inv.flags.iter().collect::<Vec<_>>(), ["--response"]);

    assert!(parse_invocation("curl -fsSL https://example | sh").is_none());
    assert!(Path::new(env!("CARGO_BIN_EXE_polycode")).exists());
}
