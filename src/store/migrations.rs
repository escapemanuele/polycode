use rusqlite::Connection;

use super::StoreError;

pub const DATABASE_SCHEMA_VERSION: u32 = 2;

pub(crate) fn migrate(connection: &Connection) -> Result<(), StoreError> {
    let version =
        connection.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))?;
    match version {
        DATABASE_SCHEMA_VERSION => Ok(()),
        0 => {
            migrate_v1(connection)?;
            migrate_v2(connection)
        }
        1 => migrate_v2(connection),
        unsupported => Err(StoreError::UnsupportedDatabaseVersion(unsupported)),
    }
}

fn migrate_v1(connection: &Connection) -> Result<(), StoreError> {
    connection.execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE config_snapshots (
                     id TEXT PRIMARY KEY NOT NULL,
                     schema_version INTEGER NOT NULL CHECK (schema_version > 0),
                     payload_json TEXT NOT NULL,
                     content_hash TEXT NOT NULL CHECK (length(content_hash) = 64),
                     created_at TEXT NOT NULL
                 );
                 CREATE TABLE runs (
                     id TEXT PRIMARY KEY NOT NULL,
                     status TEXT NOT NULL,
                     workflow TEXT NOT NULL,
                     config_snapshot_id TEXT NOT NULL,
                     snapshot_schema_version INTEGER NOT NULL CHECK (snapshot_schema_version > 0),
                     snapshot_json TEXT NOT NULL,
                     revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
                     created_at TEXT NOT NULL,
                     updated_at TEXT NOT NULL,
                     FOREIGN KEY (config_snapshot_id) REFERENCES config_snapshots(id) ON DELETE RESTRICT
                 );
                 CREATE TABLE events (
                     run_id TEXT NOT NULL,
                     sequence INTEGER NOT NULL CHECK (sequence > 0),
                     event_id TEXT NOT NULL UNIQUE,
                     event_type TEXT NOT NULL,
                     payload_json TEXT NOT NULL,
                     occurred_at TEXT NOT NULL,
                     recorded_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                     PRIMARY KEY (run_id, sequence),
                     FOREIGN KEY (run_id) REFERENCES runs(id) ON DELETE RESTRICT
                 );
                 CREATE TRIGGER config_snapshots_no_update
                 BEFORE UPDATE ON config_snapshots
                 BEGIN
                     SELECT RAISE(ABORT, 'config snapshots are immutable');
                 END;
                 CREATE TRIGGER config_snapshots_no_delete
                 BEFORE DELETE ON config_snapshots
                 BEGIN
                     SELECT RAISE(ABORT, 'config snapshots are immutable');
                 END;
                 CREATE INDEX runs_status_updated_idx ON runs(status, updated_at DESC);
                 CREATE INDEX runs_config_snapshot_idx ON runs(config_snapshot_id);
                 CREATE INDEX events_run_occurred_idx ON events(run_id, occurred_at);
                 PRAGMA user_version = 1;
                 COMMIT;",
    )?;
    Ok(())
}

fn migrate_v2(connection: &Connection) -> Result<(), StoreError> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE run_workspaces (
             run_id TEXT PRIMARY KEY NOT NULL,
             source_repo_path TEXT NOT NULL,
             git_common_dir TEXT NOT NULL,
             base_commit TEXT NOT NULL CHECK (length(base_commit) IN (40, 64)),
             worktree_path TEXT NOT NULL UNIQUE,
             branch_name TEXT,
             mode TEXT NOT NULL CHECK (mode IN ('branch', 'detached')),
             status TEXT NOT NULL CHECK (
                 status IN ('preparing', 'ready', 'removing', 'removed', 'broken')
             ),
             branch_owned INTEGER NOT NULL DEFAULT 0 CHECK (branch_owned IN (0, 1)),
             removal_head TEXT CHECK (removal_head IS NULL OR length(removal_head) IN (40, 64)),
             last_error TEXT,
             revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             FOREIGN KEY (run_id) REFERENCES runs(id) ON DELETE RESTRICT,
             CHECK (
                 (mode = 'branch' AND branch_name IS NOT NULL) OR
                 (mode = 'detached' AND branch_name IS NULL)
             )
         );
         CREATE TABLE run_apply_operations (
             run_id TEXT PRIMARY KEY NOT NULL,
             status TEXT NOT NULL CHECK (
                 status IN ('prepared', 'applied_to_source', 'recorded', 'failed')
             ),
             patch_hash TEXT NOT NULL CHECK (length(patch_hash) = 64),
             run_revision INTEGER NOT NULL CHECK (run_revision >= 0),
             last_error TEXT,
             revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             FOREIGN KEY (run_id) REFERENCES runs(id) ON DELETE RESTRICT,
             FOREIGN KEY (run_id) REFERENCES run_workspaces(run_id) ON DELETE RESTRICT
         );
         CREATE INDEX run_workspaces_status_idx ON run_workspaces(status, updated_at);
         CREATE INDEX run_workspaces_common_dir_idx ON run_workspaces(git_common_dir);
         CREATE INDEX run_apply_operations_status_idx
             ON run_apply_operations(status, updated_at);
         PRAGMA user_version = 2;
         COMMIT;",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use serde_json::json;

    use super::*;
    use crate::domain::{
        ConfigSnapshotId, EventId, EventMetadata, Role, Run, RunId, StageDefinition, StageId,
        StageKind, WorkflowDefinition, WorkflowKind,
    };
    use crate::store::{ResolvedConfigSnapshot, SqliteStore};

    #[test]
    fn v1_database_upgrades_without_changing_existing_run() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        migrate_v1(&connection).unwrap();
        let mut store = SqliteStore { connection };
        let at: DateTime<Utc> = std::time::SystemTime::now().into();
        let config_id = ConfigSnapshotId::new("v1-config").unwrap();
        let workflow = WorkflowDefinition::new(
            WorkflowKind::Fast,
            vec![StageDefinition::new(
                StageId::new("implementation").unwrap(),
                StageKind::Implementation,
                Role::Implementer,
                vec![],
            )],
        )
        .unwrap();
        let run = Run::new(
            RunId::from_u128(900),
            "survive migration",
            workflow,
            config_id.clone(),
            at,
        )
        .unwrap();
        let config = ResolvedConfigSnapshot::new(config_id, 1, json!({"v": 1}), at).unwrap();
        let event = run.created_event(EventMetadata::new(EventId::from_u128(901), at));
        store.create_run(&run, &config, &[event]).unwrap();

        migrate(&store.connection).unwrap();

        assert_eq!(store.schema_version().unwrap(), DATABASE_SCHEMA_VERSION);
        assert_eq!(store.load_run(run.id()).unwrap().run, run);
        assert_eq!(store.load_events(run.id()).unwrap().len(), 1);
        assert_eq!(
            store
                .load_config_snapshot(run.config_snapshot_id())
                .unwrap(),
            config
        );
        assert!(store.load_workspace(run.id()).unwrap().is_none());
        assert!(store.load_apply_operation(run.id()).unwrap().is_none());
    }
}
