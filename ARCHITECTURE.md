# Polycode Architecture

## Status

Milestone 8 adds native Codex CLI beside native Claude Code. Both adapters discover installed CLI/auth through read-only native commands, execute structured non-interactive JSONL inside isolated worktree through shared managed-process substrate, persist opaque native session separately from process invocation, and write verified provider-neutral artifacts. Native authentication/configuration remains authoritative; no vendor API is called directly. Gemini, multi-provider routing, async runtime, native process backend, and TUI remain deliberately absent.

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
CLI
    |
application service + query DTOs
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
    -> decode RunSnapshotV1 or RunSnapshotV2
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

Schema v2 adds infrastructure records:

```text
run_workspaces(run_id, source_repo_path, git_common_dir, base_commit,
               worktree_path, branch_name, mode, status, branch_owned,
               removal_head, last_error, revision, created_at, updated_at)

run_apply_operations(run_id, status, patch_hash, run_revision,
                     last_error, revision, created_at, updated_at)
```

`RunSnapshot` remains logical orchestration state. `RunWorkspace` is a one-to-one physical resource record; it can be broken by external Git/filesystem changes without making domain rehydration depend on path existence. Existing v1 databases migrate forward without rewriting snapshots, events, or configuration.

Schema v3 adds immutable user intent:

```text
run_inputs(run_id, schema_version, task, created_at)
```

`RunInput` owns normalized task text outside `Run`, configuration, workspace, and events. New-run transaction inserts `RunInput`, configuration, `RunSnapshotV2`, and initial event atomically. Database triggers reject input update/delete. Legacy v1/v2 databases gain empty input table without fabricated task data; old `RunSnapshotV1.task` remains readable but is intentionally ignored by aggregate rehydration. `RunSnapshotV2` no longer contains task text.

Schema v4 adds separately owned process infrastructure:

```text
managed_processes(id, run_id, stage_id, attempt,
                  backend_kind, backend_session_id, status,
                  spec_schema_version, spec_json, command_fingerprint,
                  stdout_offset, stdout_cursor_revision,
                  stderr_offset, stderr_cursor_revision,
                  exit summary, interrupt_requested, revision, timestamps)
```

Schema v5 extends process identity with positive invocation number and immutable stdin path/hash, then adds provider sessions and artifacts:

```text
provider_sessions(id, run_id, stage_id, attempt, provider_id,
                  native_session_id, current_process_id, status,
                  protocol_version, invocation, model_id, cli_version,
                  pending attention range, revision, timestamps)

artifacts(id, run_id, stage_id, attempt, kind, status, role,
          provider_id, model_id, path, content_hash, content_size,
          base_commit, timestamps)
```

Provider-session identity `(run, stage, attempt, provider)` is immutable and distinct from backend/process identity. One attempt may use multiple invocations while continuing one native conversation. Artifact rows are insert-only; bytes are written and fsynced before metadata commit, then hash/size verified on insertion and load.

Launch identity is immutable and unique per `(run_id, stage_id, attempt, invocation)`. Lifecycle and each output cursor use independent compare-and-swap revisions. Process rows reference runs but never enter `RunSnapshot`; existing v1-v4 state migrates without rewriting run, event, configuration, input, workspace, or apply data.

Resolved config payloads are recursively key-sorted and compact-encoded before SHA-256 hashing. An existing config ID accepts an exact idempotent insert only; different content or metadata is rejected. Database triggers reject update and delete operations, enforcing insert-only storage beneath the Rust API.

Every update performs compare-and-swap:

```sql
UPDATE runs
SET snapshot_json = ?, revision = revision + 1, ...
WHERE id = ? AND revision = ?
```

Zero changed rows means `ConcurrentModification`. Snapshot update precedes event inserts inside one `BEGIN IMMEDIATE` transaction; any event constraint failure rolls back snapshot, revision, and event changes together. Store allocates contiguous event sequence values from the prior per-run maximum. Event timestamps must be non-decreasing, may be equal, and final event time must equal persisted `run.updated_at`.

Current aggregate snapshot excludes task input, artifact metadata, and provider-session state because `Run` does not own them. Provider-neutral session/checkpoint events remain durable history. `FakeProvider` reconstructs deterministic cursor from those events; Claude and Codex reconstruct separately owned provider-session/process/output state without weakening `Run::rehydrate`.

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

Workflow definitions are validated DAGs with unique stage IDs and known, non-self, non-duplicate required or optional dependencies. Built-in Fast, Standard, Deep, and Review workflows are ordinary Rust graph data.

## Milestone 4 workflow execution

Scheduler loop is graph-driven:

```text
load validated Run + Ready workspace
    -> reject active apply intent
    -> evaluate every Pending stage dependency set
    -> atomically mark all newly Ready or blocked stages
    -> resolve stage role against provider capability
    -> consume one provider record (one signal or atomic signal batch)
    -> atomically commit Run state + complete event batch
    -> evaluate graph again
```

No branch checks `WorkflowKind` during scheduling. Review fan-out makes `deep_analysis` and `independent_review` Ready in one dependency pass after research; synthesis joins their terminal outcomes through optional edges. Initial scheduler executes one eligible stage at a time even when several are Ready, preserving deterministic tests while retaining DAG parallelism for a later process backend.

`FakeProvider` is first provider. Scenarios script start, progress, usage, human attention, pause, interruption, completion, failure, and explicit delay gates. Every consumed signal has one identifying semantic event (`ProviderStarted`, `ProviderProgress`, `ProviderNeedsUser`, `ProviderUsageUpdated`, `ProviderPaused`, `ProviderInterrupted`, `ProviderCompleted`, or `ProviderFailed`). Signal index and attempt derive from per-stage event history, resetting only after explicit `StageRetryScheduled`; restart therefore cannot silently replay an already committed fake signal. `ProviderNeedsUser` links provider/session to the independently persisted `NeedsUser` lifecycle event and attention request.

Execution commits recheck `WorkspaceStatus::Ready` inside same `BEGIN IMMEDIATE` transaction used for run compare-and-swap and event append. Active `Prepared` or `AppliedToSource` apply intent rejects execution through existing store guard. Preflight checks provide typed engine errors; transactional checks close race windows.

Attention resolution, stage resume, interruption recovery, and retry are scheduler-boundary commands. They use same workspace/apply guards and atomic run commit as automatic advancement.

## Milestones 7–8 application, providers, and CLI

`RunService` is application boundary. CLI parses and prints only; service owns use-case ordering:

```text
validate RunInput + discover repository
    -> resolve immutable provider config and verify availability
    -> atomic Run + RunInput + config + created event
    -> prepare graph-selected workspace
    -> reconstruct provider from config + workflow + events
    -> drive scheduler to quiescence
    -> reload committed events and query DTO
    -> print
```

Provider construction sits behind `ProviderFactory`. Runtime factory accepts explicit `claude`, `codex`, or `fake`, persists choice in immutable run configuration, and reconstructs same provider on restart without fallback. Fake keeps `development_fake/default_success_v1`. Claude and Codex configurations persist safe selection/options only; native credentials and environment are never snapshotted. Installed CLI version and provider-confirmed model/session, when exposed, are runtime metadata.

`ClaudeProvider` uses native CLI structured print mode with `dontAsk`, never broad permission bypass. Initial prompt and continuations enter through immutable stdin. JSONL decoder accepts one complete record per poll; partial record waits, unknown valid records become non-semantic checkpoints, and invalid JSON fails without cursor advancement. System init binds opaque native session, assistant/result records map to provider-neutral usage/progress/attention/failure/completion.

Denied native tool calls become typed permission attention. SQLite stores only attention identity and exact raw-record range; human resolution reconstructs structured denial from retained output, converts only safely representable exact rule to native `--allowedTools`, and starts new `--resume <same-session>` invocation. Ambiguous/wildcard rules fail closed. Native questions require explicit `resolve --response`; answer is immutable run-private stdin, not argv or SQLite event payload.

On success, Claude result becomes human-readable stage artifact. Downstream prompt includes only direct dependency artifacts. Provider session CAS, raw-output cursor CAS, run snapshot/revision, complete semantic event batch, and artifact metadata share one `BEGIN IMMEDIATE` transaction. Fault before commit replays record; no accepted signal can exist without matching session/cursor checkpoint.

### Native Codex CLI

`CodexProvider` uses `codex exec --json`, prompt `-` on immutable stdin, and `--output-last-message` under run-private provider output. Native user/project configuration, authentication, `AGENTS.md`, rules, skills, MCP, and hook trust remain active. Codex immutable config is `native_codex` schema 1 with `exec_json_v1`, `stage_kind_v1`, and approval `never`; model `null` omits `--model` and preserves native default.

Execution controls are explicit and separate. `Implementation` and `Fix` select `workspace-write`; all other stage kinds select `read-only`. Approval is `never` for deterministic non-interactive execution but sandbox remains enabled. Dangerous sandbox/approval bypass, `danger-full-access`, ephemeral sessions, Git-check bypass, and native config/rules bypass are prohibited.

`thread.started` binds provider-issued `thread_id` to generic native session identity. Duplicate identical identity checkpoints; conflict fails closed. Recovery consumes retained output first, then resumes exact persisted thread through `codex exec ... resume <thread-id> -` in new invocation. Failed-stage retry creates new provider session and thread. If process disappears before any thread identity exists, retained output is still parsed; only absence of recoverable identity permits later initial invocation for same attempt.

Decoder handles one complete JSON line per poll. Partial line waits; unknown valid event checkpoints cursor without semantic event; invalid complete JSON fails without cursor advance. Agent messages may become progress. Reasoning content is never exposed. Command/file/MCP/web/plan items produce only bounded generic progress or checkpoints, never raw payloads. `turn.failed` and `error` fail stage. Current stable exec JSON offers no typed safely resumable approval/question request, so Codex does not fabricate `NeedsUser` from prose; this differs intentionally from Claude typed permission continuation.

One `turn.completed` contains both stable input/output token usage and successful boundary. Generic `ProviderPoll::Emission` therefore carries ordered signal batch. Scheduler applies `[Usage, Completed]` to in-memory run, then existing semantic provider transaction commits run/events, provider session Completed, artifact metadata, and one output-cursor acknowledgement. Crash before commit replays raw line; after commit both effects exist once. Fake and Claude use singleton batches, preserving behavior.

Provider waits for protocol completion plus successful managed-process exit. Final assistant file is copied into canonical immutable artifact with write-once fsync/hash semantics before transaction. Crash before metadata commit leaves replayable final file and possibly identical canonical orphan; replay verifies/reuses bytes without duplicate artifact row. Downstream prompts remain limited to direct dependency artifacts.

Before continuation, application reconciles workspace. Engine/store guards still require `WorkspaceStatus::Ready` and reject active apply intent at mutation transaction. Resume policy continues Ready/Running, resumes deliberate suspension, recovers interruption, preserves `NeedsUser`, refuses implicit retry from Failed, reports Completed/Applied, and rejects Discarded. `resolve` and `retry` perform exact explicit action then drive again.

Query DTOs (`RunListItem`, `RunDetails`, `StageSummary`, `AttentionSummary`, `UsageSummary`) isolate CLI formatting from mutable domain/store internals. List query uses indexed run columns with `run_inputs`/workspace joins and does not decode every snapshot. Detail query rehydrates one run and aggregates committed usage events. Read-only commands perform no lifecycle mutation; `runs` does not create missing database.

Execution reports contain only event rows reloaded after successful commits. CLI therefore never publishes speculative provider signals. Needs-user, pause, interruption, and failed outcomes use exit 0 as valid quiescent states; operational failures use exit 1 and Clap parse failures use exit 2.

## Process and recovery

Provider adapters depend on `ProcessBackend`, not tmux. `TmuxBackend` implements availability, exact launch, owned-session inspection, raw output reads, graceful interruption, and ownership-safe cleanup. `ProviderRequest` remains provider-neutral; `ProviderPoll` may carry signal plus neutral persistence checkpoint.

Process launch uses intent/effect/finalize:

```text
Preparing persisted
    -> immutable spec/stdin/output files materialized
    -> Starting claimed by lifecycle CAS
    -> tmux direct-argv runner launch
    -> owned session or valid exit evidence observed
    -> Running / Exited finalized
```

Tmux receives hidden runner executable, subcommand, and manifest path as separate arguments. No launch path uses `sh -c`, quoting, `eval`, or interpolated command text. Each managed process uses isolated tmux server. Session environment carries safe operational variables plus non-secret process ID, fingerprint, and one-time socket path; existing sessions are reusable or removable only when both ownership markers match persisted identity.

Parent environment is cleared before tmux server starts. Native provider variables excluded from safe session allowlist cross through bounded user-only (`0600`) Unix socket after launch, never argv, tmux environment, manifest, SQLite, or durable file. Runner receives bytes in memory, validates framing/size, clears inherited environment, and reconstructs provider environment before exact child exec. This preserves native environment-based authentication without credential persistence or command-line exposure.

Runner validates its manifest and ownership, creates a separate child process group through hidden exec bridge, redirects stdout/stderr to regular append-only files, persists live runner/child identity in atomic `runtime.json`, waits for exact child exit, then publishes atomic `exit.json`. Exec bridge converts tmux's inherited ignored SIGINT disposition into caught disposition before spawn and then Unix `exec` resets it to default in provider image; this keeps Ctrl-C termination portable across macOS and Linux. Backend interruption validates session ownership, runner pane PID, runtime fingerprint, and child process group before sending SIGINT. Cleanup is separate and retains all process files.

Process state is reconciled from independent evidence:

```text
Preparing + no session/no exit       -> safe to start
Preparing + owned session            -> Running
Starting/Running + owned session      -> Running
active + no session + valid exit      -> Exited or Interrupted
active + no session + no exit         -> Missing
mismatched/corrupt evidence           -> Broken
terminal + owned cleanup              -> Cleaned, files retained
```

Absence never implies success. Tmux sessions survive client detachment/process exit, but not reboot or tmux server loss. Without valid exit evidence, lost supervisor state becomes `Missing`; Claude and Codex map loss to semantic interruption after native identity exists and recover through same native session only after explicit run recovery.

Output files live under `runs/<run>/processes/<process>/`. Reads return raw byte chunks with start/end offsets and do not mutate SQLite. Consumer explicitly acknowledges consumed prefix through per-stream cursor CAS. Native-provider semantic records combine run/events, provider session, artifact metadata, and acknowledgement in one transaction. Crash before commit replays bytes; reads remain available after exit.

## Git safety

Git runs through `std::process::Command` with direct argument arrays; no shell command interpolation is used. Repository discovery persists canonical source path, canonical Git common directory, and immutable base commit. Paths are passed as OS arguments, while NUL-delimited Git output is used where records must be parsed. Binary patches use short-lived temporary files so large input cannot deadlock subprocess pipes; files are removed automatically after each command.

Managed worktrees live under:

```text
~/.polycode/worktrees/<sanitized repository + short common-dir hash>/<run-id>
```

Implementation worktrees use deterministic `polycode/run-<run-id>` branches. Review worktrees are explicitly detached. Source checkout may be dirty during preparation because worktree starts from committed `HEAD`; source must be fully clean during apply.

SQLite and Git cannot share a transaction. Workspace lifecycle therefore uses durable intent and reconciliation:

```text
Preparing persisted -> git worktree add -> identity validation
    -> workspace Ready + Run Ready committed atomically

Removing persisted -> ownership validation -> git worktree remove
    -> compare-and-delete owned branch -> Removed persisted
```

Reconciliation retries absent `Preparing` resources, finalizes already-created valid resources, continues `Removing`, and leaves repeated terminal operations idempotent. Missing ready resources, relocated repositories, foreign paths, branch collisions, moved branch tips, or other ambiguous evidence become `Broken`; Polycode does not guess or delete foreign data.

Workspace status changes are infrastructure control state, not new domain events. Existing `RunPreparationStarted`, `RunPrepared`, `RunApplied`, and `RunDiscarded` events capture semantic behavior; individual Git commands and cleanup progress stay out of domain history.

Apply computes exact delta from persisted base commit using a temporary Git index. `read-tree`, `git add -A`, and `git diff --cached --binary --full-index` include tracked edits, untracked files, deletions, file modes, unusual UTF-8 filenames, and binary data without touching source or worktree index. Apply then requires clean source, persists SHA-256 patch intent and run revision, runs `git apply --check`, and runs `git apply` without staging or committing.

Git apply and SQLite lifecycle finalization form another intent/effect/finalize boundary. Recovery regenerates patch and requires identical hash. Forward-check success means effect may proceed; reverse-check success with forward failure proves expected patch is already present and permits exactly-once logical finalization. Ambiguous evidence fails closed. Apply retains worktree for inspection.

Creating apply intent uses run revision compare-and-swap. While status is `Prepared` or `AppliedToSource`, ordinary run commits and workspace cleanup are rejected; final apply records operation, run snapshot, revision, and `RunApplied` event in one SQLite transaction.

Discard commits `RunStatus::Discarded` before cleanup. Cleanup independently removes worktree resources for completed, applied, or discarded runs without changing logical status. Branch deletion requires persisted ownership, no remaining checkout, and atomic expected-tip deletion; movement after removal intent produces `Broken` and preserves branch.

## Current layout

```text
src/
├── lib.rs           importable application and domain library
├── main.rs          thin process entry
├── cli/
│   ├── mod.rs       CLI schema
│   └── commands.rs  thin use-case dispatch and committed-state rendering
├── app/
│   ├── run_service.rs orchestration use cases and quiescence policy
│   ├── provider_factory.rs restart-stable provider construction
│   ├── query.rs      CLI-facing read models
│   └── error.rs      typed application failures
├── config/
│   └── mod.rs       side-effect-free configuration path resolution
├── domain/
│   ├── run.rs       aggregate, lifecycle, dependency and attention rules
│   ├── stage.rs     stage state machine
│   ├── workflow.rs  workflow identity and validated DAG definition
│   ├── attention.rs human-attention lifecycle
│   ├── event.rs     provider-neutral semantic events
│   ├── artifact.rs  typed artifact metadata
│   ├── role.rs      provider/model-independent responsibility
│   ├── rehydration.rs persistence-neutral reconstruction data
│   └── ids.rs       strong domain identities
├── engine/
│   ├── scheduler.rs deterministic DAG evaluation and guarded commits
│   ├── provider.rs  provider-neutral synchronous signal boundary
│   ├── fake.rs      validated scripts and restart-stable FakeProvider
│   └── error.rs     typed execution/protocol failures
├── process/
│   ├── backend.rs   provider-independent process supervisor contract
│   ├── manager.rs   persisted intent/effect/finalize and reconciliation
│   ├── tmux.rs      ownership-safe shell-free tmux backend
│   ├── runner.rs    hidden exact-argv child runner and durable evidence
│   ├── model.rs     process spec/status/output/exit records
│   ├── ids.rs       managed-process and backend-session identities
│   └── error.rs     typed process/backend failures
├── providers/
│   ├── session.rs   provider-neutral conversation identity and lifecycle
│   ├── checkpoint.rs atomic provider commit payload
│   ├── artifact.rs  immutable artifact record
│   ├── claude/      native discovery, argv, prompts, JSONL decoder, adapter
│   └── codex/       native discovery, exec argv, prompts, JSONL decoder, adapter
├── store/
│   ├── sqlite.rs    transactional store and indexed projections
│   ├── snapshot.rs  RunSnapshotV1/V2 migration and codec
│   ├── migrations.rs SQLite schema lifecycle
│   ├── config_snapshot.rs immutable config and canonical hash
│   ├── run_input.rs immutable normalized task input
│   ├── process.rs   process lifecycle and output-cursor CAS persistence
│   ├── provider.rs  provider-session/artifact persistence and atomic commits
│   ├── workspace.rs workspace/apply intent persistence and CAS
│   ├── path.rs      data and worktree path resolution
│   └── error.rs     typed persistence failures
├── git/
│   ├── command.rs    native Git command runner
│   ├── repository.rs canonical repository identity
│   ├── worktree.rs   create/inspect/remove and branch ownership
│   ├── patch.rs      temporary-index patch generation and apply
│   └── error.rs      typed Git failures
└── workspace/
    ├── manager.rs    intent/effect/finalize orchestration
    ├── model.rs      workspace and apply-operation records
    └── error.rs      typed lifecycle/reconciliation failures
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
- Run workspace state stays outside logical run snapshots; filesystem availability never participates in `Run::rehydrate`.
- Git effects use intent/effect/finalize sagas with explicit reconciliation; SQLite transactions never span subprocess execution.
- Apply uses patch transfer instead of merge/cherry-pick and never stages or commits source changes.
- Discard is a logical disposition; cleanup is an independent physical-resource operation.
- Built-in workflows are validated DAG data; scheduler contains no workflow-specific execution branches.
- One consumed provider record produces one durable checkpoint; an ordered signal batch shares same atomic run commit and one raw cursor acknowledgement.
- Scheduler is synchronous and single-stage deterministic in Milestone 4; async/process concurrency remains a backend concern.
- User task is immutable `RunInput`, not aggregate lifecycle state or provider configuration.
- Workflow workspace mutability derives from stage kinds (`Implementation`/`Fix`), not workflow-name branches.
- CLI provider choice is explicit; native Claude, native Codex, and development Fake profiles are restart-stable immutable run configuration.
- Application commands run scheduler to durable quiescence and render only reloaded committed state/events.
- Managed processes are separate infrastructure attempts; process exit does not directly mutate semantic run/stage state.
- Exact external argv is preserved end to end; tmux launches multiple command arguments directly rather than a shell command string.
- Process launch, interrupt, and cleanup require persisted intent plus fingerprint-bound ownership evidence.
- Raw output read and acknowledgement are separate; provider semantic commit atomically joins cursor, session, artifact metadata, run state, and events.
- Provider session, managed process, and backend session are distinct identities; continuation advances invocation without changing attempt/native conversation.
- Native Claude default model is used unless immutable configuration supplies one; model shown to user comes from provider confirmation.
- Permission continuation uses same Claude UUID and exact safely representable native allow rule; broad/ambiguous approval fails closed.
- Native Codex default model is used unless immutable configuration supplies one; no model is marked confirmed without protocol evidence.
- Codex sandbox derives from stage kind and remains enabled with approval `never`; no prose heuristic creates human attention.
