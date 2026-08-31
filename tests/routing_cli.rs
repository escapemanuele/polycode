#![cfg(unix)]

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use polycode::domain::{RunId, RunStatus};
use polycode::store::SqliteStore;
use tempfile::TempDir;

#[test]
fn recommended_standard_routes_native_fixtures_per_role_and_preserves_artifact_boundary() {
    let fixture = Fixture::new();
    let started = fixture.polycode_write(&[
        "standard",
        "Build mixed routing fixture",
        "--repo",
        fixture.repo.to_str().unwrap(),
        "--profile",
        "recommended",
    ]);
    assert_success(&started);
    let stdout = String::from_utf8(started.stdout).unwrap();
    assert!(stdout.contains("Status     completed"));
    assert!(stdout.contains("Profile    recommended (recommended_v2)"));
    assert!(stdout.contains("architect  claude"));
    assert!(stdout.contains("implementer  codex"));
    let run_id: RunId = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Run        "))
        .unwrap()
        .parse()
        .unwrap();

    let mut store = SqliteStore::open(fixture.data.join("polycode.db")).unwrap();
    let loaded = store.load_run(run_id).unwrap();
    assert_eq!(loaded.run.status(), RunStatus::Completed);
    assert_eq!(loaded.config_snapshot.schema_version(), 2);
    let sessions = store.list_provider_sessions(run_id).unwrap();
    assert_eq!(sessions.len(), 5);
    let by_stage = sessions
        .iter()
        .map(|session| (session.stage_id().as_str(), session.provider_id().as_str()))
        .collect::<HashMap<_, _>>();
    assert_eq!(by_stage["architecture"], "claude");
    assert_eq!(by_stage["implementation"], "codex");
    assert_eq!(by_stage["quality_review"], "claude");
    assert_eq!(by_stage["spec_review"], "codex");
    assert_eq!(by_stage["decision"], "claude");

    let implementation = fs::read_to_string(fixture.capture.join("implementation.stdin")).unwrap();
    assert!(implementation.contains("# architecture result"));
    let quality = fs::read_to_string(fixture.capture.join("quality_review.claude.stdin")).unwrap();
    assert!(quality.contains("# implementation result"));
    let spec = fs::read_to_string(fixture.capture.join("spec_review.stdin")).unwrap();
    assert!(spec.contains("# architecture result"));
    assert!(spec.contains("# implementation result"));
    let decision = fs::read_to_string(fixture.capture.join("decision.claude.stdin")).unwrap();
    assert!(decision.contains("# quality_review result"));
    assert!(decision.contains("# spec_review result"));

    let artifacts = store.list_artifacts(run_id).unwrap();
    assert_eq!(artifacts.len(), 5);
    for artifact in artifacts {
        let expected = by_stage[artifact.metadata().stage_id().as_str()];
        assert_eq!(
            artifact.metadata().provider_id().unwrap().as_str(),
            expected
        );
    }
    let events = store.load_events(run_id).unwrap();
    assert!(!events.iter().any(|event| {
        format!("{:?}", event.event.kind()).contains("router")
            || format!("{:?}", event.event.kind()).contains("recommended")
    }));
    assert_eq!(git_output(&fixture.repo, &["status", "--porcelain"]), "");

    drop(store);
    let status = fixture.polycode(&["status", &run_id.to_string()]);
    assert_success(&status);
    let status = String::from_utf8(status.stdout).unwrap();
    assert!(status.contains("architecture (completed) · role=architect · configured=claude"));
    assert!(status.contains("implementation (completed) · role=implementer · configured=codex"));
    assert!(status.contains("claude-session-architecture"));
    assert!(status.contains("codex-thread-implementation"));

    let applied = fixture.polycode(&["apply", &run_id.to_string()]);
    assert_success(&applied);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("hello.txt")).unwrap(),
        "created by fake Codex\n"
    );
}

/// Omitting both selection flags is the same request as `--profile
/// recommended`, and says so in the report rather than routing silently.
#[test]
fn omitting_the_selection_flags_starts_the_recommended_profile() {
    let fixture = Fixture::new();
    let started = fixture.polycode_write(&[
        "standard",
        "Build default routing fixture",
        "--repo",
        fixture.repo.to_str().unwrap(),
    ]);
    assert_success(&started);
    let stdout = String::from_utf8(started.stdout).unwrap();
    assert!(stdout.contains("Status     completed"), "{stdout}");
    assert!(
        stdout.contains("Profile    recommended (recommended_v2)"),
        "the resolved profile is named, not assumed: {stdout}"
    );

    // The same per-role split the explicit flag produces, not one uniform
    // provider standing in for a profile.
    let run_id: RunId = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Run        "))
        .unwrap()
        .parse()
        .unwrap();
    let store = SqliteStore::open(fixture.data.join("polycode.db")).unwrap();
    let by_stage = store
        .list_provider_sessions(run_id)
        .unwrap()
        .iter()
        .map(|session| {
            (
                session.stage_id().as_str().to_owned(),
                session.provider_id().as_str().to_owned(),
            )
        })
        .collect::<HashMap<_, _>>();
    assert_eq!(by_stage["architecture"], "claude");
    assert_eq!(by_stage["implementation"], "codex");
    assert_eq!(by_stage["spec_review"], "codex");
}

#[test]
fn provider_and_profile_flags_conflict_before_state_creation() {
    let fixture = Fixture::new();
    let output = fixture.polycode(&[
        "fast",
        "task",
        "--repo",
        fixture.repo.to_str().unwrap(),
        "--provider",
        "codex",
        "--profile",
        "recommended",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(!fixture.data.join("polycode.db").exists());
}

#[test]
fn recommended_does_not_hide_unexpected_probe_failure_with_codex_fallback() {
    let fixture = Fixture::new();
    let output = fixture.polycode_probe_failure(&[
        "fast",
        "task",
        "--repo",
        fixture.repo.to_str().unwrap(),
        "--profile",
        "recommended",
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("expected ident")
    );
    assert!(!fixture.data.join("polycode.db").exists());
}

#[test]
fn recommended_attention_restart_routes_response_to_same_claude_session() {
    let fixture = Fixture::new();
    let started = fixture.polycode_with_env(
        &[
            "standard",
            "Need one answer",
            "--repo",
            fixture.repo.to_str().unwrap(),
            "--profile",
            "recommended",
        ],
        true,
    );
    assert_success(&started);
    let stdout = String::from_utf8(started.stdout).unwrap();
    assert!(stdout.contains("Status     needs_user"));
    let run_id = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Run        "))
        .unwrap();
    let attention_id = stdout
        .lines()
        .skip_while(|line| *line != "Attention")
        .find_map(|line| line.split_once(" · ").map(|(id, _)| id))
        .unwrap();

    let resolved = fixture.polycode(&[
        "resolve",
        run_id,
        attention_id,
        "--response",
        "Fixture option A",
    ]);
    assert_success(&resolved);
    assert!(
        String::from_utf8(resolved.stdout)
            .unwrap()
            .contains("Status     completed")
    );
    let store = SqliteStore::open(fixture.data.join("polycode.db")).unwrap();
    let run_id: RunId = run_id.parse().unwrap();
    let architecture = store
        .list_provider_sessions(run_id)
        .unwrap()
        .into_iter()
        .find(|session| session.stage_id().as_str() == "architecture")
        .unwrap();
    assert_eq!(architecture.provider_id().as_str(), "claude");
    assert_eq!(
        architecture.native_session_id().unwrap().as_str(),
        "claude-session-architecture"
    );
    assert_eq!(architecture.invocation(), 2);
    assert_eq!(
        fs::read_to_string(fixture.capture.join("architecture.claude.stdin")).unwrap(),
        "Fixture option A"
    );
}

#[test]
fn persisted_recommended_route_never_falls_back_when_codex_disappears() {
    let fixture = Fixture::new();
    let failed = fixture.polycode_remove_codex_during_architecture(&[
        "standard",
        "Do not reroute implementation",
        "--repo",
        fixture.repo.to_str().unwrap(),
        "--profile",
        "recommended",
    ]);
    assert!(
        !failed.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&failed.stdout),
        String::from_utf8_lossy(&failed.stderr)
    );
    assert!(
        String::from_utf8(failed.stderr)
            .unwrap()
            .contains("configured provider unavailable for codex target")
    );
    let mut store = SqliteStore::open(fixture.data.join("polycode.db")).unwrap();
    let run_id = store.list_runs().unwrap()[0].id;
    let loaded = store.load_run(run_id).unwrap();
    assert_eq!(
        loaded.config_snapshot.payload()["routes"]["implementer"]["provider"],
        "codex"
    );
    let sessions = store.list_provider_sessions(run_id).unwrap();
    assert!(sessions.iter().any(|session| {
        session.stage_id().as_str() == "architecture" && session.provider_id().as_str() == "claude"
    }));
    assert!(
        !sessions
            .iter()
            .any(|session| { session.stage_id().as_str() == "implementation" })
    );
    drop(store);

    let resumed = fixture.polycode(&["resume", &run_id.to_string()]);
    assert!(!resumed.status.success());
    assert!(
        String::from_utf8(resumed.stderr)
            .unwrap()
            .contains("configured provider unavailable for codex target")
    );
}

#[test]
fn completed_codex_can_disappear_before_restarted_claude_decision() {
    let fixture = Fixture::new();
    let blocked = fixture.polycode_remove_completed_codex(&[
        "standard",
        "Finish decision without historical provider",
        "--repo",
        fixture.repo.to_str().unwrap(),
        "--profile",
        "recommended",
    ]);
    assert_success(&blocked);
    let stdout = String::from_utf8(blocked.stdout).unwrap();
    assert!(stdout.contains("Status     needs_user"));
    assert!(!fixture.fake_bin.join("codex").exists());
    let run_id = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Run        "))
        .unwrap();
    let attention_id = stdout
        .lines()
        .skip_while(|line| *line != "Attention")
        .find_map(|line| line.split_once(" · ").map(|(id, _)| id))
        .unwrap();

    let resolved = fixture.polycode(&[
        "resolve",
        run_id,
        attention_id,
        "--response",
        "Approve decision",
    ]);
    assert_success(&resolved);
    assert!(
        String::from_utf8(resolved.stdout)
            .unwrap()
            .contains("Status     completed")
    );
    let store = SqliteStore::open(fixture.data.join("polycode.db")).unwrap();
    let sessions = store
        .list_provider_sessions(run_id.parse().unwrap())
        .unwrap();
    assert!(sessions.iter().any(|session| {
        session.stage_id().as_str() == "implementation" && session.provider_id().as_str() == "codex"
    }));
    assert!(sessions.iter().any(|session| {
        session.stage_id().as_str() == "decision"
            && session.provider_id().as_str() == "claude"
            && session.invocation() == 2
    }));
}

struct Fixture {
    _temp: TempDir,
    repo: PathBuf,
    data: PathBuf,
    capture: PathBuf,
    fake_bin: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let repo = temp.path().join("repo");
        let data = temp.path().join("data");
        let capture = temp.path().join("capture");
        let fake_bin = temp.path().join("bin");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&fake_bin).unwrap();
        for tool in ["git", "tmux"] {
            std::os::unix::fs::symlink(find_on_path(tool).unwrap(), fake_bin.join(tool)).unwrap();
        }
        for (name, mode) in [("claude", "claude"), ("codex", "codex")] {
            let wrapper = fake_bin.join(name);
            fs::write(
                &wrapper,
                if name == "codex" {
                    format!(
                        "#!/bin/sh\nmarker=\"$POLYCODE_FAKE_CODEX_CAPTURE_DIR/remove-after-probe\"\nif [ \"$POLYCODE_FAKE_CODEX_REMOVE_AFTER_PROBE\" = 1 ] && [ -f \"$marker\" ] && [ \"$*\" = \"--version\" ]; then\n  echo 'configured Codex disappeared' >&2\n  exit 42\nfi\nif [ \"$POLYCODE_FAKE_CODEX_REMOVE_AFTER_PROBE\" = 1 ] && [ \"$*\" = \"exec resume --help\" ]; then\n  '{}' codex \"$@\"\n  code=$?\n  mkdir -p \"$POLYCODE_FAKE_CODEX_CAPTURE_DIR\"\n  touch \"$marker\"\n  exit $code\nfi\nexec '{}' codex \"$@\"\n",
                        env!("CARGO_BIN_EXE_polycode-test-agent"),
                        env!("CARGO_BIN_EXE_polycode-test-agent")
                    )
                } else {
                    format!(
                        "#!/bin/sh\nexec '{}' {mode} \"$@\"\n",
                        env!("CARGO_BIN_EXE_polycode-test-agent")
                    )
                },
            )
            .unwrap();
            let mut permissions = fs::metadata(&wrapper).unwrap().permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(&wrapper, permissions).unwrap();
        }
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "test@example.invalid"]);
        git(&repo, &["config", "user.name", "Test"]);
        fs::write(repo.join("README.md"), "baseline\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-qm", "initial"]);
        Self {
            _temp: temp,
            repo,
            data,
            capture,
            fake_bin,
        }
    }

    fn polycode(&self, args: &[&str]) -> Output {
        self.polycode_with_env(args, false)
    }

    fn polycode_write(&self, args: &[&str]) -> Output {
        let mut command = self.command(args);
        command.env("POLYCODE_FAKE_CODEX_WRITE", "1");
        command.output().unwrap()
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_polycode"));
        command
            .args(args)
            .env("PATH", &self.fake_bin)
            .env("POLYCODE_DATA_DIR", &self.data)
            .env("CODEX_HOME", self.data.join("codex-home"))
            .env("POLYCODE_FAKE_CODEX_CAPTURE_DIR", &self.capture)
            .env("POLYCODE_FAKE_CLAUDE_CAPTURE_DIR", &self.capture);
        command
    }

    fn polycode_with_env(&self, args: &[&str], question: bool) -> Output {
        let mut command = self.command(args);
        if question {
            command.env("POLYCODE_FAKE_CLAUDE_QUESTION", "1");
        }
        command.output().unwrap()
    }

    fn polycode_remove_codex_during_architecture(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_polycode"));
        command
            .args(args)
            .env("PATH", &self.fake_bin)
            .env("POLYCODE_DATA_DIR", &self.data)
            .env("CODEX_HOME", self.data.join("codex-home"))
            .env("POLYCODE_FAKE_CODEX_CAPTURE_DIR", &self.capture)
            .env("POLYCODE_FAKE_CLAUDE_CAPTURE_DIR", &self.capture)
            .env(
                "POLYCODE_FAKE_CLAUDE_REMOVE_CODEX",
                self.fake_bin.join("codex"),
            )
            .output()
            .unwrap()
    }

    fn polycode_probe_failure(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_polycode"))
            .args(args)
            .env("PATH", &self.fake_bin)
            .env("POLYCODE_DATA_DIR", &self.data)
            .env("CODEX_HOME", self.data.join("codex-home"))
            .env("POLYCODE_FAKE_CLAUDE_PROBE_FAILURE", "1")
            .output()
            .unwrap()
    }

    fn polycode_remove_completed_codex(&self, args: &[&str]) -> Output {
        let mut command = self.command(args);
        command
            .env("POLYCODE_FAKE_CLAUDE_QUESTION_STAGE", "decision")
            .env(
                "POLYCODE_FAKE_CLAUDE_REMOVE_COMPLETED_CODEX",
                self.fake_bin.join("codex"),
            )
            .output()
            .unwrap()
    }
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert_success(&output);
}

fn git_output(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert_success(&output);
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
