# Polycode

> One task. Multiple coding models. Each doing what it does best.

Polycode is a local-first terminal orchestrator for native coding-agent CLIs. It is designed to coordinate agents such as Claude Code, Codex CLI, and Gemini CLI as specialized engineering roles without replacing their existing authentication or execution model.

Polycode is early-stage software. This repository currently contains **Milestone 1: Domain Model and State Machine**. Execution and persistence remain future work.

## Principles

- Native coding-agent CLIs remain first-class providers.
- Role, provider, and model are separate concepts.
- Workflow behavior is explicit and testable.
- Machine state will be canonical in local SQLite; artifacts remain human-readable.
- Every implementation run will use an isolated Git worktree and require explicit apply.
- Defaults should be useful without turning Polycode into a generic agent framework.

## Current CLI

```console
polycode --help
polycode --version
polycode doctor
polycode runs
```

`doctor` and `runs` expose honest bootstrap placeholders. Milestone 1 is importable as `polycode::domain`; provider detection arrives with provider work and run persistence arrives in Milestone 2.

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

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) and [LEGACY_BEHAVIOR.md](LEGACY_BEHAVIOR.md). Future milestone descriptions are design constraints, not claims of implemented behavior.

## License

Licensed under either Apache License 2.0 or MIT license, at your option.
