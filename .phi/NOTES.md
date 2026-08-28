# Notes

## technical
- mascot.rs's state_style/accent_style are `const fn`, so they call theme color CONSTANTS directly (theme::ACCENT/MUTED/ATTENTION/SUCCESS/DANGER for states; theme::STRUCTURE for Architecture, theme::MUTED for other activities) — they cannot call the non-const helpers muted()/activity_accent().
- Step 4 resolution: diff_hunk() (STRUCTURE/blue + BOLD) is now the sole consumer in render.rs's hunk-header branch; activity_accent() was REMOVED from theme.rs because it had no const-compatible caller (mascot's accent_style is a const fn that uses the STRUCTURE constant directly).
- Regression guard `raw_color_lives_only_in_theme` lives in src/tui/theme.rs tests module (so its own "Color::" literal is exempted by the theme.rs skip rule). It recursively walks {CARGO_MANIFEST_DIR}/src/tui, skips theme.rs by filename, and fails on any `Color::` outside the single allow-list entry ("render.rs","Color::Reset").

## gotcha
- `LEGACY_BEHAVIOR.md:119` ("## Milestone 1 resolutions") is intentional historical content, not stale status — a future "fix stale milestone" pass must NOT touch it. Only `PROJECT.md`/`.project-meta.yaml` carried the stale "Milestone 1 complete" current-status claim; both now read "Milestone 13d3 — TUI visual polish (in progress)".
- Writing in-file regression-guard #[test]s (e.g. raw_color_lives_only_in_theme in theme.rs) trips pedantic clippy in 3 ways: a `///` doc comment with bare code tokens → doc_markdown (use `//` line comments instead; test fns need no public docs); a nested helper fn declared after a `let` → items_after_statements (declare nested fns first);
- CI job "quality" in .github/workflows/ci.yml runs `cargo fmt --check` alongside build/clippy/test — so a locally-green state (build+test+clippy all passing) can STILL fail CI when the working tree isn't rustfmt-clean. After editing by hand (esp.

## state
- Commit 177a176 (fix(clippy): drop unused Read/Seek import + use is_ok_and in providers/claude) is pushed to feat/m13d3-visual-polish; CI has not re-run for it yet. User scoped this turn to commit+push only, so merging PR #11 is deferred until they ask — do not auto-merge.
