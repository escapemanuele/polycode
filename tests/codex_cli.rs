#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

use polycode::domain::{ArtifactKind, Role, RunId, RunStatus};
use polycode::store::SqliteStore;

#[test]
fn native_codex_fixture_runs_through_tmux_preserves_source_then_applies() {
    let fixture = Fixture::new();
    let marker = "SUPER_SECRET_TASK_MARKER";
    let started = fixture.polycode(
        &[
            "fast",
            marker,
            "--repo",
            fixture.repo.to_str().unwrap(),
            "--provider",
            "codex",
        ],
        true,
        false,
    );
    assert_success(&started);
    let stdout = String::from_utf8(started.stdout).unwrap();
    assert!(stdout.contains("Status     completed"));
    assert!(stdout.contains("implementer  codex"));
    assert!(stdout.contains("native=codex-thread-implementation"));
    assert!(stdout.contains("Usage      11 input units · 7 output units"));
    let run_id = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Run        "))
        .unwrap();

    assert!(!fixture.repo.join("hello.txt").exists());
    assert_eq!(
        fs::read_to_string(fixture.repo.join("README.md")).unwrap(),
        "baseline\n"
    );
    let argv = fs::read_to_string(fixture.capture.join("implementation.argv")).unwrap();
    let stdin = fs::read_to_string(fixture.capture.join("implementation.stdin")).unwrap();
    assert!(!argv.contains(marker));
    assert!(stdin.contains(marker));
    assert!(argv.contains("--sandbox\nworkspace-write"));
    assert!(argv.contains("--ask-for-approval\nnever"));
    for forbidden in [
        "--yolo",
        "danger-full-access",
        "--ephemeral",
        "--skip-git-repo-check",
        "--ignore-user-config",
        "--ignore-rules",
        "--dangerously-bypass-hook-trust",
    ] {
        assert!(!argv.contains(forbidden));
    }
    let artifact = fixture
        .data
        .join("runs")
        .join(run_id)
        .join("artifacts")
        .join("implementation.md");
    assert!(
        fs::read_to_string(artifact)
            .unwrap()
            .contains("Fake Codex completed")
    );

    let status = fixture.polycode(&["status", run_id], false, false);
    assert_success(&status);
    let status = String::from_utf8(status.stdout).unwrap();
    assert!(status.contains("configured=codex/native default"));
    assert!(status.contains("conversation=completed"));
    assert!(!status.contains("fixture-secret"));

    let applied = fixture.polycode(&["apply", run_id], false, false);
    assert_success(&applied);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("hello.txt")).unwrap(),
        "created by fake Codex\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.repo.join("README.md")).unwrap(),
        "fixture changed by fake Codex\n"
    );
}

#[test]
fn doctor_reports_codex_health_without_leaking_auth_output_or_creating_db() {
    let fixture = Fixture::new();
    let healthy = fixture.polycode(&["doctor"], false, false);
    assert_success(&healthy);
    let stdout = String::from_utf8(healthy.stdout).unwrap();
    assert!(stdout.contains("Milestone 9 role routing + recommended_v1"));
    assert!(stdout.contains("Codex CLI: available (codex-cli fixture-1)"));
    assert!(stdout.contains("Codex auth: ready (ChatGPT)"));
    assert!(!stdout.contains("fixture-secret"));
    assert!(!fixture.data.join("polycode.db").exists());

    let logged_out = fixture.polycode(&["doctor"], false, true);
    assert_success(&logged_out);
    assert!(
        String::from_utf8(logged_out.stdout)
            .unwrap()
            .contains("Codex auth: not authenticated")
    );
    let rejected = fixture.polycode(
        &[
            "fast",
            "task",
            "--repo",
            fixture.repo.to_str().unwrap(),
            "--provider",
            "codex",
        ],
        false,
        true,
    );
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8(rejected.stderr)
            .unwrap()
            .contains("not authenticated")
    );
    assert!(!fixture.data.join("polycode.db").exists());
}

#[test]
fn standard_run_uses_separate_threads_direct_artifacts_and_stage_sandboxes() {
    let fixture = Fixture::new();
    let started = fixture.polycode(
        &[
            "standard",
            "Analyze then implement fixture task",
            "--repo",
            fixture.repo.to_str().unwrap(),
            "--provider",
            "codex",
        ],
        false,
        false,
    );
    assert_success(&started);
    let stdout = String::from_utf8(started.stdout).unwrap();
    assert!(stdout.contains("Status     completed"));
    let run_id: RunId = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Run        "))
        .unwrap()
        .parse()
        .unwrap();

    let mut store = SqliteStore::open(fixture.data.join("polycode.db")).unwrap();
    assert_eq!(
        store.load_run(run_id).unwrap().run.status(),
        RunStatus::Completed
    );
    let sessions = store.list_provider_sessions(run_id).unwrap();
    assert_eq!(sessions.len(), 5);
    let native = sessions
        .iter()
        .map(|session| session.native_session_id().unwrap().as_str().to_owned())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(native.len(), 5);
    assert!(native.contains("codex-thread-architecture"));
    assert!(native.contains("codex-thread-implementation"));
    assert!(native.contains("codex-thread-quality_review"));
    assert!(native.contains("codex-thread-spec_review"));
    assert!(native.contains("codex-thread-decision"));

    let implementation = fs::read_to_string(fixture.capture.join("implementation.stdin")).unwrap();
    assert!(implementation.contains("# architecture result"));
    let quality = fs::read_to_string(fixture.capture.join("quality_review.stdin")).unwrap();
    assert!(quality.contains("# implementation result"));
    assert!(!quality.contains("# architecture result"));
    assert!(quality.contains("Judge HOW the implementation is engineered"));
    assert!(quality.contains("Do not repeat a complete requirement"));

    let spec = fs::read_to_string(fixture.capture.join("spec_review.stdin")).unwrap();
    assert!(spec.contains("# architecture result"));
    assert!(spec.contains("# implementation result"));
    assert!(spec.contains("Judge WHAT behavior was delivered"));
    assert!(spec.contains("Missing, Wrong, or Unrequested"));

    let decision = fs::read_to_string(fixture.capture.join("decision.stdin")).unwrap();
    assert!(decision.contains("# quality_review result"));
    assert!(decision.contains("# spec_review result"));
    assert!(!decision.contains("# architecture result"));
    assert!(!decision.contains("# implementation result"));
    assert!(decision.contains("implementation quality and specification compliance"));
    assert!(
        fs::read_to_string(fixture.capture.join("implementation.argv"))
            .unwrap()
            .contains("--sandbox\nworkspace-write")
    );
    for stage in ["architecture", "quality_review", "spec_review", "decision"] {
        assert!(
            fs::read_to_string(fixture.capture.join(format!("{stage}.argv")))
                .unwrap()
                .contains("--sandbox\nread-only")
        );
    }
    let artifact_root = fixture
        .data
        .join("runs")
        .join(run_id.to_string())
        .join("artifacts");
    assert!(artifact_root.join("quality_review.md").is_file());
    assert!(artifact_root.join("spec_review.md").is_file());
    let artifacts = store.list_artifacts(run_id).unwrap();
    let quality_artifact = artifacts
        .iter()
        .find(|artifact| artifact.metadata().stage_id().as_str() == "quality_review")
        .unwrap();
    assert_eq!(
        quality_artifact.metadata().kind(),
        ArtifactKind::CodeQualityReview
    );
    assert_eq!(
        quality_artifact.metadata().role(),
        Role::CodeQualityReviewer
    );
    let spec_artifact = artifacts
        .iter()
        .find(|artifact| artifact.metadata().stage_id().as_str() == "spec_review")
        .unwrap();
    assert_eq!(spec_artifact.metadata().kind(), ArtifactKind::SpecReview);
    assert_eq!(spec_artifact.metadata().role(), Role::SpecReviewer);
    assert_eq!(git_output(&fixture.repo, &["status", "--porcelain"]), "");
}

#[test]
fn retry_creates_new_provider_session_and_new_native_thread() {
    let fixture = Fixture::new();
    let failed = fixture.polycode_with_fail_once(&[
        "fast",
        "retry fixture task",
        "--repo",
        fixture.repo.to_str().unwrap(),
        "--provider",
        "codex",
    ]);
    assert_success(&failed);
    let failed = String::from_utf8(failed.stdout).unwrap();
    assert!(failed.contains("Status     failed"));
    let run_id = failed
        .lines()
        .find_map(|line| line.strip_prefix("Run        "))
        .unwrap();

    let retried = fixture.polycode_with_fail_once(&["retry", run_id, "implementation"]);
    assert_success(&retried);
    assert!(
        String::from_utf8(retried.stdout)
            .unwrap()
            .contains("Status     completed")
    );
    let run_id: RunId = run_id.parse().unwrap();
    let store = SqliteStore::open(fixture.data.join("polycode.db")).unwrap();
    let sessions = store.list_provider_sessions(run_id).unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].attempt(), 1);
    assert_eq!(sessions[1].attempt(), 2);
    assert_eq!(
        sessions[0].native_session_id().unwrap().as_str(),
        "codex-thread-implementation-attempt-1"
    );
    assert_eq!(
        sessions[1].native_session_id().unwrap().as_str(),
        "codex-thread-implementation-attempt-2"
    );
    let second_process = store
        .load_managed_process(sessions[1].current_process_id().unwrap())
        .unwrap();
    assert!(
        !second_process
            .spec()
            .argv()
            .iter()
            .any(|arg| arg == "resume")
    );
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
        let repo = temp.path().join("repository with spaces");
        let data = temp.path().join("data");
        let capture = temp.path().join("capture");
        let fake_bin = temp.path().join("fake-bin");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&fake_bin).unwrap();
        for tool in ["git", "tmux"] {
            let executable = find_on_path(tool).unwrap_or_else(|| panic!("{tool} is required"));
            std::os::unix::fs::symlink(executable, fake_bin.join(tool)).unwrap();
        }
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "test@example.invalid"]);
        git(&repo, &["config", "user.name", "Test"]);
        fs::write(repo.join("README.md"), "baseline\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-qm", "initial"]);
        let wrapper = fake_bin.join("codex");
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexec '{}' codex \"$@\"\n",
                env!("CARGO_BIN_EXE_polycode-test-agent")
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&wrapper).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&wrapper, permissions).unwrap();
        Self {
            _temp: temp,
            repo,
            data,
            capture,
            fake_bin,
        }
    }

    fn polycode(&self, args: &[&str], write: bool, logged_out: bool) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_polycode"));
        command
            .args(args)
            .env("PATH", &self.fake_bin)
            .env("POLYCODE_DATA_DIR", &self.data)
            .env("POLYCODE_FAKE_CODEX_CAPTURE_DIR", &self.capture);
        if write {
            command.env("POLYCODE_FAKE_CODEX_WRITE", "1");
        }
        if logged_out {
            command.env("POLYCODE_FAKE_CODEX_UNAUTHENTICATED", "1");
        }
        command.output().unwrap()
    }

    fn polycode_with_fail_once(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_polycode"))
            .args(args)
            .env("PATH", &self.fake_bin)
            .env("POLYCODE_DATA_DIR", &self.data)
            .env("POLYCODE_FAKE_CODEX_CAPTURE_DIR", &self.capture)
            .env(
                "POLYCODE_FAKE_CODEX_FAIL_ONCE_DIR",
                self.capture.join("fail-once"),
            )
            .output()
            .unwrap()
    }
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
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

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
