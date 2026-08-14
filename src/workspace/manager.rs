use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::domain::{EventId, EventMetadata, Run, RunId, RunStatus, RunTransition, WorkflowKind};
use crate::git::{
    GitRepository, apply_patch, branch_exists, branch_tip, check_patch, create_worktree,
    delete_owned_branch, generate_patch, inspect_worktree, remove_worktree, source_is_clean,
};
use crate::store::{RunRevision, SqliteStore, worktree_root};

use super::{
    ApplyStatus, RunApplyOperation, RunWorkspace, WorkspaceError, WorkspaceMode, WorkspaceStatus,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconciliationOutcome {
    Ready(RunWorkspace),
    Removed(RunWorkspace),
    Unchanged(RunWorkspace),
    Broken(RunWorkspace),
}

pub struct WorkspaceManager {
    root: PathBuf,
    git: crate::git::Git,
    #[cfg(test)]
    fault: Option<FaultPoint>,
}

impl WorkspaceManager {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            git: crate::git::Git::default(),
            #[cfg(test)]
            fault: None,
        }
    }

    #[cfg(test)]
    fn with_fault(root: impl Into<PathBuf>, fault: FaultPoint) -> Self {
        Self {
            root: root.into(),
            git: crate::git::Git::default(),
            fault: Some(fault),
        }
    }

    /// Creates default manager rooted at Polycode data directory.
    ///
    /// # Errors
    /// Returns data-path resolution error when no override or home exists.
    pub fn from_environment() -> Result<Self, WorkspaceError> {
        Ok(Self::new(worktree_root()?))
    }

    /// Persists preparation intent, creates isolated worktree, validates it,
    /// then atomically marks workspace and logical run ready.
    ///
    /// # Errors
    /// Returns typed lifecycle, persistence, repository, ownership, or Git errors.
    pub fn prepare_run_workspace(
        &self,
        store: &mut SqliteStore,
        run_id: RunId,
        repository_path: impl AsRef<Path>,
    ) -> Result<RunWorkspace, WorkspaceError> {
        if store.load_workspace(run_id)?.is_some() {
            return Err(WorkspaceError::WorkspaceAlreadyExists(run_id));
        }
        let loaded = store.load_run(run_id)?;
        if loaded.run.status() != RunStatus::Created {
            return Err(invalid_run_status(&loaded.run, "workspace preparation"));
        }
        let repository = GitRepository::discover(repository_path)?;
        let mode = if loaded.run.workflow_kind() == WorkflowKind::Review {
            WorkspaceMode::Detached
        } else {
            WorkspaceMode::Branch
        };
        let branch = (mode == WorkspaceMode::Branch).then(|| format!("polycode/run-{run_id}"));
        if let Some(branch) = branch.as_deref() {
            if branch_exists(&self.git, &repository, branch)? {
                return Err(WorkspaceError::BranchConflict(branch.to_owned()));
            }
        }
        let path = self.workspace_path(&repository, run_id)?;
        if path.exists() {
            return Err(WorkspaceError::WorkspacePathConflict(path));
        }

        let intent_time = next_time(&loaded.run);
        let mut workspace = RunWorkspace::preparing(
            run_id,
            repository.source_path().to_path_buf(),
            repository.git_common_dir().to_path_buf(),
            repository.head_commit().to_owned(),
            path,
            branch,
            mode,
            intent_time,
        )?;
        let mut run = loaded.run;
        let begin_event = run.transition(RunTransition::BeginPreparation, metadata(intent_time))?;
        let begin =
            store.begin_workspace_preparation(&workspace, &run, loaded.revision, &begin_event)?;
        self.fault(FaultPoint::WorkspaceIntent)?;

        self.create_intended_worktree(&repository, &workspace)?;
        self.fault(FaultPoint::WorktreeCreated)?;
        self.validate_workspace(&workspace, true)?;

        let ready_time = next_time(&run);
        workspace.mark_ready(ready_time);
        let ready_event = run.transition(RunTransition::FinishPreparation, metadata(ready_time))?;
        store.finalize_workspace_preparation(
            &workspace,
            workspace.revision(),
            &run,
            begin.revision(),
            &ready_event,
        )?;
        store
            .load_workspace(run_id)?
            .ok_or(WorkspaceError::WorkspaceMissing(run_id))
    }

    /// Reconciles persisted intent against current Git/filesystem state.
    ///
    /// # Errors
    /// Returns persistence errors or unrecoverable Git command failures. Unsafe
    /// mismatches are persisted as `Broken` and returned as an outcome.
    pub fn reconcile(
        &self,
        store: &mut SqliteStore,
        run_id: RunId,
    ) -> Result<ReconciliationOutcome, WorkspaceError> {
        let workspace = store
            .load_workspace(run_id)?
            .ok_or(WorkspaceError::WorkspaceMissing(run_id))?;
        match workspace.status() {
            WorkspaceStatus::Preparing => self.reconcile_preparing(store, workspace),
            WorkspaceStatus::Ready => {
                if let Err(error) = self.validate_source(&workspace) {
                    return Self::break_workspace(store, workspace, error.to_string());
                }
                if !workspace.worktree_path().exists() {
                    return Self::break_workspace(store, workspace, "ready worktree is missing");
                }
                if let Err(error) = self.validate_workspace(&workspace, false) {
                    return Self::break_workspace(store, workspace, error.to_string());
                }
                Ok(ReconciliationOutcome::Unchanged(workspace))
            }
            WorkspaceStatus::Removing => self.finish_removal(store, workspace),
            WorkspaceStatus::Removed => Ok(ReconciliationOutcome::Unchanged(workspace)),
            WorkspaceStatus::Broken => Ok(ReconciliationOutcome::Broken(workspace)),
        }
    }

    /// Applies exact workspace delta to clean source checkout without staging or committing.
    ///
    /// # Errors
    /// Rejects non-completed/review runs, dirty sources, changed patch identity,
    /// failed preflight, duplicate apply, and ambiguous recovery state.
    pub fn apply(&self, store: &mut SqliteStore, run_id: RunId) -> Result<(), WorkspaceError> {
        let loaded = store.load_run(run_id)?;
        if loaded.run.status() == RunStatus::Applied {
            return Err(WorkspaceError::ApplyAlreadyPerformed(run_id));
        }
        if loaded.run.status() != RunStatus::Completed {
            return Err(invalid_run_status(&loaded.run, "apply"));
        }
        let workspace = Self::ready_workspace(store, run_id)?;
        if workspace.mode() != WorkspaceMode::Branch {
            return Err(WorkspaceError::ReviewWorkspaceNotApplicable);
        }
        self.validate_workspace(&workspace, false)?;
        let repository = self.validate_source(&workspace)?;
        let patch = generate_patch(
            &self.git,
            workspace.worktree_path(),
            workspace.base_commit(),
        )?;
        if patch.is_empty() {
            return Err(WorkspaceError::EmptyPatch);
        }
        let patch_hash = hash_bytes(&patch);

        if let Some(operation) = store.load_apply_operation(run_id)? {
            return self.reconcile_apply(
                store,
                loaded.run,
                loaded.revision,
                &repository,
                &patch,
                &patch_hash,
                operation,
            );
        }
        if !source_is_clean(&self.git, &repository)? {
            return Err(WorkspaceError::SourceCheckoutDirty(
                repository.source_path().to_path_buf(),
            ));
        }
        let operation =
            store.insert_apply_operation(run_id, &patch_hash, loaded.revision, now())?;
        if !check_patch(&self.git, &repository, &patch, false)? {
            let _failed = store.update_apply_operation(
                &operation,
                ApplyStatus::Failed,
                Some("git apply --check failed"),
                now(),
            )?;
            return Err(WorkspaceError::PatchCheckFailed);
        }
        if !source_is_clean(&self.git, &repository)? {
            store.update_apply_operation(
                &operation,
                ApplyStatus::Failed,
                Some("source changed after apply intent"),
                now(),
            )?;
            return Err(WorkspaceError::SourceCheckoutDirty(
                repository.source_path().to_path_buf(),
            ));
        }
        if let Err(error) = apply_patch(&self.git, &repository, &patch) {
            store.update_apply_operation(
                &operation,
                ApplyStatus::Failed,
                Some(&error.to_string()),
                now(),
            )?;
            return Err(error.into());
        }
        self.fault(FaultPoint::GitApplied)?;
        let operation =
            store.update_apply_operation(&operation, ApplyStatus::AppliedToSource, None, now())?;
        Self::finalize_apply(store, loaded.run, loaded.revision, &operation)
    }

    /// Records logical discard first, then removes owned workspace resources.
    ///
    /// # Errors
    /// Returns lifecycle, persistence, ownership, or Git cleanup errors.
    pub fn discard(&self, store: &mut SqliteStore, run_id: RunId) -> Result<(), WorkspaceError> {
        let loaded = store.load_run(run_id)?;
        if loaded.run.status() != RunStatus::Discarded {
            let mut run = loaded.run;
            let at = next_time(&run);
            let event = run.transition(RunTransition::Discard, metadata(at))?;
            store.commit_run_update(&run, loaded.revision, &[event])?;
        }
        if store.load_workspace(run_id)?.is_some() {
            self.cleanup(store, run_id)?;
        }
        Ok(())
    }

    /// Removes owned Git resources without changing logical run status.
    ///
    /// # Errors
    /// Only completed/applied/discarded runs are eligible. Ownership mismatches
    /// become a persisted broken workspace instead of deleting foreign data.
    pub fn cleanup(&self, store: &mut SqliteStore, run_id: RunId) -> Result<(), WorkspaceError> {
        let loaded = store.load_run(run_id)?;
        if !matches!(
            loaded.run.status(),
            RunStatus::Completed | RunStatus::Applied | RunStatus::Discarded
        ) {
            return Err(invalid_run_status(&loaded.run, "workspace cleanup"));
        }
        if store
            .load_apply_operation(run_id)?
            .is_some_and(|operation| {
                matches!(
                    operation.status(),
                    ApplyStatus::Prepared | ApplyStatus::AppliedToSource
                )
            })
        {
            return Err(WorkspaceError::ApplyInProgress(run_id));
        }
        let mut workspace = store
            .load_workspace(run_id)?
            .ok_or(WorkspaceError::WorkspaceMissing(run_id))?;
        if workspace.status() == WorkspaceStatus::Removed {
            return Ok(());
        }
        if workspace.status() == WorkspaceStatus::Removing {
            return match self.finish_removal(store, workspace)? {
                ReconciliationOutcome::Removed(_) | ReconciliationOutcome::Unchanged(_) => Ok(()),
                ReconciliationOutcome::Broken(workspace) => Err(WorkspaceError::WorkspaceBroken {
                    run_id,
                    reason: workspace.last_error().unwrap_or("unknown error").to_owned(),
                }),
                ReconciliationOutcome::Ready(_) => Err(WorkspaceError::WorkspaceBroken {
                    run_id,
                    reason: "removal unexpectedly returned a ready workspace".to_owned(),
                }),
            };
        }
        let repository = match self.validate_source(&workspace) {
            Ok(repository) => repository,
            Err(error) => {
                Self::persist_broken(store, &mut workspace, error.to_string())?;
                return Err(error);
            }
        };
        let removal_head = if workspace.worktree_path().exists() {
            let identity = match self.validate_workspace(&workspace, false) {
                Ok(identity) => identity,
                Err(error) => {
                    Self::persist_broken(store, &mut workspace, error.to_string())?;
                    return Err(error);
                }
            };
            workspace.confirm_branch_ownership();
            identity.head_commit
        } else if workspace.branch_owned() {
            let branch = workspace
                .branch_name()
                .ok_or(WorkspaceError::MissingBranch(workspace.mode()))?;
            match branch_tip(&self.git, &repository, branch)? {
                None => workspace.base_commit().to_owned(),
                Some(tip) if tip == workspace.base_commit() => tip,
                Some(_) => {
                    let error = WorkspaceError::WorkspaceOwnershipMismatch {
                        run_id,
                        reason: "worktree is absent and advanced branch tip cannot be proven owned"
                            .to_owned(),
                    };
                    Self::persist_broken(store, &mut workspace, error.to_string())?;
                    return Err(error);
                }
            }
        } else {
            workspace.base_commit().to_owned()
        };
        let prior_revision = workspace.revision();
        workspace.mark_removing(removal_head, now());
        store.update_workspace(&workspace, prior_revision)?;
        self.fault(FaultPoint::RemovalIntent)?;
        let workspace = store
            .load_workspace(run_id)?
            .ok_or(WorkspaceError::WorkspaceMissing(run_id))?;
        match self.finish_removal(store, workspace)? {
            ReconciliationOutcome::Removed(_) | ReconciliationOutcome::Unchanged(_) => Ok(()),
            ReconciliationOutcome::Broken(workspace) => Err(WorkspaceError::WorkspaceBroken {
                run_id,
                reason: workspace.last_error().unwrap_or("unknown error").to_owned(),
            }),
            ReconciliationOutcome::Ready(_) => Err(WorkspaceError::WorkspaceBroken {
                run_id,
                reason: "removal did not reach terminal state".to_owned(),
            }),
        }
    }

    fn reconcile_preparing(
        &self,
        store: &mut SqliteStore,
        mut workspace: RunWorkspace,
    ) -> Result<ReconciliationOutcome, WorkspaceError> {
        let repository = match self.validate_source(&workspace) {
            Ok(repository) => repository,
            Err(error) => {
                return Self::break_workspace(store, workspace, error.to_string());
            }
        };
        if workspace.worktree_path().exists() {
            if let Err(error) = self.validate_workspace(&workspace, true) {
                return Self::break_workspace(store, workspace, error.to_string());
            }
        } else {
            if let Some(branch) = workspace.branch_name() {
                if branch_exists(&self.git, &repository, branch)? {
                    return Self::break_workspace(
                        store,
                        workspace,
                        "intended branch exists without intended worktree",
                    );
                }
            }
            if let Err(error) = self.create_intended_worktree(&repository, &workspace) {
                return Self::break_workspace(store, workspace, error.to_string());
            }
            if let Err(error) = self.validate_workspace(&workspace, true) {
                return Self::break_workspace(store, workspace, error.to_string());
            }
        }

        let loaded = store.load_run(workspace.run_id())?;
        if loaded.run.status() != RunStatus::Preparing {
            return Self::break_workspace(
                store,
                workspace,
                format!(
                    "run status is {:?}, expected Preparing",
                    loaded.run.status()
                ),
            );
        }
        let mut run = loaded.run;
        let at = next_time(&run);
        let event = run.transition(RunTransition::FinishPreparation, metadata(at))?;
        let prior_revision = workspace.revision();
        workspace.mark_ready(at);
        store.finalize_workspace_preparation(
            &workspace,
            prior_revision,
            &run,
            loaded.revision,
            &event,
        )?;
        let workspace = store
            .load_workspace(workspace.run_id())?
            .ok_or(WorkspaceError::WorkspaceMissing(workspace.run_id()))?;
        Ok(ReconciliationOutcome::Ready(workspace))
    }

    fn finish_removal(
        &self,
        store: &mut SqliteStore,
        mut workspace: RunWorkspace,
    ) -> Result<ReconciliationOutcome, WorkspaceError> {
        let repository = match self.validate_source(&workspace) {
            Ok(repository) => repository,
            Err(error) => return Self::break_workspace(store, workspace, error.to_string()),
        };
        if workspace.worktree_path().exists() {
            if let Err(error) = self.validate_workspace(&workspace, false) {
                return Self::break_workspace(store, workspace, error.to_string());
            }
            if let Err(error) = remove_worktree(&self.git, &repository, workspace.worktree_path()) {
                return Self::break_workspace(store, workspace, error.to_string());
            }
        }
        if workspace.worktree_path().exists() {
            return Self::break_workspace(store, workspace, "worktree remains after removal");
        }
        if workspace.branch_owned() {
            let branch = workspace
                .branch_name()
                .ok_or(WorkspaceError::MissingBranch(workspace.mode()))?;
            let expected_tip =
                workspace
                    .removal_head()
                    .ok_or(WorkspaceError::InvalidStoredWorkspace(
                        "removal head is missing",
                    ))?;
            if let Err(error) = delete_owned_branch(&self.git, &repository, branch, expected_tip) {
                return Self::break_workspace(store, workspace, error.to_string());
            }
        }
        let prior_revision = workspace.revision();
        workspace.mark_removed(now());
        store.update_workspace(&workspace, prior_revision)?;
        let workspace = store
            .load_workspace(workspace.run_id())?
            .ok_or(WorkspaceError::WorkspaceMissing(workspace.run_id()))?;
        Ok(ReconciliationOutcome::Removed(workspace))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "recovery requires persisted run, workspace patch, repository, and operation identities"
    )]
    fn reconcile_apply(
        &self,
        store: &mut SqliteStore,
        run: Run,
        run_revision: RunRevision,
        repository: &GitRepository,
        patch: &[u8],
        patch_hash: &str,
        mut operation: RunApplyOperation,
    ) -> Result<(), WorkspaceError> {
        if operation.status() == ApplyStatus::Recorded {
            return Err(WorkspaceError::ApplyAlreadyPerformed(run.id()));
        }
        if operation.patch_hash() != patch_hash {
            return Err(WorkspaceError::PatchHashMismatch);
        }
        if operation.run_revision() != run_revision.value() {
            return Err(WorkspaceError::WorkspaceBroken {
                run_id: run.id(),
                reason: "run revision changed after apply intent".to_owned(),
            });
        }
        let forward = check_patch(&self.git, repository, patch, false)?;
        let reverse = check_patch(&self.git, repository, patch, true)?;
        match operation.status() {
            ApplyStatus::Prepared if forward && !reverse => {
                if !source_is_clean(&self.git, repository)? {
                    return Err(WorkspaceError::SourceCheckoutDirty(
                        repository.source_path().to_path_buf(),
                    ));
                }
                if let Err(error) = apply_patch(&self.git, repository, patch) {
                    store.update_apply_operation(
                        &operation,
                        ApplyStatus::Failed,
                        Some(&error.to_string()),
                        now(),
                    )?;
                    return Err(error.into());
                }
                self.fault(FaultPoint::GitApplied)?;
                operation = store.update_apply_operation(
                    &operation,
                    ApplyStatus::AppliedToSource,
                    None,
                    now(),
                )?;
            }
            ApplyStatus::Prepared | ApplyStatus::AppliedToSource if !forward && reverse => {
                if operation.status() == ApplyStatus::Prepared {
                    operation = store.update_apply_operation(
                        &operation,
                        ApplyStatus::AppliedToSource,
                        None,
                        now(),
                    )?;
                }
            }
            ApplyStatus::Failed if forward && !reverse => {
                if !source_is_clean(&self.git, repository)? {
                    return Err(WorkspaceError::SourceCheckoutDirty(
                        repository.source_path().to_path_buf(),
                    ));
                }
                operation =
                    store.update_apply_operation(&operation, ApplyStatus::Prepared, None, now())?;
                if let Err(error) = apply_patch(&self.git, repository, patch) {
                    store.update_apply_operation(
                        &operation,
                        ApplyStatus::Failed,
                        Some(&error.to_string()),
                        now(),
                    )?;
                    return Err(error.into());
                }
                self.fault(FaultPoint::GitApplied)?;
                operation = store.update_apply_operation(
                    &operation,
                    ApplyStatus::AppliedToSource,
                    None,
                    now(),
                )?;
            }
            _ => return Err(WorkspaceError::AmbiguousApplyState),
        }
        Self::finalize_apply(store, run, run_revision, &operation)
    }

    fn finalize_apply(
        store: &mut SqliteStore,
        mut run: Run,
        run_revision: RunRevision,
        operation: &RunApplyOperation,
    ) -> Result<(), WorkspaceError> {
        let at = next_time(&run);
        let event = run.transition(RunTransition::Apply, metadata(at))?;
        store.finalize_apply_operation(operation, &run, run_revision, &event, at)?;
        Ok(())
    }

    fn ready_workspace(store: &SqliteStore, run_id: RunId) -> Result<RunWorkspace, WorkspaceError> {
        let workspace = store
            .load_workspace(run_id)?
            .ok_or(WorkspaceError::WorkspaceMissing(run_id))?;
        if workspace.status() != WorkspaceStatus::Ready {
            return Err(WorkspaceError::InvalidWorkspaceStatus {
                run_id,
                status: workspace.status(),
                expected: "Ready",
            });
        }
        Ok(workspace)
    }

    fn create_intended_worktree(
        &self,
        repository: &GitRepository,
        workspace: &RunWorkspace,
    ) -> Result<(), WorkspaceError> {
        let parent = workspace.worktree_path().parent().ok_or_else(|| {
            WorkspaceError::WorkspacePathConflict(workspace.worktree_path().to_path_buf())
        })?;
        std::fs::create_dir_all(parent)?;
        create_worktree(
            &self.git,
            repository,
            workspace.worktree_path(),
            workspace.base_commit(),
            workspace.branch_name(),
        )?;
        Ok(())
    }

    fn validate_source(&self, workspace: &RunWorkspace) -> Result<GitRepository, WorkspaceError> {
        let repository = GitRepository::discover(workspace.source_repo_path())?;
        if repository.source_path() != workspace.source_repo_path()
            || repository.git_common_dir() != workspace.git_common_dir()
        {
            return Err(WorkspaceError::WorkspaceOwnershipMismatch {
                run_id: workspace.run_id(),
                reason: "source repository identity changed".to_owned(),
            });
        }
        let expected_path = self.workspace_path(&repository, workspace.run_id())?;
        if expected_path != workspace.worktree_path() {
            return Err(WorkspaceError::WorkspaceOwnershipMismatch {
                run_id: workspace.run_id(),
                reason: "persisted worktree path is not deterministic for repository and run"
                    .to_owned(),
            });
        }
        Ok(repository)
    }

    fn validate_workspace(
        &self,
        workspace: &RunWorkspace,
        require_base_head: bool,
    ) -> Result<crate::git::WorktreeIdentity, WorkspaceError> {
        let identity = inspect_worktree(&self.git, workspace.worktree_path())?;
        if identity.path != workspace.worktree_path()
            || identity.git_common_dir != workspace.git_common_dir()
        {
            return Err(WorkspaceError::WorkspaceOwnershipMismatch {
                run_id: workspace.run_id(),
                reason: "path or Git common directory differs".to_owned(),
            });
        }
        let expected_branch = workspace.branch_name();
        if identity.branch.as_deref() != expected_branch {
            return Err(WorkspaceError::WorkspaceOwnershipMismatch {
                run_id: workspace.run_id(),
                reason: format!(
                    "branch differs: expected {expected_branch:?}, found {:?}",
                    identity.branch
                ),
            });
        }
        if require_base_head && identity.head_commit != workspace.base_commit() {
            return Err(WorkspaceError::WorkspaceOwnershipMismatch {
                run_id: workspace.run_id(),
                reason: "initial worktree HEAD differs from persisted base".to_owned(),
            });
        }
        Ok(identity)
    }

    fn workspace_path(
        &self,
        repository: &GitRepository,
        run_id: RunId,
    ) -> Result<PathBuf, WorkspaceError> {
        std::fs::create_dir_all(&self.root)?;
        let root = self.root.canonicalize()?;
        let name = repository
            .source_path()
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("repository");
        let sanitized = sanitize_component(name);
        let common = repository.git_common_dir().to_str().ok_or_else(|| {
            crate::git::GitError::NonUtf8Path(repository.git_common_dir().to_path_buf())
        })?;
        let digest = hash_bytes(common.as_bytes());
        Ok(root
            .join(format!("{sanitized}-{}", &digest[..12]))
            .join(run_id.to_string()))
    }

    fn break_workspace(
        store: &mut SqliteStore,
        mut workspace: RunWorkspace,
        reason: impl Into<String>,
    ) -> Result<ReconciliationOutcome, WorkspaceError> {
        Self::persist_broken(store, &mut workspace, reason)?;
        let workspace = store
            .load_workspace(workspace.run_id())?
            .ok_or(WorkspaceError::WorkspaceMissing(workspace.run_id()))?;
        Ok(ReconciliationOutcome::Broken(workspace))
    }

    fn persist_broken(
        store: &mut SqliteStore,
        workspace: &mut RunWorkspace,
        reason: impl Into<String>,
    ) -> Result<(), WorkspaceError> {
        let prior_revision = workspace.revision();
        workspace.mark_broken(reason, now());
        store.update_workspace(workspace, prior_revision)?;
        Ok(())
    }

    #[cfg(test)]
    fn fault(&self, point: FaultPoint) -> Result<(), WorkspaceError> {
        if self.fault == Some(point) {
            return Err(WorkspaceError::InjectedCrash(point.name()));
        }
        Ok(())
    }

    #[cfg(not(test))]
    #[allow(
        clippy::unnecessary_wraps,
        reason = "production and test fault hooks intentionally share call contract"
    )]
    fn fault(&self, _point: FaultPoint) -> Result<(), WorkspaceError> {
        let _ = &self.root;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FaultPoint {
    WorkspaceIntent,
    WorktreeCreated,
    RemovalIntent,
    GitApplied,
}

#[cfg(test)]
impl FaultPoint {
    const fn name(self) -> &'static str {
        match self {
            Self::WorkspaceIntent => "after workspace intent",
            Self::WorktreeCreated => "after worktree creation",
            Self::RemovalIntent => "after removal intent",
            Self::GitApplied => "after Git apply",
        }
    }
}

fn invalid_run_status(run: &Run, operation: &'static str) -> WorkspaceError {
    WorkspaceError::InvalidRunStatus {
        run_id: run.id(),
        status: run.status(),
        operation,
    }
}

fn metadata(at: DateTime<Utc>) -> EventMetadata {
    EventMetadata::new(EventId::new(), at)
}

fn next_time(run: &Run) -> DateTime<Utc> {
    now().max(*run.updated_at())
}

fn hash_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut hash = String::with_capacity(64);
    for byte in digest {
        hash.push(char::from(HEX[usize::from(byte >> 4)]));
        hash.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    hash
}

fn now() -> DateTime<Utc> {
    std::time::SystemTime::now().into()
}

fn sanitize_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let sanitized = sanitized.trim_matches(['-', '.']);
    let sanitized = sanitized.chars().take(64).collect::<String>();
    if sanitized.is_empty() {
        "repository".to_owned()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::process::Command;

    use chrono::Duration;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::domain::{
        ConfigSnapshotId, Role, StageDefinition, StageId, StageKind, StageTransition,
        WorkflowDefinition,
    };
    use crate::store::ResolvedConfigSnapshot;

    struct Fixture {
        _temp: TempDir,
        source: PathBuf,
        root: PathBuf,
        store: SqliteStore,
        run_id: RunId,
    }

    impl Fixture {
        fn new(kind: WorkflowKind) -> Self {
            let temp = TempDir::new().unwrap();
            let source = temp.path().join("source repo with spaces");
            init_repository(&source);
            let root = temp.path().join("managed worktrees");
            let mut store = SqliteStore::open_in_memory().unwrap();
            let run_id = create_run(&mut store, kind, 1);
            Self {
                _temp: temp,
                source,
                root,
                store,
                run_id,
            }
        }

        fn manager(&self) -> WorkspaceManager {
            WorkspaceManager::new(&self.root)
        }

        fn prepare(&mut self) -> RunWorkspace {
            self.manager()
                .prepare_run_workspace(&mut self.store, self.run_id, &self.source)
                .unwrap()
        }

        fn complete(&mut self) {
            complete_run(&mut self.store, self.run_id);
        }
    }

    #[test]
    fn branch_workspace_is_isolated_and_source_may_be_dirty_at_creation() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        fs::write(fixture.source.join("local-only.txt"), "dirty source\n").unwrap();

        let workspace = fixture.prepare();
        let discovered = GitRepository::discover(&fixture.source).unwrap();

        assert_eq!(workspace.mode(), WorkspaceMode::Branch);
        assert_eq!(workspace.status(), WorkspaceStatus::Ready);
        assert!(!workspace.worktree_path().starts_with(&fixture.source));
        assert_eq!(
            workspace.branch_name(),
            Some(format!("polycode/run-{}", fixture.run_id).as_str())
        );
        assert!(fixture.source.join("local-only.txt").exists());
        assert_eq!(workspace.source_repo_path(), discovered.source_path());
        assert_eq!(workspace.git_common_dir(), discovered.git_common_dir());
        assert_eq!(workspace.base_commit(), discovered.head_commit());
        assert!(!workspace.worktree_path().join("local-only.txt").exists());
        fs::write(workspace.worktree_path().join("README.md"), "worktree\n").unwrap();
        assert_eq!(
            fs::read_to_string(fixture.source.join("README.md")).unwrap(),
            "base\n"
        );
        assert_eq!(
            fixture.store.load_run(fixture.run_id).unwrap().run.status(),
            RunStatus::Ready
        );
    }

    #[test]
    fn review_workspace_is_detached_and_discard_removes_it() {
        let mut fixture = Fixture::new(WorkflowKind::Review);
        let workspace = fixture.prepare();
        assert_eq!(workspace.mode(), WorkspaceMode::Detached);
        assert_eq!(workspace.branch_name(), None);

        fixture
            .manager()
            .discard(&mut fixture.store, fixture.run_id)
            .unwrap();
        fixture
            .manager()
            .discard(&mut fixture.store, fixture.run_id)
            .unwrap();
        fixture
            .manager()
            .cleanup(&mut fixture.store, fixture.run_id)
            .unwrap();

        assert!(!workspace.worktree_path().exists());
        assert_eq!(
            fixture
                .store
                .load_workspace(fixture.run_id)
                .unwrap()
                .unwrap()
                .status(),
            WorkspaceStatus::Removed
        );
        assert_eq!(
            fixture.store.load_run(fixture.run_id).unwrap().run.status(),
            RunStatus::Discarded
        );
    }

    #[test]
    fn multiple_runs_get_distinct_worktrees_and_branches() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let second = create_run(&mut fixture.store, WorkflowKind::Standard, 2);
        let manager = fixture.manager();

        let first_workspace = manager
            .prepare_run_workspace(&mut fixture.store, fixture.run_id, &fixture.source)
            .unwrap();
        let second_workspace = manager
            .prepare_run_workspace(&mut fixture.store, second, &fixture.source)
            .unwrap();

        assert_ne!(
            first_workspace.worktree_path(),
            second_workspace.worktree_path()
        );
        assert_ne!(
            first_workspace.branch_name(),
            second_workspace.branch_name()
        );
        assert!(first_workspace.worktree_path().exists());
        assert!(second_workspace.worktree_path().exists());
    }

    #[test]
    fn apply_transfers_tracked_untracked_deleted_and_binary_without_staging_or_commit() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        fs::write(fixture.source.join("delete me.txt"), "remove\n").unwrap();
        git(&fixture.source, ["add", "."]);
        git(&fixture.source, ["commit", "-m", "add deletion fixture"]);
        let source_head = git_text(&fixture.source, ["rev-parse", "HEAD"]);
        let workspace = fixture.prepare();
        fixture.complete();

        fs::write(workspace.worktree_path().join("README.md"), "changed\n").unwrap();
        git(workspace.worktree_path(), ["add", "README.md"]);
        git(
            workspace.worktree_path(),
            ["commit", "-m", "commit run change"],
        );
        fs::write(
            workspace.worktree_path().join("new file ü.txt"),
            "untracked\n",
        )
        .unwrap();
        fs::write(
            workspace.worktree_path().join("tab\tnewline\nü.txt"),
            "odd name\n",
        )
        .unwrap();
        fs::remove_file(workspace.worktree_path().join("delete me.txt")).unwrap();
        fs::write(
            workspace.worktree_path().join("binary.bin"),
            [0_u8, 1, 2, 0, 255, 128],
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(fixture.source.join("README.md")).unwrap(),
            "base\n"
        );
        assert!(git_text(&fixture.source, ["status", "--porcelain"]).is_empty());

        fixture
            .manager()
            .apply(&mut fixture.store, fixture.run_id)
            .unwrap();

        assert_eq!(
            fs::read_to_string(fixture.source.join("README.md")).unwrap(),
            "changed\n"
        );
        assert_eq!(
            fs::read_to_string(fixture.source.join("new file ü.txt")).unwrap(),
            "untracked\n"
        );
        assert_eq!(
            fs::read_to_string(fixture.source.join("tab\tnewline\nü.txt")).unwrap(),
            "odd name\n"
        );
        assert!(!fixture.source.join("delete me.txt").exists());
        assert_eq!(
            fs::read(fixture.source.join("binary.bin")).unwrap(),
            [0_u8, 1, 2, 0, 255, 128]
        );
        assert_eq!(
            git_text(&fixture.source, ["rev-parse", "HEAD"]),
            source_head
        );
        assert!(git_status(&fixture.source, ["diff", "--cached", "--quiet"]));
        assert!(git_status(
            workspace.worktree_path(),
            ["diff", "--cached", "--quiet"]
        ));
        assert_eq!(
            fixture.store.load_run(fixture.run_id).unwrap().run.status(),
            RunStatus::Applied
        );
        assert_eq!(
            fixture
                .store
                .load_apply_operation(fixture.run_id)
                .unwrap()
                .unwrap()
                .status(),
            ApplyStatus::Recorded
        );
        assert!(workspace.worktree_path().exists());
        assert!(matches!(
            fixture.manager().apply(&mut fixture.store, fixture.run_id),
            Err(WorkspaceError::ApplyAlreadyPerformed(_))
        ));
    }

    #[test]
    fn dirty_source_rejects_apply_without_mutation() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let workspace = fixture.prepare();
        fixture.complete();
        fs::write(workspace.worktree_path().join("README.md"), "run change\n").unwrap();
        fs::write(fixture.source.join("local.txt"), "mine\n").unwrap();

        let result = fixture.manager().apply(&mut fixture.store, fixture.run_id);

        assert!(matches!(
            result,
            Err(WorkspaceError::SourceCheckoutDirty(_))
        ));
        assert_eq!(
            fs::read_to_string(fixture.source.join("README.md")).unwrap(),
            "base\n"
        );
        assert!(
            fixture
                .store
                .load_apply_operation(fixture.run_id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn preflight_conflict_records_failure_and_does_not_partially_apply() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let workspace = fixture.prepare();
        fixture.complete();
        fs::write(workspace.worktree_path().join("README.md"), "run change\n").unwrap();
        fs::write(fixture.source.join("README.md"), "source advanced\n").unwrap();
        git(&fixture.source, ["add", "README.md"]);
        git(&fixture.source, ["commit", "-m", "advance source"]);

        let result = fixture.manager().apply(&mut fixture.store, fixture.run_id);

        assert!(matches!(result, Err(WorkspaceError::PatchCheckFailed)));
        assert_eq!(
            fs::read_to_string(fixture.source.join("README.md")).unwrap(),
            "source advanced\n"
        );
        assert_eq!(
            fixture
                .store
                .load_apply_operation(fixture.run_id)
                .unwrap()
                .unwrap()
                .status(),
            ApplyStatus::Failed
        );
    }

    #[test]
    fn creation_crash_windows_reconcile_without_duplicates() {
        for fault in [FaultPoint::WorkspaceIntent, FaultPoint::WorktreeCreated] {
            let mut fixture = Fixture::new(WorkflowKind::Standard);
            let crashing = WorkspaceManager::with_fault(&fixture.root, fault);
            assert!(matches!(
                crashing.prepare_run_workspace(&mut fixture.store, fixture.run_id, &fixture.source),
                Err(WorkspaceError::InjectedCrash(_))
            ));
            assert_eq!(
                fixture
                    .store
                    .load_workspace(fixture.run_id)
                    .unwrap()
                    .unwrap()
                    .status(),
                WorkspaceStatus::Preparing
            );

            let outcome = fixture
                .manager()
                .reconcile(&mut fixture.store, fixture.run_id)
                .unwrap();
            assert!(matches!(outcome, ReconciliationOutcome::Ready(_)));
            assert!(matches!(
                fixture
                    .manager()
                    .reconcile(&mut fixture.store, fixture.run_id)
                    .unwrap(),
                ReconciliationOutcome::Unchanged(_)
            ));
        }
    }

    #[test]
    fn discard_after_creation_crash_removes_proven_branch() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let crashing = WorkspaceManager::with_fault(&fixture.root, FaultPoint::WorktreeCreated);
        assert!(
            crashing
                .prepare_run_workspace(&mut fixture.store, fixture.run_id, &fixture.source)
                .is_err()
        );
        let workspace = fixture
            .store
            .load_workspace(fixture.run_id)
            .unwrap()
            .unwrap();
        let reference = format!("refs/heads/{}", workspace.branch_name().unwrap());

        fixture
            .manager()
            .discard(&mut fixture.store, fixture.run_id)
            .unwrap();

        assert!(!workspace.worktree_path().exists());
        assert!(!git_status(
            &fixture.source,
            ["rev-parse", "--verify", "--quiet", &reference]
        ));
    }

    #[test]
    fn apply_crash_after_git_effect_finalizes_once() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let workspace = fixture.prepare();
        fixture.complete();
        fs::write(workspace.worktree_path().join("README.md"), "recovered\n").unwrap();
        fs::write(
            workspace.worktree_path().join("recovered-new.txt"),
            "created once\n",
        )
        .unwrap();
        let crashing = WorkspaceManager::with_fault(&fixture.root, FaultPoint::GitApplied);

        assert!(matches!(
            crashing.apply(&mut fixture.store, fixture.run_id),
            Err(WorkspaceError::InjectedCrash(_))
        ));
        assert_eq!(
            fs::read_to_string(fixture.source.join("README.md")).unwrap(),
            "recovered\n"
        );
        assert_eq!(
            fs::read_to_string(fixture.source.join("recovered-new.txt")).unwrap(),
            "created once\n"
        );
        assert_eq!(
            fixture.store.load_run(fixture.run_id).unwrap().run.status(),
            RunStatus::Completed
        );
        assert_eq!(
            fixture
                .store
                .load_apply_operation(fixture.run_id)
                .unwrap()
                .unwrap()
                .status(),
            ApplyStatus::Prepared
        );
        assert!(matches!(
            fixture
                .manager()
                .discard(&mut fixture.store, fixture.run_id),
            Err(WorkspaceError::Store(
                crate::store::StoreError::RunFrozenForApply(_)
            ))
        ));
        assert!(matches!(
            fixture
                .manager()
                .cleanup(&mut fixture.store, fixture.run_id),
            Err(WorkspaceError::ApplyInProgress(_))
        ));

        fixture
            .manager()
            .apply(&mut fixture.store, fixture.run_id)
            .unwrap();
        assert_eq!(
            fs::read_to_string(fixture.source.join("README.md")).unwrap(),
            "recovered\n"
        );
        assert_eq!(
            fs::read_to_string(fixture.source.join("recovered-new.txt")).unwrap(),
            "created once\n"
        );
        assert_eq!(
            fixture.store.load_run(fixture.run_id).unwrap().run.status(),
            RunStatus::Applied
        );
    }

    #[test]
    fn removal_crash_reconciles_and_keeps_logical_completion() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let workspace = fixture.prepare();
        fixture.complete();
        let crashing = WorkspaceManager::with_fault(&fixture.root, FaultPoint::RemovalIntent);

        assert!(matches!(
            crashing.cleanup(&mut fixture.store, fixture.run_id),
            Err(WorkspaceError::InjectedCrash(_))
        ));
        assert_eq!(
            fixture
                .store
                .load_workspace(fixture.run_id)
                .unwrap()
                .unwrap()
                .status(),
            WorkspaceStatus::Removing
        );
        assert_eq!(
            fixture.store.load_run(fixture.run_id).unwrap().run.status(),
            RunStatus::Completed
        );

        assert!(matches!(
            fixture
                .manager()
                .reconcile(&mut fixture.store, fixture.run_id)
                .unwrap(),
            ReconciliationOutcome::Removed(_)
        ));
        assert!(!workspace.worktree_path().exists());
    }

    #[test]
    fn removal_reconciles_when_git_effect_already_happened() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let workspace = fixture.prepare();
        fixture.complete();
        let crashing = WorkspaceManager::with_fault(&fixture.root, FaultPoint::RemovalIntent);
        assert!(
            crashing
                .cleanup(&mut fixture.store, fixture.run_id)
                .is_err()
        );
        git_os(
            &fixture.source,
            [
                OsStr::new("worktree"),
                OsStr::new("remove"),
                OsStr::new("--force"),
                workspace.worktree_path().as_os_str(),
            ],
        );

        let outcome = fixture
            .manager()
            .reconcile(&mut fixture.store, fixture.run_id)
            .unwrap();

        assert!(matches!(outcome, ReconciliationOutcome::Removed(_)));
        assert!(!workspace.worktree_path().exists());
        let reference = format!("refs/heads/{}", workspace.branch_name().unwrap());
        assert!(!git_status(
            &fixture.source,
            ["rev-parse", "--verify", "--quiet", &reference]
        ));
    }

    #[test]
    fn workspace_persists_across_store_reopen() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("persistent source");
        init_repository(&source);
        let root = temp.path().join("persistent worktrees");
        let database = temp.path().join("state").join("polycode.db");
        let run_id;
        let expected;
        {
            let mut store = SqliteStore::open(&database).unwrap();
            run_id = create_run(&mut store, WorkflowKind::Standard, 77);
            expected = WorkspaceManager::new(&root)
                .prepare_run_workspace(&mut store, run_id, &source)
                .unwrap();
        }

        let mut reopened = SqliteStore::open(&database).unwrap();
        let restored = reopened.load_workspace(run_id).unwrap().unwrap();

        assert_eq!(restored, expected);
        assert!(matches!(
            WorkspaceManager::new(&root)
                .reconcile(&mut reopened, run_id)
                .unwrap(),
            ReconciliationOutcome::Unchanged(_)
        ));
    }

    #[test]
    fn review_workspace_rejects_apply_even_when_logically_completed() {
        let mut fixture = Fixture::new(WorkflowKind::Review);
        let workspace = fixture.prepare();
        fixture.complete();
        fs::write(workspace.worktree_path().join("README.md"), "review edit\n").unwrap();

        let result = fixture.manager().apply(&mut fixture.store, fixture.run_id);

        assert!(matches!(
            result,
            Err(WorkspaceError::ReviewWorkspaceNotApplicable)
        ));
        assert_eq!(
            fs::read_to_string(fixture.source.join("README.md")).unwrap(),
            "base\n"
        );
    }

    #[test]
    fn deterministic_paths_reject_collisions_and_existing_branches() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let manager = fixture.manager();
        let repository = GitRepository::discover(&fixture.source).unwrap();
        let first = manager.workspace_path(&repository, fixture.run_id).unwrap();
        let second = manager.workspace_path(&repository, fixture.run_id).unwrap();
        assert_eq!(first, second);
        assert!(first.is_absolute());
        assert!(first.to_string_lossy().contains("source-repo-with-spaces-"));
        fs::create_dir_all(&first).unwrap();
        assert!(matches!(
            manager.prepare_run_workspace(&mut fixture.store, fixture.run_id, &fixture.source),
            Err(WorkspaceError::WorkspacePathConflict(_))
        ));

        let mut branch_fixture = Fixture::new(WorkflowKind::Standard);
        let branch = format!("polycode/run-{}", branch_fixture.run_id);
        git(&branch_fixture.source, ["branch", &branch]);
        assert!(matches!(
            branch_fixture.manager().prepare_run_workspace(
                &mut branch_fixture.store,
                branch_fixture.run_id,
                &branch_fixture.source,
            ),
            Err(WorkspaceError::BranchConflict(found)) if found == branch
        ));
        assert!(
            branch_fixture
                .store
                .load_workspace(branch_fixture.run_id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn relocated_source_is_marked_broken_not_guessed() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let workspace = fixture.prepare();
        let relocated = fixture.source.with_file_name("relocated source");
        fs::rename(&fixture.source, &relocated).unwrap();

        let outcome = fixture
            .manager()
            .reconcile(&mut fixture.store, fixture.run_id)
            .unwrap();

        assert!(matches!(outcome, ReconciliationOutcome::Broken(_)));
        assert_eq!(
            fixture
                .store
                .load_workspace(fixture.run_id)
                .unwrap()
                .unwrap()
                .status(),
            WorkspaceStatus::Broken
        );
        assert!(workspace.worktree_path().exists());
    }

    #[test]
    fn foreign_path_and_missing_ready_workspace_become_broken_without_deletion() {
        let mut foreign_fixture = Fixture::new(WorkflowKind::Standard);
        let crashing =
            WorkspaceManager::with_fault(&foreign_fixture.root, FaultPoint::WorkspaceIntent);
        assert!(
            crashing
                .prepare_run_workspace(
                    &mut foreign_fixture.store,
                    foreign_fixture.run_id,
                    &foreign_fixture.source,
                )
                .is_err()
        );
        let intended = foreign_fixture
            .store
            .load_workspace(foreign_fixture.run_id)
            .unwrap()
            .unwrap();
        init_repository(intended.worktree_path());
        let outcome = foreign_fixture
            .manager()
            .reconcile(&mut foreign_fixture.store, foreign_fixture.run_id)
            .unwrap();
        assert!(matches!(outcome, ReconciliationOutcome::Broken(_)));
        assert!(intended.worktree_path().join("README.md").exists());

        let mut missing_fixture = Fixture::new(WorkflowKind::Standard);
        let ready = missing_fixture.prepare();
        git_os(
            &missing_fixture.source,
            [
                OsStr::new("worktree"),
                OsStr::new("remove"),
                OsStr::new("--force"),
                ready.worktree_path().as_os_str(),
            ],
        );
        let outcome = missing_fixture
            .manager()
            .reconcile(&mut missing_fixture.store, missing_fixture.run_id)
            .unwrap();
        assert!(matches!(outcome, ReconciliationOutcome::Broken(_)));
    }

    #[test]
    fn moved_owned_branch_is_never_deleted_during_recovery() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let workspace = fixture.prepare();
        fixture.complete();
        let branch = workspace.branch_name().unwrap().to_owned();
        let crashing = WorkspaceManager::with_fault(&fixture.root, FaultPoint::RemovalIntent);
        assert!(
            crashing
                .cleanup(&mut fixture.store, fixture.run_id)
                .is_err()
        );

        fs::write(
            workspace.worktree_path().join("README.md"),
            "new branch tip\n",
        )
        .unwrap();
        git(workspace.worktree_path(), ["add", "README.md"]);
        git(
            workspace.worktree_path(),
            ["commit", "-m", "move owned branch"],
        );
        let moved_tip = git_text(workspace.worktree_path(), ["rev-parse", "HEAD"]);

        let outcome = fixture
            .manager()
            .reconcile(&mut fixture.store, fixture.run_id)
            .unwrap();
        assert!(matches!(outcome, ReconciliationOutcome::Broken(_)));
        assert_eq!(
            git_text(
                &fixture.source,
                ["rev-parse", &format!("refs/heads/{branch}")]
            ),
            moved_tip
        );
    }

    #[test]
    fn advanced_branch_without_worktree_is_not_assumed_owned() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let workspace = fixture.prepare();
        fixture.complete();
        fs::write(workspace.worktree_path().join("README.md"), "advanced\n").unwrap();
        git(workspace.worktree_path(), ["add", "README.md"]);
        git(
            workspace.worktree_path(),
            ["commit", "-m", "advance branch"],
        );
        let branch = workspace.branch_name().unwrap().to_owned();
        let tip = git_text(workspace.worktree_path(), ["rev-parse", "HEAD"]);
        git_os(
            &fixture.source,
            [
                OsStr::new("worktree"),
                OsStr::new("remove"),
                OsStr::new("--force"),
                workspace.worktree_path().as_os_str(),
            ],
        );

        assert!(matches!(
            fixture
                .manager()
                .cleanup(&mut fixture.store, fixture.run_id),
            Err(WorkspaceError::WorkspaceOwnershipMismatch { .. })
        ));
        assert_eq!(
            git_text(
                &fixture.source,
                ["rev-parse", &format!("refs/heads/{branch}")]
            ),
            tip
        );
        assert_eq!(
            fixture
                .store
                .load_workspace(fixture.run_id)
                .unwrap()
                .unwrap()
                .status(),
            WorkspaceStatus::Broken
        );
    }

    #[test]
    fn workspace_revision_rejects_stale_writer() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let workspace = fixture.prepare();
        let mut first = workspace.clone();
        let mut stale = workspace;
        first.mark_broken("first writer", now());
        fixture
            .store
            .update_workspace(&first, first.revision())
            .unwrap();
        stale.mark_broken("stale writer", now());

        let error = fixture
            .store
            .update_workspace(&stale, stale.revision())
            .unwrap_err();

        assert!(matches!(
            error,
            crate::store::StoreError::WorkspaceConcurrentModification { .. }
        ));
        assert_eq!(
            fixture
                .store
                .load_workspace(fixture.run_id)
                .unwrap()
                .unwrap()
                .last_error(),
            Some("first writer")
        );
    }

    #[test]
    fn corrupt_workspace_record_never_becomes_valid_infrastructure_state() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        fixture.prepare();
        fixture
            .store
            .connection
            .execute(
                "UPDATE run_workspaces SET base_commit = ?1 WHERE run_id = ?2",
                rusqlite::params!["z".repeat(40), fixture.run_id.to_string()],
            )
            .unwrap();

        assert!(matches!(
            fixture.store.load_workspace(fixture.run_id),
            Err(crate::store::StoreError::InvalidWorkspaceRecord(_))
        ));
        assert_eq!(
            fixture.store.load_run(fixture.run_id).unwrap().run.status(),
            RunStatus::Ready
        );
    }

    #[test]
    fn ready_workspace_and_run_event_roll_back_together() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let crashing = WorkspaceManager::with_fault(&fixture.root, FaultPoint::WorkspaceIntent);
        assert!(
            crashing
                .prepare_run_workspace(&mut fixture.store, fixture.run_id, &fixture.source)
                .is_err()
        );
        let mut workspace = fixture
            .store
            .load_workspace(fixture.run_id)
            .unwrap()
            .unwrap();
        let loaded = fixture.store.load_run(fixture.run_id).unwrap();
        let duplicate_id = fixture.store.load_events(fixture.run_id).unwrap()[0]
            .event
            .id();
        let mut run = loaded.run;
        let at = *run.updated_at() + Duration::milliseconds(1);
        let event = run
            .transition(
                RunTransition::FinishPreparation,
                EventMetadata::new(duplicate_id, at),
            )
            .unwrap();
        workspace.mark_ready(at);

        assert!(
            fixture
                .store
                .finalize_workspace_preparation(
                    &workspace,
                    workspace.revision(),
                    &run,
                    loaded.revision,
                    &event,
                )
                .is_err()
        );
        assert_eq!(
            fixture
                .store
                .load_workspace(fixture.run_id)
                .unwrap()
                .unwrap()
                .status(),
            WorkspaceStatus::Preparing
        );
        assert_eq!(
            fixture.store.load_run(fixture.run_id).unwrap().run.status(),
            RunStatus::Preparing
        );
    }

    #[test]
    fn applied_operation_and_run_event_roll_back_together() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let workspace = fixture.prepare();
        fixture.complete();
        fs::write(
            workspace.worktree_path().join("README.md"),
            "applied effect\n",
        )
        .unwrap();
        let crashing = WorkspaceManager::with_fault(&fixture.root, FaultPoint::GitApplied);
        assert!(crashing.apply(&mut fixture.store, fixture.run_id).is_err());
        let prepared = fixture
            .store
            .load_apply_operation(fixture.run_id)
            .unwrap()
            .unwrap();
        let operation = fixture
            .store
            .update_apply_operation(&prepared, ApplyStatus::AppliedToSource, None, now())
            .unwrap();
        let loaded = fixture.store.load_run(fixture.run_id).unwrap();
        let duplicate_id = fixture.store.load_events(fixture.run_id).unwrap()[0]
            .event
            .id();
        let mut run = loaded.run;
        let at = *run.updated_at() + Duration::milliseconds(1);
        let event = run
            .transition(RunTransition::Apply, EventMetadata::new(duplicate_id, at))
            .unwrap();

        assert!(
            fixture
                .store
                .finalize_apply_operation(&operation, &run, loaded.revision, &event, at,)
                .is_err()
        );
        assert_eq!(
            fixture
                .store
                .load_apply_operation(fixture.run_id)
                .unwrap()
                .unwrap()
                .status(),
            ApplyStatus::AppliedToSource
        );
        assert_eq!(
            fixture.store.load_run(fixture.run_id).unwrap().run.status(),
            RunStatus::Completed
        );
    }

    fn create_run(store: &mut SqliteStore, kind: WorkflowKind, id: u128) -> RunId {
        let run_id = RunId::from_u128(id);
        let stage_id = StageId::new("implementation").unwrap();
        let workflow = WorkflowDefinition::new(
            kind,
            vec![StageDefinition::new(
                stage_id,
                StageKind::Implementation,
                Role::Implementer,
                vec![],
            )],
        )
        .unwrap();
        let created_at = now();
        let config_id = ConfigSnapshotId::new(format!("config-{id}")).unwrap();
        let run = Run::new(
            run_id,
            "test workspace",
            workflow,
            config_id.clone(),
            created_at,
        )
        .unwrap();
        let config = ResolvedConfigSnapshot::new(config_id, 1, json!({}), created_at).unwrap();
        let event = run.created_event(metadata(created_at));
        store.create_run(&run, &config, &[event]).unwrap();
        run_id
    }

    fn complete_run(store: &mut SqliteStore, run_id: RunId) {
        let loaded = store.load_run(run_id).unwrap();
        let mut run = loaded.run;
        let mut revision = loaded.revision;
        let at = *run.updated_at() + Duration::milliseconds(1);
        let event = run.transition(RunTransition::Start, metadata(at)).unwrap();
        revision = store
            .commit_run_update(&run, revision, &[event])
            .unwrap()
            .revision();
        let stage = StageId::new("implementation").unwrap();
        for transition in [
            StageTransition::MarkReady,
            StageTransition::Start,
            StageTransition::Complete,
        ] {
            let at = *run.updated_at() + Duration::milliseconds(1);
            let event = run
                .transition_stage(&stage, transition, metadata(at))
                .unwrap();
            revision = store
                .commit_run_update(&run, revision, &[event])
                .unwrap()
                .revision();
        }
        let at = *run.updated_at() + Duration::milliseconds(1);
        let event = run
            .transition(RunTransition::Complete, metadata(at))
            .unwrap();
        store.commit_run_update(&run, revision, &[event]).unwrap();
    }

    fn init_repository(path: &Path) {
        fs::create_dir_all(path).unwrap();
        git(path, ["init", "-b", "main"]);
        git(path, ["config", "user.name", "Polycode Test"]);
        git(path, ["config", "user.email", "polycode@example.invalid"]);
        git(path, ["config", "commit.gpgsign", "false"]);
        git(path, ["config", "core.hooksPath", ".git/polycode-no-hooks"]);
        git(path, ["config", "core.autocrlf", "false"]);
        fs::write(path.join("README.md"), "base\n").unwrap();
        git(path, ["add", "README.md"]);
        git(path, ["commit", "-m", "initial"]);
    }

    fn git<const N: usize>(cwd: &Path, args: [&str; N]) {
        git_os(cwd, args.map(OsStr::new));
    }

    fn git_os<const N: usize>(cwd: &Path, args: [&OsStr; N]) {
        let output = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_text<const N: usize>(cwd: &Path, args: [&str; N]) -> String {
        let output = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn git_status<const N: usize>(cwd: &Path, args: [&str; N]) -> bool {
        Command::new("git")
            .current_dir(cwd)
            .args(args)
            .status()
            .unwrap()
            .success()
    }
}
