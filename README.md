# Polycode

> One task. Multiple coding models. Each doing what it does best.

Polycode is a local-first terminal orchestrator for native coding-agent CLIs. It is designed to coordinate agents such as Claude Code, Codex CLI, and Gemini CLI as specialized engineering roles without replacing their existing authentication or execution model.

Polycode is early-stage software. This repository currently contains **Milestone 6: managed external-process infrastructure**: validated domain state, synchronous restart-safe SQLite persistence, crash-reconcilable isolated Git worktrees, data-driven DAG scheduling, deterministic `FakeProvider` execution, end-to-end run controls, and a shell-safe `ProcessBackend` with real tmux supervision. Real coding-agent adapters and UI remain future work.

## Principles

- Native coding-agent CLIs remain first-class providers.
- Role, provider, and model are separate concepts.
- Workflow behavior is explicit and testable.
- Machine state is canonical in local SQLite; artifacts remain human-readable.
- Every implementation run uses an isolated Git worktree and requires explicit apply.
- Defaults should be useful without turning Polycode into a generic agent framework.

## Current CLI

```console
polycode --help
polycode --version
polycode doctor
polycode fast "Fix the parser" --provider fake
polycode standard "Add export support" --repo /path/to/repo --provider fake
polycode deep "Redesign authentication" --provider fake
polycode review "Review the error boundary" --provider fake
polycode runs
polycode status <run-id>
polycode resume <run-id>
polycode resolve <run-id> <attention-id>
polycode retry <run-id> <stage-id>
polycode apply <run-id>
polycode discard <run-id>
```

Workflow commands use current directory unless `--repo` is supplied. Milestone 5 requires explicit `--provider fake`; no production provider is implied. Default fake scenario emits deterministic start, progress, usage, and completion signals for every graph stage. It does not edit files.

`resume`, `resolve`, and `retry` reconcile persisted workspace first, reconstruct provider from immutable configuration, then execute to next quiescent condition. `resume` never bypasses attention or retries failure. `resolve` and `retry` perform requested explicit transition then continue. `apply` transfers actual completed workspace changes; empty diff is successful no-op. `discard` records logical disposition before owned-resource cleanup.

Normal blocked states (`needs_user`, paused, interrupted, failed) exit successfully because command completed and durable state remains inspectable. Operational failures exit 1; Clap argument errors exit 2. CLI progress comes only from committed semantic events.

`doctor` reports paths/schema and tmux availability without creating missing database. `runs` also leaves missing database untouched and uses indexed projections plus immutable input/workspace joins. Public application, domain, execution, process, persistence, Git, and workspace APIs are available under `polycode::app`, `polycode::domain`, `polycode::engine`, `polycode::process`, `polycode::store`, `polycode::git`, and `polycode::workspace`.

## Build

Prerequisites:

- Rust stable 1.85 or newer.
- tmux on macOS or Linux for managed external processes and process integration tests.

```bash
cargo build
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Enable diagnostics with standard `RUST_LOG` directives:

```bash
RUST_LOG=polycode=debug cargo run -- doctor
```

## Configuration

Default user configuration path:

```text
~/.config/polycode/config.toml
```

Resolution order:

1. `$POLYCODE_CONFIG_DIR/config.toml`
2. `$XDG_CONFIG_HOME/polycode/config.toml`
3. `$HOME/.config/polycode/config.toml`

Repository overrides will eventually live at `<repo>/.polycode.toml`. Current code resolves paths but does not read or create configuration files.

Default SQLite path:

```text
~/.polycode/polycode.db
```

Set `POLYCODE_DATA_DIR` to override its parent directory. Path resolution is side-effect free; opening the store creates the directory/database and applies schema migrations. SQLite stores immutable user input and resolved configuration separately from versioned run snapshots and semantic events. New tasks are outer-trimmed, preserve Unicode and line breaks, and cannot be updated or deleted. Pre-M5 runs remain inspectable but cannot resume through CLI when immutable input or reconstructible execution configuration is absent.

Managed worktrees default to:

```text
~/.polycode/worktrees/<sanitized-repository>-<common-dir-hash>/<run-id>
```

`POLYCODE_DATA_DIR` relocates database, managed worktree root, and managed process data. Implementation workflows use deterministic `polycode/run-<run-id>` branches; review workflows use detached worktrees. Source changes occur only through explicit apply, which requires a clean source checkout, generates a binary patch from persisted base commit, runs `git apply --check`, then applies without staging or committing.

Managed process attempts use separately persisted infrastructure below future provider adapters. Each attempt gets a durable directory:

```text
~/.polycode/runs/<run-id>/processes/<process-id>/
├── spec.json
├── runtime.json
├── stdout.log
├── stderr.log
└── exit.json
```

Tmux launches Polycode's hidden runner with an exact argument vector, never a shell command string. Runner redirects provider stdout/stderr to regular append-only files, so CLI/TUI detachment cannot break provider output. SQLite owns acknowledged byte offsets; reads do not advance them. Exit and runtime evidence are identity-checked against immutable launch fingerprint. This infrastructure is public for future provider adapters but has no user-facing process commands in M6.

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) and [LEGACY_BEHAVIOR.md](LEGACY_BEHAVIOR.md). Real-provider and UI descriptions remain future design constraints, not claims of implemented behavior.

## License

Licensed under either Apache License 2.0 or MIT license, at your option.
