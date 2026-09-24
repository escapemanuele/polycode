//! Mission use cases: create and shape the plan, start packages as child
//! runs, and keep package state in step with committed run evidence.
//!
//! The service owns ordering and persistence; the aggregate owns the rules.
//! Every mutation loads the mission, applies one domain operation, and
//! commits the aggregate with its event batch under compare-and-swap.
//! Run state is never inferred from anything but the run store.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::domain::{
    DecisionAuthor, DecisionId, IntegrationEvidence, Mission, MissionChange, MissionId, RunId,
    RunStatus, WorkPackage, WorkPackageContract, WorkPackageId, WorkPackageStatus,
};
use crate::git::GitRepository;
use crate::store::{LoadedMission, MissionInput, MissionRevision, SqliteStore, database_file};
use crate::workspace::WorkspaceStatus;

use super::mission_query::{self, MissionDetails, MissionListItem};
use super::provider_factory::{ProviderFactory, ProviderResolver};
use super::{
    AppError, EffortRequest, ExecutionReport, ExecutionSelection, ImageGenerationPlan, RunService,
    StartProgress, query,
};

/// Use-case boundary for missions. Opens the store per call, like
/// [`RunService`], so several processes can share one database.
pub struct MissionService {
    database: PathBuf,
}

/// One package as the lead or user describes it before it exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewWorkPackage {
    pub id: WorkPackageId,
    pub contract: WorkPackageContract,
    pub dependencies: Vec<WorkPackageId>,
}

impl MissionService {
    #[must_use]
    pub const fn new(database: PathBuf) -> Self {
        Self { database }
    }

    /// Resolves the database from the environment, as the CLI does.
    ///
    /// # Errors
    /// Returns path resolution errors.
    pub fn from_environment() -> Result<Self, AppError> {
        Ok(Self::new(database_file()?))
    }

    /// Creates one mission over a Git checkout, recording where it began.
    ///
    /// # Errors
    /// Returns repository discovery, input validation, or persistence errors.
    pub fn create_mission(
        &self,
        title: impl Into<String>,
        goal: impl Into<String>,
        repository_path: impl AsRef<Path>,
    ) -> Result<MissionDetails, AppError> {
        let repository = GitRepository::discover(repository_path)?;
        let now = now();
        let id = MissionId::new();
        let mission = Mission::new(id, now);
        let input = MissionInput::new(
            id,
            title,
            goal,
            repository.source_path().to_string_lossy().into_owned(),
            repository.head_commit(),
            now,
        )?;
        let mut store = SqliteStore::open(&self.database)?;
        let revision = store.create_mission(&mission, &input, &mission.created_events())?;
        mission_query::details(
            &mut store,
            &LoadedMission {
                mission,
                input,
                revision,
            },
        )
    }

    /// Every mission, newest first, with package counts. Each mission's
    /// runs are observed first, as on every other read, so the counts a
    /// row shows are current.
    ///
    /// # Errors
    /// Returns persistence errors.
    pub fn list_missions(&self) -> Result<Vec<MissionListItem>, AppError> {
        let mut store = SqliteStore::open(&self.database)?;
        mission_query::list(&mut store, observe_runs)
    }

    /// One mission with its packages in dependency order.
    ///
    /// Before projecting, the committed status of every package's current
    /// run is observed, so a run that finished or stopped since the last
    /// look is reflected. That is the one write a read performs, and it
    /// derives only from run state already committed by whoever drives it.
    ///
    /// # Errors
    /// Returns not-found or persistence errors.
    pub fn inspect_mission(&self, mission_id: MissionId) -> Result<MissionDetails, AppError> {
        let mut store = SqliteStore::open(&self.database)?;
        let loaded = observe_runs(&mut store, mission_id)?;
        mission_query::details(&mut store, &loaded)
    }

    /// Adds one package to the plan.
    ///
    /// # Errors
    /// Returns domain rule violations or persistence errors.
    pub fn add_package(
        &self,
        mission_id: MissionId,
        package: NewWorkPackage,
    ) -> Result<MissionDetails, AppError> {
        self.mutate(mission_id, |mission, now| {
            mission.add_package(package.id, package.contract, package.dependencies, now)
        })
    }

    /// Replaces the contract of a package nothing has run for.
    ///
    /// # Errors
    /// Returns domain rule violations or persistence errors.
    pub fn revise_package(
        &self,
        mission_id: MissionId,
        package_id: &WorkPackageId,
        contract: WorkPackageContract,
    ) -> Result<MissionDetails, AppError> {
        self.mutate(mission_id, |mission, now| {
            mission.revise_contract(package_id, contract, now)
        })
    }

    /// Replaces the dependencies of a package nothing has run for.
    ///
    /// # Errors
    /// Returns domain rule violations or persistence errors.
    pub fn set_dependencies(
        &self,
        mission_id: MissionId,
        package_id: &WorkPackageId,
        dependencies: Vec<WorkPackageId>,
    ) -> Result<MissionDetails, AppError> {
        self.mutate(mission_id, |mission, now| {
            mission.set_dependencies(package_id, dependencies, now)
        })
    }

    /// Starts a child run for one ready package and binds it.
    ///
    /// The run's immutable input is the package handoff rendered from the
    /// mission: goal, contract, integrated dependencies, and decisions.
    /// The run is bound to the package the moment it is persisted, before
    /// its workspace is prepared, so a drive that stops early still leaves
    /// the package pointing at its run. The run then proceeds exactly as
    /// `polycode <workflow>` would, and the package is observed once the
    /// drive reaches quiescence.
    ///
    /// # Errors
    /// Returns domain rule violations, run start failures, or persistence
    /// errors. When the run started but could not be bound, the error names
    /// it so it can be attached by hand.
    pub fn start_package<F>(
        &self,
        runs: &RunService<F>,
        mission_id: MissionId,
        package_id: &WorkPackageId,
        selection: Option<ExecutionSelection>,
        effort: EffortRequest,
    ) -> Result<(ExecutionReport, MissionDetails), AppError>
    where
        F: ProviderFactory,
    {
        let mut store = SqliteStore::open(&self.database)?;
        let loaded = store.load_mission(mission_id)?;
        let package = loaded
            .mission
            .package(package_id)
            .ok_or_else(|| AppError::PackageNotFound(mission_id, package_id.clone()))?;
        if package.status() != WorkPackageStatus::Ready {
            return Err(crate::domain::MissionError::InvalidPackageTransition {
                package_id: package_id.clone(),
                status: package.status(),
                action: "starting a run",
                expected: "a ready package",
            }
            .into());
        }
        let task = handoff_task(&loaded, package);
        let workflow = package.contract().workflow;
        let repository = PathBuf::from(loaded.input.source_repo_path());
        drop(store);

        let bound: RefCell<Option<Result<(), AppError>>> = RefCell::new(None);
        let report = runs.start_run_observed(
            workflow,
            task,
            &repository,
            selection,
            effort,
            &ImageGenerationPlan::disabled(),
            &|progress| {
                if let StartProgress::PreparingWorkspace(run_id) = progress
                    && bound.borrow().is_none()
                {
                    let outcome = self.mutate_quietly(mission_id, |mission, now| {
                        mission.start_package(package_id, run_id, now)
                    });
                    *bound.borrow_mut() = Some(outcome.map(|_| ()));
                }
            },
        )?;
        match bound.into_inner() {
            Some(Ok(())) => {}
            Some(Err(error)) => {
                return Err(AppError::PackageRunUnbound {
                    mission_id,
                    package_id: package_id.clone(),
                    run_id: report.details.id,
                    reason: error.to_string(),
                });
            }
            None => {
                return Err(AppError::PackageRunUnbound {
                    mission_id,
                    package_id: package_id.clone(),
                    run_id: report.details.id,
                    reason: "the run never reported being persisted".to_owned(),
                });
            }
        }
        let details = self.inspect_mission(mission_id)?;
        Ok((report, details))
    }

    /// Binds an existing run to a ready package, for work started outside
    /// the mission. The run must belong to the mission's repository.
    ///
    /// # Errors
    /// Returns domain rule violations, repository mismatches, or persistence
    /// errors.
    pub fn attach_run(
        &self,
        mission_id: MissionId,
        package_id: &WorkPackageId,
        run_id: RunId,
    ) -> Result<MissionDetails, AppError> {
        let mut store = SqliteStore::open(&self.database)?;
        let loaded = store.load_mission(mission_id)?;
        let run = query::inspect(&mut store, run_id)?;
        let expected = Path::new(loaded.input.source_repo_path());
        if run.repository.as_deref() != Some(expected) {
            return Err(AppError::MissionRepositoryMismatch {
                run_id,
                expected: expected.to_path_buf(),
                actual: run.repository,
            });
        }
        drop(store);
        self.mutate(mission_id, |mission, now| {
            mission.start_package(package_id, run_id, now)
        })
    }

    /// Records that a delivered package's change reached the source
    /// checkout. The evidence is read from the run store: the run is
    /// `Applied`, or it completed with an empty delta against its base and
    /// there was nothing to transfer. A completed run whose delta is still
    /// in its worktree is not integrated, whatever anyone says about it.
    ///
    /// # Errors
    /// Returns domain rule violations, missing evidence, or persistence
    /// errors.
    pub fn integrate_package<F>(
        &self,
        runs: &RunService<F>,
        mission_id: MissionId,
        package_id: &WorkPackageId,
    ) -> Result<MissionDetails, AppError>
    where
        F: ProviderResolver,
    {
        let mut store = SqliteStore::open(&self.database)?;
        let loaded = observe_runs(&mut store, mission_id)?;
        let package = loaded
            .mission
            .package(package_id)
            .ok_or_else(|| AppError::PackageNotFound(mission_id, package_id.clone()))?;
        let run_id = package
            .current_run()
            .ok_or_else(|| AppError::PackageNotFound(mission_id, package_id.clone()))?;
        let status = store.load_run(run_id)?.run.status();
        let workspace = store
            .load_workspace(run_id)?
            .map(|workspace| workspace.status());
        drop(store);
        let evidence = match status {
            RunStatus::Applied => IntegrationEvidence::Applied,
            // A completed run's worktree is released by exactly one path:
            // `apply` finding an empty delta. So a completed run without a
            // worktree already went through apply and had nothing to move;
            // one that still has its worktree is asked for its delta now.
            RunStatus::Completed if workspace == Some(WorkspaceStatus::Removed) => {
                IntegrationEvidence::NoChanges
            }
            RunStatus::Completed if workspace == Some(WorkspaceStatus::Ready) => {
                let preview = runs.preview_run_diff(run_id)?;
                if preview.total_bytes == 0 && preview.changed_files.is_empty() {
                    IntegrationEvidence::NoChanges
                } else {
                    return Err(AppError::PackageNotIntegrated {
                        run_id,
                        status,
                        changed_files: preview.changed_files.len(),
                    });
                }
            }
            RunStatus::Completed => {
                return Err(AppError::PackageWorkspaceUnavailable { run_id, workspace });
            }
            other => {
                return Err(AppError::PackageNotIntegrated {
                    run_id,
                    status: other,
                    changed_files: 0,
                });
            }
        };
        self.mutate(mission_id, |mission, now| {
            mission.integrate_package(package_id, evidence, now)
        })
    }

    /// Returns a failed package to the plan so a new run can serve it.
    ///
    /// # Errors
    /// Returns domain rule violations or persistence errors.
    pub fn retry_package(
        &self,
        mission_id: MissionId,
        package_id: &WorkPackageId,
    ) -> Result<MissionDetails, AppError> {
        self.mutate(mission_id, |mission, now| {
            mission.retry_package(package_id, now)
        })
    }

    /// Cancels one package nothing depends on and no run is serving.
    ///
    /// # Errors
    /// Returns domain rule violations or persistence errors.
    pub fn cancel_package(
        &self,
        mission_id: MissionId,
        package_id: &WorkPackageId,
        reason: impl Into<String>,
    ) -> Result<MissionDetails, AppError> {
        let reason = reason.into();
        self.mutate(mission_id, |mission, now| {
            mission.cancel_package(package_id, reason, now)
        })
    }

    /// Records one design decision.
    ///
    /// # Errors
    /// Returns domain rule violations or persistence errors.
    pub fn record_decision(
        &self,
        mission_id: MissionId,
        title: impl Into<String>,
        rationale: impl Into<String>,
        author: DecisionAuthor,
    ) -> Result<MissionDetails, AppError> {
        let title = title.into();
        let rationale = rationale.into();
        self.mutate(mission_id, |mission, now| {
            mission.record_decision(DecisionId::new(), title, rationale, author, now)
        })
    }

    /// Completes a mission whose every package is integrated or cancelled.
    ///
    /// # Errors
    /// Returns domain rule violations or persistence errors.
    pub fn complete_mission(&self, mission_id: MissionId) -> Result<MissionDetails, AppError> {
        self.mutate(mission_id, Mission::complete)
    }

    /// Cancels a mission and every open package; running packages must be
    /// stopped first.
    ///
    /// # Errors
    /// Returns domain rule violations or persistence errors.
    pub fn cancel_mission(
        &self,
        mission_id: MissionId,
        reason: impl Into<String>,
    ) -> Result<MissionDetails, AppError> {
        let reason = reason.into();
        self.mutate(mission_id, |mission, now| mission.cancel(reason, now))
    }

    /// Observes runs, applies one domain operation, commits, and projects.
    fn mutate(
        &self,
        mission_id: MissionId,
        operation: impl FnOnce(
            &mut Mission,
            DateTime<Utc>,
        ) -> Result<MissionChange, crate::domain::MissionError>,
    ) -> Result<MissionDetails, AppError> {
        let mut store = SqliteStore::open(&self.database)?;
        let mut loaded = observe_runs(&mut store, mission_id)?;
        let now = now().max(*loaded.mission.updated_at());
        let change = operation(&mut loaded.mission, now)?;
        if change.changed() {
            store.commit_mission_update(&loaded.mission, loaded.revision, &change.events)?;
            // The operation may have bound a run that already has a
            // committed outcome (attach); observe once more so the
            // projection never shows a package behind its run.
            loaded = observe_runs(&mut store, mission_id)?;
        }
        mission_query::details(&mut store, &loaded)
    }

    /// One domain operation without the observe pass or projection, for use
    /// inside another operation's progress callback.
    fn mutate_quietly(
        &self,
        mission_id: MissionId,
        operation: impl FnOnce(
            &mut Mission,
            DateTime<Utc>,
        ) -> Result<MissionChange, crate::domain::MissionError>,
    ) -> Result<MissionRevision, AppError> {
        let mut store = SqliteStore::open(&self.database)?;
        let mut loaded = store.load_mission(mission_id)?;
        let now = now().max(*loaded.mission.updated_at());
        let change = operation(&mut loaded.mission, now)?;
        if change.changed() {
            loaded.revision =
                store.commit_mission_update(&loaded.mission, loaded.revision, &change.events)?;
        }
        Ok(loaded.revision)
    }
}

fn now() -> DateTime<Utc> {
    std::time::SystemTime::now().into()
}

/// Reports every package's current run status to the mission and commits
/// whatever moved. Reads run state only; never drives a run.
fn observe_runs(store: &mut SqliteStore, mission_id: MissionId) -> Result<LoadedMission, AppError> {
    let mut loaded = store.load_mission(mission_id)?;
    if loaded.mission.status().is_closed() {
        return Ok(loaded);
    }
    let observed: Vec<(WorkPackageId, RunId)> = loaded
        .mission
        .packages()
        .iter()
        .filter(|package| package.status().has_active_run())
        .filter_map(|package| Some((package.id().clone(), package.current_run()?)))
        .collect();
    let mut events = Vec::new();
    for (package_id, run_id) in observed {
        let run = store.load_run(run_id)?.run;
        let status = run.status();
        let reason = match status {
            RunStatus::NeedsUser => run
                .attention_requests()
                .iter()
                .find(|request| request.status().is_pending())
                .map(|request| format!("{:?}: {}", request.kind(), request.summary())),
            RunStatus::Failed => query::inspect(store, run_id)?.failure_reason,
            _ => None,
        };
        let now = now().max(*loaded.mission.updated_at());
        let change =
            loaded
                .mission
                .observe_run(&package_id, run_id, status, reason.as_deref(), now)?;
        events.extend(change.events);
    }
    if !events.is_empty() {
        loaded.revision = store.commit_mission_update(&loaded.mission, loaded.revision, &events)?;
    }
    Ok(loaded)
}

/// The immutable task a child run receives: everything the package's
/// worker needs, rendered from canonical mission state rather than typed
/// by hand. Deterministic for a given mission and package.
pub(crate) fn handoff_task(loaded: &LoadedMission, package: &WorkPackage) -> String {
    let contract = package.contract();
    let mut text = String::new();
    let _ = write!(
        text,
        "# Work package: {} ({})\n\nPart of mission \"{}\": {}\n\n",
        contract.title,
        package.id(),
        loaded.input.title(),
        loaded.input.goal()
    );
    text.push_str("## Goal\n\n");
    text.push_str(&contract.goal);
    text.push_str("\n\n");
    if !contract.rationale.is_empty() {
        text.push_str("## Why this exists\n\n");
        text.push_str(&contract.rationale);
        text.push_str("\n\n");
    }
    if !contract.scope.is_empty() {
        text.push_str("## Scope\n\n");
        text.push_str(&contract.scope);
        text.push_str("\n\n");
    }
    if !contract.acceptance_criteria.is_empty() {
        text.push_str("## Acceptance criteria\n\n");
        for criterion in &contract.acceptance_criteria {
            let _ = writeln!(text, "- {criterion}");
        }
        text.push('\n');
    }
    if !contract.verification.is_empty() {
        text.push_str("## Verification expectations\n\n");
        text.push_str(&contract.verification);
        text.push_str("\n\n");
    }
    let dependencies: Vec<&WorkPackage> = package
        .dependencies()
        .iter()
        .filter_map(|id| loaded.mission.package(id))
        .collect();
    if !dependencies.is_empty() {
        text.push_str("## Already integrated before this package\n\n");
        for dependency in dependencies {
            let _ = writeln!(
                text,
                "- {} ({}): {}",
                dependency.contract().title,
                dependency.id(),
                dependency.contract().goal
            );
        }
        text.push('\n');
    }
    if !loaded.mission.decisions().is_empty() {
        text.push_str("## Decisions to respect\n\n");
        for decision in loaded.mission.decisions() {
            let _ = writeln!(text, "- {}: {}", decision.title, decision.rationale);
        }
        text.push('\n');
    }
    text.push_str(
        "Stay within this package. Work another package needs belongs to that package; \
         note it under `## Bottom line` instead of doing it here.\n",
    );
    text
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use chrono::TimeZone;
    use tempfile::TempDir;

    use super::*;
    use crate::app::{DevelopmentFakeProviderFactory, UniformProvider};
    use crate::domain::{MissionStatus, WorkflowKind};

    struct Fixture {
        temp: TempDir,
        repo: PathBuf,
        database: PathBuf,
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
            Self {
                temp,
                repo,
                database,
            }
        }

        fn missions(&self) -> MissionService {
            MissionService::new(self.database.clone())
        }

        fn runs(&self) -> RunService<DevelopmentFakeProviderFactory> {
            RunService::new(
                self.database.clone(),
                self.temp.path().join("data/worktrees"),
                DevelopmentFakeProviderFactory::new(self.temp.path().join("runs")),
            )
        }
    }

    fn git(path: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn contract(title: &str, workflow: WorkflowKind) -> WorkPackageContract {
        WorkPackageContract {
            title: title.to_owned(),
            goal: format!("deliver {title}"),
            rationale: String::new(),
            scope: String::new(),
            acceptance_criteria: vec![],
            verification: String::new(),
            workflow,
        }
    }

    fn package(id: &str, title: &str, dependencies: &[&str]) -> NewWorkPackage {
        NewWorkPackage {
            id: WorkPackageId::new(id).unwrap(),
            contract: contract(title, WorkflowKind::Fast),
            dependencies: dependencies
                .iter()
                .map(|dependency| WorkPackageId::new(*dependency).unwrap())
                .collect(),
        }
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "one mission, start to finish")]
    fn a_package_runs_on_the_fake_provider_and_integration_readies_its_dependent() {
        let fixture = Fixture::new();
        let missions = fixture.missions();
        let runs = fixture.runs();
        let mission = missions
            .create_mission("JEV", "persistent autonomous lives", &fixture.repo)
            .unwrap();
        assert_eq!(mission.status, MissionStatus::Planning);
        assert_eq!(mission.repository, fixture.repo.canonicalize().unwrap());

        missions
            .add_package(mission.id, package("persistence", "Persistence", &[]))
            .unwrap();
        let details = missions
            .add_package(mission.id, package("memory", "Memory", &["persistence"]))
            .unwrap();
        let persistence = WorkPackageId::new("persistence").unwrap();
        let memory = WorkPackageId::new("memory").unwrap();
        assert_eq!(
            details.package(&persistence).unwrap().status,
            WorkPackageStatus::Ready
        );
        assert_eq!(
            details.package(&memory).unwrap().status,
            WorkPackageStatus::Planned
        );

        let error = missions
            .start_package(
                &runs,
                mission.id,
                &memory,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortRequest::ProfileDefault,
            )
            .unwrap_err();
        assert!(matches!(error, AppError::Mission(_)), "{error}");

        let (report, details) = missions
            .start_package(
                &runs,
                mission.id,
                &persistence,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortRequest::ProfileDefault,
            )
            .unwrap();
        assert_eq!(report.details.status, RunStatus::Completed);
        let task = report.details.task.unwrap();
        assert!(task.starts_with("# Work package: Persistence (persistence)"));
        assert!(task.contains("Part of mission \"JEV\": persistent autonomous lives"));
        let run_id = report.details.id;
        let summary = details.package(&persistence).unwrap();
        assert_eq!(summary.status, WorkPackageStatus::Delivered);
        assert_eq!(summary.current_run, Some(run_id));
        assert_eq!(summary.run_status, Some(RunStatus::Completed));
        assert_eq!(details.status, MissionStatus::Active);
        assert_eq!(
            details.attention.awaiting_integration,
            vec![persistence.clone()]
        );

        // The run knows its package, and cannot be deleted while it does.
        let binding = SqliteStore::open(&fixture.database)
            .unwrap()
            .mission_of_run(run_id)
            .unwrap()
            .unwrap();
        assert_eq!(binding.package_id, persistence);

        // The fake provider changes nothing, so the completed run's empty
        // delta is the integration evidence; apply reports the same.
        let details = missions
            .integrate_package(&runs, mission.id, &persistence)
            .unwrap();
        assert_eq!(
            details.package(&persistence).unwrap().status,
            WorkPackageStatus::Integrated
        );
        assert_eq!(
            details.package(&memory).unwrap().status,
            WorkPackageStatus::Ready
        );
        assert!(details.attention.is_empty());

        // The dependent's handoff names what came before it.
        let (report, _) = missions
            .start_package(
                &runs,
                mission.id,
                &memory,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortRequest::ProfileDefault,
            )
            .unwrap();
        assert!(report.details.task.unwrap().contains(
            "## Already integrated before this package\n\n- Persistence (persistence): deliver Persistence"
        ));
        let (outcome, _) = runs.apply_run(report.details.id).unwrap();
        assert_eq!(outcome, crate::app::ApplyOutcome::NoChanges);
        missions
            .integrate_package(&runs, mission.id, &memory)
            .unwrap();
        let details = missions.complete_mission(mission.id).unwrap();
        assert_eq!(details.status, MissionStatus::Completed);
        assert_eq!(details.packages.len(), 2);

        let listed = missions.list_missions().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].integrated, 2);
        assert_eq!(listed[0].status, MissionStatus::Completed);

        // The mission's own history explains the state.
        let events = SqliteStore::open(&fixture.database)
            .unwrap()
            .load_mission_events(mission.id)
            .unwrap();
        assert!(matches!(
            events.last().unwrap().event.kind(),
            crate::domain::MissionEventKind::MissionCompleted
        ));
    }

    #[test]
    fn attaching_a_run_checks_its_repository_and_a_foreign_run_is_refused() {
        let fixture = Fixture::new();
        let missions = fixture.missions();
        let runs = fixture.runs();
        let mission = missions
            .create_mission("JEV", "goal", &fixture.repo)
            .unwrap();
        missions
            .add_package(mission.id, package("a", "A", &[]))
            .unwrap();
        let a = WorkPackageId::new("a").unwrap();
        let report = runs
            .start_run(
                WorkflowKind::Fast,
                "standalone",
                &fixture.repo,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortRequest::ProfileDefault,
                &ImageGenerationPlan::disabled(),
            )
            .unwrap();
        let details = missions
            .attach_run(mission.id, &a, report.details.id)
            .unwrap();
        // A completed run attached to a ready package is observed at once.
        assert_eq!(
            details.package(&a).unwrap().status,
            WorkPackageStatus::Delivered
        );

        let other = fixture.temp.path().join("other");
        fs::create_dir(&other).unwrap();
        git(&other, &["init", "-q"]);
        git(&other, &["config", "user.email", "test@example.com"]);
        git(&other, &["config", "user.name", "Test"]);
        fs::write(other.join("x"), "x\n").unwrap();
        git(&other, &["add", "x"]);
        git(&other, &["commit", "-qm", "initial"]);
        let foreign = runs
            .start_run(
                WorkflowKind::Fast,
                "elsewhere",
                &other,
                Some(ExecutionSelection::Uniform(UniformProvider::Fake)),
                EffortRequest::ProfileDefault,
                &ImageGenerationPlan::disabled(),
            )
            .unwrap();
        missions
            .add_package(mission.id, package("b", "B", &[]))
            .unwrap();
        let error = missions
            .attach_run(
                mission.id,
                &WorkPackageId::new("b").unwrap(),
                foreign.details.id,
            )
            .unwrap_err();
        assert!(
            matches!(error, AppError::MissionRepositoryMismatch { .. }),
            "{error}"
        );
    }

    #[test]
    fn the_handoff_quotes_contract_dependencies_and_decisions() {
        let at = Utc.with_ymd_and_hms(2026, 9, 22, 9, 0, 0).unwrap();
        let id = MissionId::from_u128(1);
        let mut mission = Mission::new(id, at);
        let persistence = WorkPackageId::new("persistence").unwrap();
        let memory = WorkPackageId::new("memory").unwrap();
        mission
            .add_package(
                persistence.clone(),
                WorkPackageContract {
                    title: "Persistence".to_owned(),
                    goal: "persist character state".to_owned(),
                    rationale: String::new(),
                    scope: String::new(),
                    acceptance_criteria: vec![],
                    verification: String::new(),
                    workflow: WorkflowKind::Fast,
                },
                vec![],
                at,
            )
            .unwrap();
        mission
            .add_package(
                memory.clone(),
                WorkPackageContract {
                    title: "Memory".to_owned(),
                    goal: "give characters memory".to_owned(),
                    rationale: "memory needs persistence".to_owned(),
                    scope: "src/memory only".to_owned(),
                    acceptance_criteria: vec!["memories survive restart".to_owned()],
                    verification: "cargo test memory".to_owned(),
                    workflow: WorkflowKind::Standard,
                },
                vec![persistence],
                at,
            )
            .unwrap();
        mission
            .record_decision(
                DecisionId::from_u128(3),
                "Persist first",
                "memory needs a substrate",
                DecisionAuthor::Lead,
                at,
            )
            .unwrap();
        let loaded = LoadedMission {
            input: MissionInput::new(id, "JEV", "autonomous lives", "/repo", "abc", at).unwrap(),
            revision: MissionRevision::initial(),
            mission,
        };
        let text = handoff_task(&loaded, loaded.mission.package(&memory).unwrap());
        assert!(text.starts_with("# Work package: Memory (memory)\n\nPart of mission \"JEV\": autonomous lives\n\n## Goal\n\ngive characters memory\n"));
        assert!(text.contains("## Why this exists\n\nmemory needs persistence\n"));
        assert!(text.contains("## Scope\n\nsrc/memory only\n"));
        assert!(text.contains("## Acceptance criteria\n\n- memories survive restart\n"));
        assert!(text.contains("## Verification expectations\n\ncargo test memory\n"));
        assert!(text.contains("## Already integrated before this package\n\n- Persistence (persistence): persist character state\n"));
        assert!(
            text.contains("## Decisions to respect\n\n- Persist first: memory needs a substrate\n")
        );
        assert!(text.ends_with("instead of doing it here.\n"));
    }
}
