use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::domain::{
    AttentionRequestId, ConfigSnapshotId, EventId, EventMetadata, Run, RunId, RunStatus, StageId,
    StageStatus, WorkflowDefinition, WorkflowKind,
};
use crate::engine::{EngineStatus, WorkflowEngine};
use crate::git::GitRepository;
use crate::process::{ManagedProcessStatus, ProcessManager};
use crate::store::{RunInput, SqliteStore, database_file, worktree_root};
use crate::workspace::{ReconciliationOutcome, WorkspaceError, WorkspaceManager};

use super::provider_factory::ProviderFactory;
use super::{AppError, CommittedEvent, RunDetails, RunListItem, query};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied,
    NoChanges,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionReport {
    pub details: RunDetails,
    pub committed_events: Vec<CommittedEvent>,
    pub outcome: QuiescentState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuiescentState {
    Completed,
    NeedsUser,
    Paused,
    Interrupted,
    Failed,
    Applied,
    Discarded,
    WaitingForProvider { stage_id: StageId },
    Active { run_status: RunStatus },
}

pub struct RunService<F> {
    database: PathBuf,
    worktrees: PathBuf,
    provider_factory: F,
}

impl<F> RunService<F>
where
    F: ProviderFactory,
{
    #[must_use]
    pub const fn new(database: PathBuf, worktrees: PathBuf, provider_factory: F) -> Self {
        Self {
            database,
            worktrees,
            provider_factory,
        }
    }

    /// Uses Polycode data paths from environment.
    ///
    /// # Errors
    /// Returns path resolution errors.
    pub fn from_environment(provider_factory: F) -> Result<Self, AppError> {
        Ok(Self::new(
            database_file()?,
            worktree_root()?,
            provider_factory,
        ))
    }

    /// Creates, prepares, and executes one built-in workflow to quiescence.
    ///
    /// # Errors
    /// Returns validation, repository, persistence, workspace, or engine errors.
    pub fn start_run(
        &self,
        workflow_kind: WorkflowKind,
        task: impl Into<String>,
        repository_path: impl AsRef<Path>,
        provider: Option<&str>,
    ) -> Result<ExecutionReport, AppError> {
        let repository = GitRepository::discover(repository_path)?;
        let created_at = now();
        let run_id = RunId::new();
        let input = RunInput::new(run_id, task, created_at)?;
        let workflow = WorkflowDefinition::built_in(workflow_kind);
        let config_id = ConfigSnapshotId::new(format!("m5-{run_id}"))?;
        let config =
            self.provider_factory
                .config_for_new_run(provider, config_id.clone(), created_at)?;
        let run = Run::new(run_id, workflow, config_id, created_at);
        let created = run.created_event(EventMetadata::new(EventId::new(), created_at));
        let mut store = SqliteStore::open(&self.database)?;
        store.create_run_with_input(&run, &input, &config, &[created])?;

        let manager = WorkspaceManager::new(&self.worktrees);
        manager.prepare_run_workspace(&mut store, run_id, repository.source_path())?;
        let status = self.drive(&mut store, run_id, ResumeAction::Continue)?;
        Self::report(&mut store, run_id, 0, status.as_ref())
    }

    /// Continues one persisted run according to safe lifecycle policy.
    ///
    /// # Errors
    /// Returns legacy-input/config, reconciliation, guard, or execution errors.
    pub fn resume_run(&self, run_id: RunId) -> Result<ExecutionReport, AppError> {
        let mut store = SqliteStore::open(&self.database)?;
        let before = last_sequence(&store, run_id)?;
        self.reconcile(&mut store, run_id)?;
        let status = self.drive(&mut store, run_id, ResumeAction::Resume)?;
        Self::report(&mut store, run_id, before, status.as_ref())
    }

    /// Resolves one attention request, then executes to quiescence.
    ///
    /// # Errors
    /// Returns identity, guard, lifecycle, persistence, or provider errors.
    pub fn resolve_attention(
        &self,
        run_id: RunId,
        request_id: AttentionRequestId,
    ) -> Result<ExecutionReport, AppError> {
        self.resolve_attention_with_response(run_id, request_id, None)
    }

    /// Resolves one attention request with optional provider-native response text.
    ///
    /// # Errors
    /// Returns identity, guard, lifecycle, persistence, or provider errors.
    pub fn resolve_attention_with_response(
        &self,
        run_id: RunId,
        request_id: AttentionRequestId,
        response: Option<&str>,
    ) -> Result<ExecutionReport, AppError> {
        let mut store = SqliteStore::open(&self.database)?;
        let before = last_sequence(&store, run_id)?;
        self.reconcile(&mut store, run_id)?;
        let mut engine = self.engine(&mut store, run_id)?;
        engine.resolve_attention_with_response(&mut store, run_id, request_id, response)?;
        let status = drive_attached(&mut engine, &mut store, run_id)?;
        Self::report(&mut store, run_id, before, Some(&status))
    }

    /// Retries one failed stage, then executes to quiescence.
    ///
    /// # Errors
    /// Returns identity, retry-safety, guard, persistence, or provider errors.
    pub fn retry_stage(
        &self,
        run_id: RunId,
        stage_id: &StageId,
    ) -> Result<ExecutionReport, AppError> {
        let mut store = SqliteStore::open(&self.database)?;
        let before = last_sequence(&store, run_id)?;
        self.reconcile(&mut store, run_id)?;
        let mut engine = self.engine(&mut store, run_id)?;
        engine.retry_stage(&mut store, run_id, stage_id)?;
        let status = drive_attached(&mut engine, &mut store, run_id)?;
        Self::report(&mut store, run_id, before, Some(&status))
    }

    /// Applies completed workspace changes to source checkout.
    ///
    /// # Errors
    /// Returns workspace ownership, lifecycle, patch, Git, or store errors.
    pub fn apply_run(&self, run_id: RunId) -> Result<(ApplyOutcome, ExecutionReport), AppError> {
        let mut store = SqliteStore::open(&self.database)?;
        let before = last_sequence(&store, run_id)?;
        let manager = WorkspaceManager::new(&self.worktrees);
        let outcome = match manager.apply(&mut store, run_id) {
            Ok(()) => ApplyOutcome::Applied,
            Err(WorkspaceError::EmptyPatch) => ApplyOutcome::NoChanges,
            Err(error) => return Err(error.into()),
        };
        let report = Self::report(&mut store, run_id, before, None)?;
        Ok((outcome, report))
    }

    /// Discards one run and cleans its owned workspace resources.
    ///
    /// # Errors
    /// Returns workspace ownership, lifecycle, Git, or store errors.
    pub fn discard_run(&self, run_id: RunId) -> Result<ExecutionReport, AppError> {
        let mut store = SqliteStore::open(&self.database)?;
        let before = last_sequence(&store, run_id)?;
        let process_manager = ProcessManager::from_environment()?;
        for process in store.list_managed_processes(run_id)? {
            let inspection = process_manager.inspect(&mut store, process.id())?;
            let inspection = if inspection.process.status().is_active() {
                process_manager.interrupt(&mut store, process.id())?
            } else {
                inspection
            };
            if matches!(
                inspection.process.status(),
                ManagedProcessStatus::Exited
                    | ManagedProcessStatus::Interrupted
                    | ManagedProcessStatus::Missing
                    | ManagedProcessStatus::Broken
            ) {
                process_manager.cleanup(&mut store, process.id())?;
            }
        }
        WorkspaceManager::new(&self.worktrees).discard(&mut store, run_id)?;
        Self::report(&mut store, run_id, before, None)
    }

    /// Returns indexed summaries without creating a missing database.
    ///
    /// # Errors
    /// Returns query, migration, or path errors.
    pub fn list_runs(&self) -> Result<Vec<RunListItem>, AppError> {
        if !self.database.exists() {
            return Ok(Vec::new());
        }
        query::list(&SqliteStore::open(&self.database)?)
    }

    /// Returns detailed persisted state without mutation.
    ///
    /// # Errors
    /// Returns not-found, corrupt state, or query errors.
    pub fn inspect_run(&self, run_id: RunId) -> Result<RunDetails, AppError> {
        query::inspect(&mut SqliteStore::open(&self.database)?, run_id)
    }

    fn reconcile(&self, store: &mut SqliteStore, run_id: RunId) -> Result<(), AppError> {
        let outcome = WorkspaceManager::new(&self.worktrees).reconcile(store, run_id)?;
        match outcome {
            ReconciliationOutcome::Ready(_)
            | ReconciliationOutcome::Unchanged(_)
            | ReconciliationOutcome::Removed(_)
            | ReconciliationOutcome::Broken(_) => Ok(()),
        }
    }

    fn engine(
        &self,
        store: &mut SqliteStore,
        run_id: RunId,
    ) -> Result<WorkflowEngine<F::Provider>, AppError> {
        let loaded = store.load_run(run_id)?;
        let input = store
            .load_run_input(run_id)?
            .ok_or(AppError::LegacyRunInput(run_id))?;
        let events = store.load_events(run_id)?;
        let provider = self.provider_factory.for_run(
            run_id,
            &loaded.config_snapshot,
            loaded.run.workflow(),
            &events,
        )?;
        Ok(WorkflowEngine::new(provider, input.task().to_owned()))
    }

    fn drive(
        &self,
        store: &mut SqliteStore,
        run_id: RunId,
        action: ResumeAction,
    ) -> Result<Option<EngineStatus>, AppError> {
        let loaded = store.load_run(run_id)?;
        match loaded.run.status() {
            RunStatus::NeedsUser
            | RunStatus::Failed
            | RunStatus::Completed
            | RunStatus::Applied => {
                return Ok(None);
            }
            RunStatus::Discarded => return Err(AppError::DiscardedRun(run_id)),
            _ => {}
        }
        let mut engine = self.engine(store, run_id)?;
        if action == ResumeAction::Resume {
            match loaded.run.status() {
                RunStatus::Paused => {
                    engine.resume_run(store, run_id)?;
                }
                RunStatus::Interrupted => {
                    engine.recover_run(store, run_id)?;
                }
                RunStatus::Running => {
                    resume_suspended_stages(&mut engine, store, &loaded.run)?;
                }
                _ => {}
            }
        }
        Ok(Some(drive_attached(&mut engine, store, run_id)?))
    }

    fn report(
        store: &mut SqliteStore,
        run_id: RunId,
        after_sequence: u64,
        engine_status: Option<&EngineStatus>,
    ) -> Result<ExecutionReport, AppError> {
        let committed_events = store
            .load_events(run_id)?
            .into_iter()
            .filter(|event| event.sequence > after_sequence)
            .map(|event| CommittedEvent {
                sequence: event.sequence,
                stage_id: event.event.stage_id().cloned(),
                kind: event.event.kind().clone(),
            })
            .collect();
        let details = query::inspect(store, run_id)?;
        let outcome = quiescent_state(details.status, engine_status);
        Ok(ExecutionReport {
            details,
            committed_events,
            outcome,
        })
    }
}

fn drive_attached<P: crate::engine::Provider>(
    engine: &mut WorkflowEngine<P>,
    store: &mut SqliteStore,
    run_id: RunId,
) -> Result<EngineStatus, AppError> {
    loop {
        let status = engine.drive(store, run_id)?;
        if matches!(status, EngineStatus::WaitingForProvider { .. })
            && engine.provider().keep_attached()
        {
            std::thread::sleep(std::time::Duration::from_millis(150));
            continue;
        }
        return Ok(status);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResumeAction {
    Continue,
    Resume,
}

fn resume_suspended_stages<P: crate::engine::Provider>(
    engine: &mut WorkflowEngine<P>,
    store: &mut SqliteStore,
    run: &Run,
) -> Result<(), AppError> {
    for stage in run.stages() {
        match stage.status() {
            StageStatus::Paused => {
                engine.resume_stage(store, run.id(), stage.id())?;
            }
            StageStatus::Interrupted => {
                engine.recover_stage(store, run.id(), stage.id())?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn last_sequence(store: &SqliteStore, run_id: RunId) -> Result<u64, AppError> {
    Ok(store
        .load_events(run_id)?
        .last()
        .map_or(0, |event| event.sequence))
}

fn now() -> DateTime<Utc> {
    std::time::SystemTime::now().into()
}

fn quiescent_state(status: RunStatus, engine_status: Option<&EngineStatus>) -> QuiescentState {
    if let Some(EngineStatus::WaitingForProvider { stage_id }) = engine_status {
        return QuiescentState::WaitingForProvider {
            stage_id: stage_id.clone(),
        };
    }
    match status {
        RunStatus::Completed => QuiescentState::Completed,
        RunStatus::NeedsUser => QuiescentState::NeedsUser,
        RunStatus::Paused => QuiescentState::Paused,
        RunStatus::Interrupted => QuiescentState::Interrupted,
        RunStatus::Failed => QuiescentState::Failed,
        RunStatus::Applied => QuiescentState::Applied,
        RunStatus::Discarded => QuiescentState::Discarded,
        RunStatus::Running if matches!(engine_status, Some(EngineStatus::Paused { .. })) => {
            QuiescentState::Paused
        }
        RunStatus::Running if matches!(engine_status, Some(EngineStatus::Interrupted { .. })) => {
            QuiescentState::Interrupted
        }
        run_status => QuiescentState::Active { run_status },
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use tempfile::TempDir;

    use super::*;
    use crate::app::{DevelopmentFakeProviderFactory, ProviderFactory};
    use crate::domain::{AttentionKind, ConfigSnapshotId, DomainEventKind, ProviderId, Role};
    use crate::engine::{
        FakeEvent, FakeProvider, FakeScenario, Provider, ProviderError, ProviderPoll,
        ProviderRequest,
    };
    use crate::store::{ResolvedConfigSnapshot, SequencedEvent};

    #[derive(Clone)]
    struct ScriptedFactory {
        scenario: FakeScenario,
    }

    #[derive(Clone, Default)]
    struct RecordingFactory {
        tasks: Arc<Mutex<Vec<String>>>,
    }

    struct RecordingProvider {
        inner: FakeProvider,
        tasks: Arc<Mutex<Vec<String>>>,
    }

    #[derive(Clone, Default)]
    struct GatedFactory {
        released: Arc<AtomicBool>,
    }

    impl Provider for RecordingProvider {
        fn id(&self) -> &ProviderId {
            self.inner.id()
        }

        fn supports_role(&self, role: Role) -> bool {
            self.inner.supports_role(role)
        }

        fn poll(
            &mut self,
            store: &mut SqliteStore,
            request: &ProviderRequest,
        ) -> Result<ProviderPoll, ProviderError> {
            self.tasks.lock().unwrap().push(request.task().to_owned());
            self.inner.poll(store, request)
        }
    }

    impl ProviderFactory for RecordingFactory {
        type Provider = RecordingProvider;

        fn config_for_new_run(
            &self,
            provider: Option<&str>,
            id: ConfigSnapshotId,
            created_at: DateTime<Utc>,
        ) -> Result<ResolvedConfigSnapshot, AppError> {
            DevelopmentFakeProviderFactory.config_for_new_run(provider, id, created_at)
        }

        fn for_run(
            &self,
            run_id: RunId,
            config: &ResolvedConfigSnapshot,
            workflow: &WorkflowDefinition,
            events: &[SequencedEvent],
        ) -> Result<Self::Provider, AppError> {
            Ok(RecordingProvider {
                inner: DevelopmentFakeProviderFactory.for_run(run_id, config, workflow, events)?,
                tasks: Arc::clone(&self.tasks),
            })
        }
    }

    impl ProviderFactory for GatedFactory {
        type Provider = FakeProvider;

        fn config_for_new_run(
            &self,
            provider: Option<&str>,
            id: ConfigSnapshotId,
            created_at: DateTime<Utc>,
        ) -> Result<ResolvedConfigSnapshot, AppError> {
            DevelopmentFakeProviderFactory.config_for_new_run(provider, id, created_at)
        }

        fn for_run(
            &self,
            run_id: RunId,
            config: &ResolvedConfigSnapshot,
            workflow: &WorkflowDefinition,
            events: &[SequencedEvent],
        ) -> Result<Self::Provider, AppError> {
            let _ = DevelopmentFakeProviderFactory.for_run(run_id, config, workflow, events)?;
            let scenario = FakeScenario::new().stage("implementation").events([
                FakeEvent::Started,
                FakeEvent::progress("checkpoint before process exit"),
                FakeEvent::delay("process-restarted"),
                FakeEvent::Completed,
            ]);
            let mut provider = FakeProvider::new(scenario)?;
            if self.released.load(Ordering::SeqCst) {
                provider.release("implementation", "process-restarted")?;
            }
            Ok(provider)
        }
    }

    impl ProviderFactory for ScriptedFactory {
        type Provider = FakeProvider;

        fn config_for_new_run(
            &self,
            provider: Option<&str>,
            id: ConfigSnapshotId,
            created_at: DateTime<Utc>,
        ) -> Result<ResolvedConfigSnapshot, AppError> {
            DevelopmentFakeProviderFactory.config_for_new_run(provider, id, created_at)
        }

        fn for_run(
            &self,
            _run_id: RunId,
            _config: &ResolvedConfigSnapshot,
            _workflow: &WorkflowDefinition,
            _events: &[SequencedEvent],
        ) -> Result<Self::Provider, AppError> {
            Ok(FakeProvider::new(self.scenario.clone())?)
        }
    }

    struct Fixture {
        _temp: TempDir,
        repo: PathBuf,
        database: PathBuf,
        worktrees: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = TempDir::new().unwrap();
            let repo = temp.path().join("repo");
            fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-q"]);
            git(&repo, &["config", "user.email", "test@example.com"]);
            git(&repo, &["config", "user.name", "Test"]);
            fs::write(repo.join("README.md"), "baseline\n").unwrap();
            git(&repo, &["add", "README.md"]);
            git(&repo, &["commit", "-qm", "initial"]);
            let database = temp.path().join("data/polycode.db");
            let worktrees = temp.path().join("data/worktrees");
            Self {
                _temp: temp,
                repo,
                database,
                worktrees,
            }
        }

        fn default_service(&self) -> RunService<DevelopmentFakeProviderFactory> {
            RunService::new(
                self.database.clone(),
                self.worktrees.clone(),
                DevelopmentFakeProviderFactory,
            )
        }

        fn scripted_service(&self, scenario: FakeScenario) -> RunService<ScriptedFactory> {
            RunService::new(
                self.database.clone(),
                self.worktrees.clone(),
                ScriptedFactory { scenario },
            )
        }
    }

    #[test]
    fn every_cli_workflow_completes_and_multiple_runs_are_queryable() {
        let fixture = Fixture::new();
        let service = fixture.default_service();
        let head = git_output(&fixture.repo, &["rev-parse", "HEAD"]);
        for kind in [
            WorkflowKind::Fast,
            WorkflowKind::Standard,
            WorkflowKind::Deep,
            WorkflowKind::Review,
        ] {
            let report = service
                .start_run(
                    kind,
                    format!("task for {kind:?}"),
                    &fixture.repo,
                    Some("fake"),
                )
                .unwrap();
            assert_eq!(report.details.status, RunStatus::Completed);
            assert!(
                report
                    .details
                    .stages
                    .iter()
                    .all(|stage| stage.status == StageStatus::Completed)
            );
        }
        assert_eq!(service.list_runs().unwrap().len(), 4);
        assert_eq!(git_output(&fixture.repo, &["rev-parse", "HEAD"]), head);
        assert!(git_output(&fixture.repo, &["status", "--porcelain"]).is_empty());
    }

    #[test]
    fn restart_preserves_completed_run_input_and_does_not_replay_signals() {
        let fixture = Fixture::new();
        let report = fixture
            .default_service()
            .start_run(
                WorkflowKind::Deep,
                "  Unicode α\nsecond line  ",
                &fixture.repo,
                Some("fake"),
            )
            .unwrap();
        let run_id = report.details.id;
        drop(report);

        let restarted = fixture.default_service();
        let inspected = restarted.inspect_run(run_id).unwrap();
        assert_eq!(inspected.task.as_deref(), Some("Unicode α\nsecond line"));
        assert_eq!(inspected.status, RunStatus::Completed);
        let resumed = restarted.resume_run(run_id).unwrap();
        assert!(resumed.committed_events.is_empty());
        assert_eq!(resumed.details, inspected);
    }

    #[test]
    fn normalized_immutable_task_reaches_every_provider_poll_exactly() {
        let fixture = Fixture::new();
        let factory = RecordingFactory::default();
        let observed = Arc::clone(&factory.tasks);
        let service = RunService::new(fixture.database.clone(), fixture.worktrees.clone(), factory);

        service
            .start_run(
                WorkflowKind::Standard,
                "  α first line\nsecond line  ",
                &fixture.repo,
                Some("fake"),
            )
            .unwrap();

        let tasks = observed.lock().unwrap();
        assert!(!tasks.is_empty());
        assert!(tasks.iter().all(|task| task == "α first line\nsecond line"));
    }

    #[test]
    fn needs_user_survives_restart_and_requires_explicit_resolution() {
        let fixture = Fixture::new();
        let scenario = || {
            FakeScenario::new().stage("implementation").events([
                FakeEvent::Started,
                FakeEvent::needs_user(AttentionKind::Decision, "Choose API shape"),
                FakeEvent::progress("Apply choice"),
                FakeEvent::Completed,
            ])
        };
        let blocked = fixture
            .scripted_service(scenario())
            .start_run(
                WorkflowKind::Fast,
                "attention task",
                &fixture.repo,
                Some("fake"),
            )
            .unwrap();
        assert_eq!(blocked.details.status, RunStatus::NeedsUser);
        let request = blocked.details.attention[0].id;
        let run_id = blocked.details.id;

        let restarted = fixture.scripted_service(scenario());
        let still_blocked = restarted.resume_run(run_id).unwrap();
        assert_eq!(still_blocked.details.status, RunStatus::NeedsUser);
        assert!(still_blocked.committed_events.is_empty());
        let completed = restarted.resolve_attention(run_id, request).unwrap();
        assert_eq!(completed.details.status, RunStatus::Completed);
        assert!(
            completed
                .committed_events
                .iter()
                .any(|event| matches!(&event.kind, DomainEventKind::AttentionResolved { .. }))
        );
    }

    #[test]
    fn resume_recovers_paused_and_interrupted_stages() {
        for suspended in [FakeEvent::Paused, FakeEvent::Interrupted] {
            let fixture = Fixture::new();
            let scenario = || {
                FakeScenario::new().stage("implementation").events([
                    FakeEvent::Started,
                    suspended.clone(),
                    FakeEvent::Completed,
                ])
            };
            let blocked = fixture
                .scripted_service(scenario())
                .start_run(
                    WorkflowKind::Fast,
                    "recover task",
                    &fixture.repo,
                    Some("fake"),
                )
                .unwrap();
            assert_eq!(blocked.details.status, RunStatus::Running);
            let completed = fixture
                .scripted_service(scenario())
                .resume_run(blocked.details.id)
                .unwrap();
            assert_eq!(completed.details.status, RunStatus::Completed);
        }
    }

    #[test]
    fn provider_delay_is_reported_without_busy_spin_or_speculative_events() {
        let fixture = Fixture::new();
        let scenario = || {
            FakeScenario::new().stage("implementation").events([
                FakeEvent::Started,
                FakeEvent::delay("external-ready"),
                FakeEvent::Completed,
            ])
        };
        let blocked = fixture
            .scripted_service(scenario())
            .start_run(
                WorkflowKind::Fast,
                "delayed task",
                &fixture.repo,
                Some("fake"),
            )
            .unwrap();
        assert!(matches!(
            blocked.outcome,
            QuiescentState::WaitingForProvider { ref stage_id }
                if stage_id.as_str() == "implementation"
        ));
        let resumed = fixture
            .scripted_service(scenario())
            .resume_run(blocked.details.id)
            .unwrap();
        assert!(matches!(
            resumed.outcome,
            QuiescentState::WaitingForProvider { .. }
        ));
        assert!(resumed.committed_events.is_empty());
    }

    #[test]
    fn restart_continues_after_last_checkpoint_without_replay() {
        let fixture = Fixture::new();
        let factory = GatedFactory::default();
        let released = Arc::clone(&factory.released);
        let service = RunService::new(
            fixture.database.clone(),
            fixture.worktrees.clone(),
            factory.clone(),
        );
        let blocked = service
            .start_run(
                WorkflowKind::Fast,
                "checkpoint task",
                &fixture.repo,
                Some("fake"),
            )
            .unwrap();
        assert!(matches!(
            blocked.outcome,
            QuiescentState::WaitingForProvider { .. }
        ));
        assert!(blocked.committed_events.iter().any(|event| matches!(
            &event.kind,
            DomainEventKind::ProviderProgress { message, .. }
                if message == "checkpoint before process exit"
        )));
        let run_id = blocked.details.id;
        drop(service);

        released.store(true, Ordering::SeqCst);
        let restarted =
            RunService::new(fixture.database.clone(), fixture.worktrees.clone(), factory);
        let completed = restarted.resume_run(run_id).unwrap();
        assert_eq!(completed.details.status, RunStatus::Completed);
        assert!(!completed.committed_events.iter().any(|event| matches!(
            &event.kind,
            DomainEventKind::ProviderStarted { .. } | DomainEventKind::ProviderProgress { .. }
        )));
    }

    #[test]
    fn failed_run_requires_retry_and_empty_apply_is_successful_no_op() {
        let fixture = Fixture::new();
        let scenario = || {
            FakeScenario::new()
                .stage("implementation")
                .events([FakeEvent::Started, FakeEvent::failed("compile failed")])
        };
        let failed = fixture
            .scripted_service(scenario())
            .start_run(
                WorkflowKind::Fast,
                "failing task",
                &fixture.repo,
                Some("fake"),
            )
            .unwrap();
        assert_eq!(failed.details.status, RunStatus::Failed);
        let resumed = fixture
            .scripted_service(scenario())
            .resume_run(failed.details.id)
            .unwrap();
        assert!(resumed.committed_events.is_empty());
        let retried = fixture
            .scripted_service(scenario())
            .retry_stage(
                failed.details.id,
                &crate::domain::StageId::new("implementation").unwrap(),
            )
            .unwrap();
        assert_eq!(retried.details.status, RunStatus::Failed);
        assert!(
            retried
                .committed_events
                .iter()
                .any(|event| matches!(&event.kind, DomainEventKind::StageRetryScheduled))
        );

        let complete = fixture
            .default_service()
            .start_run(
                WorkflowKind::Fast,
                "empty apply",
                &fixture.repo,
                Some("fake"),
            )
            .unwrap();
        let (outcome, applied) = fixture
            .default_service()
            .apply_run(complete.details.id)
            .unwrap();
        assert_eq!(outcome, ApplyOutcome::NoChanges);
        assert_eq!(applied.details.status, RunStatus::Completed);
        let discarded = fixture
            .default_service()
            .discard_run(complete.details.id)
            .unwrap();
        assert_eq!(discarded.details.status, RunStatus::Discarded);
        assert_eq!(
            discarded.details.workspace_status,
            Some(crate::workspace::WorkspaceStatus::Removed)
        );
    }

    #[test]
    fn apply_moves_only_worktree_delta_after_explicit_command() {
        let fixture = Fixture::new();
        let complete = fixture
            .default_service()
            .start_run(
                WorkflowKind::Fast,
                "apply integration",
                &fixture.repo,
                Some("fake"),
            )
            .unwrap();
        let run_id = complete.details.id;
        let store = SqliteStore::open(&fixture.database).unwrap();
        let workspace = store.load_workspace(run_id).unwrap().unwrap();
        fs::write(
            workspace.worktree_path().join("README.md"),
            "changed by run\n",
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(fixture.repo.join("README.md")).unwrap(),
            "baseline\n"
        );
        drop(store);

        let (outcome, report) = fixture.default_service().apply_run(run_id).unwrap();
        assert_eq!(outcome, ApplyOutcome::Applied);
        assert_eq!(report.details.status, RunStatus::Applied);
        assert_eq!(
            fs::read_to_string(fixture.repo.join("README.md")).unwrap(),
            "changed by run\n"
        );
    }

    #[test]
    fn missing_provider_is_rejected_before_database_creation() {
        let fixture = Fixture::new();
        let error = fixture
            .default_service()
            .start_run(WorkflowKind::Fast, "task", &fixture.repo, None)
            .unwrap_err();
        assert!(matches!(error, AppError::NoProductionProvider));
        assert!(!fixture.database.exists());
    }

    #[test]
    fn legacy_runs_remain_inspectable_but_resume_fails_with_precise_boundary() {
        let fixture = Fixture::new();
        let created_at = now();
        let run_id = RunId::from_u128(9_000_001);
        let config_id = ConfigSnapshotId::new("legacy-config").unwrap();
        let run = Run::new(
            run_id,
            WorkflowDefinition::built_in(WorkflowKind::Fast),
            config_id.clone(),
            created_at,
        );
        let config = ResolvedConfigSnapshot::new(
            config_id,
            1,
            serde_json::json!({"provider": "fake"}),
            created_at,
        )
        .unwrap();
        let event = run.created_event(EventMetadata::new(EventId::new(), created_at));
        let mut store = SqliteStore::open(&fixture.database).unwrap();
        store.create_run(&run, &config, &[event]).unwrap();
        WorkspaceManager::new(&fixture.worktrees)
            .prepare_run_workspace(&mut store, run_id, &fixture.repo)
            .unwrap();
        drop(store);

        let service = fixture.default_service();
        assert_eq!(service.inspect_run(run_id).unwrap().task, None);
        assert!(matches!(
            service.resume_run(run_id),
            Err(AppError::LegacyRunInput(id)) if id == run_id
        ));
    }

    #[test]
    fn unsupported_legacy_execution_config_is_not_guessed() {
        let fixture = Fixture::new();
        let created_at = now();
        let run_id = RunId::from_u128(9_000_002);
        let config_id = ConfigSnapshotId::new("legacy-execution-config").unwrap();
        let run = Run::new(
            run_id,
            WorkflowDefinition::built_in(WorkflowKind::Fast),
            config_id.clone(),
            created_at,
        );
        let input = RunInput::new(run_id, "legacy task", created_at).unwrap();
        let config = ResolvedConfigSnapshot::new(
            config_id,
            1,
            serde_json::json!({"provider": "fake"}),
            created_at,
        )
        .unwrap();
        let event = run.created_event(EventMetadata::new(EventId::new(), created_at));
        let mut store = SqliteStore::open(&fixture.database).unwrap();
        store
            .create_run_with_input(&run, &input, &config, &[event])
            .unwrap();
        WorkspaceManager::new(&fixture.worktrees)
            .prepare_run_workspace(&mut store, run_id, &fixture.repo)
            .unwrap();
        drop(store);

        assert!(matches!(
            fixture.default_service().resume_run(run_id),
            Err(AppError::LegacyExecutionConfig(id)) if id == run_id
        ));
    }

    fn git(path: &Path, args: &[&str]) {
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

    fn git_output(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
}
