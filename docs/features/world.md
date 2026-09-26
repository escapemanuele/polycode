# The Senate (`polycode world`)

A local-only HTTP server that opens a 3D browser view of the current campaign: `src/world/server.rs` serves the embedded `world/` app and a small JSON API read fresh, on every request, from `src/world/projection.rs`.

## Sub-features
- single-instance: on start, the server writes `<data-dir>/world.json` (`pid`, `port`, `token`, mode `0600`). A later `polycode world` first probes that instance's `/api/health` with its token; if it answers, the new invocation just prints/opens the existing URL and exits, instead of binding a second server. The file is removed on a clean exit.
- loopback-only: the listener binds `127.0.0.1` exclusively; every request is also rejected with 403 unless its `Host` header is exactly `127.0.0.1:<port>` or `localhost:<port>` (a DNS-rebinding guard — a hostile page resolving that hostname to `127.0.0.1` still cannot get through with a spoofed `Host`).
- token: a random 128-bit hex token, generated from `/dev/urandom`, travels to the browser only in the URL fragment (`#token=...`) so it never reaches server logs or a `Referer` header. Every `/api/*` request must carry it as `X-Senate-Token`, compared in constant time; a POST additionally rejects a present `Origin` header that is not this server's own origin.
- embedded-assets: `build.rs` walks `world/` at compile time and generates a URL-path -> (content-type, bytes) table via `include_bytes!`; `src/world/assets.rs` looks a request path up in that table only, so there is no filesystem read and no path-traversal surface at runtime.
- projection: `GET /api/world` returns schema 1 — `campaign` (title, goal, state, settled/total/active/needs_you), `consul` (`quiet`/`conferring`/`awaiting_you` plus fixed sentences over committed facts, never model-written), `orders` (one per work package: Cohort number in creation order, `state` `planned`/`ready`/`working`/`in_review`/`blocked`/`delivered`/`settled`/`failed`/`cancelled`, `activity` from the current run's running or waiting stage kind — `reading`, `designing`, `coding`, `testing`, `reviewing`, `deciding`), `censor` (`reviewing` while a review stage of some package's run is running), `attention`, `curia` (always `closed` for now). It is computed from the same `MissionDetails` the terminal renders, plus each current run's stages; nothing is stored.
- api: `GET /api/health`; `GET /api/world`; `GET /api/order/<package-id>` (contract, stages, verification and review bottom lines verbatim); `GET /api/consul` (the lead's latest answer, verbatim minus its plan-changes section, and whether a turn is in progress); `POST /api/consul {"message": "..."}` asks the lead through `MissionService::ask_lead` on a background thread (202; refused with a sentence while a turn is running). Proposed plan changes are never applied from the browser. Every `/api/*` route takes an optional `?mission=<id>`, which beats the server's `--mission` default, so one server serves every campaign.
- world: the browser draws a Forum (campaign board, Consul, Aquila mosaic), the Legion Hall (one desk per Order with a live run; a Cohort sits there, its standard carries the Cohort number and turns seal-red with `!` when the Order needs you), the Censors' exedra (the reviewer and the submitted tablet while a review stage runs), and a closed Curia and Tabularium. Motion happens only on a state change between two snapshots (a Cohort walks in when an Order starts; a tablet travels to the Censor, then to the rack by the board when delivered). The first snapshot is placed without motion, and nothing moves when nothing is running.
- tui: `W` on the missions screens starts `polycode world --mission <selected>` as its own process group and opens the browser; closing the TUI leaves it running and closing the browser leaves agents running.
- headers: every response carries `X-Content-Type-Options: nosniff`; `/api/*` responses carry `Cache-Control: no-store`; HTML responses carry a `Content-Security-Policy` restricted to `'self'` (plus `data:`/`blob:` images).
- demo mode: `--demo <scenario>` appends `?demo=<scenario>` to the browser URL before the token fragment; the front end (`world/js/api.js`) reads it and serves fixtures instead of `/api/world`.

## How to get to it (user POV)
Run `polycode world` from anywhere; it opens (or reuses) a local server and launches your browser to it. Pass `--mission <id>` to bind the view to one mission, `--no-open` to just print the URL, and `--demo <scenario>` to look at a fixed scenario instead of live state. Ctrl-C in the terminal that started it closes the server (agents keep working; nothing about a mission depends on the Senate being open).

## Driving it
```bash
polycode world
polycode world --mission <mission-id>
polycode world --port 4123
polycode world --no-open
polycode world --demo review
```

## Where it lives
- `build.rs` — embeds every file under `world/` into the binary; `cargo:rerun-if-changed=world`.
- `src/world/mod.rs` — module wiring, `pub fn run`.
- `src/world/assets.rs` — embedded-asset lookup (`lookup`, `index`).
- `src/world/server.rs` — the HTTP server: single-instance check, listener, host/token/origin checks, routing, response headers.
- `src/world/projection.rs` — `project` (pure: `MissionDetails` + stages → `WorldState`), `snapshot`, `order_detail`, `consul_log`, `ask_consul`.
- `src/tui/desktop.rs` — `enter_senate`; `src/tui/input.rs` binds `W` to `Intent::EnterSenate`.
- `world/js/director.js` — the only place that maps projection fields to what the scene shows; `world/js/demo.js` — deterministic fixtures in the same shape (`?demo=quiet|one|review|blocked|complete|empty|story`).
- `src/store/path.rs` — `world_state_file()`, the single-instance marker path.
- `src/cli/mod.rs` — `WorldArgs` (`--mission`, `--port`, `--no-open`, `--demo`).
- `src/cli/commands.rs` — `world` dispatch.
- `world/` — the browser app itself: `index.html`, `style.css`, `js/*.js`, `vendor/three.module.min.js`.

## Gotchas
- The server never reads `world/` from disk at runtime; a new or edited file there needs a rebuild (`build.rs` reruns on any change under `world/`, so a normal `cargo build`/`cargo run` picks it up).
- Reusing a running instance ignores `--port`; `--mission` is carried to the tab as `?mission=` so the reused server shows the requested campaign. `--demo` still applies, since it is a browser-side query parameter.
- Accepted sockets inherit the listener's non-blocking mode on macOS; `handle_connection` switches them back to blocking, or a large asset (three.js, 800 KB) is cut off mid-send. The socket test fetches it whole after a deliberate read delay.
- Reads go through `MissionService::inspect_mission`, which observes runs first (the same one write the TUI refresh makes).
- The token lives only in the URL fragment and `sessionStorage`; it is never sent to `/api/*` except as the `X-Senate-Token` header, and the front end strips it from the visible URL after reading it once.
- `--port 0` (the default) means "OS-assigned"; the printed URL always carries the port actually bound, not the flag's value.
- A `Host` or `Origin` mismatch is a 403 even for a same-machine client with the right token: the check is on the header, not on where the connection came from.

## Tests
- `src/world/server.rs` — host-header, token-comparison, origin-check, and browser-URL unit tests; an integration-style test drives `GET /api/health` over a real loopback socket with and without a valid token/host, and fetches the whole vendored three.js.
- `src/world/projection.rs` — working/in-review/testing/blocked/delivered/quiet projections, Cohort numbering by creation, conferring lead, schema field names.
- `src/world/assets.rs` — index lookup, unknown-path 404, and traversal-path (`/../Cargo.toml`) non-match.
