# stapler-mcp

A from-scratch, single-binary reimplementation of several third-party MCP
(Model Context Protocol) servers this user previously ran per-session via
`npx`/`uvx`. Those tools were causing severe memory bloat: every subagent
in a Claude Code session independently spawned its own full copy of every
MCP server (confirmed via `pstree` on a live 9-subagent tree — 9 separate
process trees for the same tools), and `npx`/`uvx` each pay 60-100MB of
cold-resolve wrapper overhead per launch on top of the real server. One
tool in particular (`mcp-website-downloader`, invoked via
`uvx --from git+...`) left 40+ fully-duplicated installs in `~/.cache/uv`
(9.7GB) because `uvx` re-resolves per invocation instead of reusing a
cached environment.

## Architecture: thin client + shared daemon

This is the entire point of the project, not an optional nicety.

```
Claude Code session 1 ──▶ stapler-mcp (thin stdio MCP server) ─┐
Claude Code session 2 ──▶ stapler-mcp (thin stdio MCP server) ─┼──▶ ~/.stapler-mcp/daemon.sock ──▶ stapler-mcp --daemon
Claude Code subagent N ─▶ stapler-mcp (thin stdio MCP server) ─┘         (one process, one browser pool,
                                                                           one HTTP client, one cache —
                                                                           shared machine-wide)
```

- **`stapler-mcp`** (no flags) — what Claude Code actually launches per
  session as the MCP server. On startup it checks whether the daemon is
  already reachable at `~/.stapler-mcp/daemon.sock`; if not, it spawns
  `stapler-mcp --daemon` detached and waits for it to become ready. Every
  tool call is then proxied over that Unix socket and the result streamed
  back over stdio. This process holds no heavyweight state of its own.
- **`stapler-mcp --daemon`** — the long-running background process that
  owns the actual heavyweight state (headless browser pool, HTTP clients,
  caches). Exactly one instance runs machine-wide. If several thin
  clients race to auto-start it simultaneously, every spawned process
  independently attempts an exclusive `flock` on `~/.stapler-mcp/daemon.lock`
  — only the winner binds the socket, the rest see `ErrAlreadyRunning` and
  exit immediately (see `internal/daemon/lock.go`).

This mirrors a fix already underway in the sibling project `stapler-squad`
(a Go AI-session-manager, also owned by this user), where its own `--mcp`
subcommand is moving from "duplicate the whole backend per subagent" to "thin
client of the one already-running service" — same pattern, different
transport (Unix socket here vs. an already-running HTTP service there).
`stapler-mcp` is an independent repository and does not depend on or modify
`stapler-squad`.

### Why a Unix domain socket

Simple, fast, local-only, no port-conflict risk across concurrent sessions.
Liveness/single-instance is a lockfile (`flock`), not "is the socket file
present" (stale sockets from a crashed daemon are safe to unlink once a new
daemon holds the lock).

### MCP protocol layer

Uses the official `github.com/modelcontextprotocol/go-sdk` (maintained in
collaboration with Google) for the stdio JSON-RPC transport and typed
tool schemas, rather than hand-rolling the protocol.

### Why chromedp

`fetch_page` (and the planned P1 browser-automation tools) render pages via
[`chromedp`](https://github.com/chromedp/chromedp) — pure Go, drives a
system Chrome/Chromium binary directly over the DevTools protocol. No
Node.js, no Playwright driver install. `internal/tools/fetch` establishes
this dependency once, daemon-side, so a later playwright-mcp-equivalent
tool set can reuse the same shared browser allocator instead of spinning up
its own.

## Tools implemented (P0)

| Tool | Package | Notes |
|---|---|---|
| `fetch_page` | `internal/tools/fetch` | Headless-render a URL via chromedp, return title + extracted text, optionally save rendered HTML to a local path (`savePath`). Requires a Chrome/Chromium binary discoverable by chromedp on the daemon's machine. |
| `brave_web_search` | `internal/tools/search` | Stateless HTTP wrapper over the Brave Search API. Reads `BRAVE_API_KEY` from the **daemon's** environment (set it wherever `stapler-mcp --daemon` ends up running, e.g. its launchd/systemd unit or shell profile). Routed through the daemon for architectural consistency even though it holds no state worth sharing. |

Both were verified end-to-end against the real compiled binary: the daemon
was started, a client dialed its Unix socket, `fetch_page` rendered
`https://example.com` and returned real title/text, and `brave_web_search`
returned a clean `"BRAVE_API_KEY is not set"` error (not a crash) when the
key was absent.

## Deferred (P1 / P2)

See [`NOTES.md`](./NOTES.md) — `mcp-read-website-fast` and
`playwright-mcp`-style browser automation are P1 (design sketched, not
implemented); `docs-mcp-server`-equivalent doc indexing is explicitly
deferred as a full product, not attempted here.

## Usage

```bash
go build -o stapler-mcp ./cmd/stapler-mcp

# What Claude Code should register as the MCP server command:
./stapler-mcp

# Manual daemon inspection (normally auto-started, not run by hand):
./stapler-mcp --daemon
```

Point Claude Code's MCP config at the built `stapler-mcp` binary with no
arguments — it will transparently auto-start (or reuse) the shared daemon.

`BRAVE_API_KEY` must be set in the environment the **daemon** process
inherits when it's spawned (i.e. wherever the first `stapler-mcp` thin
client happens to run from, since that's what execs `--daemon`) — or
export it globally before any session starts.

### State layout

Everything lives under `~/.stapler-mcp/` (override with `STAPLER_MCP_HOME`,
primarily for tests):

- `daemon.sock` — the Unix socket
- `daemon.lock` — single-instance flock + PID
- `daemon.log` — stdout/stderr of the detached daemon (it has no
  controlling terminal once spawned)

## Development

```bash
go build ./...
go test ./...     # includes a real subprocess-spawning daemon auto-start
                   # + IPC round-trip test (internal/daemonclient)
go vet ./...
gofmt -l .
```

## License

MIT — see [LICENSE](./LICENSE).
