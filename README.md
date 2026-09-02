# Polycode

**The engineering layer above coding agents.**

One task. Specialist agents. Independent review. One controlled change.

Use Claude Code, Codex and other native coding agents for what each does best.
Polycode routes engineering roles across them, independently reviews the
implementation and the specification, and keeps you in control of what reaches
your codebase.

> Don’t just run more agents. Give each agent the right job — and make them check each other.

Polycode is a local-first terminal orchestrator for native coding-agent CLIs, driven from a Ratatui control room or the CLI. It runs the locally installed `claude` and `codex` executables as specialized engineering roles with their existing authentication and native configuration; it calls no vendor API directly. Every run works in its own Git worktree and reaches your checkout only through an explicit apply, or your remote only through an explicit pull request.

Polycode is early-stage software. Validated domain state, restart-safe SQLite persistence, DAG scheduling, crash-reconcilable tmux supervision, and immutable versioned Recommended routing (`recommended_v1` and `recommended_v2` frozen, `recommended_v3` current) are in place. Gemini, runtime failover, custom routing, async runtime, native process backend, daemon mode, Advisor, and direct provider chat remain future work.

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
quiet when there is nothing to say. Asking it yourself always checks now.

```bash
polycode update --check   # check now, report, change nothing
polycode update           # check now, install after explicit confirmation
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
Fast:      Implementation -> Verify

Standard:  Architecture ---> Implementation ---> Simplification ---> Code Quality Review --+
           |                 |                   |                                         |
           +-----------------+-------------------+--> Specification Review ----------------+-> Decision
                                                 |                                         |
                                                 +--> Verify ------------------------------+

Deep:      Research -> Architecture ---> Implementation ---> Simplification ---> Code Quality Review --+
                       |                 |                   |                                         |
                       +-----------------+-------------------+--> Specification Review ----------------+-> Decision
                                                             |                                         |
                                                             +--> Verify ------------------------------+

Review:    Research -> Code Quality Review --+
                   `-> Specification Review -+-> Synthesis -> Decision
```

A decision is where a run ends, not where the operator's options do. `polycode fix <run-id>`, or `f` in the run detail view, sends a completed run back to remediate its own result:

```text
... -> Decision --> Fix 1 -> Verify 1 -> Decision 1 --> Fix 2 -> Verify 2 -> Decision 2 -> ...
```

The run grows one cycle per request, keeping its workspace, its artifacts and its identity, so it stays one thing to apply or discard. The fix answers the decision that rejected it and is bounded by that decision's blocking findings; the fresh decision reads both the fix and the verdict it answers, and judges the fix against the code rather than against its own claims. Pressing fix again answers the newest verdict. Nothing re-runs the reviews — start a review run over the result if you want them back.

Polycode never reads the verdict to decide whether a fix is warranted. A decision artifact is prose written for a person; classifying it as a rejection is not something the system can support, so the action is offered on any completed run that reached a decision and your asking is the whole signal.

Simplification edits the implementation change in place before anyone judges it: it removes accidental complexity — comments that restate code, single-caller abstractions, speculative generality — bounded by the run delta and forbidden from changing observable behavior. It runs in a writable workspace like Implementation, and both reviews then inspect the simplified result, doubling as a safety net over its edits.

Code Quality Review inspects actual repository state and judges how implementation is engineered. Specification Review independently compares delivered behavior with immutable task intent and available design evidence, classifying gaps as Missing, Wrong, or Unrequested. Both are read-only and create separate stage-ID-based Markdown artifacts. Existing persisted runs retain their original stored graph, including legacy generic review stages.

### Verification

After the last stage that edits the worktree, Polycode runs the repository's own verification commands there — no agent involved — and records every command and exit code in a Markdown artifact whose bottom line the control room quotes. The stage completes only when every command exits zero. In Standard and Deep a failed check does not fail the run: the decision still runs with the failure in front of it and the run completes, so `fix` can answer it in place — each fix cycle re-verifies — while `apply` and `pr` refuse by name until the run's latest verification passes. In Fast there is no decision, so a failed check fails the run and `retry <run-id> verify` re-runs it. The reviews run beside verification and the decision waits for it.

The repository says what "verified" means in `<repo>/.polycode.toml`:

```toml
[verify]
commands = ["cargo fmt --check", "cargo clippy --all-targets", "cargo test"]
timeout_seconds = 1800
```

Without that table Polycode runs the one command the build file implies (`Cargo.toml` → `cargo test`, `package.json` → `npm test`, `pyproject.toml`/`pytest.ini` → `pytest`, `go.mod` → `go test ./...`), and with no recognised build file it completes having checked nothing and says so. Commands are argv, not shell — no pipes or `&&` — and the sequence stops at the first failure.

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
| Global | `x` | Dismiss notification |
| Run | `r`, `s`, `t`, `u` | Resume/recover, stop (keeps the run and its work), retry selected failed stage, attention |
| Run | `o`, `l`, `d`, `i` | Verified artifact, raw logs, diff, toggle technical details |
| Run | `a`, `P`, `X` | Apply, publish pull request, or discard with confirmation |
| Run | `f`, `c`, `w` | Fix a completed run's decision, continue it with a new instruction, work on its decision's Follow-ups |
| Runs list | `h`, `H` | Hide/unhide selected run, show/hide hidden runs |
| Viewer | `↑`/`↓`, `PageUp`/`PageDown`, `Home`/`End` | Scroll |
| Artifact viewer | `m` | Toggle raw/rendered Markdown |
| Composer | `Tab`/`Shift-Tab`, arrows, typing/paste | Move fields, choose values, edit |

New-run composer defaults to Standard workflow, Recommended routing, and the profile's own per-role effort; arrows cycle the Workflow, Execution, and Effort fields (profile default, native, low, medium, high, xhigh). Execution choices are Recommended, Claude only, Codex only, and Fake. Selection maps directly to M9 `ExecutionSelection`; UI never recomputes routes.

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

New runs accept `--effort native|low|medium|high|xhigh` for every role, or `--effort role=level[,role=level]` to name some roles and leave the rest to the routing profile (CLI), or the Effort field in the TUI composer. Omitted effort means the profile's own policy: `recommended_v3` states a level per role — Researcher, Architect, both reviewers and EngineeringLead at `high`, Implementer at `medium`, Simplifier at `low` — so expensive reasoning goes where uncertainty is, and the implementer executes an explicit plan more cheaply. Under `--provider` there is no profile policy and omitted effort stays `NativeDefault`; `--effort native` opts every role out under any selection, keeping every native invocation byte-identical to pre-M13b behavior. `NativeDefault` is deliberately distinct from `medium` — a runtime's default may be anything. Explicit levels are translated by each provider adapter onto its native supported control: Claude Code maps onto the `--effort low|medium|high|xhigh` session flag; Codex maps onto a `-c model_reasoning_effort="low|medium|high|xhigh"` configuration override. Adapters own these mappings; domain code never encodes provider- or model-specific aliases, so a future runtime (for example a local-model harness) can translate `high` completely differently (say, a reasoning-tier model profile). An explicit level is never silently no-opped: it always changes the native invocation, and the persisted requested effort is visible in `status` and the TUI per stage.

Effort persists as a per-role ResourcePlan inside the immutable config snapshot (schema v3; omitted effort keeps emitting the pre-M13b schema v2 payload). Old v1/v2 snapshots decode to `NativeDefault` for every role — no old run changes native runtime behavior — and unknown or malformed effort values fail closed instead of degrading to a default. Effort never rewrites routing: `--profile recommended` resolves the same `recommended_v3` routes regardless of effort, and M13a telemetry never feeds back into effort dynamically. The profile's effort rows are `Provisional` (stated from the role contracts, not measured) until an effort sweep with `eval run --effort` replaces them. Review stages receive identical change-handoff evidence at every effort level so effort comparisons measure runtime reasoning, not supplied evidence.

## Image generation (opt-in tool, not a provider)

`--allow-image-generation` lets the Implementer create original PNG images inside the run's worktree through a Polycode-owned tool whose backend is your own Codex CLI and its built-in `image_gen` tool (native ChatGPT auth, no API key, no vendor API). It is the first non-coding capability and deliberately the narrowest possible one: Claude or Codex remains the Implementer, image generation is something it is permitted to use. Routing, effort and workflow semantics are untouched.

The coding agent reaches the tool through a run-scoped MCP server (`polycode __image-tool`) that relays to a socket inside the Polycode process, which runs one `codex exec` per image and collects the PNG Codex wrote; nothing is written to `~/.claude`, `~/.codex` configuration, or the project. Authorization is sealed in the immutable run configuration (schema v4), old snapshots decode disabled, and at most four images may be generated per run. The PNG is an ordinary untracked binary in the worktree: it shows in the diff preview, apply installs its exact bytes, discard removes it. Reviewers are told an image file changed; nobody has looked at its pixels. See `docs/features/image-generation.md`.

## Implementation change map for review stages

Review stages (CodeQualityReviewer, SpecReviewer, and the legacy Reviewer role) and the Simplifier — for which the run delta is the boundary of what it may touch — receive a deterministic bounded implementation-change map in their initial prompt: the changed-file inventory (with change kind and binary markers) plus a bounded textual diff of the managed worktree relative to the immutable run base — the exact delta semantics used by apply and diff preview, including untracked files. The handoff is navigation evidence, not the source of truth: reviewers retain full access to the real worktree and must inspect code as needed. Binary contents are never injected. Oversized diffs are explicitly marked INCOMPLETE with the shown/total byte counts — never silently truncated — and the changed-file inventory is always complete even when the diff is bounded. The handoff is provider-neutral (one shared section, byte-identical across Claude and Codex) and removes redundant mechanical change discovery, especially for future runtimes launched with restricted read-only tool sets that cannot run `git diff` themselves. Researcher, Architect, Implementer, and Decision stages do not receive it; resume/continuation prompts stay compact and never re-inject it. Its prompt cost is visible through the existing injected-prompt-bytes telemetry.

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
polycode deep "Redesign authentication" --effort architect=xhigh,implementer=medium
polycode standard "Landing page with an original hero image" --provider claude --allow-image-generation
polycode standard "Add export support" --repo /path/to/repo --provider codex
polycode deep "Redesign authentication" --provider codex
polycode review "Review the error boundary" --provider codex
polycode deep "Refactor authentication" --profile recommended
polycode standard "Fix the parser"
polycode fast "Fix the parser" --provider fake
polycode runs
polycode status <run-id>
polycode resume <run-id>
polycode stop <run-id>
polycode resolve <run-id> <attention-id> [--response "answer"]
polycode retry <run-id> <stage-id>
polycode fix <run-id>
polycode apply <run-id>
polycode pr <run-id>
polycode discard <run-id>
polycode update [--check] [--yes]
```

Workflow commands use current directory unless `--repo` is supplied. Omitting both selection flags starts the `recommended` profile, the same default the TUI's new-run composer opens on; the resolved profile is named in the report rather than assumed. Recommended never falls back to the development FakeProvider, so the default can refuse to start — `recommended profile requires authenticated Claude Code or Codex CLI` — but can never quietly run a task against something that only looks like work. Fake stays something you ask for by name. Flags are mutually exclusive:

- `--provider claude|codex|fake` creates uniform routing for every role used by workflow.
- `--profile recommended` resolves the current versioned profile (`recommended_v3`) once, persists explicit routes and a requested effort per role, and never re-resolves them on restart. Runs persisted under `recommended_v1` or `recommended_v2` keep resolving their original routes and native-default effort unchanged.

`recommended_v2` is the first profile informed by role_core_v3 native-runtime evaluations (fingerprint `cb9856d2…c375b`, 3 repetitions per case, targets `claude/native_default` and `codex/native_default`, zero infrastructure failures on both). Evidence is runtime-level: each target is a whole native agent runtime that may orchestrate multiple models/subagents internally, so results are not single-model comparisons and encode no cost/token claims. Measured decisions: Implementer stays Codex (equivalent measured correctness, lower observed runtime latency; high confidence), CodeQualityReviewer stays Claude (higher measured defect recall, accepting modest non-must-fix false-positive noise; medium confidence), SpecReviewer moves to Codex (equivalent measured correctness across every criterion, lower observed runtime latency; medium confidence — latency evidence is suite-level, not role-isolated). Researcher, Architect, EngineeringLead, and legacy Reviewer are inherited from `recommended_v1` because role_core_v3 does not evaluate them. If only one native provider is ready at creation, every required role routes there with persisted fallback reason. Fake is never a Recommended fallback.

`recommended_v3` changes no route. It inherits every `recommended_v2` destination and adds a requested effort per role: Researcher, Architect, CodeQualityReviewer, SpecReviewer and EngineeringLead at `high`, Implementer at `medium`, Simplifier at `low`. Because the v2 measurements were taken at native-default effort and v3 no longer runs the implementer there, the route rows are restated as `Inherited` and the effort rows are `Provisional` (benchmark kind `expert_provisional`, like `recommended_v1` was for routes). The Architect contract now asks for an executable plan (`## Plan`, `## Verification`, `## Out of scope`, `## Assumptions`) and the Implementer confirms those assumptions before editing, which is what makes the lower implementer level defensible; an effort sweep with `eval run --effort` is the evidence that will replace the provisional rows.

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

Codex uses documented non-interactive `codex exec --json` transport with prompt on immutable stdin. Native Codex authentication, config, `AGENTS.md`, rules, skills, hooks/trust checks, and MCP configuration remain active. Polycode selects `read-only` sandbox for non-mutating stages and `workspace-write` for Implementation/Simplification/Fix, with explicit `--ask-for-approval never`; approval policy never disables sandbox. Polycode never passes `--yolo`, dangerous bypass, `danger-full-access`, `--ephemeral`, Git-check bypass, or config/rules bypass.

Codex `thread.started` supplies opaque native thread identity; recovery resumes exact ID through a new managed invocation, never `--last`. Unknown valid JSONL records become durable non-semantic checkpoints; invalid complete records leave cursor untouched. Current stable exec JSON exposes no safe typed permission/question continuation, so Polycode never infers `NeedsUser` from prose. Native denial/terminal error fails stage instead. Successful `turn.completed` atomically records usage and completion from one raw record.

`resume`, `resolve`, and `retry` reconcile persisted workspace first, reconstruct provider from immutable configuration, then execute to next quiescent condition. `resume` never bypasses attention or retries failure. `resolve` and `retry` perform requested explicit transition then continue. `apply` transfers actual completed workspace changes; empty diff is successful no-op. `pr` publishes a completed run without touching the source checkout: it commits the run's delta on its own `polycode/run-<id>` branch, pushes the branch to `origin`, and opens a pull request through the GitHub CLI (`gh`) when available — pull-request failure never undoes the push. The pull request's title and description are quoted from the `## Pull request` section every editing stage closes its artifact with, so they describe what was done rather than restating the task; the task text stands in for a run whose agent wrote none. The run stays `Completed`, so apply, fix, and discard all remain available, publishing again after a fix cycle updates the same branch and pull request, and any number of completed runs can publish concurrently because nothing serializes on the operator's checkout. `discard` records logical disposition before owned-resource cleanup.

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

Repository settings live at `<repo>/.polycode.toml`: its `[verify]` table (see Verification above) and its `[permissions]` table, which lists the native Claude Code rules every run in that repository may use without stopping to ask:

```toml
[permissions]
allow = ["Bash(yarn jest:*)", "Bash(yarn lint:css:*)", "mcp__linear-server"]
```

A rule that would grant every tool is refused. The user configuration file is resolved but never read or created, so the update opt-out below is an environment variable rather than a configuration key.

### Appearance

The interface reads these environment variables at startup and never again:

| Variable | Effect |
| --- | --- |
| `NO_COLOR` | Present and non-empty: every semantic colour collapses to the terminal's own foreground. `TERM=dumb` does the same. Nothing becomes unreadable — state is always carried by a glyph or a word as well as a colour. |
| `POLYCODE_THEME` | `vivid` paints Polycode's own colours; anything else is the default `native`, which uses named ANSI colours so every token resolves to whatever your terminal theme defines. |
| `POLYCODE_MOTION` | `off` stops all movement; `reduced` keeps state changes but stops anything that repeats; anything else is the default. |
| `COLORTERM` | Read only to know whether `vivid` can be rendered (`truecolor` or `24bit`). It never changes anything on its own: a terminal that *can* paint Polycode's colours has not asked for them. |

Both themes spell the same eight meanings, so `vivid` is a different
materialisation rather than a different design. Asked for on a terminal
without truecolor, it falls back to the named ANSI palette rather than to
approximated hues, and `NO_COLOR` outranks it either way.


Movement is bounded by the surface, not only by the preference: screens you
read — an artifact, logs, a diff, the new-run form — and every open overlay
never move, whatever `POLYCODE_MOTION` says. The preference can only lower
what a screen already permits.

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
the run database. That cache only serves the automatic check — a command you type
always performs a fresh one, so a release published minutes ago is reported as such.

```bash
polycode update --check   # check now, report, change nothing
polycode update           # check now, install after explicit confirmation, where supported
polycode update --yes     # install without the prompt
```

Automatic installation applies only to an official release binary that Polycode
itself installed and recorded. Source builds, `cargo install` destinations, and
package-manager prefixes are reported with the command that owns them rather than
overwritten. An install downloads to a staging file beside the target, verifies its
SHA-256 against the release's `SHA256SUMS`, checks the staged binary reports the
version the release claims, and only then renames it into place; the running process
keeps using the binary it already loaded, and the new one is used at the next start.

Switch update checking off. This is a full kill switch, not only a background one:
while it is set, `polycode update` and `polycode update --check` also stop reaching
the network and report that checks are disabled.

```bash
export POLYCODE_DISABLE_UPDATE_CHECK=1
```

`polycode doctor` reports the current version, the detected install source, and
whether automatic updates are available for that installation.

## Architecture

Agents should read [docs/features/README.md](docs/features/README.md) before driving or changing a feature; it maps every user-facing feature to its commands, keys, code paths and gotchas.

See [ARCHITECTURE.md](ARCHITECTURE.md) and [LEGACY_BEHAVIOR.md](LEGACY_BEHAVIOR.md). Claude, Codex, immutable role routing, `recommended_v1`/`recommended_v2`, local TUI, and separate role evaluation evidence are implemented; Gemini, adaptive routing/failover, Advisor, and direct provider chat remain future constraints.

## License

Licensed under either Apache License 2.0 or MIT license, at your option.
