//! The deterministic `verify` provider.
//!
//! Every other provider is a coding agent. This one runs the repository's
//! own verification commands inside the run's worktree, records every
//! command and exit code in a Markdown artifact, and completes the stage
//! only when every exit code is zero. No prompt is built and no model is
//! consulted; the verdict is the exit codes and nothing else.
//!
//! It is synchronous by design. One poll runs the whole sequence and
//! returns the terminal signal, which means a long test suite holds the
//! worker for its duration — the same thread that would otherwise be
//! polling a native CLI. The trade is accepted: a verification that could be
//! interrupted halfway would need its own process supervision, and the
//! commands it runs are the repository's, already written to be run to
//! completion.

mod artifact;
mod config;
mod runner;

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::domain::{ProviderId, Role, StageStatus};
use crate::engine::{Provider, ProviderError, ProviderPoll, ProviderRequest, ProviderSignal};
use crate::store::{SqliteStore, StoreError, process_root};

use artifact::Verdict;
use runner::CommandReport;

pub use config::{CONFIG_FILE, DEFAULT_TIMEOUT};

/// Runs a workspace's verification commands and records the outcome.
pub struct VerifyProvider {
    id: ProviderId,
    /// Where artifacts are written: `<root>/<run-id>/artifacts/`, the same
    /// tree the native adapters use, so the control room finds them without
    /// knowing which provider wrote them.
    artifact_root: PathBuf,
}

impl VerifyProvider {
    /// A provider writing artifacts under the configured data directory.
    ///
    /// # Errors
    /// Returns the data-directory resolution failure.
    pub fn from_environment() -> Result<Self, VerifyError> {
        Ok(Self::new(process_root()?))
    }

    /// A provider writing artifacts under an explicit root; evaluations and
    /// tests use this to keep their artifacts out of the user's data.
    ///
    /// # Panics
    /// Only if the static provider identifier were ever invalid, which the
    /// routing tests pin.
    #[must_use]
    pub fn new(artifact_root: PathBuf) -> Self {
        Self {
            id: ProviderId::new(crate::app::VERIFY_PROVIDER_ID)
                .expect("static provider ID must be valid"),
            artifact_root,
        }
    }

    fn now() -> DateTime<Utc> {
        std::time::SystemTime::now().into()
    }

    /// Runs the whole pass for one request and records its artifact.
    fn verify(
        &self,
        store: &mut SqliteStore,
        request: &ProviderRequest,
    ) -> Result<Verdict, VerifyError> {
        // Loaded once, before the commands run: the row carries both the
        // repository the worktree was cut from, which can hold the
        // configuration the worktree does not, and the base commit the
        // artifact is stamped with.
        let workspace = store.load_workspace(request.run_id())?;
        let source_repo = workspace
            .as_ref()
            .map(|workspace| workspace.source_repo_path().to_owned());
        let (plan, reports, verdict) =
            match config::plan_for(request.workspace_path(), source_repo.as_deref()) {
                Ok(plan) => {
                    let reports = run_until_first_failure(&plan, request.workspace_path())?;
                    let verdict = artifact::verdict(&plan, &reports);
                    (Some(plan), reports, verdict)
                }
                // A configuration the stage cannot read is a finding about the
                // repository, so it is reported the way a failing command is:
                // in the artifact and as the stage's failure reason.
                Err(VerifyError::Config(message)) => (None, Vec::new(), Verdict::Failed(message)),
                Err(error) => return Err(error),
            };
        let content = artifact::render(plan.as_ref(), &reports, &verdict);
        let base_commit = workspace.map(|workspace| workspace.base_commit().to_owned());
        let record = artifact::persist(
            &self.artifact_root,
            request,
            &self.id,
            base_commit.as_deref(),
            &content,
            verdict.artifact_status(),
            Self::now(),
        )?;
        store.insert_artifact(&record)?;
        Ok(verdict)
    }

    /// The verdict already recorded for this exact attempt, if a previous
    /// poll wrote its artifact but the process died before the terminal
    /// signal was committed. Reporting it again is correct; running the
    /// commands again would write a second artifact the store refuses.
    ///
    /// Two places to look, because the write is not atomic with the row:
    /// the store row, and — when the crash landed between the file and
    /// the row — the deterministic artifact path itself. In the second
    /// case the row is inserted here for the existing file, so the stage
    /// cannot wedge on an `ArtifactConflict` between two runs of the same
    /// commands that produced different bytes.
    fn recorded_verdict(
        &self,
        store: &mut SqliteStore,
        request: &ProviderRequest,
    ) -> Result<Option<String>, VerifyError> {
        let existing = store
            .list_artifacts(request.run_id())?
            .into_iter()
            .find(|artifact| {
                artifact.metadata().stage_id() == request.stage_id()
                    && artifact.attempt() == request.attempt()
            });
        if let Some(existing) = existing {
            let content = std::fs::read_to_string(existing.path())?;
            return Ok(artifact::bottom_line_of(&content));
        }
        let path = artifact::artifact_path(&self.artifact_root, request);
        if !path.is_file() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path)?;
        let Some(bottom_line) = artifact::bottom_line_of(&String::from_utf8_lossy(&bytes)) else {
            // Not an artifact this module wrote; leave it to `write_once`
            // to refuse and surface the conflict.
            return Ok(None);
        };
        let base_commit = store
            .load_workspace(request.run_id())?
            .map(|workspace| workspace.base_commit().to_owned());
        let record = artifact::describe(
            path,
            &bytes,
            request,
            &self.id,
            base_commit.as_deref(),
            artifact::status_of_bottom_line(&bottom_line),
            Self::now(),
        )?;
        store.insert_artifact(&record)?;
        Ok(Some(bottom_line))
    }
}

/// Runs the plan's commands in order and stops at the first that does not
/// exit zero. Later commands are not run: their result would say nothing
/// about the change that is not already said by the failure, and a
/// formatting failure should not cost a full test suite.
fn run_until_first_failure(
    plan: &config::VerifyPlan,
    worktree: &Path,
) -> Result<Vec<CommandReport>, VerifyError> {
    let mut reports = Vec::with_capacity(plan.commands.len());
    for command in &plan.commands {
        let report = runner::run(command, worktree, plan.timeout)?;
        let failed = !report.exit.succeeded();
        reports.push(report);
        if failed {
            break;
        }
    }
    Ok(reports)
}

impl Provider for VerifyProvider {
    fn provider_id_for(&self, _request: &ProviderRequest) -> Result<ProviderId, ProviderError> {
        Ok(self.id.clone())
    }

    fn supports_role(&self, role: Role) -> bool {
        role == Role::Verifier
    }

    /// Two polls per attempt, driven by the durable signal cursor like the
    /// Fake provider: the first starts the stage, the second runs every
    /// command and ends it. Nothing is held between the two, so a process
    /// that dies in between simply runs the pass on the next poll.
    fn poll(
        &mut self,
        store: &mut SqliteStore,
        request: &ProviderRequest,
    ) -> Result<ProviderPoll, ProviderError> {
        if request.observe_only() {
            // Nothing runs across polls, so there is never anything to
            // observe; and observation must not start the commands.
            return Ok(ProviderPoll::Pending);
        }
        match (request.signal_index(), request.stage_status()) {
            (0, StageStatus::Ready) => Ok(ProviderPoll::Signal(ProviderSignal::Started {
                model_id: None,
                session_id: None,
            })),
            (1, StageStatus::Running) => {
                if let Some(bottom_line) = self.recorded_verdict(store, request)? {
                    return Ok(ProviderPoll::Signal(signal_for(&bottom_line)));
                }
                let verdict = self.verify(store, request)?;
                Ok(ProviderPoll::Signal(match verdict {
                    Verdict::Passed { .. } | Verdict::NothingChecked => ProviderSignal::Completed,
                    Verdict::Failed(_) => ProviderSignal::Failed(verdict.bottom_line()),
                }))
            }
            (index, status) => Err(ProviderError::new(format!(
                "verify stage {} has no signal at cursor {index} while {status:?}",
                request.stage_id()
            ))),
        }
    }
}

/// The terminal signal a recorded bottom line stands for.
fn signal_for(bottom_line: &str) -> ProviderSignal {
    if bottom_line.starts_with("passed") || bottom_line.starts_with("nothing checked") {
        ProviderSignal::Completed
    } else {
        ProviderSignal::Failed(bottom_line.to_owned())
    }
}

#[derive(Debug, Error)]
pub enum VerifyError {
    #[error("verification filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The repository's own configuration could not be used; reported as a
    /// failed stage, never as an infrastructure error.
    #[error("{0}")]
    Config(String),
    #[error("verification artifact exceeds {0} bytes")]
    ArtifactTooLarge(usize),
    #[error("verification artifact already exists with different content: {0}")]
    ArtifactConflict(PathBuf),
    #[error("verification artifact record is invalid: {0}")]
    Artifact(String),
}

impl From<VerifyError> for ProviderError {
    fn from(error: VerifyError) -> Self {
        Self::new(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::{DateTime, Utc};
    use serde_json::json;

    use super::*;
    use crate::domain::{
        ConfigSnapshotId, EventId, EventMetadata, Run, RunId, RunTransition, StageId, StageKind,
        WorkflowDefinition, WorkflowKind,
    };
    use crate::store::ResolvedConfigSnapshot;
    use crate::workspace::{RunWorkspace, WorkspaceMode};

    fn run_id() -> RunId {
        RunId::from_u128(7)
    }

    struct Harness {
        temp: tempfile::TempDir,
        worktree: PathBuf,
        provider: VerifyProvider,
        store: SqliteStore,
    }

    impl Harness {
        /// A store holding one run with a verify stage — artifacts belong
        /// to a run — but no workspace, so the provider is exercised on the
        /// path where no base commit is known.
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let worktree = temp.path().join("worktree");
            std::fs::create_dir(&worktree).unwrap();
            let created_at: DateTime<Utc> = std::time::SystemTime::now().into();
            let config_id = ConfigSnapshotId::new("verify-test").unwrap();
            let run = Run::new(
                run_id(),
                WorkflowDefinition::built_in(WorkflowKind::Fast),
                config_id.clone(),
                created_at,
            );
            let config = ResolvedConfigSnapshot::new(
                config_id,
                2,
                json!({"schema_version":2,"profile":"uniform","profile_version":"uniform_v1","routes":{},"providers":{}}),
                created_at,
            )
            .unwrap();
            let created = run.created_event(EventMetadata::new(EventId::new(), created_at));
            let mut store = SqliteStore::open_in_memory().unwrap();
            store.create_run(&run, &config, &[created]).unwrap();
            Self {
                provider: VerifyProvider::new(temp.path().join("runs")),
                worktree,
                store,
                temp,
            }
        }

        fn config(&self, text: &str) {
            std::fs::write(self.worktree.join(CONFIG_FILE), text).unwrap();
        }

        /// Registers a workspace row whose source repository holds `text`,
        /// so what the test exercises is the provider's own lookup — the
        /// stored row reaching the config reader — and not just the reader.
        fn source_repo_config(&mut self, text: &str) {
            let source = self.temp.path().join("source-repo");
            std::fs::create_dir_all(&source).unwrap();
            std::fs::write(source.join(CONFIG_FILE), text).unwrap();
            let now: DateTime<Utc> = std::time::SystemTime::now().into();
            let workspace = RunWorkspace::preparing(
                run_id(),
                source.clone(),
                source.join(".git"),
                "0".repeat(40),
                self.worktree.clone(),
                None,
                WorkspaceMode::Detached,
                now,
            )
            .unwrap();
            let loaded = self.store.load_run(run_id()).unwrap();
            let mut run = loaded.run;
            let event = run
                .transition(
                    RunTransition::BeginPreparation,
                    EventMetadata::new(EventId::new(), now),
                )
                .unwrap();
            self.store
                .begin_workspace_preparation(&workspace, &run, loaded.revision, &event)
                .unwrap();
        }

        fn request(&self, signal_index: usize, status: StageStatus) -> ProviderRequest {
            ProviderRequest::new(
                run_id(),
                StageId::new("verify").unwrap(),
                StageKind::Verify,
                status,
                Role::Verifier,
                "task".to_owned(),
                self.worktree.clone(),
                1,
                signal_index,
                None,
                Vec::new(),
            )
        }

        /// Drives both polls and returns the terminal signal.
        fn run(&mut self) -> ProviderSignal {
            let request = self.request(0, StageStatus::Ready);
            let started = self.provider.poll(&mut self.store, &request).unwrap();
            assert!(matches!(
                started,
                ProviderPoll::Signal(ProviderSignal::Started {
                    model_id: None,
                    session_id: None
                })
            ));
            let request = self.request(1, StageStatus::Running);
            match self.provider.poll(&mut self.store, &request).unwrap() {
                ProviderPoll::Signal(signal) => signal,
                other => panic!("expected a terminal signal, got {other:?}"),
            }
        }

        fn artifact(&self) -> String {
            let artifacts = self.store.list_artifacts(run_id()).unwrap();
            assert_eq!(artifacts.len(), 1, "exactly one artifact per attempt");
            let artifact = &artifacts[0];
            assert_eq!(
                artifact.metadata().kind(),
                crate::domain::ArtifactKind::Verify
            );
            assert_eq!(artifact.metadata().role(), Role::Verifier);
            assert_eq!(
                artifact.metadata().provider_id().map(ProviderId::as_str),
                Some("verify")
            );
            std::fs::read_to_string(artifact.path()).unwrap()
        }
    }

    #[test]
    fn passing_commands_complete_the_stage_with_a_passed_bottom_line() {
        let mut harness = Harness::new();
        harness.config("[verify]\ncommands = [\"true\", \"echo done\"]\n");

        assert_eq!(harness.run(), ProviderSignal::Completed);

        let artifact = harness.artifact();
        assert!(artifact.contains("## Bottom line\npassed — 2 commands\n"));
        assert!(artifact.contains("### $ true\nexit: 0\n"));
        assert!(artifact.contains("### $ echo done\nexit: 0\nstdout:\n```text\ndone\n```\n"));
        assert_eq!(
            harness.store.list_artifacts(run_id()).unwrap()[0]
                .metadata()
                .status(),
            crate::domain::ArtifactStatus::Complete
        );
    }

    #[test]
    fn the_first_failing_command_fails_the_stage_and_skips_the_rest() {
        let mut harness = Harness::new();
        harness.config("[verify]\ncommands = [\"true\", \"false\", \"echo never\"]\n");

        assert_eq!(
            harness.run(),
            ProviderSignal::Failed("failed — false exited 1".to_owned())
        );

        let artifact = harness.artifact();
        assert!(artifact.contains("## Bottom line\nfailed — false exited 1\n"));
        assert!(artifact.contains("### $ false\nexit: 1\n"));
        assert!(artifact.contains("### $ echo never\nskipped: not run after the first failure\n"));
        assert_eq!(
            harness.store.list_artifacts(run_id()).unwrap()[0]
                .metadata()
                .status(),
            crate::domain::ArtifactStatus::Failed
        );
    }

    #[test]
    fn a_command_past_the_configured_timeout_fails_the_stage_as_timed_out() {
        let mut harness = Harness::new();
        harness.config("[verify]\ncommands = [\"sleep 5\"]\ntimeout_seconds = 1\n");

        assert_eq!(
            harness.run(),
            ProviderSignal::Failed("failed — sleep 5 timed out after 1 s".to_owned())
        );
        assert!(harness.artifact().contains("exit: timed out after 1 s\n"));
    }

    #[test]
    fn a_source_repository_config_verifies_a_worktree_that_carries_none() {
        let mut harness = Harness::new();
        // Detection would answer `npm test` here, which for a monorepo is
        // the wrong suite and can be red for reasons no change caused.
        std::fs::write(harness.worktree.join("package.json"), "{}\n").unwrap();
        harness.source_repo_config("[verify]\ncommands = [\"echo scoped\"]\n");

        assert_eq!(harness.run(), ProviderSignal::Completed);

        let artifact = harness.artifact();
        assert!(artifact.contains("## Bottom line\npassed — 1 command\n"));
        assert!(artifact.contains("### $ echo scoped\nexit: 0\n"));
        // The artifact names the checkout, so a green stage stays readable
        // back to the file that configured it.
        assert!(
            artifact.contains("## Source\n`.polycode.toml` `[verify]` table (source repository)\n")
        );
    }

    #[test]
    fn the_worktrees_own_config_still_wins_over_the_source_repository() {
        let mut harness = Harness::new();
        harness.source_repo_config("[verify]\ncommands = [\"echo from the source repo\"]\n");
        harness.config("[verify]\ncommands = [\"echo from the worktree\"]\n");

        assert_eq!(harness.run(), ProviderSignal::Completed);

        let artifact = harness.artifact();
        assert!(artifact.contains("### $ echo from the worktree\n"));
        assert!(!artifact.contains("from the source repo"));
        assert!(artifact.contains("## Source\n`.polycode.toml` `[verify]` table (worktree)\n"));
    }

    #[test]
    fn a_cargo_project_without_configuration_verifies_with_cargo_test() {
        let harness = Harness::new();
        std::fs::write(harness.worktree.join("Cargo.toml"), "[package]\n").unwrap();

        let plan = config::plan_for(&harness.worktree, None).unwrap();

        assert_eq!(plan.commands, ["cargo test"]);
        assert_eq!(plan.source, config::CommandSource::Detected("Cargo.toml"));
        assert_eq!(plan.timeout, DEFAULT_TIMEOUT);
    }

    #[test]
    fn malformed_configuration_fails_the_stage_with_the_parse_error() {
        let mut harness = Harness::new();
        harness.config("[verify]\ncommands = \"not a list\"\n");

        let signal = harness.run();

        let ProviderSignal::Failed(reason) = signal else {
            panic!("expected failure, got {signal:?}");
        };
        assert!(reason.starts_with("failed — .polycode.toml: "), "{reason}");
        let artifact = harness.artifact();
        assert!(artifact.contains(&reason));
        assert!(artifact.contains("## Source\n`.polycode.toml` (could not be read)\n"));
    }

    #[test]
    fn an_empty_directory_completes_having_checked_nothing() {
        let mut harness = Harness::new();

        assert_eq!(harness.run(), ProviderSignal::Completed);

        let artifact = harness.artifact();
        assert!(
            artifact
                .contains("## Bottom line\nnothing checked — no commands configured or detected\n")
        );
        assert!(!artifact.contains("### $"));
    }

    #[test]
    fn a_repeated_terminal_poll_reports_the_recorded_verdict_without_rerunning() {
        let mut harness = Harness::new();
        let counter = harness.worktree.join("count");
        harness.config(&format!(
            "[verify]\ncommands = [\"touch {}\", \"false\"]\n",
            counter.display()
        ));
        assert!(matches!(harness.run(), ProviderSignal::Failed(_)));
        std::fs::remove_file(&counter).unwrap();

        let request = harness.request(1, StageStatus::Running);
        let again = harness.provider.poll(&mut harness.store, &request).unwrap();

        assert_eq!(
            again,
            ProviderPoll::Signal(ProviderSignal::Failed("failed — false exited 1".to_owned()))
        );
        assert!(!counter.exists(), "the commands did not run a second time");
    }

    /// The write is not atomic with the row: a crash between the two must
    /// leave a poll that reports what the file says, records it, and runs
    /// nothing — not one that re-runs the commands into a conflicting file.
    #[test]
    fn an_artifact_file_without_a_row_is_adopted_instead_of_rerun() {
        let mut harness = Harness::new();
        let marker = harness.worktree.join("ran");
        harness.config(&format!(
            "[verify]\ncommands = [\"touch {}\"]\n",
            marker.display()
        ));
        let request = harness.request(1, StageStatus::Running);
        let path = artifact::artifact_path(&harness.provider.artifact_root, &request);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "# Verification\n\n## Bottom line\nfailed — cargo test exited 101\n\n## Source\n`.polycode.toml` `[verify]` table\n",
        )
        .unwrap();
        assert!(harness.store.list_artifacts(run_id()).unwrap().is_empty());

        let poll = harness.provider.poll(&mut harness.store, &request).unwrap();

        assert_eq!(
            poll,
            ProviderPoll::Signal(ProviderSignal::Failed(
                "failed — cargo test exited 101".to_owned()
            ))
        );
        assert!(!marker.exists(), "the commands did not run");
        let artifacts = harness.store.list_artifacts(run_id()).unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].path(), path);
        assert_eq!(
            artifacts[0].metadata().status(),
            crate::domain::ArtifactStatus::Failed
        );
    }

    #[test]
    fn an_observing_poll_never_runs_the_commands() {
        let mut harness = Harness::new();
        let marker = harness.worktree.join("ran");
        harness.config(&format!(
            "[verify]\ncommands = [\"touch {}\"]\n",
            marker.display()
        ));

        let request = harness.request(1, StageStatus::Running).observing();
        let poll = harness.provider.poll(&mut harness.store, &request).unwrap();

        assert_eq!(poll, ProviderPoll::Pending);
        assert!(!marker.exists());
    }

    #[test]
    fn the_provider_serves_only_the_verifier_role() {
        let harness = Harness::new();
        assert!(harness.provider.supports_role(Role::Verifier));
        assert!(!harness.provider.supports_role(Role::Implementer));
        assert_eq!(
            harness
                .provider
                .provider_id_for(&harness.request(0, StageStatus::Ready))
                .unwrap()
                .as_str(),
            "verify"
        );
    }
}
