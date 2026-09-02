# Run lifecycle

Start one task as a run, watch it move through its stages, and get it moving again when it stops for you, for a failure, or for a crash.

## Sub-features
- start: `fast`/`standard`/`deep`/`review` create a run, prepare its workspace, and drive it to the first quiescent state.
- statuses: Created -> Preparing -> Ready -> Running -> {NeedsUser, Paused, Interrupted, Completed, Failed} -> {Applied, Discarded}. In Standard/Deep a failed verify stage still yields `Completed` (its edge into the decision is optional) and apply is gated separately; in Fast it is a leaf and the run is `Failed`. See verification.md.
- ready-boundary: `Ready` is a persisted atomic boundary; configuration and workspace succeeded but nothing executed yet.
- resume-vs-recover: `Paused` (user asked) takes Resume; `Interrupted` (process or runtime lost) takes Recover. One command, `resume`, does both.
- stop: interrupts live managed processes, then commits a run-level `Interrupted`; nothing is discarded.
- retry: returns one `Failed` stage to `Pending`, together with every downstream stage its failure skipped, only while every other downstream stage is still `Pending` or `Ready`.
- attention: `NeedsUser` holds one or more attention requests (permission, decision, question) that `resolve` answers.
- inspect: `runs` lists runs; `status` prints routing, per-stage evidence, attention and usage.
- diagnosis: every stage that stopped or has not started says why. A failed stage prints `reason: <provider text>`; a Pending/Ready stage prints `waiting on: <ids>` (plus `(degraded: <ids>)` for satisfied optional edges) or `blocked by: <id> (failed|skipped)`. The run-level reason is the blocking stage's, and the TUI's activity strip says the same in prose.

## How to get to it (user POV)
From a Git checkout, run one workflow command with the task text. The command prints committed events and the run's details, then exits when the run is quiescent (completed, needs you, paused, interrupted, failed, or waiting for a provider). Use `polycode runs` to find the run id and `polycode status <run-id>` to inspect it. In the TUI the same actions are `r` (resume/recover), `s` (stop), `t` (retry the selected failed stage) and `u` (resolve attention) on the run detail screen.

## Driving it
```bash
polycode fast "<task>" [--repo <path>] [--provider claude|codex|fake | --profile recommended] [--effort native|low|medium|high]
polycode standard "<task>" [same flags]
polycode deep "<task>" [same flags]
polycode review "<task>" [same flags]
polycode runs
polycode status <run-id>              # failed stages add `reason:`; pending ones add `waiting on:` / `blocked by:`
polycode resume <run-id>
polycode stop <run-id>
polycode retry <run-id> <stage-id>
polycode resolve <run-id> <attention-id>                    # approve a permission request
polycode resolve <run-id> <attention-id> --response "<answer>"   # answer a question
```
TUI keys on the run detail screen: `r` resume/recover, `s` stop, `t` retry selected failed stage, `u` open attention overlay (↑/↓ pick request, type a response, Enter resolves).

## Where it lives
- `src/cli/mod.rs` — clap definitions (`RunArgs`, `Command::{Runs,Status,Resume,Stop,Retry,Resolve}`).
- `src/cli/commands.rs` — `start`, `parse_effort`, `print_report`, `print_details`; `QuiescentState` hints printed after each report.
- `src/app/run_service.rs` — `start_run`, `resume_run`, `stop_run`, `retry_stage`, `resolve_attention_with_response`, `inspect_run`, `list_runs`; `ABANDONED_AFTER` 30 s observe pass.
- `src/domain/run.rs` — `Run` aggregate, `RunTransition`, `ensure_retry_safe` (`RetryWouldInvalidate`), `skipped_descendants`.
- `src/engine/scheduler.rs` — `retry_stage` returns the skipped descendants to `Pending` in the same commit.
- `src/domain/stage.rs` — stage state machine.
- `src/domain/attention.rs` — attention request lifecycle.
- `src/app/query.rs` — `RunDetails`, `StageSummary`, `AttentionSummary` DTOs behind `status`; `failure_reason`, `blocking`, `StageWaitingSummary`, `StageDependencyRef`, `BlockedDependencyRef`.
- `src/cli/commands.rs` — `waiting_line`, `dependency_ids`, `blocked_ids`, `outcome_word`: the one extra indented line per stage.
- `src/tui/render.rs` — `status_sentences`, `failed_stage_sentence`, `hero_activity_text`, `activity_message`, `waiting_message`, `blocked_message`: the same diagnosis as prose in the activity strip.
- `tests/cli.rs` — restart survival, default profile, read-only `runs`/`status`.
- `tests/codex_cli.rs` — `stop_interrupts_a_live_run_while_its_driver_is_still_attached`, detach + resume consuming retained output, retry creating a new native thread.

## Gotchas
- Blocked quiescent states (`needs_user`, paused, interrupted, failed) exit 0; only operational errors exit 1 and clap errors exit 2. Do not treat exit 0 as "completed".
- `resume` never bypasses attention and never retries a failed stage; use `resolve` or `retry` explicitly.
- Retry is rejected once any downstream stage, direct or transitive, has started or reached an outcome other than `Skipped`; the error is `RetryWouldInvalidate`. Skipped stages return to `Pending` with the retried one, so a Fast implementation whose failure skipped `verify` can still be retried.
- `stop` is refused unless the run is `Running` or `NeedsUser`; an `Interrupted` run reports its existing state instead of a second interruption.
- `stop` cannot interrupt a verify stage mid-way: its commands are not managed processes, the synchronous poll runs them to the end, and the driver's commit then loses to the stop; `resume` reconciles the state afterwards.
- `stop` is retried on lost-revision races because another Polycode process is usually still driving the run; the classifier is `AppError::is_concurrent_modification`, which asks each wrapper. `#[error(transparent)]` makes `.source()` skip the wrapper level, so walking the source chain silently missed the store error and 7 to 20 percent of stops failed under load. Add explicit `is_retryable`-style methods to wrappers; never rely on `.source()` through transparent errors.
- A run reading `Running` whose processes all ended and nothing touched for 30 s is settled by a read (`runs`, `status`, TUI refresh) through `ResumeAction::Observe`; a read never resumes provider work.
- Pre-M5 runs are inspectable but cannot resume when immutable input or execution config is absent (`<legacy input unavailable>` in `status`).
- Routes are resolved once at creation; provider loss after creation fails the stage with configured-provider-unavailable, never reroutes.
- The run-level failure reason is the *blocking* stage's, not the first failed one in workflow order. A failed optional dependency that nothing required (a review beside `synthesis`) never becomes the run's reason; `StageSummary::blocking` marks the one that does.
- `blocked by` states each dependency's own outcome: a dependency that was skipped is reported as skipped, never as failed.
- `waiting on` is printed only for a `Pending` or `Ready` stage. A stage whose dependencies are all satisfied prints nothing extra, even in the moment before the scheduler marks it Ready.
