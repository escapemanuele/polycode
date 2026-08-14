# Polycode Orchestration

Polycode coordinates recoverable engineering work performed by native coding-agent providers while preserving explicit human control over run progress and results.

## Language

**Run**:
A recoverable orchestration effort bound for its lifetime to one immutable input and one effective configuration snapshot.
_Avoid_: Job, execution

**Run input**:
Immutable normalized user intent for one run, stored separately from mutable lifecycle state and effective configuration. Leading/trailing whitespace is removed; internal Unicode and line breaks are preserved.
_Avoid_: Task field in Run, provider prompt, mutable description

**Stage**:
A resumable unit of engineering work within a run, with declared dependencies and one terminal outcome.
_Avoid_: Step, phase

**Run status**:
The run's current lifecycle condition, including a ready boundary after preparation and before execution. Cleanup is not a run status.
_Avoid_: Run state

**Stage status**:
The stage's current lifecycle condition. Ready means dependencies were checked and work is eligible to start.
_Avoid_: Stage state

**Execution-finished**:
A lifecycle condition with no active work. Failed work may still be retried, and a completed run may still be applied or discarded.
_Avoid_: Terminal

**Lifecycle-closed**:
A condition that rejects normal future lifecycle mutation: applied or discarded for a run, completed or skipped for a stage.
_Avoid_: Finished, terminal

**Ready**:
An atomic lifecycle boundary where preparation or dependency validation succeeded, but execution has not started.
_Avoid_: Waiting, pending

**Paused**:
A deliberate suspension requested by the user and eligible to resume from the condition that was suspended.
_Avoid_: Stopped, interrupted

**Interrupted**:
An unplanned suspension caused by loss of the active process or runtime, eligible for recovery from the condition that was interrupted.
_Avoid_: Paused, failed

**Resume**:
Continue deliberately paused work from its saved condition.
_Avoid_: Recover, retry

**Recover**:
Continue unexpectedly interrupted work from its saved condition.
_Avoid_: Resume, retry

**Attention request**:
A persisted, uniquely identified request for human permission, a decision, or an answer before affected work can continue. Multiple pending requests preserve creation order across pause or interruption.
_Avoid_: Needs-you signal, prompt

**Retry**:
An explicit decision that returns a failed stage to pending. Retry remains available while every downstream stage is pending or ready; recovery never silently retries failed work.
_Avoid_: Resume, recover

**Required dependency**:
A predecessor whose successful completion is necessary before a stage is ready.
_Avoid_: Hard dependency

**Optional dependency**:
A predecessor that must reach an outcome before a stage is ready, but whose unsuccessful outcome permits degraded progress.
_Avoid_: Soft dependency

**Degraded readiness**:
Readiness achieved after one or more optional dependencies ended unsuccessfully, preserving that reduced-evidence condition.
_Avoid_: Partial success

**Configuration snapshot**:
The immutable effective configuration selected when a run is created and reused for every recovery or resume of that run.
_Avoid_: Current configuration, config copy

**Application service**:
The use-case boundary coordinating repository discovery, atomic creation, workspace lifecycle, provider reconstruction, scheduler commands, and committed query results.
_Avoid_: CLI command handler, domain aggregate

**Quiescence**:
A durable condition where synchronous execution has no immediate legal work: completed, awaiting attention, paused, interrupted, failed, applied, discarded, or provider-delayed.
_Avoid_: Every stop is an error, busy polling

**Run snapshot**:
A versioned persisted representation of current run state, accepted into the domain only after migration and full invariant validation.
_Avoid_: Serialized run, event replay

**Run workspace**:
The separately persisted physical Git resource assigned to one run: source repository identity, immutable base commit, managed worktree, and optional owned branch. It is infrastructure state, not part of the run snapshot.
_Avoid_: Repository fields in Run, execution directory

**Workspace mode**:
The explicit Git shape of a run workspace: branch-backed for implementation work or detached for review-only work.
_Avoid_: Mode inferred from branch text

**Workspace status**:
The recoverable lifecycle of the physical Git resource: preparing, ready, removing, removed, or broken. It does not determine run status.
_Avoid_: Run status, filesystem existence alone

**Workspace intent**:
A durable declaration of an intended Git resource change recorded before its non-transactional filesystem effect.
_Avoid_: Best-effort metadata, open database transaction around Git

**Workspace reconciliation**:
Validation of persisted workspace intent against current Git/filesystem evidence, followed only by a safe idempotent completion or an explicit broken outcome.
_Avoid_: Blind retry, filesystem inference

**Base commit**:
The immutable commit from which a run workspace was created and against which its apply delta is generated.
_Avoid_: Current source HEAD, mutable branch tip

**Rehydration data**:
Persistence-neutral, untrusted current-state input consumed by `Run::rehydrate`; it is not a valid run until all workflow, lifecycle, ownership, attention, suspension, and timeline invariants pass.
_Avoid_: Deserialized run, trusted snapshot

**Run revision**:
A per-run compare-and-swap counter changed by every committed state mutation. A stale expected revision means concurrent modification, not retryable success.
_Avoid_: Event sequence, schema version

**Atomic run commit**:
One SQLite transaction containing a run snapshot/revision update and its complete semantic-event batch, so neither side can become durable alone.
_Avoid_: Save then log, eventual event write

**Event sequence**:
A per-run ordinal defining authoritative semantic-event order independently from wall-clock timestamps.
_Avoid_: Timestamp order, global sequence

**Workflow**:
An opinionated identity plus a validated dependency graph describing which stages belong to a run.
_Avoid_: Pipeline script, custom graph

**Workflow scheduler**:
The execution service that repeatedly evaluates a workflow graph and advances eligible stages without workflow-specific procedures.
_Avoid_: Pipeline runner, stage script

**Provider signal**:
One provider-neutral report accepted from a provider while a stage executes, such as progress, usage, human attention, suspension, interruption, failure, or completion.
_Avoid_: Log line, provider callback

**Provider checkpoint**:
A durable semantic record proving one provider signal was consumed, from which provider progress can continue without silently repeating accepted work.
_Avoid_: In-memory cursor, artifact presence

**Role**:
An engineering responsibility assigned to a stage, independent from provider and model selection.
_Avoid_: Agent, model

**Stage kind**:
The semantic work performed by a stage, independent from the role responsible for it.
_Avoid_: Role, provider

**Provider**:
An extensible adapter identity for a native coding-agent CLI and its native session behavior.
_Avoid_: Model, role, vendor API

**Model**:
A provider-resolved model identity used for one provider session.
_Avoid_: Role, provider

**Apply**:
The explicit act of transferring a completed implementation run's changes back to its source checkout.
_Avoid_: Merge, commit

**Apply operation**:
A persisted recoverable intent identifying one exact patch transfer and whether its Git effect is prepared, present in the source, recorded, or failed.
_Avoid_: Generic job, implicit retry

**Patch hash**:
A SHA-256 identity of exact apply bytes used to detect changed workspace output across crash recovery; it is evidence of operation identity, not authentication.
_Avoid_: Content signature, run revision

**Branch ownership**:
Persisted and revalidated evidence that a managed branch was created for one run and may be deleted only while its expected tip still matches.
_Avoid_: Prefix-only deletion permission

**Discard**:
The explicit terminal disposition that abandons a run's working changes while retaining its recorded history.
_Avoid_: Delete, cleanup

**Cleanup**:
An artifact-retention operation that may remove run resources without changing the run's lifecycle status.
_Avoid_: Cleaned status, discard
