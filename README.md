# stapler-mcp

A from-scratch reimplementation of several third-party MCP (Model Context
Protocol) servers this user previously ran per-session via `npx`/`uvx`. Those
tools were causing severe memory bloat: every subagent in a Claude Code
session independently spawned its own full copy of every MCP server
(confirmed via `pstree` on a live 9-subagent tree — 9 separate process trees
for the same tools), and `npx`/`uvx` each pay 60-100MB of cold-resolve
wrapper overhead per launch on top of the real server. One tool in
particular (`mcp-website-downloader`, invoked via `uvx --from git+...`) left
40+ fully-duplicated installs in `~/.cache/uv` (9.7GB) because `uvx`
re-resolves per invocation instead of reusing a cached environment.

Originally a Go implementation; rewritten in Rust so the same core logic can
ship two ways — a native CLI and a zero-native-binary `npx` package — without
duplicating the thin-client/daemon architecture or the tool logic twice. See
[`NOTES.md`](./NOTES.md) for the phase-by-phase build log and what's deferred.

## Architecture: thin client + shared daemon

This is the entire point of the project, not an optional nicety.

```
Claude Code session 1 ──▶ stapler-mcp (thin stdio MCP server) ─┐
Claude Code session 2 ──▶ stapler-mcp (thin stdio MCP server) ─┼──▶ ~/.stapler-mcp/daemon.sock ──▶ stapler-mcp --daemon
Claude Code subagent N ─▶ stapler-mcp (thin stdio MCP server) ─┘         (one process, one browser pool,
                                                                           one HTTP client, one cache —
                                                                           shared machine-wide)
```

- **thin client** (no flags) — what Claude Code actually launches per
  session as the MCP server. On startup it checks whether the daemon is
  already reachable at `~/.stapler-mcp/daemon.sock`; if not, it spawns
  `--daemon` detached and waits for it to become ready. Every tool call is
  then proxied over that Unix socket and the result streamed back over
  stdio. This process holds no heavyweight state of its own.
- **`--daemon`** — the long-running background process that owns the actual
  heavyweight state (headless browser pool, HTTP clients, caches). Exactly
  one instance runs machine-wide. If several thin clients race to
  auto-start it simultaneously, every spawned process independently
  attempts an exclusive lock on `~/.stapler-mcp/daemon.lock` — only the
  winner binds the socket, the rest see "already running" and exit
  immediately (`crates/native/src/lock.rs` / `crates/wasm/src/glue/lock.js`).

This mirrors a fix already underway in the sibling project `stapler-squad`
(also owned by this user), where its own `--mcp` subcommand is moving from
"duplicate the whole backend per subagent" to "thin client of the one
already-running service" — same pattern, different transport. `stapler-mcp`
is an independent repository and does not depend on or modify `stapler-squad`.

### Why a Unix domain socket

Simple, fast, local-only, no port-conflict risk across concurrent sessions.
Liveness/single-instance is a lockfile, not "is the socket file present"
(stale sockets from a crashed daemon are safe to unlink once a new daemon
holds the lock).

## Two distributions, one core

The actual logic (protocol framing, `EnsureDaemon` state machine, tool
business logic) lives once in `crates/core` — a `#![no OS calls]` crate whose
only OS-touching surface is a set of trait "ports" (`crates/core/src/ports.rs`):
socket, process lock, process spawn, HTTP, browser, file store, env, clock.
Two adapters implement those ports:

| | `crates/native` + `crates/cli` | `crates/wasm` + `npm/` |
|---|---|---|
| Target | native binary (`cargo install`) | `wasm32-unknown-unknown` compiled to WASM, run inside Node |
| Socket/lock/spawn | `tokio`, `std::fs::File::try_lock`, `std::process::Command` | Node `net`/`fs.mkdirSync`/`child_process`, called from Rust via `wasm-bindgen` JS imports |
| HTTP | `reqwest` | Node's built-in `fetch` |
| Browser automation | `chromiumoxide` (drives system Chrome over CDP) | `playwright-core` with `channel: "chrome"` (system Chrome, no download) |
| MCP stdio transport | `rmcp` (official Rust SDK) | `@modelcontextprotocol/sdk` (official TS SDK, low-level `Server` API) |
| Why | full native performance/control, no Node dependency | `npx`-installable with **zero native binary** — nothing for macOS Gatekeeper to block, since the package ships pure `.wasm` + JS, not a downloaded executable |

Both adapters speak the identical wire protocol and are **interoperable on
the same machine**: a native daemon will happily answer a Node thin client
and vice versa (verified in `crates/cli/tests/daemon_ping.rs` and
`npm/test/e2e.test.js`, and manually cross-checked in both directions during
development). Thin clients only ever care whether `ping` succeeds — never
which implementation is on the other end.

Tool input/output schemas are derived once (via `schemars` on shared types in
`crates/core/src/schema.rs`) and reused by both the native `rmcp` tool
registration and a wasm-exported `list_tools_json()` the Node side serves
verbatim — never hand-authored twice.

## Tools implemented

| Tool | Notes |
|---|---|
| `fetch_page` | Headless-render a URL, return title + extracted text, optionally save rendered HTML to a local path (`savePath`). |
| `brave_web_search` | Stateless HTTP wrapper over the Brave Search API. Reads `BRAVE_API_KEY` from the **daemon's** environment. Base URL overridable via `BRAVE_API_BASE_URL` (used by both adapters' test suites to point at a mock server). |
| `read_website` | Fetch a URL (optionally BFS-crawling same-host links up to `maxDepth`/`maxPages`), extract main content via Readability-style extraction, return it as Markdown. Cached by URL hash on the daemon — a cache hit skips the network fetch entirely (at the cost of not expanding that page's links further). Respects `robots.txt`. |
| `download_website` | Same crawl/`robots.txt` mechanics as `read_website`, saves raw HTML per page under `saveDir` instead of extracting Markdown. Merges what were two separate third-party MCP servers this user ran before. |
| `stapler_index_docs` / `stapler_search_docs` / `stapler_list_indexed_sources` / `stapler_remove_indexed_source` | Native-only: crawl a doc site, embed it locally (`fastembed`/`all-MiniLM-L6-v2`), and semantically search it — a narrower-scope, in-process replacement for the Node `docs-mcp-server`. `stapler_`-prefixed to avoid a tool-name collision with that server's own `search_docs`. |
| `stapler_browser_navigate` / `stapler_browser_click` / `stapler_browser_type` / `stapler_browser_snapshot` | `playwright-mcp`-style browser automation via accessibility-tree snapshots, with a `sessionId` threaded across calls and a 300s idle-timeout reaper for abandoned sessions. |
| `stapler_browser_close_session` | Close a browser session and release its resources ahead of the idle-timeout reaper. |
| `stapler_browser_tabs` | List/open/switch/close tabs within a session via an `action` field (`list`/`new`/`select`/`close`), mirroring `@playwright/mcp`'s `browser_tabs`. |
| `stapler_browser_hover` | Hover the pointer over an element identified by a snapshot `ref`, for triggering hover-revealed UI. |
| `stapler_browser_select_option` | Select one or more options in a `<select>` element identified by a snapshot `ref`. |
| `stapler_browser_press_key` | Press a single keyboard key, optionally focusing an element by `ref` first. |
| `stapler_browser_wait_for` | Wait until given text appears/disappears and/or a minimum time elapses before returning the next snapshot. |
| `stapler_browser_list_sessions` | List all open sessions daemon-wide (session id, tab count, idle time in milliseconds, and `blocked`/`crashed` diagnostic flags) — useful for finding sessions leaked by a crashed subagent. |
| `stapler_browser_close_all_sessions` | Close every open session daemon-wide and report which succeeded/failed. Closes are attempted concurrently so one wedged session doesn't block the rest. **No per-caller scoping**: this closes sessions daemon-wide regardless of which caller opened them — safe for a single-agent daemon, but a multi-tenant daemon would need real authorization before exposing it. |

All tools are verified end-to-end against real dependencies (a real headless
Chrome, a real mock/live HTTP server) rather than mocked ports — on both
adapters, except docs-index, which is native-only. See
[`NOTES.md`](./NOTES.md) for the phase-by-phase verification detail and the
real bugs each round of testing found.

## Deferred

See [`NOTES.md`](./NOTES.md) for the full log. Wiring wasm/npm packaging
into CI ([#8](https://github.com/tstapler/stapler-mcp/issues/8)) and the
wasm HTTP adapter not capturing `final_url` on redirect
([#9](https://github.com/tstapler/stapler-mcp/issues/9)) are both closed —
CI now builds `npm/pkg/` via `wasm-bindgen` and runs `npm test` on every
push, and the fix landed in `crates/wasm/src/glue/http.js`. Publishing the
npm package to the public registry remains deliberately undone (needs
explicit sign-off + credentials; `npm/package.json` keeps `"private": true`
as a guard). The one still-open item is whether docs-index's deliberately
narrow scope (Markdown/HTML only, one local embedding model, brute-force
cosine search) is ever worth widening
([#10](https://github.com/tstapler/stapler-mcp/issues/10)) — "no action
yet."

## Usage

### Native

```bash
cargo build -p stapler-mcp   # produces target/debug/stapler-mcp (or --release)

# What Claude Code should register as the MCP server command:
./target/debug/stapler-mcp

# Manual daemon inspection (normally auto-started, not run by hand):
./target/debug/stapler-mcp --daemon
```

### npm / Node (no Rust toolchain needed)

```bash
cd npm && npm install   # pulls in @modelcontextprotocol/sdk, playwright-core
# npm/pkg/ must exist — see Development below for how it's generated
node bin/stapler-mcp.js
```

Either way, `BRAVE_API_KEY` must be set in the environment the **daemon**
process inherits when it's spawned (i.e. wherever the first thin client
happens to run from, since that's what execs `--daemon`) — or export it
globally before any session starts.

### State layout

Everything lives under `~/.stapler-mcp/` (override with `STAPLER_MCP_HOME`,
primarily for tests):

- `daemon.sock` — the Unix socket
- `daemon.lock` — single-instance lock (a real `flock`-backed file natively;
  a liveness-checked lock directory on the Node side) + PID
- `daemon.log` — stdout/stderr of the detached daemon (it has no
  controlling terminal once spawned)

## Development

```bash
cargo build            # native workspace only (crates/wasm is excluded from default-members)
cargo test              # includes a real subprocess-spawning daemon + both real tools, end-to-end

# wasm/Node side:
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version <matching the wasm-bindgen crate version>
cargo build -p stapler-mcp-wasm --target wasm32-unknown-unknown
wasm-bindgen --target nodejs target/wasm32-unknown-unknown/debug/stapler_mcp_wasm.wasm --out-dir npm/pkg
cd npm && npm install && npm test
```

## License

MIT — see [LICENSE](./LICENSE).
