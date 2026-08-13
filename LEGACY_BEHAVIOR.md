# Legacy Behavior Reference

## Source inspected

`agents-v3.0.0` was inspected as a Bash reference implementation, not executed or copied into this repository.

```text
version: 3.0.0
size: 3,820 lines / 134,777 bytes
sha256: cd47666bc57651054522bee35e1e8906ba96f1c5fe310f75e8ccfc8df81d2c80
syntax: valid Bash (`bash -n`)
```

This document records behavior worth preserving. It does not endorse the script's internal architecture.

## Workflow behavior

### Fast

```text
Implementation (Codex) -> result
```

Fast requires Codex and has no review or decision stage in version 3.0.0.

### Standard

```text
Architecture (Claude) -> Implementation (Codex) -> Review (Claude) -> Decision (Claude)
```

Planning, review, and decision can each be disabled by snapshotted run configuration. Disabled stages produce typed skipped artifacts where downstream context needs them.

### Deep

```text
Research (Claude) -> Architecture (Claude) -> Implementation (Codex)
    -> Review (Claude) -> Decision (Claude)
```

Research and architecture share one process lane but remain separate resumable stages. Research failure prevents architecture; architecture failure prevents implementation.

### Review

```text
Research -> Deep analysis ---------+
                                      -> Final synthesis -> Decision
Independent Codex review ----------+
```

Review runs use detached worktrees and read-only provider execution. Codex review is optional: missing CLI, disabled configuration, or branch failure does not block final synthesis. Claude analysis failure also degrades to independent final synthesis. This is concrete evidence for optional DAG dependencies with explicit fallback policy.

### Fix

Fix continues an implementation run rather than creating an unrelated run. It consumes review findings, edits through Codex, and invalidates the old decision artifact before generating a new decision. Review-only runs reject fix because they are intentionally read-only.

## Run and Git behavior

- Run IDs combine local timestamp and random suffix.
- Repository identity derives from Git common-directory path plus a truncated SHA-256 digest, so linked worktrees resolve to one repository registry.
- Every run records source path, worktree path, base commit, branch, mode, task, current stage, process session, and timestamps.
- Implementation runs create dedicated `agents/...` branches. Review runs create detached worktrees.
- Dirty source checkout is allowed when starting, but run always starts from committed `HEAD` and emits a warning.
- Source checkout remains untouched until explicit apply.
- Apply requires a clean source checkout, creates a binary patch from base commit, includes untracked worktree files through intent-to-add, validates with `git apply --check`, applies without committing, and retains run/worktree data.
- Discard stops live tmux, archives `.ai` artifacts, force-removes worktree, deletes only branches under the tool-owned prefix, and retains archived history.
- Multiple runs can coexist for one repository.

## Persistence and recovery

- Registry state survives tmux death and reboot.
- Completed stages are recognized through sentinel files; failed unfinished stages have separate sentinels.
- Resume reattaches when tmux session exists. Otherwise it recovers provider session/thread IDs from persisted JSONL, clears only unfinished failure markers, recreates tmux, and skips completed stages.
- Claude session IDs and Codex thread IDs are persisted from provider logs and native hooks, then passed to native CLI resume commands.
- Explicit stop kills tmux and records `paused`, distinct from accidental interruption.
- Observed run status vocabulary: `created`, `ready`, `running`, `needs_user`, `interrupted`, `paused`, `completed`, `applied`, `discarded`, and `cleaned`; code also recognizes `archived` when listing stale runs.

Polycode must preserve recovery outcomes while replacing sentinel inference with canonical SQLite stage state.

## Provider and process behavior

- Claude and Codex remain native CLIs using native authentication.
- Provider-specific stream JSON/JSONL stays inside provider/runtime handling.
- Provider stdout writes to regular log files. A separate dashboard tails those files. No provider writes into a UI-owned pipe; UI/process disconnection therefore cannot trigger the historical broken-pipe failure.
- Codex retries with its default model only when the requested preferred model is unavailable or inaccessible. It never retries ordinary task failures because files may already be modified.
- Review falls back to Claude-only operation when Codex is unavailable.
- Provider session, elapsed time, tokens, cached tokens, reasoning tokens, exit code, and provider-reported cost are recorded per stage.
- Dollar cost remains unavailable unless provider reports it; no inferred subscription or API pricing.
- Doctor distinguishes required local tools, provider presence, optional `gh`, result pager, and workflow availability. Authentication is verified lazily when provider starts.

## Attention and hooks

- Per-run Claude and Codex hooks capture lifecycle events and session IDs.
- Permission/input signals create persisted `NEEDS_YOU` data, append journal events, set run status to `needs_user`, and may emit a macOS notification.
- Non-interactive execution reports attention but does not pretend to provide an approval dialog.
- Existing repository-owned Codex hooks are never overwritten.

Polycode should persist typed `Permission`, `Decision`, and `Question` requests in SQLite and support a queue. Legacy single-file attention remains behavioral evidence, not target storage design.

## Configuration and artifacts

- User config and repository override are merged once when run is created.
- Effective configuration is persisted inside run and reused on resume. Polycode should similarly snapshot resolved profile, role mappings, models, policies, and relevant provider capabilities for deterministic recovery.
- Markdown artifacts carry `agents.artifact/v1` frontmatter with type, run, stage, provider, model, status, creation time, and base commit.
- Journal and usage data use append-only JSONL.
- PR review optionally captures GitHub metadata and diff using `gh`, falling back to local repository evidence.

## Intentional architectural departures

Polycode preserves behavior above but does not copy these implementation choices:

- JSON plus sentinel files as canonical state
- workflow scheduling encoded through shell processes waiting on files
- role-to-model mapping embedded in orchestration functions
- provider execution coupled directly to tmux and shell globals
- generated provider hook files as cross-system event source of truth
- broad shell cleanup based on generated-file globs

## Milestone 1 decisions surfaced

- Model explicit `Paused` run status, or document why user-requested stop maps to `Interrupted`. They carry different intent; explicit `Paused` is preferred.
- Decide whether `Ready` is persisted run state or derived from prepared stages. Prefer deriving readiness unless recovery needs an atomic prepared boundary.
- Treat `Cleaned` as an artifact-retention operation, not core run lifecycle status.
- Encode optional dependency outcomes so synthesis can proceed after unavailable, skipped, or failed optional reviewers while retaining evidence of degradation.
- Persist immutable effective run configuration separately from mutable user/repository configuration.
