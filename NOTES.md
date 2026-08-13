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

- **Phase 5** — `crates/core/src/tools/browser.rs` plus native
  (`crates/native/src/browser.rs`, `crates/native/src/ax.rs`) and wasm
  (`crates/wasm/src/browser.rs`, `crates/wasm/src/glue/browser.js`) adapters:
  `playwright-mcp`-style browser automation via accessibility-tree snapshots
  — `stapler_browser_navigate`/`stapler_browser_click`/`stapler_browser_type`/
  `stapler_browser_snapshot`. Extends `BrowserDriver` with session-scoped
  `navigate`/`click`/`type_text`/`snapshot` and a `SessionId`/`Locator`/
  `AxNode`/`AxSnapshot` vocabulary threaded across calls, plus
  `PortError::NotFound`/`SessionCrashed`. Confirms the asymmetry flagged when
  this was deferred: `chromiumoxide` only exposes the raw CDP `Accessibility`
  domain, so the native adapter got its own hand-rolled AX-tree walker
  (`ax.rs`) while the wasm/`playwright-core` adapter got accessible-role/name
  locators for free.
  - Session lifecycle is the same shape on both adapters: an in-memory
    session registry, a 300s idle-timeout reaper, and
    `Target.targetCrashed`/`page.on('crash')`-based crash detection that
    surfaces as an actionable `SessionCrashed` error on the next call instead
    of hanging. Open-session caps differ and are left un-reconciled: native's
    `MAX_OPEN_SESSIONS = 20` (`browser.rs`) vs. wasm's `MAX_SESSIONS = 50`
    (`glue/browser.js`); both cap accessibility-tree walks at
    `MAX_SNAPSHOT_NODES = 500`.
  - **Real bugs found by testing against real Chrome, not by compiling or
    reviewing**: an AX-tree walker that minted a fresh `ref` string on every
    capture made a `ref` returned by one call unresolvable by the very next
    call on the same still-open page — fixed by threading
    `previous_refs`/`previous_by_backend_id` so a node's ref is reused when
    its `BackendNodeId` is recognized from the prior capture, while a genuine
    re-navigation still gets a fresh map. The SSRF re-check only watched
    `Page.frameNavigated` (committed URL), which never fires for a navigation
    to an unreachable link-local host since Chrome commits to its own error
    page instead of the attempted destination — fixed by adding a second
    listener on `Page.frameRequestedNavigation` (intended destination, fires
    regardless of success). Both found by the `#[ignore]`d real-Chrome tests
    in `crates/cli/tests/browser_session.rs` (8 tests, all passing), not by
    unit tests against a mock.
  - **Concurrency/SSRF races found in adversarial PR review**, not by initial
    testing: a same-call redirect to a blocked host (e.g. the cloud metadata
    service) could leak a full snapshot before the block was surfaced;
    concurrent calls against the same session could interleave, or let the
    idle reaper close a session mid-call — closed with a per-session
    lock/mutex serializing calls on both adapters, reused as a busy signal so
    the reaper skips in-flight sessions; a soft check-then-insert race let
    concurrent no-`session_id` `navigate()` calls overshoot
    `MAX_OPEN_SESSIONS` — closed with `NewSessionSlotGuard`, an RAII slot
    reservation taken before the first await. Also closed matching
    IPv4-mapped/IPv4-compatible IPv6 SSRF gaps on both adapters, and
    tightened `webcrawl.rs`'s own guard to match (full `0.0.0.0/8`, legacy
    `::a.b.c.d` form) as a byproduct.
  - Verified via `crates/cli/tests/browser_session.rs` (8 tests gated behind
    real Chrome, `#[ignore]`d in CI, all passing locally) and
    `npm/test/browser_glue.test.js` (21/21 passing, up from 14/14 before the
    adversarial-review fixes).

## Deferred

One item below is narrow enough that it's tracked as a GitHub issue rather
than fully re-derived here — see the issue for current status/discussion,
this file just gives the pointer. The other two originally tracked here
(wasm `final_url`, npm CI/packaging) have since been fixed and closed; kept
below for the historical record.

### docs-index scope: wider format support, multi-provider embeddings, ANN index

The native, single-doc-format v1 shipped as Phase 4 (`docs.rs`,
`stapler_index_docs`/`stapler_search_docs`/`stapler_list_indexed_sources`/
`stapler_remove_indexed_source`) — see the "Done" section above. It does not
match the original `@arabold/docs-mcp-server`'s full scope (90+ format
parsers, multi-provider embeddings, a web UI): it's Markdown/HTML-via-crawl
only, one pinned local embedding model, no UI, brute-force cosine search.
That legacy server itself was disconnected from `~/.claude.json` in
[issue #2](https://github.com/tstapler/stapler-mcp/issues/2). Whether the
wider format/provider/ANN-index surface is ever worth adding is tracked as
[issue #10](https://github.com/tstapler/stapler-mcp/issues/10) — explicitly
"no action yet," revisit only if the `search_docs` latency benchmark
([#5](https://github.com/tstapler/stapler-mcp/issues/5) — closed; measured
~7ms/call release at `MAX_CHUNKS_PER_SOURCE` scale, comfortably sub-second)
stops holding at real-world scale, or a concrete non-Markdown/non-local-
embedding need shows up.

### wasm `final_url` on redirect — fixed

`crates/wasm/src/http.rs`'s adapter didn't populate `final_url` after a
redirect the way the native adapter does, so `sourceUrl` metadata on a
redirected docs-index source was the pre-redirect URL on the wasm side only.
Fixed by returning `resp.url` from the `fetch()` glue and reading it via
`Reflect::get`. Was [issue #9](https://github.com/tstapler/stapler-mcp/issues/9) — closed.

### npm packaging/publishing polish (Phase 6) — CI + fast path done, publish still deferred

- Wire `wasm-bindgen` into CI, so `npm/pkg/` is built fresh every run instead
  of hand-built by an end user — done, CI's `wasm` job now runs
  `wasm-bindgen` (pinned to the `wasm-bindgen` crate's `Cargo.lock` version)
  followed by `npm ci && npm test`.
- Opportunistic native-binary fast path: before spawning the Node-hosted
  daemon, check whether a `cargo install`ed native `stapler-mcp` binary is
  already on `PATH`, and prefer spawning that instead (real `flock`, real
  `chromiumoxide`, multi-core) — safe because a binary the user built
  themselves was never downloaded/quarantined, so it doesn't reintroduce the
  Gatekeeper problem the whole wasm/Node distribution exists to avoid. Done
  in `crates/wasm/src/glue/process.js`'s `findNativeBinary`.
- Publish to npm — still not done. Deliberately left out: claiming a public
  package name is a hard-to-reverse, externally-visible action that needs
  explicit sign-off and registry credentials, not something to do as a
  matter of course. `npm/package.json` keeps `"private": true` as a guard
  against an accidental publish in the meantime.

Was [issue #8](https://github.com/tstapler/stapler-mcp/issues/8) — closed
with the publish sub-part explicitly left undone; re-open (or file a fresh
issue) if actual registry publishing is ever wanted.

## Non-goals (for now)

- No Windows support — the native adapter's Unix domain sockets are
  POSIX-only. Fine for this user's Linux/macOS machines; would need a
  named-pipe transport to support Windows. (The Node adapter's `net` module
  is cross-platform, but hasn't been tested on Windows either.)
- No TLS/auth on the socket — filesystem permissions on `~/.stapler-mcp/`
  (0700) are the only access control. Acceptable for a single-user,
  single-machine daemon; would need revisiting if this ever became
  multi-user or networked.
