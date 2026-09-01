# Control room (TUI)

Watch and drive every run from one terminal screen without touching the database, Git or tmux yourself.

## Sub-features
- runs-screen: list of runs; hide/unhide runs; Enter opens detail.
- run-detail: stage timeline, routes, attention, usage; `i` toggles the technical (evidence) view.
- viewers: artifact (`o`/Enter, `m` raw/rendered), raw process logs (`l`), workspace diff (`d`).
- composer: `n` opens the new-run form (Task, Workflow, Repository, Execution, Effort).
- actions: resume, stop, retry, attention, apply, publish, fix, continue, follow-ups, discard, all through one serialized worker.
- update-prompt: an overlay offering to install a newer official release; lowest priority overlay.
- help: `?` overlay listing every key.

## How to get to it (user POV)
Run `polycode` with no arguments in an interactive terminal, or `polycode tui`. Non-interactive stdin/stdout prints CLI help instead. The TUI starts on the Runs screen with the current directory as the composer's repository. Quitting (`q`, Ctrl-C) detaches the frontend only; tmux-owned providers keep running.

## Driving it
```bash
polycode
polycode tui
```
Global: `↑`/`↓` or `j`/`k` navigate, `PageUp`/`PageDown` scroll by 10, `Home`/`End` top/bottom in viewers, `Enter` open/confirm, `Esc` back/close, `n` new run, `R` runs screen, `x` dismiss notification, `?` help, `q` or `Ctrl-C` quit/detach.
Run detail: `Enter`/`o` open selected stage artifact, `r` resume/recover, `s` stop, `t` retry selected failed stage, `u` attention overlay, `l` raw logs, `d` workspace diff, `a` apply (Enter confirms), `P` pull request (Enter confirms), `X` discard (Enter confirms), `f` fix, `c` continue, `w` follow-ups, `i` technical details.
Runs list: `h` hide/unhide selected run, `H` show/hide hidden runs.
Artifact viewer: `m` toggle raw/rendered Markdown.
Composer: `Tab`/`Shift-Tab` move fields, `←`/`→` cycle Workflow and Execution, typing/paste edits Task and Repository, `Enter` submits, `Esc` back.
Update overlay: `↑`/`↓` toggle Yes/No, `Enter` confirm, `Esc` dismiss for this process.

## Where it lives
- `src/tui/input.rs` — `map_key` / `map_text_key`: the only key-to-intent tables.
- `src/tui/app.rs` — `handle_intent`, overlay handlers, composer submit, eligibility messages (`stop_unavailable_reason`, `fix_unavailable_reason`, `continue_unavailable_reason`).
- `src/tui/state.rs` — `Screen`, `Overlay`, `NewRunForm`, `ExecutionChoice`, `EffortChoice`, `CONCURRENT_AGENTS` (4).
- `src/tui/render.rs` — rendering incl. the help overlay text.
- `src/tui/worker.rs` — `WorkerCommand` enum; one standard thread serializes all mutations.
- `src/tui/terminal.rs` — raw mode / alternate screen RAII and panic restore.
- `src/tui/theme.rs`, `src/tui/motion.rs` — appearance (see configuration.md).
- `src/lib.rs` — `frontend_mode`: no command + interactive terminal opens the TUI.
- `tests/cli.rs` — `no_args_non_tty_prints_help_and_explicit_tui_fails_without_control_sequences`.

## Gotchas
- Text mode (Task/Repository fields, attention and continue overlays) uses `map_text_key`: letters are input, not commands. `q` does not quit there; Ctrl-C still does.
- The composer's Effort field (focus 4) is never cycled: `handle_new_run_intent` only routes ←/→ to `cycle_value` for focus 1 and 3, and `active_text_mut` returns None for focus 4. Effort stays at Native default in the TUI until that dispatch is fixed. `--effort` on the CLI works.
- Quit never interrupts, discards or applies anything; reopen and press `r` to reconcile durable state and consume retained output.
- Read paths are side-effect free except the 30 s abandoned-run observe pass; they never acknowledge provider output or create apply intent.
- The TUI caps concurrently working agents at 4 (`CONCURRENT_AGENTS`); a booked fix that cannot start yet stays booked silently. The CLI has no such cap.
- Attention overlays outrank the update overlay; the update prompt is shown at most once per process.
- Stop dispatches without confirmation; apply, publish and discard require Enter in a confirmation overlay.
- The README key table omits `s`, `f`, `c`, `w`, `h`, `H`, `x`, `m`, `i`; the help overlay in `render.rs` is complete.
