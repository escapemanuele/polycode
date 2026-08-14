# Polycode Orchestration

Polycode coordinates recoverable engineering work performed by native coding-agent providers while preserving explicit human control over run progress and results.

## Language

**Run**:
A recoverable orchestration effort for one task, bound for its lifetime to one effective configuration snapshot.
_Avoid_: Job, execution

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

**Run snapshot**:
A versioned persisted representation of current run state, accepted into the domain only after migration and full invariant validation.
_Avoid_: Serialized run, event replay

**Event sequence**:
A per-run ordinal defining authoritative semantic-event order independently from wall-clock timestamps.
_Avoid_: Timestamp order, global sequence

**Workflow**:
An opinionated identity plus a validated dependency graph describing which stages belong to a run.
_Avoid_: Pipeline script, custom graph

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

**Discard**:
The explicit terminal disposition that abandons a run's working changes while retaining its recorded history.
_Avoid_: Delete, cleanup

**Cleanup**:
An artifact-retention operation that may remove run resources without changing the run's lifecycle status.
_Avoid_: Cleaned status, discard
