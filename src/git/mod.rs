//! Focused native Git CLI boundary for repository and worktree operations.

mod command;
mod error;
mod patch;
mod repository;
mod worktree;

pub use error::GitError;
pub use repository::GitRepository;

pub(crate) use command::{Git, git_version};
#[cfg(test)]
pub(crate) use patch::ChangeKind;
pub(crate) use patch::{
    ChangedFileRecord, PatchPreview, apply_patch, check_patch, generate_change_evidence,
    generate_patch, generate_patch_preview, source_is_clean, tree_is_clean,
};
pub(crate) use worktree::{
    WorktreeIdentity, branch_exists, branch_tip, create_branch_in_worktree, create_worktree,
    delete_owned_branch, detach_worktree, inspect_worktree, remove_worktree,
};
