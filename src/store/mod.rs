//! `SQLite` persistence boundary for validated domain aggregates.

mod config_snapshot;
mod error;
mod migrations;
mod path;
mod snapshot;
mod sqlite;
mod workspace;

pub use config_snapshot::ResolvedConfigSnapshot;
pub use error::StoreError;
pub use migrations::DATABASE_SCHEMA_VERSION;
pub use path::{database_file, worktree_root};
pub use snapshot::RUN_SNAPSHOT_SCHEMA_VERSION;
pub use sqlite::{CommitResult, LoadedRun, RunRevision, RunSummary, SequencedEvent, SqliteStore};
