# Resource observability and effort policy

See what each stage consumed and how long it took, and request how much native-runtime effort a run should use, without either influencing routing.

## Sub-features
- usage: provider-native units per stage (input, output, cache read, cache write, reasoning output) plus Claude `modelUsage` per-model view; `unavailable` means not reported, never zero.
- latency: wall-clock from first `ProviderStarted` to the last terminal provider event per invocation, from committed event timestamps.
- invocations and prompt bytes: count of persisted native invocations and exact stdin bytes piped per invocation (SHA-256-verified stdin file).
- effort: `--effort native|low|medium|high` persisted per role as a `ResourcePlan` in config schema v3; adapters map it to `claude --effort <level>` or `codex -c model_reasoning_effort="<level>"`.
- surfaces: `status` prints `effort=<requested> [→ <observed>]` per stage and Usage lines per provider; the TUI technical view (`i`) shows the same evidence.

## How to get to it (user POV)
Add `--effort` to any workflow command, or set the Effort field in the TUI composer. Read usage and latency in `polycode status <run-id>` (Stages and Usage sections) or in the TUI with `i` on the run detail screen. Eval results also record `requested_effort`.

## Driving it
```bash
polycode standard "<task>" --effort high
polycode standard "<task>" --effort native    # same as omitting the flag
polycode status <run-id>
```
TUI: `i` on run detail toggles technical details; the composer Effort field is intended to cycle Native default / Low / Medium / High (see gotcha).

## Where it lives
- `src/domain/effort.rs` — `EffortSetting` (`NativeDefault | Level(Low|Medium|High)`), `EffortLevel`.
- `src/cli/commands.rs` — `parse_effort` (unknown words fail closed).
- `src/app/routing.rs` — `ResourcePlan` (`effort`, `efforts`, `from_snapshot`), schema v3 emission.
- `src/app/provider_factory.rs` — adapter cache keyed by `(ExecutionTarget, EffortSetting)`, `with_effort`.
- `src/providers/claude/command.rs`, `src/providers/codex/command.rs` — native flag mapping.
- `src/app/query.rs` — `UsageSummary`, `StageExecutionEvidence`, `RunUsage`; folded at read time from events.
- `src/domain/event.rs` — `ProviderUsageUpdated` with optional native dimensions and `native_models`.
- `src/cli/commands.rs` — `usage_lines` (never sums across providers).
- `src/tui/render.rs` — `technical_row("Effort", ...)`.

## Gotchas
- `NativeDefault` is not `medium`; omitted effort keeps every native invocation byte-identical to pre-effort behavior and emits the pre-M13b schema v2 payload.
- Old v1/v2 config snapshots decode to `NativeDefault` for every role; unknown or malformed effort values and a resource plan smuggled into v2 fail closed.
- Neither native CLI confirms applied effort; `observed` is only shown when the runtime reports something, and Polycode never invents it.
- Never compare Claude and Codex usage units; comparable dimensions are latency, invocation count, injected prompt bytes and eval pass/fail.
- Injected prompt bytes exclude everything the runtime reads on its own (repository, CLAUDE.md/AGENTS.md, MCP, skills, system prompts).
- Telemetry never feeds routing, retries, permissions or effort; there is no escalation. Retry-with-higher-effort is deferred (M13b.1).
- The TUI composer cannot currently change Effort with ←/→ (dispatch only covers focus 1 and 3); use the CLI flag. See control-room.md.
