# Polycode Architecture

## Status

Milestone 0 implements only the executable shell: CLI parsing, tracing initialization, and configuration path resolution. Domain state, workflows, providers, process backends, persistence, Git worktrees, and TUI are deliberately absent.

Legacy `agents-v3.0.0` was inspected after bootstrap. [LEGACY_BEHAVIOR.md](LEGACY_BEHAVIOR.md) records its behavioral contract, recovery edge cases, and intentional architectural departures.

## Product boundary

Polycode orchestrates native coding-agent CLIs. A provider represents a local CLI and its native authentication/session behavior; it does not imply direct use of a vendor API.

These concepts remain independent:

```text
workflow stage -> engineering role -> provider -> model
```

Workflow definitions depend on roles. A profile resolver will later select a provider/model and role-specific fallbacks without embedding model names in workflow semantics.

## Planned system boundaries

```text
CLI / TUI
    |
run manager
    |
workflow engine ---- domain state/events ---- SQLite store
    |
provider interface
    |
process backend (tmux first, native supervisor later)
    |
native coding-agent CLI inside isolated Git worktree
```

Human-readable artifacts, JSONL logs, and conversations complement SQLite. They never replace canonical machine state.

## State and events

Runs, stages, attention requests, provider sessions, artifacts, and usage will use explicit typed state. Important changes will emit common semantic events. Persistence, scheduling, UI, notification, and journals consume those events instead of independently inferring state from files or provider-specific JSON.

Milestone 1 will define and test legal state transitions. This document intentionally does not preempt that domain design with placeholder types.

## Workflow execution

Built-in workflows will be Rust data, scheduled as dependency DAGs rather than hard-coded procedural sequences. Stage definitions will carry role, dependencies, optional dependencies, artifact type, retry policy, and fallback policy as each concept becomes executable.

FakeProvider is the first provider. Core scheduling, interruption, attention, recovery, and concurrency must work deterministically without installed agents, network access, or subscriptions.

## Process and recovery

Provider logic will depend on a process-backend interface, not tmux. TmuxBackend remains the first concrete implementation. Provider output must be consumed and persisted independently of attached clients so terminal or TUI disconnection cannot cause broken pipes or lost events.

SQLite will be canonical for resumable state. Completed stages will not be inferred from artifact presence and will not be silently repeated after restart.

Each run also snapshots its resolved configuration. Resume must use that immutable snapshot rather than silently adopting later user or repository configuration changes.

## Git safety

Every run will execute in an isolated worktree. Implementation runs use dedicated branches; review-only runs may be detached. Applying changes is explicit, validates source-checkout safety and patch applicability, preserves untracked run files, avoids automatic commits, and retains run data after apply or discard.

## Milestone 0 layout

```text
src/
├── main.rs          process entry and tracing initialization
├── cli/
│   ├── mod.rs       CLI schema
│   └── commands.rs  command dispatch and bootstrap responses
└── config/
    └── mod.rs       side-effect-free configuration path resolution
```

Modules are added only when behavior needs them. Initial dependencies are limited to Clap, anyhow, tracing, and tracing-subscriber.

## Decisions

- Single Cargo package and binary; no workspace.
- Rust 2024 edition with Rust 1.85 minimum, matching stable edition support.
- Configuration lookup uses standard environment variables and no platform-directory crate because required location is explicitly `~/.config/polycode`.
- Missing subcommand prints help until the runs TUI exists.
- No configuration file is created during bootstrap.
- Internal domain/database schema must not depend on provisional branding beyond filesystem and package identity where unavoidable.
- Legacy user-requested stop has meaning distinct from process interruption. Milestone 1 should model `Paused` explicitly unless transition tests demonstrate a simpler lossless representation.
