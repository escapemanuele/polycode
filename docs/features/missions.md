# Missions

Plan a project goal above individual runs, break it into work packages with dependencies, and drive each one as a child run.

## Sub-features
- mission-lifecycle: Planning -> Active -> {Completed, Cancelled}. A mission starts `Planning` and moves to `Active` the moment its first package starts; `Completed` and `Cancelled` close it, and no package moves afterwards.
- package-lifecycle: Planned -> Ready -> Running -> {Blocked, Failed} -> Delivered -> Integrated, or Cancelled from Planned/Ready/Failed. `Ready` means every dependency is `Integrated`, so a run started for the package sees their changes in its base; `Delivered` means the child run completed; `Integrated` means its change reached the source checkout.
- readiness: a package becomes `Ready` only once every package it depends on is `Integrated`. Adding, revising or re-pointing dependencies can only touch a package nothing has run for yet.
- frozen-contract: a package's contract (title, goal, rationale, scope, acceptance criteria, verification, workflow) is free to revise while `Planned`, `Ready` or `Failed`; once a run has served it, the contract is frozen and `mission revise` is refused.
- observe-pass: `mission show` and every mutating command first maps each running package's bound run to committed run status before doing anything else: `NeedsUser`, `Paused` or `Interrupted` -> `Blocked` (with the pending request, or a note to resume or discard), `Completed`/`Applied` -> `Delivered`, `Failed`/`Discarded` -> `Failed` (with the run's failure reason); a run is never driven by a read.
- handoff: the task a child run receives is rendered from mission state, not typed by hand — the package's goal, rationale, scope, acceptance criteria, verification, its integrated dependencies' contracts, and every recorded decision (`handoff_task` in `src/app/mission_service.rs`).
- integration-evidence: `mission integrate` accepts a package only once its run's committed evidence supports it: `IntegrationEvidence::Applied` (the run was applied to the source checkout) or `IntegrationEvidence::NoChanges` (the run `Completed` with an empty diff against its base, so nothing needed transferring, and integrates straight away without a prior `apply`). A `Completed` run that still has a worktree delta is refused until `polycode apply` runs.
- handoff-record: the moment a started package's run is persisted, a `mission_handoffs` row binds it to the mission state its task was rendered from — the contract's hash, the task's hash and size, the dependencies and decisions it named — in the same transaction as the binding. `mission show` prints nothing for it unless the contract no longer hashes to what the run was given; a run attached by hand has no record.
- delivery-result: when a package's run is first seen `Completed`/`Applied`, its evidence is captured from the run store and kept with the package (`WorkPackageResult`): changed files against the base (bounded to 200, marked incomplete past that), the latest verify stage's status, every review stage's status, the latest decision's status, and the newest editing stage's, each review's and the decision's own `## Bottom line` quoted verbatim, plus the decision's `## Follow-ups` verbatim. Nothing is summarized. It survives the worktree being released.
- rework: `mission fix` / `mission continue` grow the delivered package's own run by a fix or continue cycle (exactly `polycode fix` and the TUI's `c`); the package reads in progress again on the same run and is delivered afresh, with a new result, when the cycle completes. A cycle started outside the mission (`polycode fix <run>`) is noticed too: a delivered run with more stages than its result covers is re-delivered on the next observe pass.
- resume: `mission resume` calls `polycode resume` for every package run that is prepared, running, paused, or interrupted, then observes: the fan-in for packages started on native providers, whose runs outlive the command that started them (`mission start` returns when the provider is at work). Several ready packages may be started one after another.
- run-binding: a run bound to a package (`mission start` or `mission attach`) cannot be deleted (`AppError`/`StoreError::RunBoundToMission`) while the binding stands.
- decisions: `mission decide` records one design decision (title, rationale, author `user`|`lead`); insert-only, quoted into every later package's handoff.
- attention: `mission show`'s "Needs you" section lists every package that is `Blocked` (with its run's pending attention summary), `Failed` (with its reason), or `Delivered` and awaiting `mission integrate`.
- tui: the control room's `M` screen lists missions and opens one; from there `S` starts a ready package, `I` integrates a delivered one, and Enter opens a package's run (see control-room.md). Planning commands stay CLI-only.

## How to get to it (user POV)
Create a mission over a Git checkout with `polycode mission new`, add work packages with their dependencies, and start a ready package with `polycode mission start`; that drives its child run to quiescence in the foreground, exactly like `polycode fast` would, then reports the mission. Once a package's run is `Applied` (or completed with nothing to apply), run `polycode mission integrate` to mark it delivered into the checkout, which readies any package that depended on it. `polycode mission show` at any point prints every package, its current run, recorded decisions, and what needs you.

## Driving it
```bash
polycode mission new "<title>" --goal "<goal>" [--repo <path>]
polycode mission list
polycode mission show <mission-id>
polycode mission add <mission-id> <package-id> --title "<title>" --goal "<goal>" [--why "<rationale>"] [--scope "<scope>"] [--accept "<criterion>" ...] [--verify "<verification>"] [--workflow fast|standard|deep|review] [--depends-on <package-id> ...]
polycode mission revise <mission-id> <package-id> [--title "<title>"] [--goal "<goal>"] [--why "<rationale>"] [--scope "<scope>"] [--accept "<criterion>" ...] [--verify "<verification>"] [--workflow fast|standard|deep|review]
polycode mission depends <mission-id> <package-id> [--on <package-id> ...]
polycode mission start <mission-id> <package-id> [--provider claude|codex|fake | --profile recommended] [--effort native|low|medium|high|xhigh]
polycode mission attach <mission-id> <package-id> <run-id>
polycode mission integrate <mission-id> <package-id>
polycode mission retry <mission-id> <package-id>
polycode mission fix <mission-id> <package-id>
polycode mission continue <mission-id> <package-id> "<instruction>"
polycode mission resume <mission-id>
polycode mission cancel-package <mission-id> <package-id> [--reason "<reason>"]
polycode mission decide <mission-id> "<title>" --why "<rationale>" [--by user|lead]
polycode mission complete <mission-id>
polycode mission cancel <mission-id> [--reason "<reason>"]
```
Every command prints the mission after the change: status, repository, goal, packages in dependency order with status/goal/dependencies/current run/reason, a delivered package's evidence line (`delivered: <n> file(s) changed; verify <status>; <review statuses>`, then `said:`/`decision:` quoting the artifacts' own bottom lines), decisions, and a "Needs you" section (or "Nothing needs you."). `mission start` additionally prints the child run's own report first, the same shape `polycode fast` prints. `mission list` prints one line per mission (`<id>  <status>  <integrated>/<packages> integrated  <active> active  <attention> need you  <title>`), or `No missions yet.`.

## Where it lives
- `src/domain/mission.rs` — `Mission` aggregate, `WorkPackage`, `WorkPackageContract`, `WorkPackageStatus`, `MissionStatus`, `DecisionAuthor`, `MissionAttention`, `MissionError`, `IntegrationEvidence`.
- `src/store/mission.rs` — persistence for missions, packages, decisions, and the run-to-package binding.
- `src/store/migrations.rs` — `migrate_v10` creates the mission schema.
- `src/app/mission_service.rs` — `MissionService` use cases (`create_mission`, `add_package`, `revise_package`, `set_dependencies`, `start_package`, `attach_run`, `integrate_package`, `rework_package`, `resume_mission`, `retry_package`, `cancel_package`, `record_decision`, `complete_mission`, `cancel_mission`), `handoff_task`, the observe pass (`observe_runs`).
- `src/app/mission_query.rs` — `MissionDetails`, `MissionListItem`, `WorkPackageSummary`, `DecisionSummary`, `HandoffSummary` read models.
- `src/app/mission_result.rs` — `capture`: the delivery result read from the run store; `preview_delta`: the same bounded delta apply would move.
- `src/store/mission.rs` — `MissionHandoffRecord`, `commit_mission_update_with`, `list_mission_handoffs`, `contract_sha256`; `migrate_v11` creates `mission_handoffs`.
- `src/cli/mod.rs` — `MissionCommand`, `ContractArgs`, `ReviseArgs`.
- `src/cli/commands.rs` — `mission` dispatch, `print_mission`, `print_mission_list`, `build_contract`, `revise_contract`, `parse_workflow`, `parse_decision_author`.
- `src/tui/render.rs` — `render_missions`, `render_mission_detail`; `src/tui/app.rs` — `start_selected_package`, `open_integrate_confirmation`, `refresh_missions`.

## Gotchas
- `mission fix` needs what `polycode fix` needs: a `Completed` run with a decision stage. A `fast` package's run has none, so the fix is refused by the run and the package stays delivered; use `mission continue` on a `standard`/`deep` package, or retry the package on a new run.
- The result's changed-file list is read only while the run's worktree is `Ready`; a run whose apply found nothing has no worktree and lists nothing, which is also the truth.
- A corrupt artifact fails the delivery capture (and so the `show`/mutation that triggered it) rather than leaving a hole in a result that is kept for good; a stage that wrote no artifact simply quotes nothing. A rework that fails clears the previous result: a `Failed` package shows its reason, never an old delivery.
- `mission start` drives its child run to quiescence in the foreground exactly like `polycode fast`/`standard`/`deep`/`review` does; it is not fire-and-forget, and if the run started but could not be bound (a crash between the two), the error names the run id so `polycode mission attach <mission> <package> <run-id>` can bind it by hand.
- `mission cancel` and `mission cancel-package` refuse while the package has a live run (`Running` or `Blocked`, which includes a stopped run) — `polycode discard <run-id>` first; the package then reads `Failed` and can be cancelled.
- Cancelling a package other packages still depend on is refused; cancel or integrate the dependents first, or cancel the whole mission.
- `mission integrate` after `polycode apply <run-id>` when the run carries a real change, or straight away when the run `Completed` with nothing to change; any other `Completed` run (a worktree delta still unapplied) is refused.
- A package's contract and dependencies can only be revised while nothing has run for it (`Planned`, `Ready`, or `Failed`); once a run is bound, `mission revise`/`mission depends` are refused and the package must be retried or cancelled instead.
- `mission attach` requires the run to belong to the mission's own repository; a run started against a different checkout is refused with a repository mismatch.
- A run bound to a mission package cannot be deleted while the binding stands; integrate or cancel the package first.
