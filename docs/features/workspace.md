# Workspace isolation, apply, discard, pull request

Every run works in its own Git worktree; the source checkout changes only when you explicitly apply, and a run can instead be published as a branch and pull request.

## Sub-features
- worktree: `~/.polycode/worktrees/<sanitized-repo>-<common-dir-hash>/<run-id>` from the committed `HEAD` at creation (the base commit).
- modes: implementation workflows own a `polycode/run-<run-id>` branch; review workflows use a detached worktree.
- apply: patch transfer from base commit to a clean source checkout; no staging, no commit.
- pr: commit the delta on the run's branch, push to `origin`, open a PR through `gh`; source checkout untouched.
- discard: record `Discarded`, then remove owned worktree and branch.
- diff-preview: the same delta apply would move, read-only (`d` in the TUI).
- reconciliation: persisted workspace intent is validated against Git evidence before any lifecycle command.

## How to get to it (user POV)
Nothing is required to get a worktree; every run gets one. When a run is `Completed`, choose `apply` to put the changes in your checkout, `pr` to publish without touching it, or `discard` to drop them. In the TUI these are `a`, `P` and `X`, each followed by Enter in a confirmation overlay. `d` previews the diff first.

## Driving it
```bash
polycode apply <run-id>
polycode pr <run-id>
polycode discard <run-id>
polycode status <run-id>      # Workspace and Base lines
```
TUI run detail: `d` diff preview, `a` then Enter apply, `P` then Enter publish, `X` then Enter discard.

## Where it lives
- `src/workspace/manager.rs` — prepare/apply/publish/discard sagas, `publish`.
- `src/workspace/model.rs` — `RunWorkspace`, `WorkspaceStatus`, apply operation records.
- `src/workspace/github.rs` — `gh pr list --head` / `gh pr create` boundary.
- `src/git/worktree.rs`, `src/git/patch.rs`, `src/git/remote.rs`, `src/git/repository.rs` — Git commands with direct argv.
- `src/store/workspace.rs` — workspace and apply-intent persistence with CAS.
- `src/store/path.rs` — `worktree_root`, `POLYCODE_DATA_DIR`.
- `src/app/run_service.rs` — `apply_run`, `publish_run`, `discard_run`, `preview_run_diff`.
- `tests/codex_cli.rs` — `native_codex_fixture_runs_through_tmux_preserves_source_then_applies`.

## Gotchas
- Apply requires a fully clean source checkout; preparation does not (the worktree starts from committed `HEAD`).
- Apply never stages or commits; an empty diff is a successful no-op (`No workspace changes to apply.`).
- `pr` rejects non-completed runs, detached (review) workspaces, empty deltas and repositories without an `origin` remote. PR failure (no `gh`, not authenticated) is reported in the receipt and never undoes the push.
- `pr` never force-pushes; a diverged remote branch is an error by design. `GIT_TERMINAL_PROMPT=0` turns a credential prompt into an error instead of a hang.
- After `pr` the run stays `Completed`, so apply, fix and discard remain available; publishing again after a fix updates the same branch and PR.
- Discard commits the logical disposition before cleanup; cleanup is idempotent and retains process files. Branch deletion needs persisted ownership and an unchanged tip, otherwise the workspace turns `Broken` and the branch is kept.
- `Broken` workspace status (moved repo, foreign path, branch collision) blocks execution; Polycode never guesses or deletes foreign data.
- Ordinary run commits and cleanup are rejected while an apply intent is `Prepared` or `AppliedToSource`.
