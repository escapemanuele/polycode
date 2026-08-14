# Polycode

> One task. Multiple coding models. Each doing what it does best.

Polycode is a local-first terminal orchestrator for native coding-agent CLIs. It is designed to coordinate agents such as Claude Code, Codex CLI, and Gemini CLI as specialized engineering roles without replacing their existing authentication or execution model.

Polycode is early-stage software. This repository currently contains **Milestone 2: Persistence / SQLite**: validated domain state from Milestone 1 plus synchronous, restart-safe local persistence. Workflow execution remains future work.

## Principles

- Native coding-agent CLIs remain first-class providers.
- Role, provider, and model are separate concepts.
- Workflow behavior is explicit and testable.
- Machine state is canonical in local SQLite; artifacts remain human-readable.
- Every implementation run will use an isolated Git worktree and require explicit apply.
- Defaults should be useful without turning Polycode into a generic agent framework.

## Current CLI

```console
polycode --help
polycode --version
polycode doctor
polycode runs
```

`doctor` reports resolved config/database paths and existing schema version without creating a missing database. `runs` opens the local store and lists indexed run summaries. Domain and persistence APIs are importable as `polycode::domain` and `polycode::store`; provider detection arrives with provider work.

## Build

Prerequisite: Rust stable 1.85 or newer.

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

Set `POLYCODE_DATA_DIR` to override its parent directory. Path resolution is side-effect free; opening the store creates the directory/database and applies schema migrations. SQLite stores immutable resolved configuration separately from versioned run snapshots and semantic events.

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) and [LEGACY_BEHAVIOR.md](LEGACY_BEHAVIOR.md). Milestone 3+ descriptions are design constraints, not claims of implemented behavior.

## License

Licensed under either Apache License 2.0 or MIT license, at your option.
