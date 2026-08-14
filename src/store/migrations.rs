use rusqlite::Connection;

use super::StoreError;

pub const DATABASE_SCHEMA_VERSION: u32 = 1;

pub(crate) fn migrate(connection: &Connection) -> Result<(), StoreError> {
    let version =
        connection.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))?;
    match version {
        DATABASE_SCHEMA_VERSION => Ok(()),
        0 => {
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
        unsupported => Err(StoreError::UnsupportedDatabaseVersion(unsupported)),
    }
}
