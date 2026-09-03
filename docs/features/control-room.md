# Control room (TUI)

Watch and drive every run from one terminal screen without touching the database, Git or tmux yourself.

## Sub-features
- runs-screen: list of runs; archive/unarchive runs; delete an archived run for good; Enter opens detail.
- run-detail: stage timeline, routes, attention, usage; `i` toggles the technical (evidence) view. Opening a failed run lands the selection on the blocking failed stage, whose hero carries a `WHY IT FAILED` section with the whole (sanitized, 200-char capped) provider reason; the Runs-screen overview shows the same section for a failed run before it is opened.
- activity-strip: one prose sentence for what is happening now — a failed stage's own reason ("Implementation failed: compile failed"), or why a stage has not started ("Waiting on: Architecture", "Blocked: Quality review failed, Spec review was skipped", "Waiting on you", "Stage suspended", "Stage skipped by the workflow").
- viewers: artifact (`o`/Enter, `m` raw/rendered), raw process logs (`l`), workspace diff (`d`).
- composer: `n` opens the new-run form (Task, Workflow, Repository, Execution, Effort).
- actions: resume, stop, retry, attention, apply, publish, fix, continue, follow-ups, discard, delete, all through one serialized worker.
- update-prompt: an overlay offering to install a newer official release; lowest priority overlay.
- help: `?` overlay listing every key.

## How to get to it (user POV)
Run `polycode` with no arguments in an interactive terminal, or `polycode tui`. Non-interactive stdin/stdout prints CLI help instead. The TUI starts on the Runs screen with the current directory as the composer's repository. Quitting (`q`, Ctrl-C) stops the runs this session shows as `Running` before it closes, reporting "Stopping N agents…" while it waits.

## Driving it
```bash
polycode
polycode tui
```
Global: `↑`/`↓` or `j`/`k` navigate, `PageUp`/`PageDown` scroll by 10, `Home`/`End` top/bottom in viewers, `Enter` open/confirm, `Esc` back/close, `n` new run, `R` runs screen, `x` dismiss notification, `?` help, `q` or `Ctrl-C` quit/detach.
Run detail: `Enter`/`o` open selected stage artifact, `r` resume/recover, `s` stop, `t` retry selected failed stage (chooser: Configured provider / Claude / Codex, Enter retries), `u` attention overlay, `l` raw logs, `d` workspace diff, `a` apply (Enter confirms), `P` pull request (Enter confirms), `X` discard (Enter confirms), `f` fix, `c` continue, `w` follow-ups, `i` technical details.
Runs list: `h` archive/unarchive selected run, `H` show/hide archived runs, `D` delete an archived run for good (POD stands at the plunger; a second `D` goes through, Esc cancels). Only an archived run offers `D`.
Artifact viewer: `m` toggle raw/rendered Markdown.
Composer: `Tab`/`Shift-Tab` move fields, `←`/`→` cycle Workflow, Execution and Effort, typing/paste edits Task and Repository, `Enter` submits, `Esc` back.
Update overlay: `↑`/`↓` toggle Yes/No, `Enter` confirm, `Esc` dismiss for this process.

## Where it lives
- `src/tui/input.rs` — `map_key` / `map_text_key`: the only key-to-intent tables.
- `src/tui/app.rs` — `handle_intent`, overlay handlers, composer submit, eligibility messages (`stop_unavailable_reason`, `fix_unavailable_reason`, `continue_unavailable_reason`).
- `src/tui/state.rs` — `Screen`, `Overlay`, `NewRunForm`, `ExecutionChoice`, `EFFORT_CHOICES`/`effort_label`, `CONCURRENT_AGENTS` (4).
- `src/tui/render.rs` — rendering incl. the help overlay text; `status_sentences`, `waiting_message`, `blocked_message` compose the activity strip; `failed_stage_reason` / `failure_reason_lines` render the failure block in the hero and the Runs overview.
- `src/tui/state.rs` — `focus_blocking_failure` moves the selection onto the blocking failed stage when a run is opened from the Runs screen.
- `src/tui/worker.rs` — `WorkerCommand` enum; one standard thread serializes all mutations.
- `src/tui/terminal.rs` — raw mode / alternate screen RAII and panic restore.
- `src/tui/theme.rs`, `src/tui/motion.rs` — appearance (see configuration.md).
- `src/lib.rs` — `frontend_mode`: no command + interactive terminal opens the TUI.
- `tests/cli.rs` — `no_args_non_tty_prints_help_and_explicit_tui_fails_without_control_sequences`.

## Gotchas
- Text mode (Task/Repository fields, attention and continue overlays) uses `map_text_key`: letters are input, not commands. `q` does not quit there; Ctrl-C still does.
- Quit stops running runs but never discards or applies anything; a stopped run resumes. Quitting with agents at work therefore takes as long as their stops do, one after another. Reopen and press `r` to reconcile durable state and consume retained output.
- Startup sweeps the worktrees of `Applied` and `Discarded` runs on a background thread; nothing reports it, and a `Completed` run's worktree is never touched.
- Read paths are side-effect free except the 30 s abandoned-run observe pass; they never acknowledge provider output or create apply intent.
- The TUI caps concurrently working agents at 4 (`CONCURRENT_AGENTS`); a booked fix that cannot start yet stays booked silently. The CLI has no such cap.
- Attention overlays outrank the update overlay; the update prompt is shown at most once per process.
- The activity strip is width-bounded: a long provider reason is cut with an ellipsis, and the prefix naming the stage always survives the cut. The full text is in the failed stage's hero (`WHY IT FAILED`), the Runs-screen overview, the logs (`l`) and `polycode status`.
- Stop dispatches without confirmation; apply, publish and discard require Enter in a confirmation overlay.
