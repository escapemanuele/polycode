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
- run-binding: a run bound to a package (`mission start` or `mission attach`) cannot be deleted (`AppError`/`StoreError::RunBoundToMission`) while the binding stands.
- decisions: `mission decide` records one design decision (title, rationale, author `user`|`lead`); insert-only, quoted into every later package's handoff.
- attention: `mission show`'s "Needs you" section lists every package that is `Blocked` (with its run's pending attention summary), `Failed` (with its reason), or `Delivered` and awaiting `mission integrate`.
- no-tui-screen: missions have no TUI screen yet; drive them from the CLI only.

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
polycode mission cancel-package <mission-id> <package-id> [--reason "<reason>"]
polycode mission decide <mission-id> "<title>" --why "<rationale>" [--by user|lead]
polycode mission complete <mission-id>
polycode mission cancel <mission-id> [--reason "<reason>"]
```
Every command prints the mission after the change: status, repository, goal, packages in dependency order with status/goal/dependencies/current run/reason, decisions, and a "Needs you" section (or "Nothing needs you."). `mission start` additionally prints the child run's own report first, the same shape `polycode fast` prints. `mission list` prints one line per mission (`<id>  <status>  <integrated>/<packages> integrated  <active> active  <attention> need you  <title>`), or `No missions yet.`.

## Where it lives
- `src/domain/mission.rs` — `Mission` aggregate, `WorkPackage`, `WorkPackageContract`, `WorkPackageStatus`, `MissionStatus`, `DecisionAuthor`, `MissionAttention`, `MissionError`, `IntegrationEvidence`.
- `src/store/mission.rs` — persistence for missions, packages, decisions, and the run-to-package binding.
- `src/store/migrations.rs` — `migrate_v10` creates the mission schema.
- `src/app/mission_service.rs` — `MissionService` use cases (`create_mission`, `add_package`, `revise_package`, `set_dependencies`, `start_package`, `attach_run`, `integrate_package`, `retry_package`, `cancel_package`, `record_decision`, `complete_mission`, `cancel_mission`), `handoff_task`, the observe pass (`observe_runs`).
- `src/app/mission_query.rs` — `MissionDetails`, `MissionListItem`, `WorkPackageSummary`, `DecisionSummary` read models.
- `src/cli/mod.rs` — `MissionCommand`, `ContractArgs`, `ReviseArgs`.
- `src/cli/commands.rs` — `mission` dispatch, `print_mission`, `print_mission_list`, `build_contract`, `revise_contract`, `parse_workflow`, `parse_decision_author`.

## Gotchas
- `mission start` drives its child run to quiescence in the foreground exactly like `polycode fast`/`standard`/`deep`/`review` does; it is not fire-and-forget, and if the run started but could not be bound (a crash between the two), the error names the run id so `polycode mission attach <mission> <package> <run-id>` can bind it by hand.
- `mission cancel` and `mission cancel-package` refuse while the package has a live run (`Running` or `Blocked`, which includes a stopped run) — `polycode discard <run-id>` first; the package then reads `Failed` and can be cancelled.
- Cancelling a package other packages still depend on is refused; cancel or integrate the dependents first, or cancel the whole mission.
- `mission integrate` after `polycode apply <run-id>` when the run carries a real change, or straight away when the run `Completed` with nothing to change; any other `Completed` run (a worktree delta still unapplied) is refused.
- A package's contract and dependencies can only be revised while nothing has run for it (`Planned`, `Ready`, or `Failed`); once a run is bound, `mission revise`/`mission depends` are refused and the package must be retried or cancelled instead.
- `mission attach` requires the run to belong to the mission's own repository; a run started against a different checkout is refused with a repository mismatch.
- A run bound to a mission package cannot be deleted while the binding stands; integrate or cancel the package first.
