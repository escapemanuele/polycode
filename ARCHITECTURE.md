# Polycode Architecture

## Status

Milestone 2 implements synchronous SQLite persistence around the validated Milestone 1 domain: versioned snapshots, validated rehydration, immutable resolved configuration, atomic semantic events, per-run event sequence, and optimistic revision checks. Providers, scheduling, process backends, Git worktrees, async runtime, and TUI remain deliberately absent.

Legacy `agents-v3.0.0` was inspected after bootstrap. [LEGACY_BEHAVIOR.md](LEGACY_BEHAVIOR.md) records its behavioral contract, recovery edge cases, and intentional architectural departures.

## Product boundary

Polycode orchestrates native coding-agent CLIs. A provider represents a local CLI and its native authentication/session behavior; it does not imply direct use of a vendor API.

These concepts remain independent:

```text
workflow stage -> engineering role -> provider -> model
```

Workflow definitions depend on roles. A profile resolver will later select a provider/model and role-specific fallbacks without embedding model names in workflow semantics.

## System boundaries

```text
CLI / TUI
    |
run manager
    |
workflow engine ---- domain state/events ---- SQLite store
    |
provider interface
    |
process backend (tmux first, native supervisor later)
    |
native coding-agent CLI inside isolated Git worktree
```

Human-readable artifacts, JSONL logs, and conversations complement SQLite. They never replace canonical machine state.

## State and events

Runs, stages, and attention requests use explicit typed state. Artifacts carry typed metadata; provider sessions use neutral identities; usage has a provider-neutral signal. Important changes return common semantic events. Future persistence, scheduling, UI, notification, and journals can consume those events instead of independently inferring state from files or provider-specific JSON.

Events are semantic history and integration signals, not an event-sourcing system. Domain state persisted in SQLite is authoritative; restoration does not replay full history.

### Milestone 2 persistence boundary

Implemented flow:

```text
SQLite snapshot JSON
    -> inspect schema_version
    -> decode RunSnapshotV1
    -> migrate/normalize to RunRehydrationData
    -> Run::rehydrate
    -> full current-state invariant validation
    -> Run
```

`Run` fields remain private and `Run` does not implement `Deserialize`. `RunRehydrationData` is persistence-neutral, deliberately constructible as untrusted input, and produces no aggregate until validation succeeds.

Implemented persistence rules:

- Persistence deserializes a versioned `RunSnapshot`, migrates and normalizes it to the latest shape, then calls validated `Run::rehydrate`. Rehydration reconstructs current state without replaying every transition, but must enforce every current-state invariant.
- Immutable resolved configuration lives in a separate insert-only record keyed by `config_snapshot_id`, with schema version, JSON payload, content hash, and creation time. Exported runs inline that payload for portability.
- Each state mutation and its complete semantic-event batch commit in one SQLite transaction. Neither state nor events may commit alone.
- Events receive a per-run sequence number as authoritative ordering. UTC timestamps remain human/debugging chronology and may be equal; persisted chronology must be non-decreasing.
- State remains canonical. Event history must explain committed state, but restoration does not require full event replay.

SQLite schema v1 uses three tables:

```text
config_snapshots(id, schema_version, payload_json, content_hash, created_at)

runs(id, status, workflow, config_snapshot_id,
     snapshot_schema_version, snapshot_json, revision,
     created_at, updated_at)

events(run_id, sequence, event_id, event_type,
       payload_json, occurred_at, recorded_at)
```

Snapshot JSON holds aggregate reconstruction state; selected run columns are indexed projections and checked against decoded state on load. `events` has primary key `(run_id, sequence)` and globally unique `event_id`. Foreign keys are enabled on every connection. File-backed stores use WAL, normal synchronous mode, and a five-second busy timeout.

`repository_path` is intentionally absent from schema v1 because Milestone 1 `Run` does not own repository identity yet. It belongs in a later migration when Git/worktree lifecycle defines that domain boundary.

Resolved config payloads are recursively key-sorted and compact-encoded before SHA-256 hashing. An existing config ID accepts an exact idempotent insert only; different content or metadata is rejected. Database triggers reject update and delete operations, enforcing insert-only storage beneath the Rust API.

Every update performs compare-and-swap:

```sql
UPDATE runs
SET snapshot_json = ?, revision = revision + 1, ...
WHERE id = ? AND revision = ?
```

Zero changed rows means `ConcurrentModification`. Snapshot update precedes event inserts inside one `BEGIN IMMEDIATE` transaction; any event constraint failure rolls back snapshot, revision, and event changes together. Store allocates contiguous event sequence values from the prior per-run maximum. Event timestamps must be non-decreasing, may be equal, and final event time must equal persisted `run.updated_at`.

Current aggregate snapshot excludes artifact metadata and provider-session state because `Run` does not own either in Milestone 2. Provider-neutral session events remain durable history. Later milestones may add separately owned records through new schema/snapshot versions.

## Run lifecycle

```text
Created -> Preparing -> Ready -> Running
                                  |-> NeedsUser -> Running
                                  |-> Paused ----> Running
                                  |-> Interrupted -> Running
                                  |-> Completed -> Applied
                                  |              -> Discarded
                                  |-> Failed
                                  `-> Discarded
```

`Ready` is an explicit atomic boundary: immutable configuration and run preparation succeeded, but execution has not begun. `Run` owns all mutable stages and attention requests so lifecycle invariants can be checked in one aggregate.

`Completed`, `Failed`, and `Discarded` mean execution is finished. Only `Applied` and `Discarded` permanently close normal lifecycle mutation; `Completed` remains eligible for explicit apply or discard. Completion requires all stages to have outcomes, no unresolved attention, and no blocking failed stage.

## Stage lifecycle

```text
Pending -> Ready -> Running -> Completed
   ^                  |-> NeedsUser -> Running
   |                  |-> Paused ----> Running
   |                  |-> Interrupted -> Running
   |                  `-> Failed -> Retry -> Pending
   `---------------------- Skipped (from Pending or Ready)
```

`Ready` records that dependency outcomes were checked. Required dependencies must complete successfully. Optional dependencies must reach an outcome; failure or skip permits `Ready` with explicit degraded evidence. A failed stage is execution-finished but retryable, so it is not permanently closed. Completed and skipped stages cannot restart.

A failed stage may retry while every direct dependent remains `Pending` or `Ready`. `Ready` preserves evidence that dependency validation already occurred. Once a dependent starts execution or reaches any later outcome, retry is rejected; revisiting earlier work requires a new stage/attempt instead of rewriting history. Interrupted work uses recovery, not retry.

Pause and interruption are distinct at both levels. `Paused` records deliberate user suspension and accepts `Resume`; `Interrupted` records unexpected runtime loss and accepts `Recover`. Each suspension remembers whether work should return to `Running` or `NeedsUser`, preventing attention state from being lost.

## Attention

An attention request is uniquely identified, tied to one run and stage, and typed as permission, decision, or question. A stage may accumulate multiple requests. `NeedsUser` always corresponds to at least one unresolved request; resolving or cancelling the final request restores running state immediately or updates the saved resume target while suspended. Failing or discarding a run cancels pending attention without erasing its history.

## Workflow vocabulary

A stage kind describes work such as implementation or synthesis. A role describes responsibility such as implementer or engineering lead. Neither chooses provider nor model:

```text
workflow stage -> engineering role -> provider -> model
```

Workflow definitions are validated DAGs with unique stage IDs and known, non-self, non-duplicate required or optional dependencies. Milestone 1 validates representation and local readiness only; it does not schedule or traverse workflows for execution.

## Future workflow execution

Built-in workflows will be Rust data, scheduled as dependency DAGs rather than hard-coded procedural sequences. Stage definitions will carry role, dependencies, optional dependencies, artifact type, retry policy, and fallback policy as each concept becomes executable.

FakeProvider is the first provider. Core scheduling, interruption, attention, recovery, and concurrency must work deterministically without installed agents, network access, or subscriptions.

## Process and recovery

Provider logic will depend on a process-backend interface, not tmux. TmuxBackend remains the first concrete implementation. Provider output must be consumed and persisted independently of attached clients so terminal or TUI disconnection cannot cause broken pipes or lost events.

SQLite will be canonical for resumable state. Completed stages will not be inferred from artifact presence and will not be silently repeated after restart.

Each run also snapshots its resolved configuration. Resume must use that immutable snapshot rather than silently adopting later user or repository configuration changes.

## Git safety

Every run will execute in an isolated worktree. Implementation runs use dedicated branches; review-only runs may be detached. Applying changes is explicit, validates source-checkout safety and patch applicability, preserves untracked run files, avoids automatic commits, and retains run data after apply or discard.

## Milestone 2 layout

```text
src/
├── lib.rs           importable application and domain library
├── main.rs          thin process entry
├── cli/
│   ├── mod.rs       CLI schema
│   └── commands.rs  command dispatch and store inspection
├── config/
│   └── mod.rs       side-effect-free configuration path resolution
├── domain/
    ├── run.rs       aggregate, lifecycle, dependency and attention rules
    ├── stage.rs     stage state machine
    ├── workflow.rs  workflow identity and validated DAG definition
    ├── attention.rs human-attention lifecycle
    ├── event.rs     provider-neutral semantic events
    ├── artifact.rs  typed artifact metadata
    ├── role.rs      provider/model-independent responsibility
    ├── rehydration.rs persistence-neutral reconstruction data
    └── ids.rs       strong domain identities
└── store/
    ├── sqlite.rs    transactional store and indexed projections
    ├── snapshot.rs  versioned RunSnapshotV1 codec
    ├── migrations.rs SQLite schema lifecycle
    ├── config_snapshot.rs immutable config and canonical hash
    ├── path.rs      side-effect-free data path resolution
    └── error.rs     typed persistence failures
```

Domain operations are deterministic: callers supply UTC timestamps. Invalid transitions and persistence failures return typed `thiserror` errors; `anyhow` remains at the application boundary. Serde uses inspectable snake-case values. Aggregate deserialization is prohibited; versioned DTO decoding always ends at validated rehydration.

## Decisions

- Single Cargo package and binary; no workspace.
- Rust 2024 edition with Rust 1.85 minimum, matching stable edition support.
- Configuration lookup uses standard environment variables and no platform-directory crate because required location is explicitly `~/.config/polycode`.
- Missing subcommand prints help until the runs TUI exists.
- No configuration file is created during bootstrap.
- Internal domain/database schema must not depend on provisional branding beyond filesystem and package identity where unavoidable.
- Explicit `Ready` is persisted because preparation and dependency validation need atomic recovery boundaries.
- User-requested pause and unexpected interruption are separate states and transitions.
- Cleanup is resource retention, not a run lifecycle state.
- `rusqlite` uses bundled SQLite for deterministic local availability; persistence remains synchronous until orchestration needs async boundaries.
- JSON snapshots avoid premature normalization while indexed projection columns support current list/inspection needs.
