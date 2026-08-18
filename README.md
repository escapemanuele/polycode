# Polycode

> One task. Multiple coding models. Each doing what it does best.

Polycode is a local-first terminal orchestrator for native coding-agent CLIs. It is designed to coordinate agents such as Claude Code, Codex CLI, and Gemini CLI as specialized engineering roles without replacing their existing authentication or execution model.

Polycode is early-stage software. This repository contains **Milestone 11 role evaluation harness and routing evidence** above role-routed native Claude Code, Codex CLI, deterministic FakeProvider adapters, and Ratatui local control room. Polycode uses locally installed `claude` and `codex` executables with existing authentication/native configuration; it calls neither vendor API directly. Validated domain state, restart-safe SQLite persistence, isolated Git worktrees, DAG scheduling, crash-reconcilable tmux supervision, and immutable `recommended_v1` routing remain intact. Gemini, runtime failover, custom routing, async runtime, native process backend, daemon mode, Advisor, direct provider chat, and `recommended_v2` remain future work.

## Principles

- Native coding-agent CLIs remain first-class providers.
- Role, provider, and model are separate concepts.
- Workflow behavior is explicit and testable.
- Machine state is canonical in local SQLite; artifacts remain human-readable.
- Every implementation run uses an isolated Git worktree and requires explicit apply.
- Defaults should be useful without turning Polycode into a generic agent framework.

## Built-in workflows

New runs use these graph definitions:

```text
Fast:      Implementation

Standard:  Architecture ---> Implementation ---> Code Quality Review --+
                 |                |                                  |
                 +----------------+--> Specification Review ---------+-> Decision

Deep:      Research -> Architecture ---> Implementation ---> Code Quality Review --+
                                |                |                                  |
                                +----------------+--> Specification Review ---------+-> Decision

Review:    Research -> Code Quality Review --+
                   `-> Specification Review -+-> Synthesis -> Decision
```

Code Quality Review inspects actual repository state and judges how implementation is engineered. Specification Review independently compares delivered behavior with immutable task intent and available design evidence, classifying gaps as Missing, Wrong, or Unrequested. Both are read-only and create separate stage-ID-based Markdown artifacts. Existing persisted runs retain their original stored graph, including legacy generic review stages.

## Local control room

Run Polycode without arguments in an interactive terminal:

```console
polycode
polycode tui
```

Both open local Ratatui control room. With no command and non-interactive stdin/stdout, Polycode prints normal CLI help and emits no terminal control sequences. Explicit `polycode tui` requires interactive stdin and stdout.

Control room is projection and control surface over application layer. It lists runs, shows stage timeline and persisted routes, keeps configured and actual provider/model distinct, highlights attention, displays verified artifacts, reads bounded raw log tails without acknowledging provider output, previews same managed-worktree delta used by apply, and delegates all mutations to `RunService`.

Blocking start/resume/retry/attention/apply/discard actions run on one serialized background worker. Navigation and read-only refresh remain responsive. Pressing `q` or `Ctrl-C` detaches frontend and restores terminal; it does not interrupt, discard, clean, or apply a run. A tmux-owned provider continues independently. Reopen Polycode and explicitly resume/recover to reconcile durable state and consume retained output.

Key bindings:

| Context | Keys | Action |
|---|---|---|
| Global | `↑`/`↓`, `j`/`k` | Navigate |
| Global | `Enter`, `Esc` | Open/confirm, back/close |
| Global | `n`, `R`, `?` | New run, runs screen, help |
| Global | `q`, `Ctrl-C` | Quit/detach frontend |
| Run | `r`, `t`, `u` | Resume/recover, retry selected failed stage, attention |
| Run | `o`, `l`, `d` | Verified artifact, raw logs, diff |
| Run | `a`, `X` | Apply or discard with confirmation |
| Viewer | `↑`/`↓`, `PageUp`/`PageDown`, `Home`/`End` | Scroll |
| Composer | `Tab`/`Shift-Tab`, arrows, typing/paste | Move fields, choose values, edit |

New-run composer defaults to Standard workflow and Recommended routing. Choices are Recommended, Claude only, Codex only, and Fake. Selection maps directly to M9 `ExecutionSelection`; UI never recomputes routes.

## Evaluations

Experimental evaluation tooling compares one provider/model candidate by engineering role, not general intelligence:

```console
polycode eval list
polycode eval run --suite role_core_v1 --provider fake
polycode eval run --suite role_core_v2 --provider fake
polycode eval run --suite role_core_v1 --provider codex --model <model> --repeat 3 --allow-native-usage
polycode eval report ~/.polycode/evals/<evaluation-directory>
polycode eval report <codex-results> <claude-results>
```

Native Claude/Codex evaluation can consume subscription or provider usage. Every invocation must explicitly pass `--allow-native-usage`; consent is never stored. Fake needs no acknowledgement, produces `synthetic = true`, and is useful only for harness/CI plumbing—not routing evidence. Omitted `--model` is recorded as `configured_model = null`, meaning native configured/default model. Confirmed model remains separate and may also be null; Polycode never guesses.

`role_core_v1` contains seven historical high-signal cases. It is immutable and remains the default when `--suite` is omitted. `role_core_v2` contains the same seven conceptual cases with calibrated reviewer fixtures and scoring; select it explicitly.

V2 quality fixtures are valid minimal Cargo repositories. Quality identity matching excludes severity, while reports expose severity matches, under/over-classification, and duplicate findings separately. V2 specification truths can have multiple source locations for one conceptual finding; category remains strict. V1 and V2 result groups stay separate in reports and are never averaged together. Existing `EvalResultV1` JSON remains readable because V2 metrics are additive metric variants inside the same envelope schema.

Cases by role:

- Implementer: basic correctness, narrow scope discipline, and stopping on plan/repository contradiction.
- Code Quality Reviewer: planted unnecessary abstraction/duplicate representation/nesting, plus clean-code false positives.
- Specification Reviewer: independently planted Missing/Wrong/Unrequested behavior, plus clean-spec false positives.

Architect, Researcher, and EngineeringLead remain intentionally unevaluated until deterministic high-signal oracles exist. M11 uses no LLM judge. Reviewer findings match source-controlled ground truth one-to-one using category/severity, file, nearby line range, and concept rules. Implementer cases score pre-apply diff scope, real apply into disposable source, and fixed offline validation argv. Read-only reviewer mutation is infrastructure/safety failure.

Each repetition materializes a fresh fixture Git repository and uses its own SQLite/worktree/process roots. Ordinary `~/.polycode/polycode.db` is never opened, so eval runs never appear in `polycode runs`. Default evidence lives under:

```text
~/.polycode/evals/<evaluation-id>/<case-id>/rep-NNN/
├── result.json
├── artifact.md
├── diff.patch
├── validation.txt
├── source/
└── runtime/     isolated Polycode database, worktrees, and process evidence
```

`result.json` is schema-versioned and records suite/case fingerprints, repetition, role, configured/confirmed model, provider CLI version, benchmark vs infrastructure status, target-stage-only usage, latency, and evidence hashes. Reports aggregate role-specific metrics and expose individual failures; they never calculate monetary cost, choose a winner, generate routing, or feed runtime policy.

Interpret results narrowly. Small suites do not prove general superiority; models/providers change; three repetitions remain a small sample; fixture representativeness matters; false positives matter; results apply only to measured roles; user-global native configuration intentionally influences actual runtime behavior. Record CLI/model/suite identity, but never snapshot credentials, auth files, or full environment.

Future evidence workflow, not implemented:

```text
collect candidate result sets
  -> inspect cases and evidence manually
  -> decide role policy
  -> author recommended_v2 in source
  -> preserve recommended_v1 for existing runs
```

M11 does not change `recommended_v1`. Normal routing never reads `~/.polycode/evals`; deleting evaluation data has zero production effect. No evaluation controls exist in TUI.

## Current CLI

```console
polycode --help
polycode --version
polycode tui
polycode eval list
polycode eval run --provider fake
polycode eval report <path> [<path> ...]
polycode doctor
polycode fast "Fix the parser" --provider claude
polycode standard "Add export support" --repo /path/to/repo --provider claude
polycode deep "Redesign authentication" --provider claude
polycode review "Review the error boundary" --provider claude
polycode fast "Fix the parser" --provider codex
polycode standard "Add export support" --repo /path/to/repo --provider codex
polycode deep "Redesign authentication" --provider codex
polycode review "Review the error boundary" --provider codex
polycode deep "Refactor authentication" --profile recommended
polycode fast "Fix the parser" --provider fake
polycode runs
polycode status <run-id>
polycode resume <run-id>
polycode resolve <run-id> <attention-id> [--response "answer"]
polycode retry <run-id> <stage-id>
polycode apply <run-id>
polycode discard <run-id>
```

Workflow commands use current directory unless `--repo` is supplied. Execution selection remains explicit and flags are mutually exclusive:

- `--provider claude|codex|fake` creates uniform routing for every role used by workflow.
- `--profile recommended` resolves versioned `recommended_v1` once, persists explicit routes, and never re-resolves them on restart.

When both native providers are authenticated, provisional `recommended_v1` routes implementation to Codex and research, architecture, reviews, and decision to Claude. If only one native provider is ready at creation, every required role routes there with persisted fallback reason. Fake is never a Recommended fallback. This policy is source-controlled and provisional, not benchmark-backed.

```text
Role                   Provider (both available)
Researcher             Claude
Architect              Claude
Implementer            Codex
CodeQualityReviewer    Claude
SpecReviewer           Claude
EngineeringLead        Claude
```

Provider loss after run creation fails clearly; it never causes runtime fallback. Configured model may remain null, meaning provider-native default. Actual confirmed model/session/process are reported per stage. Fake emits start, progress, usage, and completion signals without editing files. Native providers run only in managed worktree.

Claude uses supported non-interactive stream JSON mode. Polycode leaves model selection to native Claude default unless immutable run configuration explicitly supplies model. Existing Claude authentication, `CLAUDE.md`, settings, permissions, hooks, skills, and MCP configuration remain native inputs. Polycode never adds `--dangerously-skip-permissions`.

When Claude reports denied permission or question, Polycode persists attention and stops. Permission resolution approves only exact safely representable native tool rule and resumes same Claude session UUID in new managed invocation. Questions require `--response`; response becomes run-private stdin, never argv. If permission cannot be represented safely, resume fails closed.

Codex uses documented non-interactive `codex exec --json` transport with prompt on immutable stdin. Native Codex authentication, config, `AGENTS.md`, rules, skills, hooks/trust checks, and MCP configuration remain active. Polycode selects `read-only` sandbox for non-mutating stages and `workspace-write` for Implementation/Fix, with explicit `--ask-for-approval never`; approval policy never disables sandbox. Polycode never passes `--yolo`, dangerous bypass, `danger-full-access`, `--ephemeral`, Git-check bypass, or config/rules bypass.

Codex `thread.started` supplies opaque native thread identity; recovery resumes exact ID through a new managed invocation, never `--last`. Unknown valid JSONL records become durable non-semantic checkpoints; invalid complete records leave cursor untouched. Current stable exec JSON exposes no safe typed permission/question continuation, so Polycode never infers `NeedsUser` from prose. Native denial/terminal error fails stage instead. Successful `turn.completed` atomically records usage and completion from one raw record.

`resume`, `resolve`, and `retry` reconcile persisted workspace first, reconstruct provider from immutable configuration, then execute to next quiescent condition. `resume` never bypasses attention or retries failure. `resolve` and `retry` perform requested explicit transition then continue. `apply` transfers actual completed workspace changes; empty diff is successful no-op. `discard` records logical disposition before owned-resource cleanup.

Normal blocked states (`needs_user`, paused, interrupted, failed) exit successfully because command completed and durable state remains inspectable. Operational failures exit 1; Clap argument errors exit 2. CLI progress comes only from committed semantic events.

`doctor` reports paths/schema, tmux, Claude/Codex CLI versions and auth status, plus names of known credential/config override variables without printing values or creating missing database. `status` shows immutable routing and per-stage configured/actual provider, configured/confirmed model, provider session, native session, conversation, and managed-process status. `runs` leaves missing database untouched. Public APIs are under `polycode::app`, `polycode::domain`, `polycode::engine`, `polycode::process`, `polycode::providers`, `polycode::store`, `polycode::git`, and `polycode::workspace`.

## Build

Prerequisites:

- Rust stable 1.85 or newer.
- Git.
- tmux on macOS or Linux.

Provider-specific prerequisites:

- Claude Code CLI on `PATH`, authenticated through normal native setup, for `--provider claude`.
- Codex CLI on `PATH`, authenticated through normal native setup, for `--provider codex`.

Neither native CLI is required for `--provider fake` or normal tests.

```bash
cargo build
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Normal tests use deterministic provider fixtures and never require native provider login, network, or usage. Opt-in native smoke tests:

```bash
POLYCODE_REAL_CLAUDE=1 cargo test --test claude_real -- --ignored --nocapture
POLYCODE_REAL_CODEX=1 cargo test --test codex_real -- --ignored --nocapture
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

Provider session record is distinct from managed process. It binds run/stage/attempt to opaque native provider session ID, current invocation/process, confirmed model when protocol supplies it, CLI version, lifecycle status, and optional pending attention range. Each accepted JSONL record advances raw output cursor atomically with provider-session CAS and semantic run/events. One raw record may emit an atomic signal batch; Codex completion uses this for usage plus completion. Completion additionally binds immutable artifact metadata after artifact bytes are durably written.

Native-provider stage artifacts live under:

```text
~/.polycode/runs/<run-id>/artifacts/<stage-id>.md
```

Direct dependency artifacts are included in downstream prompts. Artifact metadata records provider, confirmed model when available, attempt, base commit, path, size, and SHA-256; reads verify bytes before use.

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) and [LEGACY_BEHAVIOR.md](LEGACY_BEHAVIOR.md). Claude, Codex, immutable role routing, `recommended_v1`, local TUI, and separate role evaluation evidence are implemented; Gemini, adaptive routing/failover, `recommended_v2`, Advisor, and direct provider chat remain future constraints.

## License

Licensed under either Apache License 2.0 or MIT license, at your option.
