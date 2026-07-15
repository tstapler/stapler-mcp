# Build log and deferred work

This project was rewritten from Go to Rust so the same core logic could ship
two ways (native CLI + zero-native-binary `npx` package) without duplicating
the daemon architecture or tool logic. This file tracks what's done and
sketches what's left, so a future session can pick up without re-deriving
the plan. The full architecture plan (workspace layout, port traits, crate
choices) came out of a dedicated planning pass and is summarized in
`README.md`; this file is the phase-by-phase log plus deferred-work sketches.

## Done

- **Phase 1a** — Rust workspace scaffold (`crates/{core,native,cli}`),
  target-agnostic ports/traits, daemon dispatch, `EnsureDaemon`
  backoff/spawn state machine, native adapter (Unix socket,
  `std::fs::File::try_lock` — no `fs4`/`flock` crate needed, std covers it
  since Rust ~1.89 — detached process spawn). Verified via
  `crates/cli/tests/daemon_ping.rs`, mirroring the old Go integration test's
  five properties.
- **Phase 1b** — ported `fetch_page` (`chromiumoxide` instead of `chromedp`)
  and `brave_web_search` (`reqwest`) onto the daemon; wired the thin-client
  stdio side via `rmcp` (official Rust MCP SDK). Two real bugs found by
  actually running it, not just compiling: `Implementation::from_build_env()`
  expands `env!("CARGO_CRATE_NAME")` inside rmcp's *own* source, so it
  silently reported `rmcp`/`2.2.0` as the server identity — fixed by setting
  `Implementation` explicitly; Brave's base URL was hardcoded, made
  overridable via `BRAVE_API_BASE_URL` so tests can point at a mock server.
- **Phase 2** — `crates/wasm` (wasm-bindgen adapter, `wasm32-unknown-unknown`)
  + `npm/` (Node host). Every port has a JS-glue-backed implementation
  (`crates/wasm/src/glue/*.js`, copied into the wasm-bindgen build output as
  local snippets — this is how `#[wasm_bindgen(module = "/src/glue/x.js")]`
  externs actually get resolved under `--target nodejs`, discovered by
  probing before committing to the full build-out). Verified via
  `npm/test/e2e.test.js` (Node's built-in `node:test`) and manual
  cross-implementation interop in both directions (native daemon ↔ Node
  client, Node daemon ↔ native client — both work, confirming thin clients
  only ever need `ping` to succeed, never caring which implementation is on
  the other end).
  - Lock: no external npm dependency — `fs.mkdirSync` (atomic create) +
    liveness check via `process.kill(pid, 0)` on contention, instead of a
    `proper-lockfile` dependency (better than the timeout-based staleness
    check that package would have given: a real liveness check, no
    dependency).
  - Browser: `playwright-core` with `channel: "chrome"` (system Chrome, no
    download needed) rather than driving CDP directly from Rust for this
    target — matches upstream `playwright-mcp`'s own choice of library.
  - Schema: one `schemars` derivation on the shared core types
    (`crates/core/src/schema.rs`), exported as `list_tools_json()` and
    served verbatim by the Node side — never hand-authored twice.
  - **Real bug found by testing, not just compiling**: neither the JS
    `net.Server` (socket listener) nor the launched Chrome subprocess was
    ever closed on `shutdown`, so the Node daemon process — and, it turned
    out, the *native* daemon too (same gap, `chromiumoxide::Browser` was
    never `.close()`d either) — hung around forever after a clean shutdown.
    Fixed on both sides: `WasmListener`/`jsCloseListener` on `Drop`;
    `browser.close()` called explicitly after `daemon.run()` returns on both
    adapters (native needs `drop(daemon)` first so the `Rc<NativeBrowser>`
    clones held by handler closures release, making `Rc::get_mut` succeed).

- **Phase 3** — `crates/core/src/tools/webcrawl.rs`: merges the sketched
  `read_website` (Readability/Markdown extraction, SHA-256-keyed disk cache)
  and the third-party `website-downloader` (raw HTML to disk) into one
  shared BFS crawler (`Crawler`) with two output modes, exposed as
  `read_website`/`download_website`. Reused the existing `HttpClient`/
  `FileStore` ports — no new port trait beyond adding `FileStore::read_file`
  (needed for cache lookups; `write_file`-only wasn't enough once caching
  needed to skip re-fetching, not just re-parsing). Crate choices from the
  original plan all held up, including on `wasm32` (verified before wiring
  anything, given "medium confidence" flagged there): `dom_smoothie`
  (Readability-style extraction), `dom_query` (already a transitive
  dependency of `dom_smoothie` — reused directly for `<a href>` link
  extraction instead of adding `scraper` as a second HTML-parsing crate),
  `htmd` (HTML→Markdown), `texting_robots` (`robots.txt`), `sha2` (cache
  keys), `url` (link resolution). Verified via `crates/cli/tests/webcrawl.rs`
  and `npm/test/e2e.test.js` (same synthetic multi-page site + `robots.txt`
  in both), plus a manual interop check (Node client → native daemon,
  `read_website`).
  - Cache design: reworked mid-implementation so a cache hit skips the
    network fetch entirely, not just the Readability/Markdown re-parse —
    the first draft cached only the extracted result and still re-fetched
    every call, which undersold the whole point of caching. Trade-off this
    creates (documented in code): a cache hit doesn't rediscover that page's
    outgoing links, so crawl depth only expands from freshly-fetched pages.
    Tested directly: shut the mock server down between two calls, second
    call still succeeds (from cache) but returns exactly the one cached
    page, not the full crawl.
  - `save_path_for` (raw-HTML save path derived from a remote page's URL)
    explicitly strips `.`/`..` path segments — a real path-traversal
    boundary, not hypothetical, since the path comes from a possibly
    untrusted remote site.

- **Phase 4** — `crates/core/src/tools/docs.rs`: native-only (`#[cfg(not(target_arch
  = "wasm32"))]`) semantic search over crawled doc sources, replacing the
  Node-based `docs-mcp-server`. Reuses `webcrawl.rs`'s `Crawler` (bumped to
  `pub(crate)`) rather than a second crawl loop. Local embeddings via
  `fastembed`/`all-MiniLM-L6-v2` (`crates/native/src/embed.rs`, `Embedder`
  port trait, native-only for v1 per ADR-0001/ADR-0002) + brute-force cosine
  similarity — no vector DB. Four tools registered on the daemon and exposed
  over the thin client, **prefixed `stapler_` to avoid the exact tool-name
  collision** `docs-mcp-server` already had registered (`search_docs`) in
  `~/.claude.json`: `stapler_index_docs`, `stapler_search_docs`,
  `stapler_list_indexed_sources`, `stapler_remove_indexed_source`.
  - Storage: JSONL chunk records + JSON meta sidecar per source under
    `~/.stapler-mcp/docs-index/<source-id>/`, plus a `sources.json` manifest
    for enumeration. `MAX_CHUNKS_PER_SOURCE` is set from a real measured
    `fastembed` throughput benchmark (not guessed), sub-batched embedding
    with `tokio::task::yield_now().await` between batches so a long
    `index_docs` call doesn't stall the single-threaded daemon for its
    entire duration.
  - `SourceLocks` (in-memory per-source guard) prevents a concurrent
    `index_docs`/`remove_indexed_source` pair on the same source from
    interleaving their writes — found necessary in adversarial review as a
    normal-operation risk (two related tool calls fired close together by
    an LLM caller), not just a daemon-crash edge case.
  - `NativeFs::write_file` was made atomic (temp-file + rename, per-call-
    unique temp filename) as part of this work — a general fix that also
    benefits `read_website`'s existing page cache, not just docs-index.
  - **Security fix found in verification, not planning**: a caller-supplied
    `source` name that slugifies to an empty string (e.g. `"..."`) collided
    with every other empty-slug source on the same two on-disk files,
    letting one garbage-input call silently clobber another's data. Fixed
    with a guard rejecting empty-slug source names in `index_source`/
    `remove_indexed_source`, plus regression tests.
  - Manual relevance spot-check (real, not simulated) run against
    `https://tokio.rs/tokio/tutorial` (19 pages, 524 chunks): 4 of 5
    realistic queries had genuinely on-topic top results (spawning,
    sharing state between tasks, and channels all scored highly and were
    directly relevant); the "Mutex vs RwLock" query only surfaced
    Mutex-related content — not an embedding-model failure, the tutorial
    simply doesn't cover `RwLock`, so the model correctly found the closest
    available match. Verdict: **relevance spot-checked, acceptable** for
    `all-MiniLM-L6-v2` on real Rust/Tokio documentation.
  - Pre-existing SSRF-class risk (the crawler has no loopback/private-IP
    blocklist on the seed URL) is inherited unchanged from `read_website`/
    `download_website` — not introduced or worsened by this feature, and
    out of scope to fix here.

## Deferred

### `playwright-mcp` equivalent — browser automation tools

Full browser automation via accessibility-tree snapshots (navigate, click,
type, snapshot). The `BrowserDriver` port already established for
`fetch_page` needs extending with persistent-tab/session semantics — unlike
`fetch_page` (one-shot, fresh tab per call), automation needs a `session_id`
threaded across `navigate` → `click` → `type` → `snapshot` calls, plus an
idle-timeout reaper for abandoned sessions.

Real, asymmetric cost to flag: on the Node side, `playwright-core` gives
accessible-role/name locators for free; on the native side, `chromiumoxide`
only exposes the raw CDP `Accessibility` domain, so the native adapter has
to implement its own AX-tree walk + role/name resolution to match what Node
gets for free. Budget for this explicitly, don't assume parity between the
two adapters here.

### `docs-mcp-server` equivalent — done (Phase 4), narrower scope by design

The native, single-doc-format v1 shipped as Phase 4 (`docs.rs`,
`stapler_index_docs`/`stapler_search_docs`/`stapler_list_indexed_sources`/
`stapler_remove_indexed_source`) — see the "Done" section above. It does not
match the original `@arabold/docs-mcp-server`'s full scope (90+ format
parsers, multi-provider embeddings, a web UI): it's Markdown/HTML-via-crawl
only, one pinned local embedding model, no UI. `docs-mcp-server` itself is
still connected as of this writing (`~/.claude.json`'s `"docs"` entry) —
disconnecting it, and deciding whether the wider format/provider surface is
ever worth adding, are open follow-ups, not blocked on anything technical
(the tool-name collision that made coexistence awkward is resolved via the
`stapler_` prefix above).

### npm packaging/publishing polish (Phase 5)

- Wire `wasm-pack build --target nodejs` (or the plain `cargo build
  --target wasm32-unknown-unknown` + `wasm-bindgen` CLI fallback actually
  used so far) into CI, so `npm/pkg/` is a release artifact, never hand-built
  by an end user.
- Opportunistic native-binary fast path: before spawning the Node-hosted
  daemon, check whether a `cargo install`ed native `stapler-mcp` binary is
  already on `PATH`, and prefer spawning that instead (real `flock`, real
  `chromiumoxide`, multi-core) — safe because a binary the user built
  themselves was never downloaded/quarantined, so it doesn't reintroduce the
  Gatekeeper problem the whole wasm/Node distribution exists to avoid.
- Publish to npm; real cross-implementation interop is already proven, this
  phase is packaging/CI/docs, not new architecture.

## Non-goals (for now)

- No Windows support — the native adapter's Unix domain sockets are
  POSIX-only. Fine for this user's Linux/macOS machines; would need a
  named-pipe transport to support Windows. (The Node adapter's `net` module
  is cross-platform, but hasn't been tested on Windows either.)
- No TLS/auth on the socket — filesystem permissions on `~/.stapler-mcp/`
  (0700) are the only access control. Acceptable for a single-user,
  single-machine daemon; would need revisiting if this ever became
  multi-user or networked.
