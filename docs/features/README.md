# Feature Map

This directory tells a coding agent what each user-facing Polycode feature does, how a user reaches it, the exact commands and keys that drive it, where it lives in code, and its known traps. It is not an architecture document; see `../../ARCHITECTURE.md` for that.
Open the relevant feature file before touching or driving that feature.
Every command, flag and key here is copied from `src/cli/mod.rs` and `src/tui/input.rs`; if they disagree with a file below, the code is right and the file is stale.

## Features

| Feature | Purpose | File |
|---|---|---|
| Run lifecycle | Start, inspect, stop, resume, recover, retry and resolve one run | [run-lifecycle.md](run-lifecycle.md) |
| Workflows | Built-in Fast/Standard/Deep/Review graphs plus fix and continue cycles | [workflows.md](workflows.md) |
| Control room | Ratatui TUI: screens, overlays, exact keys | [control-room.md](control-room.md) |
| Workspace | Isolated worktrees, apply, discard, pull request | [workspace.md](workspace.md) |
| Routing | Roles to providers/models, `--provider`, `--profile recommended`, v1 frozen / v2 current | [routing.md](routing.md) |
| Native providers | Claude Code and Codex adapters, permissions, sandboxes, attention | [providers.md](providers.md) |
| Evaluations | `eval list/run/report`, suites under `evals/`, evidence layout | [evaluations.md](evaluations.md) |
| Observability and effort | Usage/latency/prompt-bytes telemetry and `--effort` | [observability-and-effort.md](observability-and-effort.md) |
| Configuration and appearance | Data/config paths and the environment variables the TUI reads | [configuration.md](configuration.md) |
| Install, update, doctor | Bootstrap installer, self-update, environment check | [install-update-doctor.md](install-update-doctor.md) |
| Process supervision | tmux-backed managed processes, `__run-process`, `__exec-process` | [process-supervision.md](process-supervision.md) |
| Release | Tagging, the release gate, PR discipline | [release.md](release.md) |

## Keeping this map current

When code under a feature's "Where it lives" paths changes, update that feature file in the same PR. Add a new file and a table row when a new user-facing feature lands; delete both when one is removed.

A stale entry looks like one of these:

- A command, flag, subcommand or key that `polycode --help`, `src/cli/mod.rs` or `src/tui/input.rs` no longer has, or has under a different name.
- A "Where it lives" path that `ls` cannot find.
- A status name, default value, profile version or suite version that the code no longer uses.
- A gotcha describing a bug that has been fixed.
- A sub-feature that exists in code but has no line here.
