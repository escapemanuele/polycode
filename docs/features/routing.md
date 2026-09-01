# Routing

Decide which native coding runtime, and optionally which model, executes each engineering role of a run, once, at creation.

## Sub-features
- uniform: `--provider claude|codex|fake` maps every role in the workflow to that provider.
- recommended: `--profile recommended` (the default when neither flag is given) resolves the current versioned profile, `recommended_v2`, and persists explicit per-role routes.
- frozen-v1: runs persisted under `recommended_v1` keep decoding their original routes.
- models: configured model is null unless the immutable config supplies one; confirmed model comes only from provider evidence.
- status: `status` prints the Routing table (role, configured provider, configured model, reason) and per-stage configured vs actual.

## How to get to it (user POV)
Omit both flags for Recommended, or pass exactly one of `--provider` or `--profile`. The TUI composer's Execution field cycles Recommended / Claude only / Codex only / Fake with ←/→. Read the resolved routes in `polycode status <run-id>` under Routing.

## Driving it
```bash
polycode standard "<task>"                          # recommended_v2
polycode standard "<task>" --profile recommended
polycode standard "<task>" --provider claude
polycode standard "<task>" --provider codex
polycode standard "<task>" --provider fake
polycode status <run-id>
```
Current `recommended_v2` map with both providers ready: Researcher, Architect, CodeQualityReviewer, EngineeringLead, legacy Reviewer -> Claude; Implementer, SpecReviewer -> Codex. With one native provider ready, every role routes there with a persisted fallback reason.

## Where it lives
- `src/app/routing.rs` — `RECOMMENDED_PROFILE_VERSION` (`recommended_v2`), `RECOMMENDED_PROFILE_VERSION_V1`, provenance tables, `resolve_config`, `RoutingPlan`, `unroutable_fix_role`.
- `src/app/provider_factory.rs` — `RuntimeProviderFactory`, `RoutedProvider` lazy adapter cache keyed by target and effort.
- `src/cli/commands.rs` — `start`: flag-to-`ExecutionSelection` mapping; only `recommended` is an accepted profile name.
- `src/store/config_snapshot.rs` — insert-only immutable config records (schema v2 routes, v3 adds resource plan).
- `src/tui/state.rs` — `ExecutionChoice`.
- `tests/routing_cli.rs` — default profile, flag conflict, no Fake fallback, persisted routes surviving provider loss.

## Gotchas
- `--provider` and `--profile` conflict at clap level (exit 2). Any profile other than `recommended` is rejected before state creation.
- Recommended never falls back to Fake; with no authenticated native CLI it refuses to start (`recommended profile requires authenticated Claude Code or Codex CLI`). Fake must be asked for by name.
- Routes are resolved only at creation; resume, retry, recovery and attention never re-run Recommended. Provider loss afterwards is a clear failure, not a reroute.
- `recommended_v2` evidence is runtime-level (role_core_v3, 3 repetitions); it encodes no cost or token claims. Researcher, Architect, EngineeringLead and legacy Reviewer are inherited from v1 without evidence.
- Codex never confirms its model; `actual` model stays `unconfirmed` for Codex stages by design.
- Effort never changes routing; see observability-and-effort.md.
