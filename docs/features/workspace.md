# Workspace isolation, apply, discard, pull request

Every run works in its own Git worktree; the source checkout changes only when you explicitly apply, and a run can instead be published as a branch and pull request.

## Sub-features
- worktree: `~/.polycode/worktrees/<sanitized-repo>-<common-dir-hash>/<run-id>` from the committed `HEAD` at creation (the base commit).
- modes: implementation workflows own a `polycode/<slug>-<id-tail>` branch — `<slug>` is the issue key from a Linear or GitHub URL or bare `KEY-123` in the task (`dotcom-17972`), else the task's first words; `<id-tail>` is the last 6 characters of the run id, and a run without input falls back to `polycode/run-<run-id>`; review workflows use a detached worktree.
- setup: the `[setup]` table of `.polycode.toml` (`commands`, `timeout_seconds`, default 3600 s), found worktree-then-source-repository like every other table, runs in the new worktree after validation and before the workspace is marked `Ready` — so no stage ever sees a tree whose gitignored build output is missing. Argv, no shell, stops at the first failure. A failing command raises `SetupFailed` carrying the command, why it failed, and the last 20 lines of each stream; a table that cannot be read raises `SetupConfig`. Preparation fails in both cases and the workspace never reaches `Ready`. Repositories with no `[setup]` table are unaffected.
- apply: patch transfer from base commit to a clean source checkout; no staging, no commit.
- verification-gate: `apply` and `pr` refuse unless the run's latest Verify stage — the last one in the graph, since every fix or continue cycle appends its own — is `Completed` (`verification did not pass: stage verify is failed`). Older verify stages do not count: a failed `verify` answered by a passed `verify_1` no longer blocks. Checked before the run-status check.
- pr: commit the delta on the run's branch, push to `origin`, open a PR through `gh`; source checkout untouched.
- pr-draft: the PR title and description are quoted from the `## Pull request` section of the latest editing stage's artifact (Implementation, Simplification, Fix, FollowUp); the task's first line and text stand in when no stage wrote one.
- discard: record `Discarded`, then remove owned worktree and branch.
- reclaim: applying removes the worktree and keeps the branch, because an applied run can be neither applied again nor fixed. A sweep at TUI startup does the same for `Applied` and `Discarded` runs that still hold one. `Completed` runs are never swept — apply, fix and continue all read their worktree.
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
- `src/workspace/manager.rs` — prepare/apply/publish/discard sagas, `publish`, `ensure_verification_passed`.
- `src/workspace/error.rs` — `WorkspaceError`, incl. `VerificationNotPassed`, `SetupConfig`, `SetupFailed`.
- `src/workspace/setup.rs` — the `[setup]` table reader and its runner, called from `prepare_run_workspace` before the workspace is marked ready.
- `src/workspace/model.rs` — `RunWorkspace`, `WorkspaceStatus`, apply operation records.
- `src/workspace/github.rs` — `gh pr list --head` / `gh pr create` boundary.
- `src/workspace/pull_request.rs` — `PullRequestDraft`, `extract` of the artifact's `## Pull request` section; `src/app/query.rs` `pull_request_draft` walks the editing artifacts newest first and takes the first that wrote the section. When none did, `publish_title`/`publish_body` in `manager.rs` name the run from the task with its links dropped, falling back to the linked issue.
- `src/git/worktree.rs`, `src/git/patch.rs`, `src/git/remote.rs`, `src/git/repository.rs` — Git commands with direct argv.
- `src/store/workspace.rs` — workspace and apply-intent persistence with CAS.
- `src/store/path.rs` — `worktree_root`, `POLYCODE_DATA_DIR`.
- `src/app/run_service.rs` — `apply_run`, `publish_run`, `discard_run`, `preview_run_diff`.
- `tests/codex_cli.rs` — `native_codex_fixture_runs_through_tmux_preserves_source_then_applies`.

## Gotchas
- Apply requires a fully clean source checkout; preparation does not (the worktree starts from committed `HEAD`).
- Apply never stages or commits; an empty diff is a successful no-op (`No workspace changes to apply.`).
- `pr` rejects runs whose latest verification did not pass, non-completed runs, detached (review) workspaces, empty deltas and repositories without an `origin` remote.
- A Standard/Deep run can be `Completed` and still unapplicable: a failed verification completes the run (the decision's edge to it is optional) precisely so it can be fixed in place; `apply`/`pr` name verification until a later cycle's check passes. A Fast run with a failed verify is `Failed` instead; `retry` the verify stage there. PR failure (no `gh`, not authenticated) is reported in the receipt and never undoes the push.
- `pr` never force-pushes; a diverged remote branch is an error by design. `GIT_TERMINAL_PROMPT=0` turns a credential prompt into an error instead of a hang.
- After `pr` the run stays `Completed`, so apply, fix and discard remain available; publishing again after a fix updates the same branch and PR. The PR body is only written on creation: a fix's fresh draft changes the commit subject but not an already-open PR's text.
- The drafted title is cut at 72 characters; a corrupt latest editing artifact fails the publish (artifact integrity fails closed) rather than silently publishing from the task.
- Discard commits the logical disposition before cleanup; cleanup is idempotent and retains process files. Branch deletion needs persisted ownership and an unchanged tip, otherwise the workspace turns `Broken` and the branch is kept.
- Reclaiming after apply keeps the branch because apply leaves its change unstaged in the source checkout — the branch holds the only commits of that work. The disposition is persisted with the removal intent (ownership is released before `Removing`), so a crash mid-removal still resumes without deleting the branch.
- `Broken` workspace status (moved repo, foreign path, branch collision) blocks execution; Polycode never guesses or deletes foreign data.
- Ordinary run commits and cleanup are rejected while an apply intent is `Prepared` or `AppliedToSource`.
