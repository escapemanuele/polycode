use std::ffi::OsString;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use tempfile::{NamedTempFile, tempdir};

use super::command::{Git, os};
use super::{GitError, GitRepository};

pub(crate) fn source_is_clean(git: &Git, repository: &GitRepository) -> Result<bool, GitError> {
    let output = git.checked(
        repository.source_path(),
        &[
            os("status"),
            os("--porcelain=v1"),
            os("-z"),
            os("--untracked-files=all"),
        ],
    )?;
    Ok(output.stdout.is_empty())
}

pub(crate) fn generate_patch(
    git: &Git,
    worktree_path: &Path,
    base_commit: &str,
) -> Result<Vec<u8>, GitError> {
    let temporary = tempdir().map_err(|source| GitError::CommandIo {
        command: "create temporary Git index directory".to_owned(),
        source,
    })?;
    let index_path = temporary.path().join("index");
    let environment = vec![(
        OsString::from("GIT_INDEX_FILE"),
        index_path.as_os_str().to_os_string(),
    )];
    git.checked_with(
        worktree_path,
        &[os("read-tree"), os(base_commit)],
        &environment,
    )?;
    git.checked_with(
        worktree_path,
        &[os("add"), os("-A"), os("--"), os(".")],
        &environment,
    )?;
    let output = git.checked_with(
        worktree_path,
        &[
            os("diff"),
            os("--cached"),
            os("--binary"),
            os("--full-index"),
            os(base_commit),
            os("--"),
        ],
        &environment,
    )?;
    Ok(output.stdout)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PatchPreview {
    pub bytes: Vec<u8>,
    pub total_bytes: u64,
    pub truncated: bool,
}

pub(crate) fn generate_patch_preview(
    git: &Git,
    worktree_path: &Path,
    base_commit: &str,
    max_bytes: usize,
) -> Result<PatchPreview, GitError> {
    let temporary = tempdir().map_err(|source| GitError::CommandIo {
        command: "create temporary Git index directory".to_owned(),
        source,
    })?;
    let index_path = temporary.path().join("index");
    let environment = vec![(
        OsString::from("GIT_INDEX_FILE"),
        index_path.as_os_str().to_os_string(),
    )];
    git.checked_with(
        worktree_path,
        &[os("read-tree"), os(base_commit)],
        &environment,
    )?;
    git.checked_with(
        worktree_path,
        &[os("add"), os("-A"), os("--"), os(".")],
        &environment,
    )?;
    let mut output = NamedTempFile::new().map_err(|source| GitError::CommandIo {
        command: "create temporary Git preview file".to_owned(),
        source,
    })?;
    git.checked_to_file(
        worktree_path,
        &[
            os("diff"),
            os("--cached"),
            os("--binary"),
            os("--full-index"),
            os(base_commit),
            os("--"),
        ],
        &environment,
        output.reopen().map_err(|source| GitError::CommandIo {
            command: "open temporary Git preview file".to_owned(),
            source,
        })?,
    )?;
    let total_bytes = output
        .as_file()
        .metadata()
        .map_err(|source| GitError::CommandIo {
            command: "inspect temporary Git preview file".to_owned(),
            source,
        })?
        .len();
    output
        .seek(SeekFrom::Start(0))
        .map_err(|source| GitError::CommandIo {
            command: "seek temporary Git preview file".to_owned(),
            source,
        })?;
    let limit = u64::try_from(max_bytes).map_err(|_| {
        GitError::InvalidOutput("diff preview byte limit is outside supported range".to_owned())
    })?;
    let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024));
    output
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|source| GitError::CommandIo {
            command: "read temporary Git preview file".to_owned(),
            source,
        })?;
    Ok(PatchPreview {
        bytes,
        total_bytes,
        truncated: total_bytes > limit,
    })
}

pub(crate) fn check_patch(
    git: &Git,
    repository: &GitRepository,
    patch: &[u8],
    reverse: bool,
) -> Result<bool, GitError> {
    let patch_file = temporary_patch(patch)?;
    let mut args = vec![os("apply"), os("--check"), os("--binary")];
    if reverse {
        args.push(os("--reverse"));
    }
    args.push(patch_file.path().as_os_str().to_os_string());
    let output = git.output(repository.source_path(), &args, &[])?;
    if output.status.success() {
        Ok(true)
    } else if output.status.code() == Some(1) {
        Ok(false)
    } else {
        Err(output.into_failure())
    }
}

pub(crate) fn apply_patch(
    git: &Git,
    repository: &GitRepository,
    patch: &[u8],
) -> Result<(), GitError> {
    let patch_file = temporary_patch(patch)?;
    git.checked_with(
        repository.source_path(),
        &[
            os("apply"),
            os("--binary"),
            patch_file.path().as_os_str().to_os_string(),
        ],
        &[],
    )?;
    Ok(())
}

fn temporary_patch(patch: &[u8]) -> Result<NamedTempFile, GitError> {
    let mut file = NamedTempFile::new().map_err(|source| GitError::CommandIo {
        command: "create temporary Git patch file".to_owned(),
        source,
    })?;
    file.write_all(patch)
        .and_then(|()| file.flush())
        .map_err(|source| GitError::CommandIo {
            command: "write temporary Git patch file".to_owned(),
            source,
        })?;
    Ok(file)
}
