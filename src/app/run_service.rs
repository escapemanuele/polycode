use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::domain::{
    AttentionRequestId, ConfigSnapshotId, EffortSetting, EventId, EventMetadata, Run, RunId,
    RunStatus, RunTransition, StageId, StageStatus, WorkflowDefinition, WorkflowKind,
};
use crate::engine::{EngineStatus, WorkflowEngine};
use crate::git::GitRepository;
use crate::process::{ManagedProcessStatus, ProcessManager};
use crate::store::{ResolvedConfigSnapshot, RunInput, SqliteStore, database_file, worktree_root};
use crate::workspace::{ReconciliationOutcome, WorkspaceError, WorkspaceManager};

use super::provider_factory::{ProviderFactory, ProviderResolver};
use super::{
    AppError, ArtifactSummary, ArtifactView, CommittedEvent, ExecutionSelection, ProcessLogView,
    RunDetails, RunDiffPreview, RunListItem, StageExecutionEvidence, query,
};

const DIFF_PREVIEW_LIMIT: usize = 2 * 1024 * 1024;
const PROCESS_LOG_TAIL_LIMIT: usize = 256 * 1024;
/// How many times a stop retries after losing a revision race with the process
/// that is still driving the run. A stop reconciles through the engine, which
/// touches the run, its managed processes, and its provider sessions, so a busy
/// driver offers several rows to collide on. Bounded well under a second in
/// total: the driver is being interrupted, so contention ends quickly.
const STOP_CONCURRENCY_ATTEMPTS: u32 = 12;
const STOP_CONCURRENCY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(40);
/// How long a run must sit untouched before a read is allowed to settle it.
///
/// Reading a run must never race the process driving it. A driver commits on
/// every provider signal it observes, so a run still under execution keeps a
/// fresh `updated_at`; only a run nothing has touched for this long is treated
/// as abandoned. Long enough to cover a stage handover, short enough that a
/// dead run tells the truth on the next refresh instead of the next stop.
const ABANDONED_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

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
    abandoned_after: std::time::Duration,
}

impl<F> RunService<F>
where
    F: ProviderResolver,
{
    #[must_use]
    pub const fn new(database: PathBuf, worktrees: PathBuf, provider_factory: F) -> Self {
        Self {
            database,
            worktrees,
            provider_factory,
            abandoned_after: ABANDONED_AFTER,
        }
    }

    /// Overrides how long a run must sit untouched before a read settles it.
    ///
    /// The default, [`ABANDONED_AFTER`], is what keeps a read off a run some
    /// other process is still driving. Only a caller that owns both sides of
    /// that race has any business shortening it.
    #[must_use]
    pub const fn abandoning_after(mut self, idle: std::time::Duration) -> Self {
        self.abandoned_after = idle;
        self
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
        selection: Option<ExecutionSelection>,
        effort: EffortSetting,
    ) -> Result<ExecutionReport, AppError>
    where
        F: ProviderFactory,
    {
        let created_at = now();
        let workflow = WorkflowDefinition::built_in(workflow_kind);
        let run_id = RunId::new();
        let config_id = ConfigSnapshotId::new(format!("config-{run_id}"))?;
        let selection = selection.ok_or(AppError::NoProductionProvider)?;
        let config = self.provider_factory.config_for_new_run(
            selection,
            effort,
            &workflow,
            config_id.clone(),
            created_at,
        )?;
        self.start_run_with_config_at(
            workflow,
            task.into(),
            repository_path.as_ref(),
            &config,
            run_id,
            created_at,
        )
    }

    pub(crate) fn start_run_with_config(
        &self,
        workflow_kind: WorkflowKind,
        task: impl Into<String>,
        repository_path: impl AsRef<Path>,
        config: &ResolvedConfigSnapshot,
    ) -> Result<ExecutionReport, AppError> {
        self.start_run_with_config_at(
            WorkflowDefinition::built_in(workflow_kind),
            task.into(),
            repository_path.as_ref(),
            config,
            RunId::new(),
            now(),
        )
    }

    fn start_run_with_config_at(
        &self,
        workflow: WorkflowDefinition,
        task: String,
        repository_path: &Path,
        config: &ResolvedConfigSnapshot,
        run_id: RunId,
        created_at: DateTime<Utc>,
    ) -> Result<ExecutionReport, AppError> {
        let repository = GitRepository::discover(repository_path)?;
        // A managed worktree is created from the source repository's committed
        // HEAD, so uncommitted work would be invisible to the agent while
        // remaining visible to the user. Apply already refuses a dirty source;
        // refusing at the start means the disagreement never happens, and it
        // runs before any run ID, config, input, event, or workspace intent is
        // persisted, so a rejected start leaves nothing behind.
        if !crate::git::source_is_clean(&crate::git::Git::default(), &repository)? {
            return Err(AppError::DirtySourceRepository);
        }
        let input = RunInput::new(run_id, task, created_at)?;
        let config_id = config.id().clone();
        let run = Run::new(run_id, workflow, config_id, created_at);
        let created = run.created_event(EventMetadata::new(EventId::new(), created_at));
        let mut store = SqliteStore::open(&self.database)?;
        store.create_run_with_input(&run, &input, config, &[created])?;

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

    /// Resolves attention only when provider policy explicitly proves safe
    /// disposable-eval continuation. Returns `None` for human-required input.
    pub(crate) fn auto_resolve_attention(
        &self,
        run_id: RunId,
        request_id: AttentionRequestId,
    ) -> Result<Option<ExecutionReport>, AppError> {
        let mut store = SqliteStore::open(&self.database)?;
        let before = last_sequence(&store, run_id)?;
        self.reconcile(&mut store, run_id)?;
        let mut engine = self.engine(&mut store, run_id)?;
        if !engine.can_auto_resolve_attention(&mut store, run_id, request_id)? {
            return Ok(None);
        }
        engine.resolve_attention(&mut store, run_id, request_id)?;
        let status = drive_attached(&mut engine, &mut store, run_id)?;
        Ok(Some(Self::report(
            &mut store,
            run_id,
            before,
            Some(&status),
        )?))
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

    /// Sends a completed run back for one remediation cycle, then executes to
    /// quiescence.
    ///
    /// The run grows a fix stage and a fresh decision over it, keeping its
    /// workspace, artifacts and identity. Polycode never reads the previous
    /// verdict to decide whether this is warranted: the decision artifact is
    /// prose written for a person, and the operator asking is the whole
    /// signal. The fix stage reads the verdict it is answering as a dependency
    /// artifact, together with the run's immutable task.
    ///
    /// # Errors
    /// Returns identity, lifecycle, guard, persistence, or provider errors.
    /// A run that is not completed, whose workspace is gone, or whose apply is
    /// under way is refused without being modified.
    pub fn request_fix(&self, run_id: RunId) -> Result<ExecutionReport, AppError> {
        let mut store = SqliteStore::open(&self.database)?;
        let before = last_sequence(&store, run_id)?;
        self.reconcile(&mut store, run_id)?;
        // Ask before committing to anything. Requesting a fix appends stages
        // and gives the workspace a branch, and a configuration sealed before
        // fix-cycle routing existed cannot execute those stages — nor can the
        // run be read afterwards, because reading it resolves its routes.
        let loaded = store.load_run(run_id)?;
        if let Some(role) =
            crate::app::unroutable_fix_role(&loaded.config_snapshot, loaded.run.workflow())?
        {
            return Err(AppError::UnroutableFixCycle { run_id, role });
        }
        // A review's workspace is detached, because a review is not meant to
        // produce changes. This is the request that changes that, so the
        // workspace earns its branch here rather than leaving the fix to write
        // into a tree apply would later refuse to transfer.
        WorkspaceManager::new(&self.worktrees).adopt_branch_for_fix(&mut store, run_id)?;
        let mut engine = self.engine(&mut store, run_id)?;
        engine.request_fix(&mut store, run_id)?;
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

    /// Stops execution while preserving everything the run produced.
    ///
    /// Stop is not disposition: the workspace, its changes, artifacts, logs,
    /// and provider session identity all survive, and `resume_run` recovers
    /// through the existing `Interrupted -> Recover` path rather than starting
    /// a new logical run or a new stage attempt.
    ///
    /// Ordering matters for crash safety. Managed processes are interrupted
    /// first — `ProcessManager::interrupt` persists its intent before
    /// signalling — and the run-level interruption is committed afterwards. A
    /// crash in between leaves interrupted processes under a still-running
    /// run, which the existing reconciliation and `ProviderSignal::Interrupted`
    /// path already resolves. The reverse order could leave a run marked
    /// stopped while its provider kept working.
    ///
    /// # Errors
    /// Returns reconciliation, process, lifecycle, or store errors. Runs that
    /// have reached a terminal disposition cannot be stopped.
    pub fn stop_run(&self, run_id: RunId) -> Result<ExecutionReport, AppError> {
        // A run is normally stopped while another Polycode process is still
        // driving it, and that driver keeps reconciling the same managed
        // process rows. Losing an optimistic-concurrency race here used to
        // abort the stop after the provider had already been signalled,
        // reporting failure while leaving the run marked Running with an
        // interrupted stage. Retry the whole stop instead: every step is
        // idempotent, so a later attempt observes the newer revisions and
        // commits the run-level interruption the user asked for.
        let mut attempt = 0;
        loop {
            match self.stop_run_once(run_id) {
                Err(error) if error.is_concurrent_modification() => {
                    attempt += 1;
                    if attempt >= STOP_CONCURRENCY_ATTEMPTS {
                        return Err(error);
                    }
                    std::thread::sleep(STOP_CONCURRENCY_BACKOFF);
                }
                outcome => return outcome,
            }
        }
    }

    fn stop_run_once(&self, run_id: RunId) -> Result<ExecutionReport, AppError> {
        let mut store = SqliteStore::open(&self.database)?;
        let before = last_sequence(&store, run_id)?;
        self.reconcile(&mut store, run_id)?;
        let loaded = store.load_run(run_id)?;
        match loaded.run.status() {
            // Already stopped: report the existing state rather than
            // committing a second interruption.
            RunStatus::Interrupted => return Self::report(&mut store, run_id, before, None),
            RunStatus::Running | RunStatus::NeedsUser => {}
            status => return Err(AppError::RunNotStoppable(run_id, status)),
        }
        let process_manager = ProcessManager::from_environment()?;
        for process in store.list_managed_processes(run_id)? {
            let inspection = process_manager.inspect(&mut store, process.id())?;
            if inspection.process.status().is_active() {
                // Interrupt only. Cleanup would remove the durable process
                // record this run needs in order to recover.
                process_manager.interrupt(&mut store, process.id())?;
            }
        }
        // Signalling the provider is not the same as recording that the stage
        // stopped. Let the engine observe the now-interrupted processes and
        // commit the stage-level interruption before the run-level one.
        // Without it the stage still reads Running under an Interrupted run,
        // and the first resume recovers only the run, leaving the user to
        // issue resume a second time.
        //
        // Observe, never Continue: the adapters decide on their own whether to
        // resume, from the persisted session status against the stage status.
        // A run carrying a stale NeedsUser session with a still-Running stage
        // would otherwise have its provider resumed by the very command asking
        // to halt it — and a pending permission that cannot be replayed safely
        // would surface as a stop failure.
        self.drive(&mut store, run_id, ResumeAction::Observe)?;
        let loaded = store.load_run(run_id)?;
        match loaded.run.status() {
            RunStatus::Running | RunStatus::NeedsUser => {}
            // Observing can settle the run on its own. A process that had
            // already exited resolves a stale Running into the run's true
            // resting state — interrupted, or failed for a process that died
            // without a result. The run is stopped either way, so report what
            // it actually is instead of forcing an Interrupt the domain would
            // rightly reject.
            _ => return Self::report(&mut store, run_id, before, None),
        }
        let mut run = loaded.run;
        let at = now();
        let event = run.transition(
            RunTransition::Interrupt,
            EventMetadata::new(EventId::new(), at),
        )?;
        store.commit_run_update(&run, loaded.revision, &[event])?;
        Self::report(&mut store, run_id, before, None)
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

    /// Sets whether a run is hidden from the default Runs list. Pure list
    /// metadata: no lifecycle transition, no revision bump, no process or
    /// workspace side effects.
    ///
    /// # Errors
    /// Returns store or path errors; unknown runs surface as `RunNotFound`.
    pub fn set_run_hidden(&self, run_id: RunId, hidden: bool) -> Result<(), AppError> {
        let mut store = SqliteStore::open(&self.database)?;
        store.set_run_hidden(run_id, hidden)?;
        Ok(())
    }

    /// Returns indexed summaries without creating a missing database.
    ///
    /// # Errors
    /// Returns query, migration, or path errors.
    pub fn list_runs(&self) -> Result<Vec<RunListItem>, AppError> {
        if !self.database.exists() {
            return Ok(Vec::new());
        }
        let mut store = SqliteStore::open(&self.database)?;
        let listed = query::list(&store)?;
        let running = listed
            .iter()
            .filter(|item| item.status == RunStatus::Running)
            .map(|item| item.id)
            .collect::<Vec<_>>();
        let mut settled = false;
        for run_id in running {
            settled |= self.settle_if_abandoned(&mut store, run_id);
        }
        if settled {
            return query::list(&store);
        }
        Ok(listed)
    }

    /// Returns detailed persisted state, settling the run first if it was
    /// abandoned.
    ///
    /// # Errors
    /// Returns not-found, corrupt state, or query errors.
    pub fn inspect_run(&self, run_id: RunId) -> Result<RunDetails, AppError> {
        let mut store = SqliteStore::open(&self.database)?;
        self.settle_if_abandoned(&mut store, run_id);
        query::inspect(&mut store, run_id)
    }

    /// Lists integrity-verified artifact metadata for one run.
    ///
    /// # Errors
    /// Returns persistence or artifact integrity failures.
    pub fn list_artifacts(&self, run_id: RunId) -> Result<Vec<ArtifactSummary>, AppError> {
        query::list_artifacts(&SqliteStore::open(&self.database)?, run_id)
    }

    /// Reads latest integrity-verified artifact for one stage.
    ///
    /// # Errors
    /// Returns not-found, non-UTF-8, persistence, or integrity failures.
    pub fn read_artifact(
        &self,
        run_id: RunId,
        stage_id: &StageId,
    ) -> Result<ArtifactView, AppError> {
        query::read_artifact(&SqliteStore::open(&self.database)?, run_id, stage_id)
    }

    /// Generates bounded read-only workspace diff from same delta semantics as apply.
    ///
    /// # Errors
    /// Returns workspace ownership/readiness or Git failures.
    pub fn preview_run_diff(&self, run_id: RunId) -> Result<RunDiffPreview, AppError> {
        let mut store = SqliteStore::open(&self.database)?;
        let preview = WorkspaceManager::new(&self.worktrees).preview_patch(
            &mut store,
            run_id,
            DIFF_PREVIEW_LIMIT,
        )?;
        Ok(query::summarize_diff(
            String::from_utf8_lossy(&preview.bytes).into_owned(),
            preview.total_bytes,
            preview.truncated,
        ))
    }

    /// Reads bounded stdout/stderr tails without acknowledging provider output.
    ///
    /// # Errors
    /// Returns process lookup, path, or retained-output failures.
    pub fn read_process_log_tail(
        &self,
        run_id: RunId,
        stage_id: &StageId,
    ) -> Result<ProcessLogView, AppError> {
        let store = SqliteStore::open(&self.database)?;
        let manager = ProcessManager::from_environment()?;
        query::process_log_tail(&store, &manager, run_id, stage_id, PROCESS_LOG_TAIL_LIMIT)
    }

    /// Returns provider-neutral evidence for exactly one stage.
    ///
    /// Usage excludes every other routed stage and reading does not mutate cursors or run state.
    ///
    /// # Errors
    /// Returns missing/corrupt run, stage, routing, event, or provider-session data.
    pub fn stage_execution_evidence(
        &self,
        run_id: RunId,
        stage_id: &StageId,
    ) -> Result<StageExecutionEvidence, AppError> {
        query::stage_execution_evidence(&mut SqliteStore::open(&self.database)?, run_id, stage_id)
    }

    /// Commits what already happened to a run nothing is driving any more.
    ///
    /// Nothing observes a run unless a command drives the engine, so a
    /// provider process that died leaves its run reading Running for as long
    /// as the user only ever reads it. Reads are how a user notices a run at
    /// all, so they settle it: a run whose processes have all ended and that
    /// nothing has touched for [`ABANDONED_AFTER`] is observed exactly the way
    /// a stop observes it, and the truth is committed once.
    ///
    /// Three conditions keep this from becoming a write on a live run. The run
    /// must be Running, no managed process may still be active — an active
    /// process means a driver is attached, and racing its commits would fail
    /// the run this call only meant to display — and the run must have been
    /// idle long enough that no driver can be mid-handover between processes.
    ///
    /// Best effort by construction: reading a run the engine cannot observe is
    /// still reading a run the user is entitled to see, so every failure here
    /// leaves the persisted state exactly as it was and the caller reports it
    /// unchanged. Returns whether observation ran.
    fn settle_if_abandoned(&self, store: &mut SqliteStore, run_id: RunId) -> bool {
        let Ok(loaded) = store.load_run(run_id) else {
            return false;
        };
        if loaded.run.status() != RunStatus::Running {
            return false;
        }
        let Ok(idle) = now()
            .signed_duration_since(*loaded.run.updated_at())
            .to_std()
        else {
            // Updated in the future: a clock this read cannot reason about.
            return false;
        };
        if idle < self.abandoned_after {
            return false;
        }
        let Ok(processes) = store.list_managed_processes(run_id) else {
            return false;
        };
        if processes.iter().any(|process| process.status().is_active()) {
            return false;
        }
        if self.reconcile(store, run_id).is_err() {
            return false;
        }
        // Observe, never Continue or Resume: a read must never become the
        // reason a provider starts working again.
        self.drive(store, run_id, ResumeAction::Observe).is_ok()
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
        let provider = self.provider_factory.resolve_for_run(
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
                _ => {}
            }
            // Recovering the run cascades stage *status* in the domain, but a
            // provider-backed stage also has to be resumed through the engine.
            // Reload so the cascade is visible, then bring every suspended
            // stage back in this same call — otherwise a stopped run reports
            // Running and needs a second resume to actually continue.
            let reloaded = store.load_run(run_id)?;
            resume_suspended_stages(&mut engine, store, &reloaded.run)?;
        }
        if action == ResumeAction::Observe {
            return Ok(Some(engine.drive_observing(store, run_id)?));
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
        if matches!(
            status,
            EngineStatus::WaitingForProvider {
                keep_attached: true,
                ..
            }
        ) {
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
    /// Record what already happened without ever starting or resuming
    /// provider work. The control-plane stop path, and nothing else.
    Observe,
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
    if let Some(EngineStatus::WaitingForProvider { stage_id, .. }) = engine_status {
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
    use std::collections::BTreeMap;
    use std::fs;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use tempfile::TempDir;

    use super::*;
    use crate::app::{DevelopmentFakeProviderFactory, ProviderFactory, UniformProvider};
    use crate::domain::{
        ArtifactId, ArtifactKind, ArtifactMetadata, ArtifactStatus, AttentionKind,
        ConfigSnapshotId, Dependency, DomainEventKind, ProviderId, Role, StageDefinition,
        StageKind,
    };
    use crate::engine::{
        FakeEvent, FakeProvider, FakeScenario, Provider, ProviderError, ProviderPoll,
        ProviderRequest,
    };
    use crate::process::{OutputStream, ProcessManager, TmuxBackend};
    use crate::providers::ArtifactRecord;
    use crate::store::{ResolvedConfigSnapshot, SequencedEvent, StoreError};

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
        fn provider_id_for(&self, request: &ProviderRequest) -> Result<ProviderId, ProviderError> {
            self.inner.provider_id_for(request)
        }

        fn supports_request(&self, request: &ProviderRequest) -> Result<bool, ProviderError> {
            self.inner.supports_request(request)
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
            selection: ExecutionSelection,
            effort: EffortSetting,
            workflow: &WorkflowDefinition,
            id: ConfigSnapshotId,
            created_at: DateTime<Utc>,
        ) -> Result<ResolvedConfigSnapshot, AppError> {
            DevelopmentFakeProviderFactory
                .config_for_new_run(selection, effort, workflow, id, created_at)
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
            selection: ExecutionSelection,
            effort: EffortSetting,
            workflow: &WorkflowDefinition,
            id: ConfigSnapshotId,
            created_at: DateTime<Utc>,
        ) -> Result<ResolvedConfigSnapshot, AppError> {
            DevelopmentFakeProviderFactory
                .config_for_new_run(selection, effort, workflow, id, created_at)
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
            selection: ExecutionSelection,
            effort: EffortSetting,
            workflow: &WorkflowDefinition,
            id: ConfigSnapshotId,
            created_at: DateTime<Utc>,
        ) -> Result<ResolvedConfigSnapshot, AppError> {
            DevelopmentFakeProviderFactory
                .config_for_new_run(selection, effort, workflow, id, created_at)
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
        temp: TempDir,
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
                temp,
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
                    Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                    EffortSetting::NativeDefault,
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

    /// The managed worktree is built from committed HEAD, so a dirty source
    /// would silently hide the user's work from the agent. Every start is
    /// refused before anything durable exists.
    #[test]
    fn a_dirty_source_repository_is_refused_before_any_run_is_persisted() {
        for (label, dirty) in [("tracked modification", true), ("untracked file", false)] {
            let fixture = Fixture::new();
            if dirty {
                fs::write(fixture.repo.join("README.md"), "uncommitted edit\n").unwrap();
            } else {
                fs::write(fixture.repo.join("scratch.txt"), "untracked\n").unwrap();
            }
            let error = fixture
                .default_service()
                .start_run(
                    WorkflowKind::Fast,
                    "task",
                    &fixture.repo,
                    Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                    EffortSetting::NativeDefault,
                )
                .unwrap_err();
            assert!(
                matches!(error, AppError::DirtySourceRepository),
                "{label}: {error:?}"
            );
            let message = error.to_string();
            assert!(message.contains("uncommitted changes"), "{label}");
            assert!(message.contains("Commit or stash"), "{label}");

            // Nothing durable may survive a refused start.
            assert!(
                !fixture.database.exists(),
                "{label}: a refused start created a database"
            );
            assert!(
                !fixture.worktrees.exists(),
                "{label}: a refused start created workspace resources"
            );
            assert_eq!(
                fixture.default_service().list_runs().unwrap().len(),
                0,
                "{label}: a refused start left a run behind"
            );
        }
    }

    /// Review workflows read the source too, so they take the same preflight.
    #[test]
    fn every_workflow_takes_the_dirty_source_preflight() {
        for kind in [
            WorkflowKind::Fast,
            WorkflowKind::Standard,
            WorkflowKind::Deep,
            WorkflowKind::Review,
        ] {
            let fixture = Fixture::new();
            fs::write(fixture.repo.join("README.md"), "uncommitted\n").unwrap();
            let error = fixture
                .default_service()
                .start_run(
                    kind,
                    "task",
                    &fixture.repo,
                    Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                    EffortSetting::NativeDefault,
                )
                .unwrap_err();
            assert!(matches!(error, AppError::DirtySourceRepository), "{kind:?}");
        }
    }

    /// Committing the same content makes the identical start succeed, so the
    /// preflight rejects dirtiness rather than the repository itself.
    #[test]
    fn a_clean_source_repository_starts_normally() {
        let fixture = Fixture::new();
        fs::write(fixture.repo.join("feature.txt"), "work\n").unwrap();
        git(&fixture.repo, &["add", "feature.txt"]);
        git(&fixture.repo, &["commit", "-qm", "feature"]);
        let report = fixture
            .default_service()
            .start_run(
                WorkflowKind::Fast,
                "task",
                &fixture.repo,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
            )
            .unwrap();
        assert_eq!(report.details.status, RunStatus::Completed);
    }

    /// The preflight applies to new runs only: an existing run resumes even if
    /// the source has since been modified.
    #[test]
    fn resuming_an_existing_run_is_not_subject_to_the_preflight() {
        let fixture = Fixture::new();
        let report = fixture
            .default_service()
            .start_run(
                WorkflowKind::Fast,
                "task",
                &fixture.repo,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
            )
            .unwrap();
        let run_id = report.details.id;
        fs::write(fixture.repo.join("README.md"), "dirty after the fact\n").unwrap();
        let resumed = fixture.default_service().resume_run(run_id).unwrap();
        assert_eq!(resumed.details.id, run_id);
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
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
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
    fn legacy_reviewer_snapshot_resumes_its_stored_graph_without_conversion() {
        let fixture = Fixture::new();
        let created_at = now();
        let run_id = RunId::from_u128(9_100_001);
        let config_id = ConfigSnapshotId::new("legacy-reviewer-config").unwrap();
        let id = |value: &str| StageId::new(value).unwrap();
        let workflow = WorkflowDefinition::new(
            WorkflowKind::Standard,
            vec![
                StageDefinition::new(
                    id("architecture"),
                    StageKind::Architecture,
                    Role::Architect,
                    vec![],
                ),
                StageDefinition::new(
                    id("implementation"),
                    StageKind::Implementation,
                    Role::Implementer,
                    vec![Dependency::required(id("architecture"))],
                ),
                StageDefinition::new(
                    id("review"),
                    StageKind::Review,
                    Role::Reviewer,
                    vec![Dependency::required(id("implementation"))],
                ),
                StageDefinition::new(
                    id("decision"),
                    StageKind::Decision,
                    Role::EngineeringLead,
                    vec![Dependency::required(id("review"))],
                ),
            ],
        )
        .unwrap();
        let run = Run::new(run_id, workflow.clone(), config_id.clone(), created_at);
        let input = RunInput::new(run_id, "legacy standard task", created_at).unwrap();
        let config = DevelopmentFakeProviderFactory
            .config_for_new_run(
                ExecutionSelection::Uniform(UniformProvider::Fake),
                EffortSetting::NativeDefault,
                &workflow,
                config_id,
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

        let reloaded = fixture.default_service().inspect_run(run_id).unwrap();
        assert_eq!(reloaded.stages.len(), 4);
        let completed = fixture.default_service().resume_run(run_id).unwrap();
        assert_eq!(completed.details.status, RunStatus::Completed);
        let loaded = SqliteStore::open(&fixture.database)
            .unwrap()
            .load_run(run_id)
            .unwrap()
            .run;
        assert_eq!(loaded.workflow(), &workflow);
        assert_eq!(
            loaded.workflow().stage(&id("review")).unwrap().role(),
            Role::Reviewer
        );
        assert!(loaded.workflow().stage(&id("quality_review")).is_none());
        assert!(loaded.workflow().stage(&id("spec_review")).is_none());
    }

    #[test]
    fn restart_after_one_specialized_review_does_not_replay_completed_branch() {
        let fixture = Fixture::new();
        let scenario = || {
            FakeScenario::new()
                .stage("architecture")
                .events([FakeEvent::Started, FakeEvent::Completed])
                .stage("implementation")
                .events([FakeEvent::Started, FakeEvent::Completed])
                .stage("quality_review")
                .events([FakeEvent::Started, FakeEvent::Completed])
                .stage("spec_review")
                .events([
                    FakeEvent::Started,
                    FakeEvent::Interrupted,
                    FakeEvent::Completed,
                ])
                .stage("decision")
                .events([FakeEvent::Started, FakeEvent::Completed])
        };
        let interrupted = fixture
            .scripted_service(scenario())
            .start_run(
                WorkflowKind::Standard,
                "restart specialized reviews",
                &fixture.repo,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
            )
            .unwrap();
        let run_id = interrupted.details.id;
        let quality = StageId::new("quality_review").unwrap();
        let spec = StageId::new("spec_review").unwrap();
        let decision = StageId::new("decision").unwrap();
        assert_eq!(
            interrupted
                .details
                .stages
                .iter()
                .find(|stage| stage.id == quality)
                .unwrap()
                .status,
            StageStatus::Completed
        );
        assert_eq!(
            interrupted
                .details
                .stages
                .iter()
                .find(|stage| stage.id == spec)
                .unwrap()
                .status,
            StageStatus::Interrupted
        );
        assert_eq!(
            interrupted
                .details
                .stages
                .iter()
                .find(|stage| stage.id == decision)
                .unwrap()
                .status,
            StageStatus::Pending
        );

        let completed = fixture
            .scripted_service(scenario())
            .resume_run(run_id)
            .unwrap();
        assert_eq!(completed.details.status, RunStatus::Completed);
        let events = SqliteStore::open(&fixture.database)
            .unwrap()
            .load_events(run_id)
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    event.event.stage_id() == Some(&quality)
                        && matches!(event.event.kind(), DomainEventKind::ProviderStarted { .. })
                })
                .count(),
            1
        );
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
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
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
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
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
                    Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                    EffortSetting::NativeDefault,
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

    /// A run held open by the provider, so Stop has something to interrupt.
    fn delayed_scenario() -> FakeScenario {
        FakeScenario::new().stage("implementation").events([
            FakeEvent::Started,
            FakeEvent::delay("external-ready"),
            FakeEvent::Completed,
        ])
    }

    #[test]
    fn stop_interrupts_execution_and_keeps_everything_the_run_produced() {
        let fixture = Fixture::new();
        let running = fixture
            .scripted_service(delayed_scenario())
            .start_run(
                WorkflowKind::Fast,
                "stoppable task",
                &fixture.repo,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
            )
            .unwrap();
        let run_id = running.details.id;
        assert_eq!(running.details.status, RunStatus::Running);
        let workspace_before = {
            let store = SqliteStore::open(&fixture.database).unwrap();
            store.load_workspace(run_id).unwrap().unwrap()
        };

        let stopped = fixture
            .scripted_service(delayed_scenario())
            .stop_run(run_id)
            .unwrap();
        assert_eq!(stopped.details.status, RunStatus::Interrupted);
        assert!(
            stopped
                .details
                .stages
                .iter()
                .all(|stage| stage.status != StageStatus::Running),
            "no stage is left executing"
        );

        // Stop is not disposition: the workspace and its worktree survive.
        let store = SqliteStore::open(&fixture.database).unwrap();
        let workspace_after = store.load_workspace(run_id).unwrap().unwrap();
        assert_eq!(workspace_after.status(), workspace_before.status());
        assert_eq!(
            workspace_after.worktree_path(),
            workspace_before.worktree_path()
        );
        assert!(workspace_after.worktree_path().exists());
    }

    #[test]
    fn a_stopped_run_survives_restart_and_resumes_without_a_new_attempt() {
        let fixture = Fixture::new();
        let started = fixture
            .scripted_service(delayed_scenario())
            .start_run(
                WorkflowKind::Fast,
                "restartable task",
                &fixture.repo,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
            )
            .unwrap();
        let run_id = started.details.id;
        fixture
            .scripted_service(delayed_scenario())
            .stop_run(run_id)
            .unwrap();

        // A brand-new service instance stands in for a process restart.
        let after_restart = fixture
            .scripted_service(delayed_scenario())
            .inspect_run(run_id)
            .unwrap();
        assert_eq!(after_restart.status, RunStatus::Interrupted);

        let attempts_before = attempt_count(&fixture, run_id);
        let resumed = fixture
            .scripted_service(delayed_scenario())
            .resume_run(run_id)
            .unwrap();
        assert_eq!(resumed.details.id, run_id, "the same logical run continues");
        // One resume must recover the run AND its stages. Asserting merely
        // "no longer Interrupted" at the run level hid a real defect: the
        // run-level recovery landed while the stage stayed suspended, so the
        // run sat in Running and the user had to issue resume a second time.
        // This scenario blocks on its delay gate, so Running is the correct
        // resting state here — what matters is that no stage is left behind.
        assert_ne!(
            resumed.details.status,
            RunStatus::Interrupted,
            "recovery leaves the interrupted state"
        );
        assert!(
            resumed
                .details
                .stages
                .iter()
                .all(|stage| stage.status != StageStatus::Interrupted),
            "one resume left a stage suspended, forcing a second resume"
        );
        assert_eq!(
            attempt_count(&fixture, run_id),
            attempts_before,
            "stopping never creates a retry attempt"
        );
    }

    /// Stop reconciles through the engine, and the provider adapters decide
    /// for themselves whether a poll should resume, from the persisted session
    /// status against the stage status. Only an observing pass tells them not
    /// to. This is a fence, not a behaviour test: the behaviour is covered by
    /// the Claude adapter's stale-NeedsUser test, but nothing there notices if
    /// this call site quietly goes back to a resuming action.
    #[test]
    fn stop_reconciles_without_ever_authorising_provider_work() {
        // Only the non-test half of this file is the call graph under test.
        let source = include_str!("run_service.rs");
        let code = source.split("#[cfg(test)]").next().unwrap();
        let stop = code
            .split("fn stop_run_once")
            .nth(1)
            .expect("stop_run_once must exist")
            .split("\n    /// ")
            .next()
            .expect("stop_run_once body");
        assert!(
            stop.contains("ResumeAction::Observe"),
            "stop must drive the engine in observing mode"
        );
        assert!(
            !stop.contains("ResumeAction::Continue") && !stop.contains("ResumeAction::Resume"),
            "stop must never drive with an action that lets an adapter resume"
        );
    }

    /// A provider process that died is a process nothing will ever drive
    /// again, so the run it belonged to has already ended — only the record
    /// still says otherwise. Reading the run is how a user notices it at all,
    /// so the read has to commit that truth rather than report work that
    /// stopped hours ago as still running.
    #[test]
    fn reading_settles_a_run_whose_provider_already_ended() {
        let fixture = Fixture::new();
        let started = fixture
            .scripted_service(delayed_scenario())
            .start_run(
                WorkflowKind::Fast,
                "abandoned task",
                &fixture.repo,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
            )
            .unwrap();
        let run_id = started.details.id;
        assert_eq!(started.details.status, RunStatus::Running);

        let details = fixture
            .scripted_service(ended_scenario())
            .abandoning_after(std::time::Duration::ZERO)
            .inspect_run(run_id)
            .unwrap();
        assert_eq!(
            details.status,
            RunStatus::Failed,
            "reading an abandoned run reports what actually happened to it"
        );
        assert!(
            details
                .stages
                .iter()
                .all(|stage| stage.status != StageStatus::Running),
            "no stage is left advertising execution"
        );
    }

    /// The run list is the first thing a user sees, and it lies in exactly the
    /// same way the detail view does.
    #[test]
    fn listing_settles_a_run_whose_provider_already_ended() {
        let fixture = Fixture::new();
        let started = fixture
            .scripted_service(delayed_scenario())
            .start_run(
                WorkflowKind::Fast,
                "abandoned task",
                &fixture.repo,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
            )
            .unwrap();
        let run_id = started.details.id;

        let listed = fixture
            .scripted_service(ended_scenario())
            .abandoning_after(std::time::Duration::ZERO)
            .list_runs()
            .unwrap();
        let item = listed
            .iter()
            .find(|item| item.id == run_id)
            .expect("the run must still be listed");
        assert_eq!(item.status, RunStatus::Failed);
    }

    /// A run is normally read while another process is still driving it, and
    /// that driver owns every row a settling read would touch. The idle grace
    /// is what keeps the two apart: a run touched a moment ago is a run
    /// someone else is working on, and reading it must change nothing.
    #[test]
    fn reading_leaves_a_run_another_process_is_still_driving_alone() {
        let fixture = Fixture::new();
        let started = fixture
            .scripted_service(delayed_scenario())
            .start_run(
                WorkflowKind::Fast,
                "driven task",
                &fixture.repo,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
            )
            .unwrap();
        let run_id = started.details.id;
        let revision_before = SqliteStore::open(&fixture.database)
            .unwrap()
            .load_run(run_id)
            .unwrap()
            .revision;

        let details = fixture
            .scripted_service(ended_scenario())
            .inspect_run(run_id)
            .unwrap();
        assert_eq!(
            details.status,
            RunStatus::Running,
            "a run inside its idle grace is left to its driver"
        );
        assert_eq!(
            SqliteStore::open(&fixture.database)
                .unwrap()
                .load_run(run_id)
                .unwrap()
                .revision,
            revision_before,
            "reading commits nothing while the grace holds"
        );
    }

    /// Settling on a read is the one place a read touches the engine, and it
    /// has two fences. It must observe, never resume — a read is not a reason
    /// for a provider to start working — and it must refuse to settle a run
    /// whose process is still alive, because that process is a driver whose
    /// commits this read would lose a revision race with. Both are call-site
    /// properties nothing downstream would notice going missing.
    #[test]
    fn reading_settles_without_ever_authorising_provider_work() {
        // Only the non-test half of this file is the call graph under test.
        let source = include_str!("run_service.rs");
        let code = source.split("#[cfg(test)]").next().unwrap();
        let settle = code
            .split("fn settle_if_abandoned(")
            .nth(1)
            .expect("settle_if_abandoned must exist")
            .split("\n    fn ")
            .next()
            .expect("settle_if_abandoned body");
        assert!(
            settle.contains("ResumeAction::Observe"),
            "a read must drive the engine in observing mode"
        );
        assert!(
            !settle.contains("ResumeAction::Continue") && !settle.contains("ResumeAction::Resume"),
            "a read must never drive with an action that lets an adapter resume"
        );
        assert!(
            settle.contains("is_active"),
            "a read must never settle a run whose process is still alive"
        );
    }

    /// A provider that has already ended: the stage reports its failure the
    /// moment anything polls it again.
    fn ended_scenario() -> FakeScenario {
        FakeScenario::new().stage("implementation").events([
            FakeEvent::Started,
            FakeEvent::failed("provider process ended without a result"),
        ])
    }

    /// Total stage attempts recorded for the run, so a Stop can be shown not
    /// to have created one.
    fn attempt_count(fixture: &Fixture, run_id: RunId) -> usize {
        let store = SqliteStore::open(&fixture.database).unwrap();
        store
            .load_events(run_id)
            .unwrap()
            .iter()
            .filter(|event| {
                matches!(
                    event.event.kind(),
                    crate::domain::DomainEventKind::StageStarted
                )
            })
            .count()
    }

    #[test]
    fn stopping_an_already_stopped_run_is_safe() {
        let fixture = Fixture::new();
        let started = fixture
            .scripted_service(delayed_scenario())
            .start_run(
                WorkflowKind::Fast,
                "idempotent stop",
                &fixture.repo,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
            )
            .unwrap();
        let run_id = started.details.id;
        fixture
            .scripted_service(delayed_scenario())
            .stop_run(run_id)
            .unwrap();
        let again = fixture
            .scripted_service(delayed_scenario())
            .stop_run(run_id)
            .unwrap();
        assert_eq!(again.details.status, RunStatus::Interrupted);
        assert!(
            again.committed_events.is_empty(),
            "a second stop commits nothing"
        );
    }

    #[test]
    fn a_finished_run_cannot_be_stopped_and_stop_never_discards() {
        let fixture = Fixture::new();
        let completed = fixture
            .default_service()
            .start_run(
                WorkflowKind::Fast,
                "finished task",
                &fixture.repo,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
            )
            .unwrap();
        let run_id = completed.details.id;
        let error = fixture.default_service().stop_run(run_id).unwrap_err();
        assert!(
            matches!(error, AppError::RunNotStoppable(_, RunStatus::Completed)),
            "{error:?}"
        );
        // The refusal changed nothing, least of all into a disposition.
        let details = fixture.default_service().inspect_run(run_id).unwrap();
        assert_eq!(details.status, RunStatus::Completed);

        fixture.default_service().discard_run(run_id).unwrap();
        let discarded = fixture.default_service().inspect_run(run_id).unwrap();
        assert_eq!(
            discarded.status,
            RunStatus::Discarded,
            "discard semantics are unchanged"
        );
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
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
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
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
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
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
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
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
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
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
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
    fn diff_preview_is_bounded_read_only_and_apply_uses_same_delta() {
        let fixture = Fixture::new();
        fs::write(fixture.repo.join("delete-me.txt"), "remove me\n").unwrap();
        git(&fixture.repo, &["add", "delete-me.txt"]);
        git(&fixture.repo, &["commit", "-qm", "add deletion fixture"]);
        let service = fixture.default_service();
        let complete = service
            .start_run(
                WorkflowKind::Fast,
                "preview integration",
                &fixture.repo,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
            )
            .unwrap();
        let run_id = complete.details.id;
        let mut store = SqliteStore::open(&fixture.database).unwrap();
        let workspace = store.load_workspace(run_id).unwrap().unwrap();
        fs::write(
            workspace.worktree_path().join("README.md"),
            "preview change\n",
        )
        .unwrap();
        fs::remove_file(workspace.worktree_path().join("delete-me.txt")).unwrap();
        fs::write(workspace.worktree_path().join("new file.txt"), "new\n").unwrap();
        let source_before = git_output(&fixture.repo, &["status", "--porcelain"]);
        let index_before = git_output(workspace.worktree_path(), &["diff", "--cached"]);
        let worktree_before = git_output(workspace.worktree_path(), &["status", "--porcelain"]);
        let loaded_before = store.load_run(run_id).unwrap();
        let events_before = store.load_events(run_id).unwrap().len();
        let workspace_before = store.load_workspace(run_id).unwrap().unwrap();
        assert!(store.load_apply_operation(run_id).unwrap().is_none());
        drop(store);

        let preview = service.preview_run_diff(run_id).unwrap();
        assert!(!preview.truncated);
        assert!(preview.text.contains("preview change"));
        assert!(
            preview
                .changed_files
                .iter()
                .any(|file| file.path == "README.md")
        );
        assert!(
            preview
                .changed_files
                .iter()
                .any(|file| file.path == "delete-me.txt")
        );
        assert!(
            preview
                .changed_files
                .iter()
                .any(|file| file.path == "new file.txt")
        );

        let mut store = SqliteStore::open(&fixture.database).unwrap();
        let loaded_after = store.load_run(run_id).unwrap();
        let workspace_after = store.load_workspace(run_id).unwrap().unwrap();
        assert_eq!(loaded_after.revision, loaded_before.revision);
        assert_eq!(store.load_events(run_id).unwrap().len(), events_before);
        assert_eq!(workspace_after.status(), workspace_before.status());
        assert_eq!(workspace_after.revision(), workspace_before.revision());
        assert!(store.load_apply_operation(run_id).unwrap().is_none());
        assert_eq!(
            git_output(&fixture.repo, &["status", "--porcelain"]),
            source_before
        );
        assert_eq!(
            git_output(workspace.worktree_path(), &["diff", "--cached"]),
            index_before
        );
        assert_eq!(
            git_output(workspace.worktree_path(), &["status", "--porcelain"]),
            worktree_before
        );
        drop(store);

        let (outcome, report) = service.apply_run(run_id).unwrap();
        assert_eq!(outcome, ApplyOutcome::Applied);
        assert_eq!(report.details.status, RunStatus::Applied);
        assert_eq!(
            fs::read_to_string(fixture.repo.join("README.md")).unwrap(),
            "preview change\n"
        );
        assert!(!fixture.repo.join("delete-me.txt").exists());
        assert_eq!(
            fs::read_to_string(fixture.repo.join("new file.txt")).unwrap(),
            "new\n"
        );
        assert!(git_output(&fixture.repo, &["diff", "--cached"]).is_empty());
    }

    #[test]
    fn diff_preview_truncates_large_workspace_output_before_returning_it() {
        let fixture = Fixture::new();
        let service = fixture.default_service();
        let complete = service
            .start_run(
                WorkflowKind::Fast,
                "large preview",
                &fixture.repo,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
            )
            .unwrap();
        let store = SqliteStore::open(&fixture.database).unwrap();
        let workspace = store.load_workspace(complete.details.id).unwrap().unwrap();
        fs::write(
            workspace.worktree_path().join("generated.txt"),
            "unique preview line 0123456789\n".repeat(100_000),
        )
        .unwrap();
        drop(store);

        let preview = service.preview_run_diff(complete.details.id).unwrap();
        assert!(preview.truncated);
        assert_eq!(preview.text.len(), DIFF_PREVIEW_LIMIT);
        assert!(preview.total_bytes > u64::try_from(DIFF_PREVIEW_LIMIT).unwrap());
    }

    #[test]
    fn artifact_read_revalidates_hash_and_does_not_mutate_run() {
        let fixture = Fixture::new();
        let service = fixture.default_service();
        let complete = service
            .start_run(
                WorkflowKind::Fast,
                "artifact integration",
                &fixture.repo,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
            )
            .unwrap();
        let run_id = complete.details.id;
        let stage_id = StageId::new("implementation").unwrap();
        let artifact_path = fixture.temp.path().join("implementation.md");
        let bytes = b"# Verified output\n";
        fs::write(&artifact_path, bytes).unwrap();
        let created_at = now();
        let metadata = ArtifactMetadata::new(
            ArtifactId::new(),
            run_id,
            stage_id.clone(),
            ArtifactKind::Implementation,
            Role::Implementer,
            ArtifactStatus::Complete,
            created_at,
        )
        .with_provider(ProviderId::new("fake").unwrap(), None);
        let artifact = ArtifactRecord::new(
            metadata,
            1,
            artifact_path.clone(),
            sha256(bytes),
            u64::try_from(bytes.len()).unwrap(),
            created_at,
        )
        .unwrap();
        let mut store = SqliteStore::open(&fixture.database).unwrap();
        store.insert_artifact(&artifact).unwrap();
        let revision = store.load_run(run_id).unwrap().revision;
        let event_count = store.load_events(run_id).unwrap().len();
        drop(store);

        assert_eq!(service.list_artifacts(run_id).unwrap().len(), 1);
        let view = service.read_artifact(run_id, &stage_id).unwrap();
        assert_eq!(view.text, "# Verified output\n");
        let mut store = SqliteStore::open(&fixture.database).unwrap();
        assert_eq!(store.load_run(run_id).unwrap().revision, revision);
        assert_eq!(store.load_events(run_id).unwrap().len(), event_count);
        drop(store);

        fs::write(&artifact_path, "tampered\n").unwrap();
        assert!(matches!(
            service.read_artifact(run_id, &stage_id),
            Err(AppError::Store(StoreError::ArtifactIntegrity(path))) if path == artifact_path
        ));
    }

    #[test]
    fn raw_log_tail_does_not_advance_provider_cursor() {
        let fixture = Fixture::new();
        let service = fixture.default_service();
        let complete = service
            .start_run(
                WorkflowKind::Fast,
                "log integration",
                &fixture.repo,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
            )
            .unwrap();
        let run_id = complete.details.id;
        let stage_id = StageId::new("implementation").unwrap();
        let mut store = SqliteStore::open(&fixture.database).unwrap();
        let manager = ProcessManager::new(
            fixture.temp.path().join("processes"),
            TmuxBackend::new("/bin/true"),
        );
        let process = manager
            .prepare(
                &mut store,
                run_id,
                stage_id.clone(),
                1,
                "/bin/true",
                Vec::new(),
                BTreeMap::new(),
            )
            .unwrap();
        let stdout = vec![b'x'; PROCESS_LOG_TAIL_LIMIT + 1_024];
        fs::write(process.spec().stdout_path(), &stdout).unwrap();
        fs::write(process.spec().stderr_path(), b"diagnostic\n").unwrap();
        let stdout_cursor = process.cursor(OutputStream::Stdout);
        let stderr_cursor = process.cursor(OutputStream::Stderr);
        let run_revision = store.load_run(run_id).unwrap().revision;
        let event_count = store.load_events(run_id).unwrap().len();
        drop(store);

        let logs = service.read_process_log_tail(run_id, &stage_id).unwrap();
        assert!(logs.stdout.truncated);
        assert_eq!(logs.stdout.text.len(), PROCESS_LOG_TAIL_LIMIT);
        assert_eq!(logs.stderr.text, "diagnostic\n");

        let mut store = SqliteStore::open(&fixture.database).unwrap();
        let after = store.load_managed_process(process.id()).unwrap();
        assert_eq!(after.cursor(OutputStream::Stdout), stdout_cursor);
        assert_eq!(after.cursor(OutputStream::Stderr), stderr_cursor);
        assert_eq!(store.load_run(run_id).unwrap().revision, run_revision);
        assert_eq!(store.load_events(run_id).unwrap().len(), event_count);
    }

    #[test]
    fn explicit_effort_persists_immutably_and_survives_service_restart() {
        let fixture = Fixture::new();
        let report = fixture
            .default_service()
            .start_run(
                WorkflowKind::Standard,
                "effort persistence",
                &fixture.repo,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::HIGH,
            )
            .unwrap();
        let run_id = report.details.id;
        for stage in &report.details.stages {
            assert_eq!(stage.requested_effort, EffortSetting::HIGH);
        }
        // Fresh service instance: requested effort reconstructs exactly from
        // the persisted immutable snapshot, not from in-memory state.
        let reloaded = fixture.default_service().inspect_run(run_id).unwrap();
        for stage in &reloaded.stages {
            assert_eq!(stage.requested_effort, EffortSetting::HIGH);
        }
    }

    #[test]
    fn omitted_effort_reports_native_default_for_every_stage() {
        let fixture = Fixture::new();
        let report = fixture
            .default_service()
            .start_run(
                WorkflowKind::Fast,
                "default effort",
                &fixture.repo,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortSetting::NativeDefault,
            )
            .unwrap();
        for stage in &report.details.stages {
            assert_eq!(stage.requested_effort, EffortSetting::NativeDefault);
            assert_ne!(stage.requested_effort, EffortSetting::MEDIUM);
        }
    }

    #[test]
    fn missing_provider_is_rejected_before_database_creation() {
        let fixture = Fixture::new();
        let error = fixture
            .default_service()
            .start_run(
                WorkflowKind::Fast,
                "task",
                &fixture.repo,
                None,
                EffortSetting::NativeDefault,
            )
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

    fn sha256(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        use std::fmt::Write as _;

        let mut hash = String::with_capacity(64);
        for byte in Sha256::digest(bytes) {
            write!(hash, "{byte:02x}").expect("writing to String cannot fail");
        }
        hash
    }
}
