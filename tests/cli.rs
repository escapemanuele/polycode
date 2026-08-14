use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

#[test]
fn deep_command_survives_process_restart_and_status_is_read_only() {
    let fixture = Fixture::new();
    let started = fixture.polycode(&[
        "deep",
        "CLI integration task",
        "--repo",
        fixture.repo.to_str().unwrap(),
        "--provider",
        "fake",
    ]);
    assert_success(&started);
    let stdout = String::from_utf8(started.stdout).unwrap();
    assert!(stdout.contains("Workflow   deep"));
    assert!(stdout.contains("Status     completed"));
    let run_id = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Run        "))
        .expect("run ID in output");

    let listed = fixture.polycode(&["runs"]);
    assert_success(&listed);
    let listed = String::from_utf8(listed.stdout).unwrap();
    assert!(listed.contains(run_id));
    assert!(listed.contains("CLI integration task"));
    assert!(listed.contains(fixture.repo.to_str().unwrap()));

    let status_before = fixture.polycode(&["status", run_id]);
    assert_success(&status_before);
    let status_before = String::from_utf8(status_before.stdout).unwrap();
    assert!(status_before.contains("Usage      50 input units · 25 output units"));

    let resumed = fixture.polycode(&["resume", run_id]);
    assert_success(&resumed);
    let resumed = String::from_utf8(resumed.stdout).unwrap();
    assert!(!resumed.contains("provider started"));
    assert!(resumed.contains("Status     completed"));

    let status_after = fixture.polycode(&["status", run_id]);
    assert_success(&status_after);
    assert_eq!(
        status_before,
        String::from_utf8(status_after.stdout).unwrap()
    );
    assert!(git_output(&fixture.repo, &["status", "--porcelain"]).is_empty());
}

#[test]
fn fake_provider_must_be_selected_explicitly() {
    let fixture = Fixture::new();
    let output = fixture.polycode(&["fast", "task", "--repo", fixture.repo.to_str().unwrap()]);

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("use --provider fake")
    );
    assert!(!fixture.data.join("polycode.db").exists());
}

#[test]
fn current_directory_is_default_repository_and_empty_runs_is_side_effect_free() {
    let fixture = Fixture::new();
    let empty = fixture.polycode(&["runs"]);
    assert_success(&empty);
    assert_eq!(String::from_utf8(empty.stdout).unwrap(), "No runs.\n");
    assert!(!fixture.data.join("polycode.db").exists());

    let output = Command::new(env!("CARGO_BIN_EXE_polycode"))
        .args(["review", "Review from cwd", "--provider", "fake"])
        .current_dir(&fixture.repo)
        .env("POLYCODE_DATA_DIR", &fixture.data)
        .output()
        .unwrap();
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Workflow   review"));
    assert!(stdout.contains("Status     completed"));
    assert!(stdout.contains(fixture.repo.to_str().unwrap()));
}

#[test]
fn doctor_reports_real_tmux_availability_without_creating_database() {
    let fixture = Fixture::new();
    let output = fixture.polycode(&["doctor"]);
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Milestone 6 managed process infrastructure"));
    assert!(stdout.contains("tmux: available (tmux "));
    assert!(!fixture.data.join("polycode.db").exists());
}

struct Fixture {
    _temp: TempDir,
    repo: std::path::PathBuf,
    data: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let repo = temp.path().join("repo");
        let data = temp.path().join("data");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "Test"]);
        fs::write(repo.join("README.md"), "baseline\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-qm", "initial"]);
        Self {
            _temp: temp,
            repo,
            data,
        }
    }

    fn polycode(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_polycode"))
            .args(args)
            .env("POLYCODE_DATA_DIR", &self.data)
            .output()
            .unwrap()
    }
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
