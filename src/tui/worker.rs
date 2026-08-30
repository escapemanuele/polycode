use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use crate::app::{ApplyOutcome, ExecutionReport, ExecutionSelection, ProviderFactory, RunService};
use crate::domain::{AttentionRequestId, EffortSetting, RunId, StageId, WorkflowKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionKind {
    Start,
    Resume,
    Stop,
    Retry,
    Fix,
    ResolveAttention,
    Apply,
    Discard,
}

impl ActionKind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Start => "starting run",
            Self::Resume => "resuming run",
            Self::Stop => "stopping run",
            Self::Retry => "retrying stage",
            Self::Fix => "fixing run",
            Self::ResolveAttention => "resolving attention",
            Self::Apply => "applying changes",
            Self::Discard => "discarding run",
        }
    }
}

#[derive(Debug)]
pub(crate) enum WorkerCommand {
    StartRun {
        workflow: WorkflowKind,
        task: String,
        repository: PathBuf,
        selection: ExecutionSelection,
        effort: EffortSetting,
    },
    ResumeRun {
        run_id: RunId,
    },
    StopRun {
        run_id: RunId,
    },
    RetryStage {
        run_id: RunId,
        stage_id: StageId,
    },
    RequestFix {
        run_id: RunId,
    },
    ResolveAttention {
        run_id: RunId,
        attention_id: AttentionRequestId,
        response: Option<String>,
    },
    ApplyRun {
        run_id: RunId,
    },
    DiscardRun {
        run_id: RunId,
    },
}

impl WorkerCommand {
    pub(crate) const fn kind(&self) -> ActionKind {
        match self {
            Self::StartRun { .. } => ActionKind::Start,
            Self::ResumeRun { .. } => ActionKind::Resume,
            Self::StopRun { .. } => ActionKind::Stop,
            Self::RetryStage { .. } => ActionKind::Retry,
            Self::RequestFix { .. } => ActionKind::Fix,
            Self::ResolveAttention { .. } => ActionKind::ResolveAttention,
            Self::ApplyRun { .. } => ActionKind::Apply,
            Self::DiscardRun { .. } => ActionKind::Discard,
        }
    }

    pub(crate) const fn run_id(&self) -> Option<RunId> {
        match self {
            Self::StartRun { .. } => None,
            Self::ResumeRun { run_id }
            | Self::StopRun { run_id }
            | Self::RetryStage { run_id, .. }
            | Self::RequestFix { run_id }
            | Self::ResolveAttention { run_id, .. }
            | Self::ApplyRun { run_id }
            | Self::DiscardRun { run_id } => Some(*run_id),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkerSuccess {
    Execution(ExecutionReport),
    Applied(ApplyOutcome, ExecutionReport),
}

impl WorkerSuccess {
    pub(crate) const fn report(&self) -> &ExecutionReport {
        match self {
            Self::Execution(report) | Self::Applied(_, report) => report,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkerResult {
    pub action: ActionKind,
    pub run_id: Option<RunId>,
    pub result: Result<WorkerSuccess, String>,
}

pub(crate) struct Worker {
    sender: Sender<WorkerCommand>,
    receiver: Receiver<WorkerResult>,
}

impl Worker {
    pub(crate) fn spawn<F>(service: RunService<F>) -> Self
    where
        F: ProviderFactory + Send + 'static,
        F::Provider: Send + 'static,
    {
        let (command_sender, command_receiver) = mpsc::channel::<WorkerCommand>();
        let (result_sender, result_receiver) = mpsc::channel::<WorkerResult>();
        std::thread::spawn(move || {
            while let Ok(command) = command_receiver.recv() {
                let action = command.kind();
                let run_id = command.run_id();
                let result = execute(&service, command).map_err(|error| error.to_string());
                if result_sender
                    .send(WorkerResult {
                        action,
                        run_id,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            sender: command_sender,
            receiver: result_receiver,
        }
    }

    pub(crate) fn send(&self, command: WorkerCommand) -> Result<(), String> {
        self.sender
            .send(command)
            .map_err(|_| "application action worker is unavailable".to_owned())
    }

    pub(crate) fn try_recv(&self) -> Result<Option<WorkerResult>, String> {
        match self.receiver.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err("application action worker disconnected".to_owned())
            }
        }
    }
}

fn execute<F>(
    service: &RunService<F>,
    command: WorkerCommand,
) -> Result<WorkerSuccess, crate::app::AppError>
where
    F: ProviderFactory,
{
    match command {
        WorkerCommand::StartRun {
            workflow,
            task,
            repository,
            selection,
            effort,
        } => service
            .start_run(workflow, task, repository, Some(selection), effort)
            .map(WorkerSuccess::Execution),
        WorkerCommand::ResumeRun { run_id } => {
            service.resume_run(run_id).map(WorkerSuccess::Execution)
        }
        WorkerCommand::StopRun { run_id } => service.stop_run(run_id).map(WorkerSuccess::Execution),
        WorkerCommand::RetryStage { run_id, stage_id } => service
            .retry_stage(run_id, &stage_id)
            .map(WorkerSuccess::Execution),
        WorkerCommand::RequestFix { run_id } => {
            service.request_fix(run_id).map(WorkerSuccess::Execution)
        }
        WorkerCommand::ResolveAttention {
            run_id,
            attention_id,
            response,
        } => service
            .resolve_attention_with_response(run_id, attention_id, response.as_deref())
            .map(WorkerSuccess::Execution),
        WorkerCommand::ApplyRun { run_id } => service
            .apply_run(run_id)
            .map(|(outcome, report)| WorkerSuccess::Applied(outcome, report)),
        WorkerCommand::DiscardRun { run_id } => {
            service.discard_run(run_id).map(WorkerSuccess::Execution)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    use super::*;
    use crate::app::{DevelopmentFakeProviderFactory, UniformProvider};
    use crate::domain::RunStatus;

    #[test]
    fn worker_executes_fake_start_off_caller_and_returns_committed_report() {
        let fixture = TempDir::new().unwrap();
        let repo = fixture.path().join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "Test"]);
        fs::write(repo.join("README.md"), "baseline\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-qm", "initial"]);
        let worker = Worker::spawn(RunService::new(
            fixture.path().join("polycode.db"),
            fixture.path().join("worktrees"),
            DevelopmentFakeProviderFactory,
        ));
        worker
            .send(WorkerCommand::StartRun {
                workflow: WorkflowKind::Standard,
                task: "worker integration".to_owned(),
                repository: repo,
                selection: ExecutionSelection::Uniform(UniformProvider::Fake),
                effort: EffortSetting::NativeDefault,
            })
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let result = loop {
            if let Some(result) = worker.try_recv().unwrap() {
                break result;
            }
            assert!(Instant::now() < deadline, "worker result timed out");
            std::thread::sleep(Duration::from_millis(10));
        };
        let success = result.result.unwrap();
        assert_eq!(result.action, ActionKind::Start);
        assert_eq!(success.report().details.status, RunStatus::Completed);
    }

    #[test]
    fn worker_propagates_owned_action_error() {
        let fixture = TempDir::new().unwrap();
        let worker = Worker::spawn(RunService::new(
            fixture.path().join("polycode.db"),
            fixture.path().join("worktrees"),
            DevelopmentFakeProviderFactory,
        ));
        worker
            .send(WorkerCommand::StartRun {
                workflow: WorkflowKind::Fast,
                task: "invalid repository".to_owned(),
                repository: fixture.path().join("missing"),
                selection: ExecutionSelection::Uniform(UniformProvider::Fake),
                effort: EffortSetting::NativeDefault,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(result) = worker.try_recv().unwrap() {
                assert!(
                    result
                        .result
                        .unwrap_err()
                        .contains("repository path is unavailable")
                );
                break;
            }
            assert!(Instant::now() < deadline, "worker error timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn git(path: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
