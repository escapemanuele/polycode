# Configuration and appearance

Know where Polycode keeps its state and which environment variables change its paths, colors and motion.

## Sub-features
- config-path: `$POLYCODE_CONFIG_DIR/config.toml`, else `$XDG_CONFIG_HOME/polycode/config.toml`, else `$HOME/.config/polycode/config.toml`; resolved, printed by `doctor`, but never read or created.
- data-dir: `POLYCODE_DATA_DIR` relocates the SQLite database (`polycode.db`), managed worktrees (`worktrees/`), process data (`runs/<run-id>/processes/`), artifacts, `update.json` and `install.json`. Default `~/.polycode`.
- appearance: `NO_COLOR`, `POLYCODE_THEME` (`vivid` or default `native`), `POLYCODE_MOTION` (`off`, `reduced`, default), `COLORTERM` (read only to decide whether `vivid` can render).
- update-kill-switch: `POLYCODE_DISABLE_UPDATE_CHECK=1` stops every network check, typed or automatic.
- diagnostics: `RUST_LOG` directives (default `polycode=info`); in TUI mode logs go to a sink so stderr cannot corrupt the screen.

## How to get to it (user POV)
There is no config file to edit today. Set environment variables before launching Polycode; the TUI reads appearance variables once at startup. `polycode doctor` prints the resolved config path, database path and schema version.

## Driving it
```bash
polycode doctor
POLYCODE_DATA_DIR=/tmp/polycode-data polycode runs
POLYCODE_THEME=vivid polycode
POLYCODE_MOTION=reduced polycode
NO_COLOR=1 polycode
POLYCODE_DISABLE_UPDATE_CHECK=1 polycode update --check
RUST_LOG=polycode=debug cargo run -- doctor
```

## Where it lives
- `src/config/mod.rs` — `config_file` resolution order.
- `src/store/path.rs` — `database_file`, `worktree_root`, `POLYCODE_DATA_DIR`.
- `src/store/migrations.rs`, `src/store/sqlite.rs` — schema lifecycle (v1..v5), WAL, busy timeout, insert-only triggers.
- `src/tui/theme.rs` — `NO_COLOR`, `COLORTERM`, `POLYCODE_THEME`.
- `src/tui/motion.rs` — `POLYCODE_MOTION`.
- `src/update/mod.rs` — `DISABLE_ENVIRONMENT_VARIABLE`, `CACHE_TTL` (24 h).
- `src/lib.rs` — `init_tracing`.
- `src/process/environment.rs` — `safe_environment_name` allowlist forwarded into managed processes.

## Gotchas
- Path resolution is side-effect free; opening the store creates the directory and database and applies migrations. `runs` and `doctor` never create a missing database.
- `<repo>/.polycode.toml` is documented as a future location; nothing reads it.
- An empty `NO_COLOR` is not a request for mono; only present-and-non-empty counts. `TERM=dumb` behaves like `NO_COLOR`.
- `vivid` on a terminal without truecolor falls back to the named ANSI palette; `NO_COLOR` outranks it.
- Read screens (artifact, logs, diff, composer) and every overlay never animate regardless of `POLYCODE_MOTION`; the variable can only lower what a screen permits.
- Only the allowlist in `safe_environment_name` (HOME, PATH, LANG, TERM, XDG_*, CLAUDE_CONFIG_DIR, GIT_CONFIG_*, SSH_AUTH_SOCK, ...) enters the tmux session; other provider variables cross once through a `0600` Unix socket into runner memory. `POLYCODE_*` variables set for the parent do not reach the provider.
- Database triggers reject update/delete on run inputs, config snapshots and artifacts; do not try to edit them through SQL.
