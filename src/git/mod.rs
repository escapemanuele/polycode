//! Focused native Git CLI boundary for repository and worktree operations.

mod command;
mod error;
mod patch;
mod repository;
mod worktree;

pub use error::GitError;
pub use repository::GitRepository;

pub(crate) use command::Git;
pub(crate) use patch::{
    PatchPreview, apply_patch, check_patch, generate_patch, generate_patch_preview, source_is_clean,
};
pub(crate) use worktree::{
    WorktreeIdentity, branch_exists, branch_tip, create_worktree, delete_owned_branch,
    inspect_worktree, remove_worktree,
};
