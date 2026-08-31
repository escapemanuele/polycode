use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::command::{Git, os, text_output};
use super::repository::validate_commit;
use super::{GitError, GitRepository};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorktreeIdentity {
    pub path: PathBuf,
    pub git_common_dir: PathBuf,
    pub head_commit: String,
    pub branch: Option<String>,
}

pub(crate) fn create_worktree(
    git: &Git,
    repository: &GitRepository,
    path: &Path,
    base_commit: &str,
    branch: Option<&str>,
) -> Result<(), GitError> {
    let mut args = vec![os("worktree"), os("add")];
    if let Some(branch) = branch {
        git.checked(
            repository.source_path(),
            &[os("check-ref-format"), os("--branch"), os(branch)],
        )?;
        args.extend([os("-b"), os(branch)]);
    } else {
        args.push(os("--detach"));
    }
    args.extend([path.as_os_str().to_os_string(), os(base_commit)]);
    git.checked(repository.source_path(), &args)?;
    Ok(())
}

pub(crate) fn inspect_worktree(git: &Git, path: &Path) -> Result<WorktreeIdentity, GitError> {
    let repository = GitRepository::discover_with(git, path)?;
    let symbolic = git.output(
        repository.source_path(),
        &[os("symbolic-ref"), os("--quiet"), os("--short"), os("HEAD")],
        &[],
    )?;
    let branch = if symbolic.status.success() {
        Some(text_output(symbolic)?)
    } else if symbolic.status.code() == Some(1) {
        None
    } else {
        return Err(symbolic.into_failure());
    };
    Ok(WorktreeIdentity {
        path: repository.source_path().to_path_buf(),
        git_common_dir: repository.git_common_dir().to_path_buf(),
        head_commit: repository.head_commit().to_owned(),
        branch,
    })
}

/// Returns one worktree to a detached HEAD at `commit`.
///
/// Refuses nothing on its own: the caller decides whether losing the current
/// HEAD is safe, because only the caller knows whether the tree is clean.
pub(crate) fn detach_worktree(git: &Git, path: &Path, commit: &str) -> Result<(), GitError> {
    validate_commit(commit)?;
    git.checked(path, &[os("checkout"), os("--detach"), os(commit)])?;
    Ok(())
}

pub(crate) fn remove_worktree(
    git: &Git,
    repository: &GitRepository,
    path: &Path,
) -> Result<(), GitError> {
    git.checked(
        repository.source_path(),
        &[
            os("worktree"),
            os("remove"),
            os("--force"),
            path.as_os_str().to_os_string(),
        ],
    )?;
    Ok(())
}

pub(crate) fn branch_exists(
    git: &Git,
    repository: &GitRepository,
    branch: &str,
) -> Result<bool, GitError> {
    Ok(branch_tip(git, repository, branch)?.is_some())
}

pub(crate) fn branch_tip(
    git: &Git,
    repository: &GitRepository,
    branch: &str,
) -> Result<Option<String>, GitError> {
    let reference = format!("refs/heads/{branch}");
    let revision = format!("{reference}^{{commit}}");
    let output = git.output(
        repository.source_path(),
        &[
            os("rev-parse"),
            os("--verify"),
            os("--quiet"),
            os(&revision),
        ],
        &[],
    )?;
    if output.status.success() {
        let commit = text_output(output)?;
        validate_commit(&commit)?;
        Ok(Some(commit))
    } else if output.status.code() == Some(1) {
        Ok(None)
    } else {
        Err(output.into_failure())
    }
}

pub(crate) fn delete_owned_branch(
    git: &Git,
    repository: &GitRepository,
    branch: &str,
    expected_tip: &str,
) -> Result<bool, GitError> {
    let Some(actual_tip) = branch_tip(git, repository, branch)? else {
        return Ok(false);
    };
    if actual_tip != expected_tip {
        return Err(GitError::InvalidOutput(format!(
            "branch {branch} moved from expected owned tip {expected_tip} to {actual_tip}"
        )));
    }
    if branch_is_checked_out(git, repository, branch)? {
        return Err(GitError::InvalidOutput(format!(
            "branch {branch} remains checked out in a worktree"
        )));
    }
    let reference = format!("refs/heads/{branch}");
    git.checked(
        repository.source_path(),
        &[
            os("update-ref"),
            os("-d"),
            OsString::from(reference),
            os(expected_tip),
        ],
    )?;
    Ok(true)
}

fn branch_is_checked_out(
    git: &Git,
    repository: &GitRepository,
    branch: &str,
) -> Result<bool, GitError> {
    let output = git.checked(
        repository.source_path(),
        &[os("worktree"), os("list"), os("--porcelain"), os("-z")],
    )?;
    let expected = format!("branch refs/heads/{branch}").into_bytes();
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .any(|field| field == expected.as_slice()))
}
