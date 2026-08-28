# Polycode

> One task. Multiple coding models. Each doing what it does best.

Polycode is a local-first terminal orchestrator for native coding-agent CLIs. It is designed to coordinate agents such as Claude Code, Codex CLI, and Gemini CLI as specialized engineering roles without replacing their existing authentication or execution model.

Polycode is early-stage software. This repository contains **Milestone 11 role evaluation harness and routing evidence** above role-routed native Claude Code, Codex CLI, deterministic FakeProvider adapters, and Ratatui local control room. Polycode uses locally installed `claude` and `codex` executables with existing authentication/native configuration; it calls neither vendor API directly. Validated domain state, restart-safe SQLite persistence, isolated Git worktrees, DAG scheduling, crash-reconcilable tmux supervision, and immutable versioned Recommended routing (`recommended_v1` frozen, `recommended_v2` current) remain intact. Gemini, runtime failover, custom routing, async runtime, native process backend, daemon mode, Advisor, and direct provider chat remain future work.

## Install

macOS (Apple Silicon and Intel) and Linux x86_64:

```bash
curl -fsSL https://raw.githubusercontent.com/escapemanuele/polycode/main/install.sh | sh
```

The installer downloads an official release binary, verifies its SHA-256 against the
release's own `SHA256SUMS`, confirms the binary reports the version the release
claims, and installs it into `~/.local/bin/polycode`. It never uses `sudo`, never
writes outside that directory and the Polycode data directory, and never edits your
shell configuration — if `~/.local/bin` is not on `PATH` it prints the line to add.

Options are environment variables, because a piped script cannot take flags:

```bash
POLYCODE_VERSION=0.1.1 sh install.sh      # install a specific release
POLYCODE_INSTALL_DIR=~/bin sh install.sh  # install somewhere else
POLYCODE_FORCE=1 sh install.sh            # replace a file Polycode does not manage
```

Windows is not supported: Polycode requires tmux. Linux ARM has no official build yet.

### Verify

```bash
polycode --version
polycode doctor
```

`doctor` reports the install source; a bootstrap installation reads
`install source: official binary` and `automatic update: supported`.

### Update

Polycode checks for new official releases automatically, at most once a day, and stays
quiet when there is nothing to say.

```bash
polycode update --check   # report status, change nothing
polycode update           # install after explicit confirmation
```

See [Updates](#updates) for what a check sends and how to switch it off.

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
polycode eval run --suite role_core_v3 --provider fake
polycode eval run --suite role_core_v1 --provider codex --model <model> --repeat 3 --allow-native-usage
polycode eval report ~/.polycode/evals/<evaluation-directory>
polycode eval report <codex-results> <claude-results>
```

Native Claude/Codex evaluation can consume subscription or provider usage. Every invocation must explicitly pass `--allow-native-usage`; consent is never stored. Fake needs no acknowledgement, produces `synthetic = true`, and is useful only for harness/CI plumbing—not routing evidence. Omitted `--model` is recorded as `configured_model = null`, meaning native configured/default model. Confirmed model remains separate and may also be null; Polycode never guesses.

`role_core_v1` contains seven historical high-signal cases. It is immutable and remains the default when `--suite` is omitted. `role_core_v2` contains the same seven conceptual cases with calibrated reviewer fixtures and scoring; select it explicitly. `role_core_v3` is the calibrated hygiene successor: implementer fixtures ignore generated `target/` and `Cargo.lock`, while read-only reviewer fixtures carry canonical locks and ignore only `target/`.

V2 and V3 quality fixtures are valid minimal Cargo repositories. Quality identity matching excludes severity, while reports expose severity matches, under/over-classification, and duplicate findings separately. V2/V3 specification truths can have multiple source locations for one conceptual finding; category remains strict. V1, V2, and V3 result groups stay separate in reports and are never averaged together. Existing `EvalResultV1` JSON remains readable because reviewer metrics are additive variants inside the same envelope schema.

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

Evidence workflow, now exercised once for `recommended_v2`:

```text
collect candidate result sets
  -> inspect cases and evidence manually
  -> decide role policy
  -> author recommended_v2 in source
  -> preserve recommended_v1 for existing runs
```

`recommended_v1` remains frozen. Normal routing never reads `~/.polycode/evals`; deleting evaluation data has zero production effect. No evaluation controls exist in TUI.

## Resource observability

M13a adds observability, not resource policy. Telemetry never influences routing, Recommended profiles, retries, permissions, or any execution decision; it only measures and reports.

What Polycode measures per stage: provider-native usage units, per-invocation wall-clock provider latency (first `ProviderStarted` to the last terminal provider event, excluding scheduler delay), the number of persisted native invocations, and injected prompt bytes — the exact stdin bytes Polycode piped into each native invocation (initial prompt plus continuations), derived from the immutable per-invocation stdin file. Injected prompt bytes deliberately exclude everything the native runtime reads on its own: repository files, CLAUDE.md/AGENTS.md, MCP context, skills, cached native context, and provider system prompts. Native runtimes may independently read large repository context that Polycode neither sees nor bounds.

Usage sources are provider-native and are NOT cross-provider normalized; never compare Claude and Codex units. Claude usage comes from the terminal result record's cumulative totals (input, output, cache read, cache write) — per-assistant-message usage is intentionally ignored because real streams repeat identical usage across content-block records and carry partial output snapshots. Claude's native `modelUsage` per-model breakdown (subagent models included) is captured as a separate typed view that overlaps the aggregate and is never summed into it; native per-model cost figures are intentionally not captured. Codex usage comes from `turn.completed`: input, cached input, cache write, output, and reasoning output are recorded as reported. Codex never confirms its actual model, so confirmed model stays unavailable rather than inferred. In every surface `unavailable` (absent) means the runtime did not report a dimension and is never rendered as zero.

Cross-provider comparable dimensions: wall-clock latency, invocation count, injected prompt bytes, and eval pass/fail outcomes. Provider-native, never comparable: input/output/cache/reasoning units and per-model native accounting.

## Effort policy (resource intent)

M13b separates four concepts that must never be conflated: **Role** answers what responsibility a stage carries; the **RoutingPlan** answers which coding runtime/model destination executes it; the **ResourcePlan** answers how much native-runtime effort is requested; **M13a telemetry** answers what resource usage was actually observed. Effort is resource intent, not a token budget — Polycode cannot reliably enforce a universal token ceiling inside native coding runtimes and does not try.

New runs accept `--effort native|low|medium|high` (CLI) or the Effort field in the TUI composer. Omitted effort means `NativeDefault`: every native invocation stays byte-identical to pre-M13b behavior, preserving each runtime's own configuration exactly. `NativeDefault` is deliberately distinct from `medium` — a runtime's default may be anything. Explicit levels are translated by each provider adapter onto its native supported control: Claude Code maps onto the `--effort low|medium|high` session flag; Codex maps onto a `-c model_reasoning_effort="low|medium|high"` configuration override. Adapters own these mappings; domain code never encodes provider- or model-specific aliases, so a future runtime (for example a local-model harness) can translate `high` completely differently (say, a reasoning-tier model profile). An explicit level is never silently no-opped: it always changes the native invocation, and the persisted requested effort is visible in `status` and the TUI per stage.

Effort persists as a per-role ResourcePlan inside the immutable config snapshot (schema v3; omitted effort keeps emitting the pre-M13b schema v2 payload). Old v1/v2 snapshots decode to `NativeDefault` for every role — no old run changes native runtime behavior — and unknown or malformed effort values fail closed instead of degrading to a default. Effort never rewrites routing: `--profile recommended` resolves the same frozen `recommended_v2` routes regardless of effort, and M13a telemetry never feeds back into effort dynamically. Review stages receive identical change-handoff evidence at every effort level so effort comparisons measure runtime reasoning, not supplied evidence.

## Implementation change map for review stages

Review stages (CodeQualityReviewer, SpecReviewer, and the legacy Reviewer role) receive a deterministic bounded implementation-change map in their initial prompt: the changed-file inventory (with change kind and binary markers) plus a bounded textual diff of the managed worktree relative to the immutable run base — the exact delta semantics used by apply and diff preview, including untracked files. The handoff is navigation evidence, not the source of truth: reviewers retain full access to the real worktree and must inspect code as needed. Binary contents are never injected. Oversized diffs are explicitly marked INCOMPLETE with the shown/total byte counts — never silently truncated — and the changed-file inventory is always complete even when the diff is bounded. The handoff is provider-neutral (one shared section, byte-identical across Claude and Codex) and removes redundant mechanical change discovery, especially for future runtimes launched with restricted read-only tool sets that cannot run `git diff` themselves. Researcher, Architect, Implementer, and Decision stages do not receive it; resume/continuation prompts stay compact and never re-inject it. Its prompt cost is visible through the existing injected-prompt-bytes telemetry.

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
polycode standard "Refactor parser" --profile recommended --effort high
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
- `--profile recommended` resolves the current versioned profile (`recommended_v2`) once, persists explicit routes, and never re-resolves them on restart. Runs persisted under `recommended_v1` keep resolving their original routes unchanged.

`recommended_v2` is the first profile informed by role_core_v3 native-runtime evaluations (fingerprint `cb9856d2…c375b`, 3 repetitions per case, targets `claude/native_default` and `codex/native_default`, zero infrastructure failures on both). Evidence is runtime-level: each target is a whole native agent runtime that may orchestrate multiple models/subagents internally, so results are not single-model comparisons and encode no cost/token claims. Measured decisions: Implementer stays Codex (equivalent measured correctness, lower observed runtime latency; high confidence), CodeQualityReviewer stays Claude (higher measured defect recall, accepting modest non-must-fix false-positive noise; medium confidence), SpecReviewer moves to Codex (equivalent measured correctness across every criterion, lower observed runtime latency; medium confidence — latency evidence is suite-level, not role-isolated). Researcher, Architect, EngineeringLead, and legacy Reviewer are inherited from `recommended_v1` because role_core_v3 does not evaluate them. If only one native provider is ready at creation, every required role routes there with persisted fallback reason. Fake is never a Recommended fallback.

```text
Role                   v1 (frozen)   v2 (current)
Researcher             Claude        Claude (inherited)
Architect              Claude        Claude (inherited)
Implementer            Codex         Codex  (measured)
CodeQualityReviewer    Claude        Claude (measured)
SpecReviewer           Claude        Codex  (measured)
EngineeringLead        Claude        Claude (inherited)
legacy Reviewer        Claude        Claude (inherited)
```

Provider loss after run creation fails clearly; it never causes runtime fallback. Configured model may remain null, meaning provider-native default. Actual confirmed model/session/process are reported per stage. Fake emits start, progress, usage, and completion signals without editing files. Native providers run only in managed worktree.

Claude uses supported non-interactive stream JSON mode. Polycode leaves model selection to native Claude default unless immutable run configuration explicitly supplies model. Existing Claude authentication, `CLAUDE.md`, settings, permissions, hooks, skills, and MCP configuration remain native inputs. Polycode never adds `--dangerously-skip-permissions`.

When Claude reports denied permission or question, Polycode persists attention and stops. Permission resolution approves only exact safely representable native tool rule and resumes same Claude session UUID in new managed invocation. Questions require `--response`; response becomes run-private stdin, never argv. If permission cannot be represented safely, resume fails closed.

Codex uses documented non-interactive `codex exec --json` transport with prompt on immutable stdin. Native Codex authentication, config, `AGENTS.md`, rules, skills, hooks/trust checks, and MCP configuration remain active. Polycode selects `read-only` sandbox for non-mutating stages and `workspace-write` for Implementation/Fix, with explicit `--ask-for-approval never`; approval policy never disables sandbox. Polycode never passes `--yolo`, dangerous bypass, `danger-full-access`, `--ephemeral`, Git-check bypass, or config/rules bypass.

Codex `thread.started` supplies opaque native thread identity; recovery resumes exact ID through a new managed invocation, never `--last`. Unknown valid JSONL records become durable non-semantic checkpoints; invalid complete records leave cursor untouched. Current stable exec JSON exposes no safe typed permission/question continuation, so Polycode never infers `NeedsUser` from prose. Native denial/terminal error fails stage instead. Successful `turn.completed` atomically records usage and completion from one raw record.

`resume`, `resolve`, and `retry` reconcile persisted workspace first, reconstruct provider from immutable configuration, then execute to next quiescent condition. `resume` never bypasses attention or retries failure. `resolve` and `retry` perform requested explicit transition then continue. `apply` transfers actual completed workspace changes; empty diff is successful no-op. `discard` records logical disposition before owned-resource cleanup.

Normal blocked states (`needs_user`, paused, interrupted, failed) exit successfully because command completed and durable state remains inspectable. Operational failures exit 1; Clap argument errors exit 2. CLI progress comes only from committed semantic events.

`doctor` reports paths/schema, tmux, Claude/Codex CLI versions and auth status, plus names of known credential/config override variables without printing values or creating missing database. `status` shows immutable routing and per-stage configured/actual provider, configured/confirmed model, provider session, native session, conversation, and managed-process status. `runs` leaves missing database untouched. Public APIs are under `polycode::app`, `polycode::domain`, `polycode::engine`, `polycode::process`, `polycode::providers`, `polycode::store`, `polycode::git`, and `polycode::workspace`.

## Build from source

Source builds are for development. They are not automatically updatable — `polycode
update` will report that and name the command that owns the installation instead of
replacing it.

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

Repository overrides will eventually live at `<repo>/.polycode.toml`. Current code resolves paths but does not read or create configuration files, so the update opt-out below is an environment variable rather than a configuration key.

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

## Updates

Polycode checks for newer official releases at most once every 24 hours and stays
silent unless there is something to say. A check reads public release metadata for
`escapemanuele/polycode` from the documented GitHub REST API and sends nothing but a
`polycode/<version>` user agent — no repository paths, task text, run identifiers,
provider or model information, telemetry, or hostname. No token is required, and
network problems are never an error: an unreachable or rate-limited GitHub simply
leaves the status unknown and startup continues unchanged.

The result is cached under the Polycode data directory (`$POLYCODE_DATA_DIR`, else
`~/.polycode/update.json`), never inside a repository you are working in and never in
the run database.

```bash
polycode update --check   # report status, change nothing
polycode update           # install after explicit confirmation, where supported
polycode update --yes     # install without the prompt
```

Automatic installation applies only to an official release binary that Polycode
itself installed and recorded. Source builds, `cargo install` destinations, and
package-manager prefixes are reported with the command that owns them rather than
overwritten. An install downloads to a staging file beside the target, verifies its
SHA-256 against the release's `SHA256SUMS`, checks the staged binary reports the
version the release claims, and only then renames it into place; the running process
keeps using the binary it already loaded, and the new one is used at the next start.

Disable automatic checks entirely:

```bash
export POLYCODE_DISABLE_UPDATE_CHECK=1
```

`polycode doctor` reports the current version, the detected install source, and
whether automatic updates are available for that installation.

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) and [LEGACY_BEHAVIOR.md](LEGACY_BEHAVIOR.md). Claude, Codex, immutable role routing, `recommended_v1`/`recommended_v2`, local TUI, and separate role evaluation evidence are implemented; Gemini, adaptive routing/failover, Advisor, and direct provider chat remain future constraints.

## License

Licensed under either Apache License 2.0 or MIT license, at your option.
