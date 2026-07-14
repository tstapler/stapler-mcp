# Deferred work: P1 and P2 tools

This project is scoped to proving the daemon/thin-client architecture and
shipping the two trivial P0 tools (`fetch_page`, `brave_web_search`), not
porting every third-party MCP server this user runs today. This file is
the design sketch for what's left, so a future session can pick each item
up without re-deriving the plan.

## P1: `mcp-read-website-fast` equivalent — `read_website`

Original: fetch → Readability-style content extraction → Markdown, with a
SHA-256-keyed disk cache and depth-limited crawl, respecting `robots.txt`.

Design sketch for a daemon-side `internal/tools/readweb` package:

- Reuse the daemon's shared `fetch.Fetcher` (chromedp) for JS-rendered
  pages, or a plain `net/http` GET for the common static-HTML case —
  decide based on a quick heuristic (e.g. try HTTP first, fall back to
  chromedp if the response looks like an SPA shell) to avoid paying
  browser-render cost on every call.
- Extraction: `go-shiori/go-readability` (Go port of Mozilla's Readability)
  is the most direct analog; feed its output through
  `github.com/JohannesKaufmann/html-to-markdown` for the Markdown step.
- Cache: content-addressed by `sha256(url)` (or `sha256(url + fetched-at
  day)` if freshness matters) under `~/.stapler-mcp/cache/read-website/`.
  This is exactly the kind of state that belongs daemon-side — a per-session
  cache would defeat the point.
- `robots.txt`: fetch and parse once per host per daemon lifetime, cache
  the parsed result in memory (a `sync.Map[host]*robotstxt.RobotsData`
  using `github.com/temoto/robotstxt`), check before crawling.
- Depth-limited crawl: BFS from the seed URL, respecting `robots.txt` and a
  `maxDepth`/`maxPages` input field; reuse the same extraction path per page.

## P1: `playwright-mcp` equivalent — browser automation tools

Original: full browser automation via accessibility-tree snapshots
(navigate, click, type, snapshot).

`internal/tools/fetch.Fetcher` already establishes the shared chromedp
allocator this needs — the daemon should own exactly one browser process
serving both `fetch_page` and this tool set, not a second one.

Design sketch:

- Add persistent-tab semantics: unlike `fetch_page` (one-shot, new tab per
  call), automation tools need a session concept — a `session_id` the
  client passes across `navigate` → `click` → `type` → `snapshot` calls, so
  the daemon can hold the `chromedp.Context` open between calls instead of
  tearing it down. Needs a `map[string]context.CancelFunc` + idle-timeout
  reaper (goroutine that closes tabs unused for e.g. 10 minutes) so
  abandoned sessions don't leak browser tabs forever.
- Accessibility-tree snapshot: chromedp exposes the CDP `Accessibility`
  domain directly (`github.com/chromedp/cdproto/accessibility`) — no need
  for a separate library, just walk `accessibility.GetFullAXTree`.
- Tools to add: `browser_navigate`, `browser_click` (by accessible
  name/role, not CSS selector — matches upstream playwright-mcp's design
  and is more LLM-friendly than raw selectors), `browser_type`,
  `browser_snapshot`.
- New IPC concern: these calls are inherently stateful/sequential per
  session, unlike the current one-shot request/response tools — worth
  revisiting whether the wire protocol needs a `session_id` field on
  `ipc.Request` at that point, or whether per-session tool names
  (`browser_click:session-abc`) is simpler. Decide when actually building
  this, not now.

## P2: `docs-mcp-server` equivalent — explicitly deferred, not attempted

Original (`@arabold/docs-mcp-server`): 90+ format parsers, vector
embeddings across multiple providers, semantic search, a web UI, a SQLite
index. This is a full product, not a trivial wrapper — reimplementing it
is out of scope for this task and likely out of scope for a while.

If ever revisited: this is the one tool where "daemon owns the state" is
most obviously correct (a SQLite index and embedding cache are exactly the
kind of thing that must not be duplicated per-subagent), but the surface
area (format parsers, embedding provider abstraction, semantic search
ranking) is large enough to warrant its own planning pass
(`/plan:mdd-start`) rather than a NOTES.md sketch.

## Non-goals (for now)

- No Windows support — Unix domain sockets + `flock` are POSIX-only. Fine
  for this user's Linux/macOS machines; would need a named-pipe transport
  to support Windows.
- No TLS/auth on the Unix socket — filesystem permissions on
  `~/.stapler-mcp/` (0700) are the only access control. Acceptable for a
  single-user, single-machine daemon; would need revisiting if this ever
  became multi-user or networked.
