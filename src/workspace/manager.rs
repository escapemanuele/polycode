use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::domain::{
    EventId, EventMetadata, Run, RunId, RunStatus, RunTransition, StageKind, StageStatus,
};
use crate::git::{
    GitRepository, apply_patch, branch_exists, branch_tip, check_patch, commit_all_in_worktree,
    create_branch_in_worktree, create_worktree, delete_owned_branch, detach_worktree,
    generate_patch, generate_patch_preview, inspect_worktree, push_branch, remote_url,
    remove_worktree, source_is_clean, tree_is_clean,
};
use crate::store::{RunInput, RunRevision, SqliteStore, worktree_root};

use super::branch_name;
use super::github::GhClient;
use super::pull_request::PullRequestDraft;
use super::{
    ApplyStatus, RunApplyOperation, RunWorkspace, WorkspaceError, WorkspaceMode, WorkspaceStatus,
};

/// What one publish actually did: the branch and commit that reached the
/// remote, and what became of the pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishReceipt {
    pub branch: String,
    pub commit: String,
    pub pull_request: PullRequestStatus,
}

/// The pull-request half of a publish, which is allowed to fall short without
/// failing the publish: once the branch is pushed, the work is safe, and a
/// missing or unauthenticated `gh` only costs the convenience.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PullRequestStatus {
    Created(String),
    AlreadyExists(String),
    Unavailable(String),
}

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
        let mode = if loaded.run.workflow().requires_writable_workspace() {
            WorkspaceMode::Branch
        } else {
            WorkspaceMode::Detached
        };
        let input = store.load_run_input(run_id)?;
        let branch = (mode == WorkspaceMode::Branch)
            .then(|| branch_name::branch_name(run_id, input.as_ref().map(RunInput::task)));
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
            WorkspaceStatus::Ready => match self.observe(&workspace) {
                Ok(()) => Ok(ReconciliationOutcome::Unchanged(workspace)),
                Err(reason) => Self::break_workspace(store, workspace, reason),
            },
            WorkspaceStatus::Removing => self.finish_removal(store, workspace),
            WorkspaceStatus::Removed => Ok(ReconciliationOutcome::Unchanged(workspace)),
            WorkspaceStatus::Broken => self.reconcile_broken(store, workspace),
        }
    }

    /// Re-observes a workspace that persisted state calls usable, healing what
    /// is safely healable before reporting anything wrong.
    ///
    /// Returns the reason the workspace is unusable, in the words that reach
    /// the operator through `last_error`.
    fn observe(&self, workspace: &RunWorkspace) -> Result<(), String> {
        if let Err(error) = self.validate_source(workspace) {
            return Err(error.to_string());
        }
        if !workspace.worktree_path().exists() {
            return Err("ready worktree is missing".to_owned());
        }
        if let Err(error) = self.restore_detached_head(workspace) {
            return Err(error.to_string());
        }
        self.validate_workspace(workspace, false)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Returns a detached worktree to its base commit after an agent moved HEAD.
    ///
    /// An agent told to review a pull request will reach for `gh pr checkout`,
    /// and inside a worktree Polycode owns exclusively that is reasonable work,
    /// not a loss of ownership — which the worktree's path and Git common
    /// directory still prove, and which this does not touch. Treating the moved
    /// HEAD as a mismatch broke the run permanently on the next reconcile, with
    /// no route back that did not edit the store by hand.
    ///
    /// Uncommitted work is the one case this refuses to resolve alone.
    /// Re-detaching would discard changes no event ever recorded, so a dirty
    /// tree under a moved HEAD stays broken for an operator to judge.
    fn restore_detached_head(&self, workspace: &RunWorkspace) -> Result<(), WorkspaceError> {
        if workspace.mode() != WorkspaceMode::Detached {
            return Ok(());
        }
        let identity = inspect_worktree(&self.git, workspace.worktree_path())?;
        if identity.branch.is_none() && identity.head_commit == workspace.base_commit() {
            return Ok(());
        }
        if !tree_is_clean(&self.git, workspace.worktree_path())? {
            return Err(WorkspaceError::WorkspaceOwnershipMismatch {
                run_id: workspace.run_id(),
                reason: format!(
                    "detached worktree carries uncommitted work at {}, away from base {}",
                    identity.head_commit,
                    workspace.base_commit()
                ),
            });
        }
        detach_worktree(
            &self.git,
            workspace.worktree_path(),
            workspace.base_commit(),
        )?;
        Ok(())
    }

    /// Re-observes a broken workspace instead of treating the verdict as final.
    ///
    /// What breaks a workspace is usually a condition outside it — a source
    /// checkout that moved, a worktree not yet on disk, an agent that walked
    /// HEAD off its base — and once the condition is gone the run is
    /// recoverable. Broken is what was observed, not a property the workspace
    /// acquired, so it is observed again; a workspace that still fails stays
    /// broken and reports the current reason rather than the original one.
    fn reconcile_broken(
        &self,
        store: &mut SqliteStore,
        mut workspace: RunWorkspace,
    ) -> Result<ReconciliationOutcome, WorkspaceError> {
        if let Err(reason) = self.observe(&workspace) {
            if workspace.last_error() != Some(reason.as_str()) {
                Self::persist_broken(store, &mut workspace, reason)?;
            }
            return Ok(ReconciliationOutcome::Broken(workspace));
        }
        let prior_revision = workspace.revision();
        workspace.mark_ready(now());
        store.update_workspace(&workspace, prior_revision)?;
        Ok(ReconciliationOutcome::Ready(workspace))
    }

    /// Gives a review's workspace a branch, so a fix cycle can reach the
    /// operator's checkout.
    ///
    /// A review is prepared detached: it produces findings, not changes, and
    /// apply refuses anything but a branch Polycode owns. Sending that run back
    /// to fix what it found is the moment the run starts producing changes, so
    /// it is the moment the workspace earns a branch — created at the
    /// worktree's current HEAD, which is the tree the fix will edit.
    ///
    /// Idempotent by construction: a run already on a branch, including one on
    /// its second fix cycle, is returned unchanged.
    ///
    /// # Errors
    /// Returns ownership, branch-conflict, Git, or persistence errors. A
    /// workspace that is not Ready is refused without being modified.
    pub fn adopt_branch_for_fix(
        &self,
        store: &mut SqliteStore,
        run_id: RunId,
    ) -> Result<RunWorkspace, WorkspaceError> {
        let mut workspace = Self::ready_workspace(store, run_id)?;
        if workspace.mode() == WorkspaceMode::Branch {
            return Ok(workspace);
        }
        let repository = self.validate_source(&workspace)?;
        self.validate_workspace(&workspace, false)?;
        let input = store.load_run_input(run_id)?;
        let branch = branch_name::branch_name(run_id, input.as_ref().map(RunInput::task));
        if branch_exists(&self.git, &repository, &branch)? {
            return Err(WorkspaceError::BranchConflict(branch));
        }
        create_branch_in_worktree(&self.git, workspace.worktree_path(), &branch)?;
        let prior_revision = workspace.revision();
        workspace.adopt_branch(branch, now());
        store.adopt_workspace_branch(&workspace, prior_revision)?;
        store
            .load_workspace(run_id)?
            .ok_or(WorkspaceError::WorkspaceMissing(run_id))
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
        ensure_verification_passed(&loaded.run)?;
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

    /// Publishes a completed run as a remote branch and pull request, never
    /// touching the operator's checkout.
    ///
    /// The counterpart to apply for a source the operator does not want
    /// written to: the run's delta is committed on the branch the run already
    /// owns, the branch is pushed to `origin`, and a pull request is opened
    /// through the GitHub CLI. Runs stay `Completed` — publish is transport,
    /// not disposition, so apply, fix, and discard all remain available, and
    /// publishing again after a fix cycle updates the same branch and pull
    /// request.
    ///
    /// Push-first by design: pull-request failures (no `gh`, not
    /// authenticated) are reported inside the receipt, because by then the
    /// work is already safe on the remote.
    ///
    /// # Errors
    /// Rejects non-completed runs, detached/review workspaces, workspaces with
    /// nothing to publish, and repositories without an `origin` remote.
    /// Returns Git errors from committing or pushing.
    pub fn publish(
        &self,
        store: &mut SqliteStore,
        run_id: RunId,
        draft: Option<&PullRequestDraft>,
    ) -> Result<PublishReceipt, WorkspaceError> {
        self.publish_with(store, run_id, draft, &GhClient::default())
    }

    /// `draft` is the pull request the latest editing stage wrote for its
    /// change, when it wrote one; the task text stands in for whatever the
    /// draft lacks, so a run that predates the contract publishes as before.
    fn publish_with(
        &self,
        store: &mut SqliteStore,
        run_id: RunId,
        draft: Option<&PullRequestDraft>,
        gh: &GhClient,
    ) -> Result<PublishReceipt, WorkspaceError> {
        let loaded = store.load_run(run_id)?;
        ensure_verification_passed(&loaded.run)?;
        if loaded.run.status() != RunStatus::Completed {
            return Err(invalid_run_status(&loaded.run, "publish"));
        }
        let workspace = Self::ready_workspace(store, run_id)?;
        if workspace.mode() != WorkspaceMode::Branch {
            return Err(WorkspaceError::ReviewWorkspaceNotApplicable);
        }
        self.validate_workspace(&workspace, false)?;
        self.validate_source(&workspace)?;
        let branch = workspace
            .branch_name()
            .ok_or(WorkspaceError::MissingBranch(workspace.mode()))?
            .to_owned();
        let worktree = workspace.worktree_path();
        // The same gate cleanup honors: a run stranded mid-apply-recovery has
        // an operation whose outcome is not yet known, and nothing else may
        // move until apply resolves it.
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
        // Every pure refusal comes before the commit, so a refused publish
        // leaves the worktree exactly as it found it.
        if remote_url(&self.git, worktree, "origin")?.is_none() {
            return Err(WorkspaceError::NoRemote(
                workspace.source_repo_path().to_path_buf(),
            ));
        }

        let task = store
            .load_run_input(run_id)?
            .map(|input| input.task().to_owned());
        let title = draft.map_or_else(
            || publish_title(task.as_deref(), run_id),
            |draft| bounded_title(&draft.title),
        );
        let commit = if tree_is_clean(&self.git, worktree)? {
            inspect_worktree(&self.git, worktree)?.head_commit
        } else {
            let message = format!("{title}\n\nPolycode run {run_id}");
            commit_all_in_worktree(&self.git, worktree, &message)?
        };
        if commit == workspace.base_commit() {
            return Err(WorkspaceError::NothingToPublish);
        }
        push_branch(&self.git, worktree, "origin", &branch)?;

        let pull_request = match gh.existing_pull_request(worktree, &branch) {
            Ok(Some(url)) => PullRequestStatus::AlreadyExists(url),
            Ok(None) => {
                let body = draft.filter(|draft| !draft.body.is_empty()).map_or_else(
                    || publish_body(task.as_deref(), run_id),
                    |draft| draft.body.clone(),
                );
                match gh.create_pull_request(worktree, &branch, &title, &body) {
                    Ok(url) => PullRequestStatus::Created(url),
                    Err(unavailable) => PullRequestStatus::Unavailable(unavailable.0),
                }
            }
            Err(unavailable) => PullRequestStatus::Unavailable(unavailable.0),
        };
        Ok(PublishReceipt {
            branch,
            commit,
            pull_request,
        })
    }

    /// Builds a bounded read-only preview from same temporary-index delta used by apply.
    ///
    /// # Errors
    /// Rejects missing/non-ready workspaces, ownership failures, invalid limits, or Git failures.
    /// Branch and detached workspaces are both inspectable; only branch workspaces remain
    /// applicable. No apply intent or canonical state is changed.
    pub(crate) fn preview_patch(
        &self,
        store: &mut SqliteStore,
        run_id: RunId,
        max_bytes: usize,
    ) -> Result<crate::git::PatchPreview, WorkspaceError> {
        if max_bytes == 0 {
            return Err(crate::git::GitError::InvalidOutput(
                "diff preview byte limit must be positive".to_owned(),
            )
            .into());
        }
        let workspace = Self::ready_workspace(store, run_id)?;
        self.validate_workspace(&workspace, false)?;
        Ok(generate_patch_preview(
            &self.git,
            workspace.worktree_path(),
            workspace.base_commit(),
            max_bytes,
        )?)
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

/// One line naming the work when no editing stage drafted a title: the
/// task's first line, for the commit subject and pull-request title.
fn publish_title(task: Option<&str>, run_id: RunId) -> String {
    let first_line = task
        .map(str::trim)
        .and_then(|task| task.lines().next())
        .map(str::trim)
        .filter(|line| !line.is_empty());
    first_line.map_or_else(|| format!("Polycode run {run_id}"), bounded_title)
}

/// A title cut to the length Git and GitHub show in full.
fn bounded_title(line: &str) -> String {
    const LIMIT: usize = 72;
    if line.chars().count() <= LIMIT {
        line.to_owned()
    } else {
        let mut title: String = line.chars().take(LIMIT - 1).collect();
        title.push('…');
        title
    }
}

fn publish_body(task: Option<&str>, run_id: RunId) -> String {
    let footer = format!("Opened by Polycode from run {run_id}.");
    match task.map(str::trim).filter(|task| !task.is_empty()) {
        Some(task) => format!("{task}\n\n---\n{footer}"),
        None => footer,
    }
}

/// Refuses to move a run's changes anywhere unless its latest verification
/// passed.
///
/// The rule: take the run's most recent verify stage — the last one in
/// definition order, since every fix or continue cycle appends its own —
/// and refuse if it failed, or if the run is `Completed` and it is not
/// `Completed`. Older verify stages do not count: a fix cycle whose
/// `verify_n` passed has answered the failure that came before it, and a
/// verification that went red on the first attempt is exactly what a fix
/// cycle exists to turn green.
///
/// Asked before the run-status check, so a failure is refused by name rather
/// than by whatever status it caused. The decision only optionally depends
/// on verification, so a failed check completes the run, reaches the lead
/// as evidence, and leaves fix and continue available; this gate is what
/// keeps such a run from being applied or published in the meantime.
fn ensure_verification_passed(run: &Run) -> Result<(), WorkspaceError> {
    let Some(latest) = run
        .stages()
        .iter()
        .rev()
        .find(|stage| stage.kind() == StageKind::Verify)
    else {
        return Ok(());
    };
    let blocked = latest.status() == StageStatus::Failed
        || (run.status() == RunStatus::Completed && latest.status() != StageStatus::Completed);
    if blocked {
        return Err(WorkspaceError::VerificationNotPassed {
            stage_id: latest.id().clone(),
            status: format!("{:?}", latest.status()).to_lowercase(),
        });
    }
    Ok(())
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
        WorkflowDefinition, WorkflowKind,
    };
    use crate::store::ResolvedConfigSnapshot;

    struct Fixture {
        temp: TempDir,
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
                temp,
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

    /// Everything the operator can see of their own checkout: every file in
    /// the working tree with its bytes, the branch they are standing on, and
    /// the commit it points at. `.git` is walked past deliberately — a run
    /// legitimately writes worktree metadata and a branch ref in there, and
    /// neither is something the operator is looking at.
    fn source_snapshot(source: &Path) -> (Vec<(PathBuf, Vec<u8>)>, String, String) {
        fn walk(directory: &Path, base: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            for entry in fs::read_dir(directory).unwrap().flatten() {
                let path = entry.path();
                if path.file_name() == Some(OsStr::new(".git")) {
                    continue;
                }
                if path.is_dir() {
                    walk(&path, base, out);
                } else {
                    out.push((
                        path.strip_prefix(base).unwrap().to_path_buf(),
                        fs::read(&path).unwrap(),
                    ));
                }
            }
        }
        let mut files = Vec::new();
        walk(source, source, &mut files);
        files.sort();
        (
            files,
            git_text(source, ["rev-parse", "HEAD"]),
            git_text(source, ["rev-parse", "--abbrev-ref", "HEAD"]),
        )
    }

    /// The invariant that entitles the word "isolated": a run owns its
    /// worktree and touches nothing the operator is working in. Asserted
    /// across the whole lifecycle rather than at creation, because the
    /// hazard is a long-running run beside someone editing the same
    /// checkout — the operator is left mid-edit here on purpose, with both
    /// an untracked file and a modified tracked one.
    #[test]
    fn a_run_leaves_the_operators_checkout_exactly_as_it_found_it() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        fs::write(fixture.source.join("local-only.txt"), "mine\n").unwrap();
        fs::write(fixture.source.join("README.md"), "mine too\n").unwrap();
        let before = source_snapshot(&fixture.source);

        let workspace = fixture.prepare();
        fs::write(
            workspace.worktree_path().join("README.md"),
            "changed by the run\n",
        )
        .unwrap();
        fs::write(workspace.worktree_path().join("added.txt"), "new\n").unwrap();
        git(workspace.worktree_path(), ["add", "."]);
        git(workspace.worktree_path(), ["commit", "-m", "run work"]);
        fixture.complete();
        assert_eq!(
            source_snapshot(&fixture.source),
            before,
            "a run changed the checkout its operator is working in"
        );

        fixture
            .manager()
            .discard(&mut fixture.store, fixture.run_id)
            .unwrap();
        fixture
            .manager()
            .cleanup(&mut fixture.store, fixture.run_id)
            .unwrap();
        assert_eq!(
            source_snapshot(&fixture.source),
            before,
            "cleaning up after a run changed the operator's checkout"
        );
    }

    /// And the one exception, stated so the invariant above is not merely a
    /// test that nothing anywhere ever writes to the source. Apply is the
    /// single path that does, it is invoked deliberately, and it refuses a
    /// checkout that is not clean.
    #[test]
    fn apply_is_the_only_thing_that_writes_to_the_operators_checkout() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let before = source_snapshot(&fixture.source);
        let workspace = fixture.prepare();
        fixture.complete();
        fs::write(
            workspace.worktree_path().join("README.md"),
            "changed by the run\n",
        )
        .unwrap();

        // Not while the operator has work in progress. Two guards enforce
        // this — one before the apply intent is recorded and one immediately
        // before the write, so a checkout that goes dirty in between is
        // caught too. This asserts the behaviour, not either guard: removing
        // the first leaves the second, and the run still refuses.
        fs::write(fixture.source.join("mid-edit.txt"), "mine\n").unwrap();
        assert!(matches!(
            fixture.manager().apply(&mut fixture.store, fixture.run_id),
            Err(WorkspaceError::SourceCheckoutDirty(_))
        ));
        fs::remove_file(fixture.source.join("mid-edit.txt")).unwrap();

        fixture
            .manager()
            .apply(&mut fixture.store, fixture.run_id)
            .unwrap();
        let after = source_snapshot(&fixture.source);
        assert_ne!(
            after, before,
            "apply is supposed to be the one thing that changes the source"
        );
        assert_eq!(after.1, before.1, "apply does not move the operator's HEAD");
        assert_eq!(
            after.2, before.2,
            "apply does not move the operator's branch"
        );
    }

    #[test]
    fn a_run_with_a_task_owns_a_branch_named_after_its_issue() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        create_run_with_task(
            &mut fixture.store,
            WorkflowKind::Standard,
            2,
            "Fix https://linear.app/a8c/issue/DOTCOM-17972/stepper-transfer-waits",
        );
        fixture.run_id = RunId::from_u128(2);

        let workspace = fixture.prepare();
        let id = fixture.run_id.to_string().to_ascii_lowercase();
        let expected = format!("polycode/dotcom-17972-{}", &id[id.len() - 6..]);
        assert_eq!(workspace.branch_name(), Some(expected.as_str()));
        let repository = GitRepository::discover(&fixture.source).unwrap();
        assert!(branch_exists(&fixture.manager().git, &repository, &expected).unwrap());
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

    /// A review is detached because it is not meant to produce changes.
    /// Sending it back to fix what it found is the moment that stops being
    /// true, so it is the moment the workspace earns a branch — and, with it,
    /// a route back into the operator's checkout that apply will accept.
    #[test]
    fn a_review_workspace_earns_a_branch_when_asked_to_fix_what_it_found() {
        let mut fixture = Fixture::new(WorkflowKind::Review);
        let workspace = fixture.prepare();
        assert_eq!(workspace.mode(), WorkspaceMode::Detached);
        fixture.complete();

        let adopted = fixture
            .manager()
            .adopt_branch_for_fix(&mut fixture.store, fixture.run_id)
            .unwrap();

        assert_eq!(adopted.mode(), WorkspaceMode::Branch);
        assert!(adopted.branch_owned());
        let branch = adopted.branch_name().unwrap().to_owned();
        assert_eq!(
            git_text(
                workspace.worktree_path(),
                ["rev-parse", "--abbrev-ref", "HEAD"]
            ),
            branch,
            "the worktree stands on the branch the store now claims"
        );
        assert!(
            matches!(
                fixture
                    .manager()
                    .reconcile(&mut fixture.store, fixture.run_id)
                    .unwrap(),
                ReconciliationOutcome::Unchanged(_)
            ),
            "and reconcile recognises the workspace it just became"
        );

        let again = fixture
            .manager()
            .adopt_branch_for_fix(&mut fixture.store, fixture.run_id)
            .unwrap();
        assert_eq!(
            again.branch_name(),
            Some(branch.as_str()),
            "a second fix cycle adopts nothing new"
        );
    }

    /// The point of the branch: what the fix writes can reach the checkout.
    /// Before adoption this same run is refused, which
    /// `review_workspace_rejects_apply_even_when_logically_completed` pins.
    #[test]
    fn a_fixed_review_can_finally_transfer_what_it_changed() {
        let mut fixture = Fixture::new(WorkflowKind::Review);
        let workspace = fixture.prepare();
        fixture.complete();
        fixture
            .manager()
            .adopt_branch_for_fix(&mut fixture.store, fixture.run_id)
            .unwrap();
        fs::write(workspace.worktree_path().join("README.md"), "fixed\n").unwrap();

        fixture
            .manager()
            .apply(&mut fixture.store, fixture.run_id)
            .unwrap();

        assert_eq!(
            fs::read_to_string(fixture.source.join("README.md")).unwrap(),
            "fixed\n"
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

    /// An agent told to review a pull request reaches for `gh pr checkout`.
    /// That is reasonable work inside a worktree Polycode owns exclusively,
    /// and it used to end the run: the next reconcile read the branch as an
    /// ownership mismatch and condemned a workspace nothing else could revive.
    #[test]
    fn a_review_worktree_that_walked_off_its_base_is_returned_to_it() {
        let mut fixture = Fixture::new(WorkflowKind::Review);
        let workspace = fixture.prepare();
        let base = workspace.base_commit().to_owned();
        git(&fixture.source, ["branch", "pull-request"]);
        git(workspace.worktree_path(), ["checkout", "pull-request"]);

        let outcome = fixture
            .manager()
            .reconcile(&mut fixture.store, fixture.run_id)
            .unwrap();

        assert!(matches!(outcome, ReconciliationOutcome::Unchanged(_)));
        assert_eq!(
            fixture
                .store
                .load_workspace(fixture.run_id)
                .unwrap()
                .unwrap()
                .status(),
            WorkspaceStatus::Ready
        );
        assert_eq!(
            git_text(workspace.worktree_path(), ["rev-parse", "HEAD"]),
            base,
            "the worktree is back on the commit the run was prepared from"
        );
        assert_eq!(
            git_text(
                workspace.worktree_path(),
                ["rev-parse", "--abbrev-ref", "HEAD"]
            ),
            "HEAD",
            "and detached again, the way a review workspace is owned"
        );
    }

    /// Healing stops where evidence begins. Re-detaching a dirty tree would
    /// destroy work no event ever recorded, so that stays an operator's call.
    #[test]
    fn uncommitted_work_under_a_moved_head_is_never_silently_discarded() {
        let mut fixture = Fixture::new(WorkflowKind::Review);
        let workspace = fixture.prepare();
        git(&fixture.source, ["branch", "pull-request"]);
        git(workspace.worktree_path(), ["checkout", "pull-request"]);
        let stray = workspace.worktree_path().join("NOTES.md");
        fs::write(&stray, "work nobody recorded\n").unwrap();

        let outcome = fixture
            .manager()
            .reconcile(&mut fixture.store, fixture.run_id)
            .unwrap();

        assert!(matches!(outcome, ReconciliationOutcome::Broken(_)));
        assert_eq!(
            fs::read_to_string(&stray).unwrap(),
            "work nobody recorded\n",
            "the run is refused, and the work survives the refusal"
        );
    }

    /// Broken records what was observed, not something the workspace became.
    /// Once the condition is gone the run is recoverable, and recovering it
    /// must not require editing the store by hand.
    #[test]
    fn a_broken_workspace_is_observed_again_rather_than_condemned() {
        let mut fixture = Fixture::new(WorkflowKind::Review);
        let workspace = fixture.prepare();
        git(&fixture.source, ["branch", "pull-request"]);
        git(workspace.worktree_path(), ["checkout", "pull-request"]);
        let stray = workspace.worktree_path().join("NOTES.md");
        fs::write(&stray, "work nobody recorded\n").unwrap();
        assert!(matches!(
            fixture
                .manager()
                .reconcile(&mut fixture.store, fixture.run_id)
                .unwrap(),
            ReconciliationOutcome::Broken(_)
        ));

        fs::remove_file(&stray).unwrap();

        let outcome = fixture
            .manager()
            .reconcile(&mut fixture.store, fixture.run_id)
            .unwrap();

        assert!(matches!(outcome, ReconciliationOutcome::Ready(_)));
        let healed = fixture
            .store
            .load_workspace(fixture.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(healed.status(), WorkspaceStatus::Ready);
        assert!(
            healed.last_error().is_none(),
            "a recovered workspace stops reporting the failure it recovered from"
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
        create_run_with_task_option(store, kind, id, None)
    }

    fn create_run_with_task(
        store: &mut SqliteStore,
        kind: WorkflowKind,
        id: u128,
        task: &str,
    ) -> RunId {
        create_run_with_task_option(store, kind, id, Some(task))
    }

    fn create_run_with_task_option(
        store: &mut SqliteStore,
        kind: WorkflowKind,
        id: u128,
        task: Option<&str>,
    ) -> RunId {
        let run_id = RunId::from_u128(id);
        let (stage_id, stage_kind, role) = if kind == WorkflowKind::Review {
            (
                StageId::new("review").unwrap(),
                StageKind::Review,
                Role::Reviewer,
            )
        } else {
            (
                StageId::new("implementation").unwrap(),
                StageKind::Implementation,
                Role::Implementer,
            )
        };
        let workflow = WorkflowDefinition::new(
            kind,
            vec![StageDefinition::new(stage_id, stage_kind, role, vec![])],
        )
        .unwrap();
        let created_at = now();
        let config_id = ConfigSnapshotId::new(format!("config-{id}")).unwrap();
        let run = Run::new(run_id, workflow, config_id.clone(), created_at);
        let config = ResolvedConfigSnapshot::new(config_id, 1, json!({}), created_at).unwrap();
        let event = run.created_event(metadata(created_at));
        match task {
            Some(task) => {
                let input = RunInput::new(run_id, task, created_at).unwrap();
                store
                    .create_run_with_input(&run, &input, &config, &[event])
                    .unwrap();
            }
            None => {
                store.create_run(&run, &config, &[event]).unwrap();
            }
        }
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
        let stage = run.stages()[0].id().clone();
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

    /// A bare repository wired up as the source's `origin`, so push has
    /// somewhere real to go without any network.
    fn add_origin(fixture: &Fixture) -> PathBuf {
        let parent = fixture.source.parent().unwrap().to_path_buf();
        git(&parent, ["init", "--bare", "-b", "main", "origin.git"]);
        let origin = parent.join("origin.git");
        git(
            &fixture.source,
            ["remote", "add", "origin", origin.to_str().unwrap()],
        );
        origin
    }

    /// A `gh` stand-in: `pr list` prints whatever `list-output` beside the
    /// script holds, `pr create` prints a fixed URL. No stub ever reaches a
    /// network.
    fn stub_gh(directory: &Path) -> GhClient {
        use std::os::unix::fs::PermissionsExt;
        let script = directory.join("gh");
        fs::write(
            &script,
            "#!/bin/sh\n\
             dir=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\n\
             case \"$1 $2\" in\n\
             \"pr list\") cat \"$dir/list-output\" 2>/dev/null; exit 0 ;;\n\
             \"pr create\") printf '%s\\n' \"$@\" > \"$dir/create-args\"; echo \"https://example.invalid/pull/7\"; exit 0 ;;\n\
             *) exit 1 ;;\n\
             esac\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        GhClient::with_executable(script)
    }

    #[test]
    fn publish_commits_pushes_and_opens_a_pull_request_without_touching_the_source() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let origin = add_origin(&fixture);
        let gh = stub_gh(fixture.temp.path());
        // The operator is mid-edit: exactly the situation apply refuses and
        // publish exists for.
        fs::write(fixture.source.join("mid-edit.txt"), "mine\n").unwrap();
        let before = source_snapshot(&fixture.source);
        let workspace = fixture.prepare();
        fixture.complete();
        fs::write(workspace.worktree_path().join("README.md"), "published\n").unwrap();
        fs::write(workspace.worktree_path().join("new.txt"), "added\n").unwrap();

        let receipt = fixture
            .manager()
            .publish_with(&mut fixture.store, fixture.run_id, None, &gh)
            .unwrap();

        let branch = format!("polycode/run-{}", fixture.run_id);
        assert_eq!(receipt.branch, branch);
        assert_eq!(
            receipt.pull_request,
            PullRequestStatus::Created("https://example.invalid/pull/7".to_owned())
        );
        let reference = format!("refs/heads/{branch}");
        assert_eq!(git_text(&origin, ["rev-parse", &reference]), receipt.commit);
        assert_eq!(
            git_text(workspace.worktree_path(), ["log", "-1", "--format=%s"]),
            format!("Polycode run {}", fixture.run_id)
        );
        assert!(git_status(
            workspace.worktree_path(),
            ["diff", "--quiet", "HEAD"]
        ));
        assert_eq!(
            source_snapshot(&fixture.source),
            before,
            "publish wrote to the operator's checkout"
        );
        // The run keeps every disposition: publish is transport.
        assert_eq!(
            fixture.store.load_run(fixture.run_id).unwrap().run.status(),
            RunStatus::Completed
        );
    }

    #[test]
    fn a_second_publish_reuses_the_branch_and_reports_the_open_pull_request() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let origin = add_origin(&fixture);
        let gh = stub_gh(fixture.temp.path());
        let workspace = fixture.prepare();
        fixture.complete();
        fs::write(workspace.worktree_path().join("README.md"), "one\n").unwrap();
        fixture
            .manager()
            .publish_with(&mut fixture.store, fixture.run_id, None, &gh)
            .unwrap();

        // The pull request now exists, and the worktree gained more work —
        // the shape of a fix cycle followed by another publish.
        fs::write(
            fixture.temp.path().join("list-output"),
            "https://example.invalid/pull/7\n",
        )
        .unwrap();
        fs::write(workspace.worktree_path().join("README.md"), "two\n").unwrap();
        let receipt = fixture
            .manager()
            .publish_with(&mut fixture.store, fixture.run_id, None, &gh)
            .unwrap();

        assert_eq!(
            receipt.pull_request,
            PullRequestStatus::AlreadyExists("https://example.invalid/pull/7".to_owned())
        );
        let reference = format!("refs/heads/polycode/run-{}", fixture.run_id);
        assert_eq!(git_text(&origin, ["rev-parse", &reference]), receipt.commit);
    }

    /// The pull request is the editing stage's own words when it wrote them:
    /// the drafted title becomes the commit subject and the pull request
    /// title, and the drafted description reaches gh unchanged.
    #[test]
    fn a_drafted_pull_request_is_quoted_over_the_task_text() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let _origin = add_origin(&fixture);
        let gh = stub_gh(fixture.temp.path());
        let workspace = fixture.prepare();
        fixture.complete();
        fs::write(workspace.worktree_path().join("new.txt"), "added\n").unwrap();
        let draft = PullRequestDraft {
            title: "Add the file the task asked for".to_owned(),
            body: "Fixes https://issues.invalid/1\n\n## Why\n\nIt was missing.".to_owned(),
        };

        fixture
            .manager()
            .publish_with(&mut fixture.store, fixture.run_id, Some(&draft), &gh)
            .unwrap();

        assert_eq!(
            git_text(workspace.worktree_path(), ["log", "-1", "--format=%s"]),
            draft.title
        );
        let args = fs::read_to_string(fixture.temp.path().join("create-args")).unwrap();
        assert!(args.contains(&format!("--title\n{}\n", draft.title)));
        assert!(args.contains(&format!("--body\n{}\n", draft.body)));
        assert!(!args.contains("Opened by Polycode"));
    }

    /// A drafted title with nothing under it still names the work; only the
    /// description falls back to the task text.
    #[test]
    fn a_draft_without_a_description_borrows_the_task_for_the_body_alone() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let _origin = add_origin(&fixture);
        let gh = stub_gh(fixture.temp.path());
        let workspace = fixture.prepare();
        fixture.complete();
        fs::write(workspace.worktree_path().join("new.txt"), "added\n").unwrap();
        let draft = PullRequestDraft {
            title: "Add the file".to_owned(),
            body: String::new(),
        };

        fixture
            .manager()
            .publish_with(&mut fixture.store, fixture.run_id, Some(&draft), &gh)
            .unwrap();

        let args = fs::read_to_string(fixture.temp.path().join("create-args")).unwrap();
        assert!(args.contains("--title\nAdd the file\n"));
        assert!(args.contains(&format!("Opened by Polycode from run {}.", fixture.run_id)));
    }

    #[test]
    fn a_missing_gh_costs_the_pull_request_but_never_the_push() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let origin = add_origin(&fixture);
        let gh = GhClient::with_executable(fixture.temp.path().join("missing-gh"));
        let workspace = fixture.prepare();
        fixture.complete();
        fs::write(workspace.worktree_path().join("README.md"), "pushed\n").unwrap();

        let receipt = fixture
            .manager()
            .publish_with(&mut fixture.store, fixture.run_id, None, &gh)
            .unwrap();

        assert!(matches!(
            receipt.pull_request,
            PullRequestStatus::Unavailable(_)
        ));
        let reference = format!("refs/heads/polycode/run-{}", fixture.run_id);
        assert_eq!(git_text(&origin, ["rev-parse", &reference]), receipt.commit);
    }

    #[test]
    fn publish_refusals_leave_the_workspace_unchanged() {
        // No origin remote: refused before anything is committed or pushed,
        // leaving the worktree's uncommitted delta exactly as it was.
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let gh = stub_gh(fixture.temp.path());
        let workspace = fixture.prepare();
        fixture.complete();
        fs::write(workspace.worktree_path().join("README.md"), "change\n").unwrap();
        assert!(matches!(
            fixture
                .manager()
                .publish_with(&mut fixture.store, fixture.run_id, None, &gh),
            Err(WorkspaceError::NoRemote(_))
        ));
        assert_eq!(
            git_text(workspace.worktree_path(), ["rev-parse", "HEAD"]),
            workspace.base_commit(),
            "a refused publish committed anyway"
        );
        assert!(!git_status(
            workspace.worktree_path(),
            ["diff", "--quiet", "HEAD"]
        ));

        // Nothing to publish: a worktree still at its base.
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let gh = stub_gh(fixture.temp.path());
        add_origin(&fixture);
        fixture.prepare();
        fixture.complete();
        assert!(matches!(
            fixture
                .manager()
                .publish_with(&mut fixture.store, fixture.run_id, None, &gh),
            Err(WorkspaceError::NothingToPublish)
        ));

        // A run that has not completed.
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        let gh = stub_gh(fixture.temp.path());
        add_origin(&fixture);
        fixture.prepare();
        assert!(matches!(
            fixture
                .manager()
                .publish_with(&mut fixture.store, fixture.run_id, None, &gh),
            Err(WorkspaceError::InvalidRunStatus { .. })
        ));

        // A detached review workspace has no branch to publish.
        let mut fixture = Fixture::new(WorkflowKind::Review);
        let gh = stub_gh(fixture.temp.path());
        add_origin(&fixture);
        fixture.prepare();
        fixture.complete();
        assert!(matches!(
            fixture
                .manager()
                .publish_with(&mut fixture.store, fixture.run_id, None, &gh),
            Err(WorkspaceError::ReviewWorkspaceNotApplicable)
        ));
    }

    #[test]
    fn a_published_run_still_cleans_up_completely() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        add_origin(&fixture);
        let gh = stub_gh(fixture.temp.path());
        let workspace = fixture.prepare();
        fixture.complete();
        fs::write(workspace.worktree_path().join("README.md"), "published\n").unwrap();
        fixture
            .manager()
            .publish_with(&mut fixture.store, fixture.run_id, None, &gh)
            .unwrap();

        fixture
            .manager()
            .discard(&mut fixture.store, fixture.run_id)
            .unwrap();
        assert!(!workspace.worktree_path().exists());
        let reference = format!("refs/heads/polycode/run-{}", fixture.run_id);
        assert!(
            !git_status(
                &fixture.source,
                ["rev-parse", "--verify", "--quiet", &reference]
            ),
            "the local branch outlived its discard"
        );
    }

    /// Publish is transport, not disposition: the same delta must still be
    /// transferable into the operator's checkout afterwards. This leans on
    /// `generate_patch` diffing the base commit against working-tree content —
    /// a publish commit on the branch must not change what apply transfers.
    #[test]
    fn apply_still_transfers_the_same_delta_after_publish() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        add_origin(&fixture);
        let gh = stub_gh(fixture.temp.path());
        let workspace = fixture.prepare();
        fixture.complete();
        fs::write(workspace.worktree_path().join("README.md"), "published\n").unwrap();
        fs::write(workspace.worktree_path().join("new.txt"), "added\n").unwrap();
        fixture
            .manager()
            .publish_with(&mut fixture.store, fixture.run_id, None, &gh)
            .unwrap();

        fixture
            .manager()
            .apply(&mut fixture.store, fixture.run_id)
            .unwrap();

        assert_eq!(
            fs::read_to_string(fixture.source.join("README.md")).unwrap(),
            "published\n"
        );
        assert_eq!(
            fs::read_to_string(fixture.source.join("new.txt")).unwrap(),
            "added\n"
        );
        assert_eq!(
            fixture.store.load_run(fixture.run_id).unwrap().run.status(),
            RunStatus::Applied
        );
    }

    #[test]
    fn a_run_frozen_mid_apply_recovery_cannot_publish() {
        let mut fixture = Fixture::new(WorkflowKind::Standard);
        add_origin(&fixture);
        let gh = stub_gh(fixture.temp.path());
        let workspace = fixture.prepare();
        fixture.complete();
        fs::write(workspace.worktree_path().join("README.md"), "changed\n").unwrap();
        let crashing = WorkspaceManager::with_fault(&fixture.root, FaultPoint::GitApplied);
        assert!(matches!(
            crashing.apply(&mut fixture.store, fixture.run_id),
            Err(WorkspaceError::InjectedCrash(_))
        ));

        assert!(matches!(
            fixture
                .manager()
                .publish_with(&mut fixture.store, fixture.run_id, None, &gh),
            Err(WorkspaceError::ApplyInProgress(_))
        ));
    }

    #[test]
    fn publish_titles_and_bodies_survive_odd_tasks() {
        let run_id = RunId::from_u128(9);
        assert_eq!(
            publish_title(None, run_id),
            format!("Polycode run {run_id}")
        );
        assert_eq!(
            publish_title(Some("  fix the flaky test\nwith details  "), run_id),
            "fix the flaky test"
        );
        let long = "α".repeat(100);
        let title = publish_title(Some(&long), run_id);
        assert_eq!(title.chars().count(), 72);
        assert!(title.ends_with('…'));
        // A drafted title is bounded the same way the task's first line is.
        assert_eq!(bounded_title(&long), title);
        assert_eq!(bounded_title("short"), "short");
        assert_eq!(
            publish_body(None, run_id),
            format!("Opened by Polycode from run {run_id}.")
        );
        assert!(publish_body(Some("task text"), run_id).starts_with("task text\n\n---\n"));
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
