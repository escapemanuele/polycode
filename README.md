# Polycode

> One task. Multiple coding models. Each doing what it does best.

Polycode is a local-first terminal orchestrator for native coding-agent CLIs. It is designed to coordinate agents such as Claude Code, Codex CLI, and Gemini CLI as specialized engineering roles without replacing their existing authentication or execution model.

Polycode is early-stage software. This repository currently contains **Milestone 7: native Claude Code provider**. Polycode now supports native Claude Code through locally installed `claude` CLI. It uses Claude Code's existing authentication and native runtime rather than calling Anthropic API directly. Validated domain state, restart-safe SQLite persistence, isolated Git worktrees, DAG scheduling, deterministic `FakeProvider`, and crash-reconcilable tmux supervision remain intact. Codex/Gemini adapters, async runtime, native process backend, and UI remain future work.

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
polycode fast "Fix the parser" --provider claude
polycode standard "Add export support" --repo /path/to/repo --provider claude
polycode deep "Redesign authentication" --provider claude
polycode review "Review the error boundary" --provider claude
polycode fast "Fix the parser" --provider fake
polycode runs
polycode status <run-id>
polycode resume <run-id>
polycode resolve <run-id> <attention-id> [--response "answer"]
polycode retry <run-id> <stage-id>
polycode apply <run-id>
polycode discard <run-id>
```

Workflow commands use current directory unless `--repo` is supplied. Provider selection is explicit: use `--provider claude` for native execution or `--provider fake` for deterministic development/testing. Fake emits start, progress, usage, and completion signals without editing files. Claude runs in managed worktree and may edit it under native Claude Code permissions.

Claude uses supported non-interactive stream JSON mode. Polycode leaves model selection to native Claude default unless immutable run configuration explicitly supplies model. Existing Claude authentication, `CLAUDE.md`, settings, permissions, hooks, skills, and MCP configuration remain native inputs. Polycode never adds `--dangerously-skip-permissions`.

When Claude reports denied permission or question, Polycode persists attention and stops. Permission resolution approves only exact safely representable native tool rule and resumes same Claude session UUID in new managed invocation. Questions require `--response`; response becomes run-private stdin, never argv. If permission cannot be represented safely, resume fails closed.

`resume`, `resolve`, and `retry` reconcile persisted workspace first, reconstruct provider from immutable configuration, then execute to next quiescent condition. `resume` never bypasses attention or retries failure. `resolve` and `retry` perform requested explicit transition then continue. `apply` transfers actual completed workspace changes; empty diff is successful no-op. `discard` records logical disposition before owned-resource cleanup.

Normal blocked states (`needs_user`, paused, interrupted, failed) exit successfully because command completed and durable state remains inspectable. Operational failures exit 1; Clap argument errors exit 2. CLI progress comes only from committed semantic events.

`doctor` reports paths/schema, tmux, Claude CLI version/auth status, and names of known credential override variables without printing values or creating missing database. `status` includes confirmed provider model, native session, conversation status, and managed-process status when available. `runs` leaves missing database untouched. Public APIs are under `polycode::app`, `polycode::domain`, `polycode::engine`, `polycode::process`, `polycode::providers`, `polycode::store`, `polycode::git`, and `polycode::workspace`.

## Build

Prerequisites:

- Rust stable 1.85 or newer.
- Git.
- tmux on macOS or Linux.
- Claude Code CLI on `PATH`, authenticated through normal native setup, for `--provider claude`.

```bash
cargo build
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Normal tests use deterministic provider fixtures and never require Claude or consume provider usage. Opt-in native smoke test:

```bash
POLYCODE_REAL_CLAUDE=1 cargo test --test claude_real -- --ignored --nocapture
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

Managed provider invocations use separately persisted process infrastructure. One stage attempt may own multiple invocations while preserving one native provider session:

```text
~/.polycode/runs/<run-id>/processes/<process-id>/
├── spec.json
├── stdin.jsonl
├── runtime.json
├── stdout.log
├── stderr.log
└── exit.json
```

Tmux launches Polycode's hidden runner with exact argument vector, never shell command string. Prompt/continuation input is stored in immutable SHA-256-bound stdin file and never command line. Runner redirects provider stdout/stderr to append-only files, so CLI detachment cannot break output. SQLite owns acknowledged byte offsets; reads do not advance them. Exit/runtime evidence is identity-checked against immutable launch fingerprint.

Each process uses isolated tmux server. Safe operational variables enter its session explicitly. Remaining native provider environment, including environment-based authentication, crosses once through user-only (`0600`) Unix socket into runner memory. Values never enter argv, tmux server environment, manifest, SQLite, or durable files; runner clears inherited environment before launching provider and reconstructs only captured native environment plus validated process overrides.

Provider session record is distinct from managed process. It binds run/stage/attempt to opaque native Claude session ID, current invocation/process, confirmed model/version, lifecycle status, and pending attention record range. Each accepted JSONL record advances raw output cursor atomically with provider-session CAS and semantic run/events. Completion additionally binds immutable artifact metadata after artifact bytes are durably written.

Claude stage artifacts live under:

```text
~/.polycode/runs/<run-id>/artifacts/<stage-id>.md
```

Direct dependency artifacts are included in downstream prompts. Artifact metadata records provider, confirmed model when available, attempt, base commit, path, size, and SHA-256; reads verify bytes before use.

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) and [LEGACY_BEHAVIOR.md](LEGACY_BEHAVIOR.md). Claude is implemented; Codex/Gemini and UI descriptions remain future constraints.

## License

Licensed under either Apache License 2.0 or MIT license, at your option.
