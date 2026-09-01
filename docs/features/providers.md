# Native providers (Claude Code, Codex CLI, Fake)

Run each stage through the locally installed `claude` or `codex` executable with its own authentication and configuration, or through the deterministic Fake provider for tests.

## Sub-features
- claude: non-interactive stream-JSON print mode, `dontAsk` permissions, prompt on immutable stdin; denied tools become typed permission attention; questions (`AskUserQuestion`) need `--response`.
- codex: `codex exec --json` with prompt `-` on stdin, `--ask-for-approval never`, sandbox `read-only` for non-mutating stage kinds and `workspace-write` for Implementation/Fix/FollowUp; no typed attention.
- codex-dead-process-completion: a `turn.completed` whose process then died (non-zero exit, signal, or vanished) completes only if the `--output-last-message` file equals the last agent message in the retained stream; otherwise the record is consumed and the stage becomes a recoverable interruption.
- fake: scripted signals (start, progress, usage, attention, pause, interruption, completion, failure) without editing files; scenario `development_fake/default_success_v1`.
- permission-continuation: `resolve` reconstructs the exact denial from retained output, converts it to a native `--allowedTools` rule, and resumes the same Claude session UUID in a new managed invocation.
- artifacts: `~/.polycode/runs/<run-id>/artifacts/<stage-id>.md`, SHA-256 verified before use; downstream prompts get direct dependency artifacts only.
- doctor: reports CLI versions, auth status and suspicious credential environment variable names.

## How to get to it (user POV)
Install and log into `claude` and/or `codex` natively, confirm with `polycode doctor`, then start runs with `--provider claude`, `--provider codex`, or Recommended. When a Claude stage stops with `needs_user`, read the attention line in `status` and answer with `resolve`. Codex has no attention path; a native denial fails the stage and you `retry`.

## Driving it
```bash
polycode doctor
polycode fast "<task>" --provider claude
polycode fast "<task>" --provider codex
polycode fast "<task>" --provider fake
polycode resolve <run-id> <attention-id>                      # approve exact permission
polycode resolve <run-id> <attention-id> --response "<text>"  # answer AskUserQuestion
POLYCODE_REAL_CLAUDE=1 cargo test --test claude_real -- --ignored --nocapture
POLYCODE_REAL_CODEX=1 cargo test --test codex_real -- --ignored --nocapture
```
TUI: `u` opens the attention overlay; ↑/↓ choose the request, type an answer if it is a question, Enter resolves.

## Where it lives
- `src/providers/claude/` — `detection.rs` (install/auth discovery), `command.rs` (argv, `--resume`, `--allowedTools`, `--effort`), `protocol.rs` (JSONL decoder, `PermissionDenial`, terminal-attention rules), `prompt.rs`, `artifact.rs`, `mod.rs` (adapter).
- `src/providers/codex/` — `detection.rs`, `command.rs` (`exec --json`, sandbox, `-c model_reasoning_effort`), `protocol.rs`, `session_meta.rs`, `mod.rs`.
- `src/providers/session.rs`, `src/providers/checkpoint.rs`, `src/providers/artifact.rs` — provider session, atomic commit payload, immutable artifact record.
- `src/engine/fake.rs` — Fake scenarios.
- `src/engine/provider.rs` — provider-neutral `ProviderRequest`/`ProviderPoll` boundary.
- `src/store/provider.rs` — provider-session and artifact persistence.
- `tests/routing_cli.rs` — `recommended_attention_restart_routes_response_to_same_claude_session`.
- `tests/codex_cli.rs`, `tests/claude_real.rs`, `tests/codex_real.rs`.

## Gotchas
- `permission_denials` in a Claude result is per-process, not per-session: every retry after a `--resume` gets a new `tool_use_id`. A list that looks cumulative across the resume boundary is not; treating it as residual history misclassifies a fresh attempt. Split by `tool_use_id` (`PermissionDenial::same_request`).
- "Safe to auto-approve" and "denial blocks completion" are two different policies (`exact_rule` vs `requires_terminal_attention`); when they were one function, successful results were marked infrastructure failures or `NeedsUser`. Only Edit/Write/MultiEdit/NotebookEdit, AskUserQuestion and unknown tools are terminal; a denied Bash/Read/Glob/Grep/LS/WebFetch never ran, so it is not terminal.
- Polycode never adds `--dangerously-skip-permissions`, `--yolo`, `danger-full-access`, `--ephemeral` or any Git/config bypass. Wildcard or ambiguous permission rules fail closed at `resolve`.
- A question response goes to run-private stdin, never argv or SQLite event payloads.
- Claude reviewer stages are prompt-prohibited from editing but have no hard sandbox; Codex reviewers do (`read-only`). This asymmetry is documented, not patched.
- Codex `thread.started` supplies the thread id; recovery resumes that exact id, never `--last`. A failed-stage retry creates a new session and thread.
- A retained `turn.completed` is not proof the turn finished: the same bytes survive a killed CLI. Without a clean exit, the proof is the `--output-last-message` file matching the last `item.completed` agent message byte for byte (only a trailing newline may differ, since `artifact::persist` appends it). "File exists and is non-empty" is not enough — a process killed mid-write leaves a readable prefix that would become a hash-verified half-artifact. Uncorroborated means interruption, not an error: raising here re-reads and re-rejects the same record on every poll and pins the stage in `running` forever, reachable only by `retry`, which throws the finished work away. Recover with `polycode resume <run-id>`.
- Unknown valid JSONL records become non-semantic checkpoints; an invalid complete record fails without advancing the cursor; a partial line waits.
- Claude usage comes from the terminal result record only; per-message usage is discarded on purpose.
