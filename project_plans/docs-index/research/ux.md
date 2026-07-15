# Research: MCP Tool UX for `docs-index`

**Audience**: an LLM coding agent (Claude Code) calling these tools on Tyler's behalf, plus Tyler typing natural-language requests that Claude Code translates into tool calls. "UX" here means tool ergonomics, not GUI/accessibility.

## 1. Existing `stapler-mcp` conventions (internal precedent)

Source: `crates/core/src/schema.rs`, `crates/cli/src/thin_client.rs`, `crates/core/src/tools/webcrawl.rs`.

**Naming**: `verb_noun`, snake_case, terse, one tool = one verb (`fetch_page`, `read_website`,
`download_website`, `brave_web_search`). No namespacing prefix beyond the verb itself. New tools
should follow the same shape: `index_docs`, `search_docs`, `list_indexed_sources`,
`remove_indexed_source` all fit this pattern directly.

**Struct naming**: `{ToolNameInPascalCase}Input` / `Output` pairs in `schema.rs`, one `schemars`
derivation shared between the native `rmcp` registration and the wasm/Node side — "one authored
definition, not two." A `docs-index` feature should add `IndexDocsInput/Output`,
`SearchDocsInput/Output`, etc. to the same file rather than a separate module, to keep the
single-source-of-truth property.

**JSON field casing**: `#[serde(rename_all = "camelCase")]` on every struct — wire format is
camelCase even though Rust fields are snake_case. Tool *descriptions* explicitly reference the
camelCase field names inline (e.g. "crawling same-host links up to maxDepth/maxPages") so the
calling LLM can match a description to the schema's field name without cross-referencing.

**Doc comments as descriptions**: field-level `///` comments state defaults and caps as concrete
numbers directly in the text the LLM sees, e.g. "How many link-hops to follow from the seed page,
defaults to 1" and "Maximum number of pages to fetch across the whole crawl, defaults to 10,
capped at 50." Tool-level descriptions (the `#[tool(description = "...")]` string) are one dense
sentence: what it does, key parameter behavior, and any caching/state note ("Cached by URL on the
daemon"). `index_docs`/`search_docs` descriptions should follow this density and state their
caps/defaults the same way.

**Limits are baked in, not just documented**: `resolve_limits()` in `webcrawl.rs` clamps
`max_depth`/`max_pages` server-side (`DEFAULT_MAX_DEPTH = 1`, `DEFAULT_MAX_PAGES = 10`,
`MAX_PAGES_CEILING = 50`, `MAX_DEPTH_CEILING = 5`) regardless of what the caller passes — the LLM
can't accidentally trigger a runaway crawl by passing a huge number. `index_docs` should clamp
`maxPages`/`maxDepth` the same way (it will directly reuse `Crawler` from `webcrawl.rs` per the
requirements, so this comes for free if `index_docs` is built on top of the same
`resolve_limits`-style clamp rather than a fresh one).

**Errors are plain strings**: every tool handler returns `Result<Json<Output>, String>`; the
daemon-call plumbing wraps lower errors with a short prefix (`format!("ensure daemon: {e}")`).
There's no structured error code/type today — errors are one human/LLM-readable sentence. New
tools should keep doing this (see §4 for what the sentences should say for docs-index-specific
failure modes), rather than introducing a new structured-error convention just for this feature.

**Caching precedent directly relevant to `index_docs`**: `read_website`/`download_website` already
have a SHA-256-URL-keyed disk cache where a cache hit skips the network fetch entirely (reworked
mid-implementation in Phase 3 specifically because caching only the parsed result, not the fetch,
"undersold the whole point of caching" — see `NOTES.md`). `index_docs` re-indexing an
already-indexed URL is the same shape of problem: decide once whether "index again" means
skip-if-cached, always-refetch, or diff-and-update, and document it in the tool description the
same way `read_website`'s description says "Cached by URL on the daemon." Don't leave it
ambiguous — see §4.

**Deferred-work note already exists**: `NOTES.md` (lines ~110-118) explicitly flagged
`docs-mcp-server` as "the one tool where 'daemon owns the state' is most obviously correct (a
SQLite index and embedding cache must not be duplicated per-subagent)." This confirms the daemon
(not the thin client) should own the vector index/embedding state, consistent with how
`read_website`'s cache already lives daemon-side.

## 2. Comparable tools: docs-mcp-server (live schema) and Context7

### docs-mcp-server (`arabold/docs-mcp-server`, Tyler's actual fork, live tool schema observed via this session's `mcp__docs__*` connection)

Ten tools, not two:

| Tool | Params | Notes |
|---|---|---|
| `search_docs` | `library` (req), `query` (req), `version?`, `limit=5` | Searches by **library name**, not URL — decouples querying from indexing. |
| `scrape_docs` | `url` (req), `library` (req), `version?`, `maxDepth=3`, `maxPages=1000`, `scope=subpages\|hostname\|domain`, `followRedirects=true`, `preserveHashes?` | Indexing is **asynchronous** — kicks off a job, does not block until done. |
| `list_libraries` | none | Enumerates what's indexed. |
| `find_version` | `library` (req), `targetVersion?` | npm-style X-range matching (`"5.x"`, `"5.2.x"`) — supports multiple indexed versions of the same library side by side. |
| `remove_docs` | `library` (req), `version?` (omit = removes latest) | |
| `refresh_version` | `library` (req), `version?` | Re-scrapes, "updating only changed pages" — explicit re-index verb, distinct from `scrape_docs`. |
| `list_jobs` | `status?` (`queued\|running\|completed\|failed\|cancelling\|cancelled`) | Job-queue introspection. |
| `get_job_info` | `jobId` (uuid, req) | |
| `cancel_job` | `jobId` (uuid, req) | |
| `fetch_url` | `url` (req), `followRedirects=true` | Single-page fetch-to-Markdown, entirely separate from the indexing/search system — this is docs-mcp-server's `read_website` equivalent, already covered by stapler-mcp's own `read_website`. |

**Takeaways for `docs-index`**:

- **Library-name identity, not raw URL, as the search key.** `search_docs(library, query)` lets
  the LLM (and Tyler) refer to "the tokio docs" by name instead of remembering/retyping the exact
  seed URL. Given the requirements call for a narrower single-user tool, `docs-index` should adopt
  a lightweight version of this: `search_docs` should take a `source` name (a short slug Tyler
  chooses or that's derived from the URL at index time — e.g. `tokio-tutorial`), not require the
  full URL again. Requiring the URL again on every search is a real friction point being designed
  away here — Context7 solves the analogous problem (see below) with a two-step
  resolve-then-fetch; docs-mcp-server solves it by having the LLM just remember the library name it
  used to index. The single-user, small-number-of-sources scope in requirements.md favors
  docs-mcp-server's simpler flat-name approach over Context7's resolve step.
- **Async job queue is real complexity, and may be overkill here.** Five of the ten tools exist
  purely to support `scrape_docs` being async (`list_jobs`, `get_job_info`, `cancel_job`, plus the
  job-shaped fields on `scrape_docs` itself, plus `refresh_version` as a related but distinct
  verb). This makes sense for docs-mcp-server's default `maxPages=1000` — indexing at that scale
  can run for many minutes. `docs-index`'s requirements describe "a handful of doc sources" and
  the crawler being reused (`webcrawl.rs`) already caps at `MAX_PAGES_CEILING = 50` — an
  order of magnitude smaller. **Recommendation**: don't build a job queue for v1. Make
  `index_docs` synchronous (same blocking-call shape as `read_website`/`download_website` today)
  and rely on the existing 50-page ceiling to keep indexing calls fast enough that async
  round-trips aren't worth the added tool-surface complexity. Revisit only if real usage shows
  indexing runs long enough that Claude Code's tool-call timeout becomes a problem.
- **Re-index is a distinct verb (`refresh_version`), not an implicit side effect of
  `scrape_docs`/`index_docs`.** This is the cleanest resolution to the "re-index an
  already-indexed URL" ambiguity flagged in requirements.md §5 (error states). See §4 below.
- **`fetch_url` (single page) is kept fully separate from the indexing/search subsystem.**
  stapler-mcp already has this split (`read_website` vs. whatever `index_docs` becomes) — good
  confirmation the existing architecture boundary is right, no need to unify them.

### Context7 (`upstash/context7-mcp`)

Two tools: `resolve-library-id` (name → Context7-compatible ID, e.g. `/vercel/next.js`) then
`get-library-docs` (ID + optional `topic` + `tokens` budget → docs). The two-step resolve pattern
exists because Context7 indexes a huge, ambiguous, multi-tenant namespace (many libraries, many
near-duplicate names) — resolving avoids the LLM guessing a wrong/stale ID. **Not needed for
`docs-index`**: Tyler's index is small and single-user, so a flat source-name lookup (matching
docs-mcp-server's simpler model, per above) is the right scope — a resolve step would be pure
overhead here.

Response shape (confirmed via search — Context7's public API/MCP schema): results come back as
`codeSnippets` (`codeTitle`, `codeDescription`, `codeLanguage`, `codeId`, `pageTitle`, `codeList`)
and `infoSnippets` (`pageId`, `breadcrumb`, `content`). The `breadcrumb` field is explicitly a
heading path within the source page — this is the citation-relevant part: it lets the calling LLM
say "per the **Configuration > Environment Variables** section of the Next.js docs" instead of
just dumping a bare paragraph. `pageTitle`/`pageId` give a stable pointer back to the source page.
This is the strongest external evidence for what `search_docs`'s response shape should include
(see §5).

## 3. Minimal tool surface for Tyler's mental model

Requirements.md's example phrasings: "index the Rust async book," "search my indexed docs for X."
Given the narrower single-user scope (a handful of sources, synchronous indexing, no version
matrix), four tools cover the full mental model without forcing ID-guessing or multi-round-trips:

1. **`index_docs`** — `url` (req), `source` (optional human name/slug; if omitted, derive one from
   the URL's host+path, e.g. `tokio.rs/tokio/tutorial` → `tokio-tutorial`), `maxDepth?`,
   `maxPages?` (same defaults/ceilings as `read_website`/`download_website` — reuse
   `resolve_limits`). Returns the resolved `source` name in the output so the LLM can immediately
   use it in a follow-up `search_docs` call without a round trip to `list_indexed_sources` first.
2. **`search_docs`** — `source` (req — the name from `index_docs`'s output or
   `list_indexed_sources`), `query` (req), `limit?` (default ~5, matching docs-mcp-server's
   default). See §5 for response shape.
3. **`list_indexed_sources`** — no params. Returns each indexed source's name, seed URL, page
   count, and last-indexed timestamp. This is the tool that lets the LLM answer "what have I
   indexed?" and resolve a source name Tyler references loosely ("my tokio docs") to the exact
   stored name, without Tyler having to remember exact slugs.
4. **`remove_indexed_source`** — `source` (req). Matches docs-mcp-server's `remove_docs`.

**Explicitly not needed for v1** (keeps the surface at 4 tools, not docs-mcp-server's 10):
a job-queue trio (no async indexing, see §2), a separate `refresh`/`resolve` tool (fold "re-index"
into `index_docs` itself — see §4), version-range matching (single version per source; if Tyler
wants to re-index a newer version of the same docs, that's a `remove_indexed_source` +
`index_docs` pair, or `index_docs` overwriting in place — a decision, not a gap, see §4).

This also directly answers requirements.md's question "what's the minimal tool surface... that
supports natural phrasing without the LLM needing many round-trips or guessing IDs?" — the
`source` name being both an `index_docs` output and a `search_docs`/`remove_indexed_source` input
is the piece that avoids ID-guessing (no opaque IDs anywhere in this surface, unlike Context7's
`/org/project` IDs, which aren't needed at this scale).

## 4. Error states and recovery-oriented messages

Following the existing convention (`Result<Json<Output>, String>`, one human/LLM-readable
sentence, no structured error codes):

- **`search_docs` on a `source` that was never indexed**: don't just say "not found" — the LLM's
  next move should be either indexing it or picking the right name. Error string should include
  the list of currently-indexed source names inline (small scope, so this is cheap: "a handful of
  doc sources"), e.g. `"no indexed source named 'tokio'; currently indexed: tokio-tutorial,
  serde-guide. Call list_indexed_sources for details, or index_docs to add a new source."` This
  saves a round trip versus forcing the LLM to call `list_indexed_sources` separately after every
  miss.
- **`index_docs` on an already-indexed URL**: **re-index in place, don't error and don't silently
  skip.** Silent skip is the worst option for an LLM caller — "index the tokio docs" issued a
  second time (e.g. because Tyler suspects the docs changed) should not silently no-op with no
  signal. Erroring ("already indexed, use refresh instead") adds a round trip for the single most
  natural repeat action a user takes. Re-indexing in place matches `read_website`'s existing
  cache-by-URL precedent (a fresh call naturally supersedes stale cache) and avoids needing a
  fifth tool. The output should say what changed: `"re-indexed tokio-tutorial: 12 pages (3
  unchanged, 9 re-fetched, 0 removed)"` — mirroring docs-mcp-server's `refresh_version` framing
  ("updating only changed pages") without needing a separate tool to get that behavior.
- **A doc source 404s / disappears on re-index**: don't fail the whole `index_docs` call if the
  *seed* URL still resolves but some previously-indexed *sub-pages* now 404 — drop those pages
  from the index and report it: `"re-indexed tokio-tutorial: 10 pages (2 removed: no longer
  reachable at /old-page, /another-page)"`. If the *seed* URL itself 404s, fail clearly with the
  URL and status: `"failed to index https://example.com/docs: 404 Not Found. Check the URL is
  still correct."` — this mirrors `fetch_page`/`read_website`'s existing failure mode (they
  already have to handle unreachable URLs) so no new error-shape precedent is needed.
- **Embedding model fails to load**: this is an operational/daemon-startup failure, not a
  per-call/per-URL one — it should fail *fast and identically* on every call to `index_docs` or
  `search_docs` until fixed (not retried/hidden), with a message that names the actual problem
  (model file missing/corrupt, out of memory, etc.) rather than a generic "internal error," since
  Tyler is the one who will read this and needs to know whether to re-download a model file, free
  memory, or file a bug. Given local embeddings run inside the daemon (no cloud API key per
  constraints), this is the daemon-startup-health-check category, analogous to how
  `brave_web_search` already surfaces a clear message when `BRAVE_API_KEY` is missing rather than
  a generic failure.

## 5. Response shape for `search_docs`

Must let the LLM caller quote the right thing back to Tyler **with a working link** — this is the
explicit requirement. Based on Context7's `infoSnippets` (`breadcrumb` + `content` + page pointer)
being the strongest precedent for citation-friendly chunking, and `ReadWebsitePage`'s existing
`{url, title, markdown}` shape in `schema.rs` being the closest internal precedent, each result
should carry:

- `text` — the matched chunk itself (not a full page — matches "return ranked relevant
  chunks/pages" from requirements.md's Must-Have).
- `score` — relevance score, so the LLM can decide how much to trust/surface a given result and
  can stop consuming results once relevance drops off, without needing a second round trip to find
  out there's nothing more useful.
- `sourceUrl` — the exact page URL the chunk came from (not just the source's seed URL) — this is
  what makes the returned link "working": if a source was crawled to depth >0, results can come
  from any crawled sub-page, and the LLK needs the specific page, not the seed.
- `heading` (or `breadcrumb`, matching Context7's naming) — the nearest heading/section title
  above the chunk in the source document, giving the LLM something better to cite than a bare
  paragraph ("per the **Async I/O** section of the Tokio tutorial...").
- `sourceTitle` — the page's `<title>` (reusing exactly what `ReadWebsitePage.title` already
  captures from `read_website`) so the LLM can name the doc, not just link it.

This is a direct, additive extension of `ReadWebsitePage {url, title, markdown}` — add `score` and
`heading` and rename `markdown` to `text` (chunk-scoped, not full-page) for the search-result case.
Reusing the existing field-naming vocabulary (`url`, `title`) rather than inventing new names for
the same concepts keeps the schema self-consistent across the whole `stapler-mcp` tool surface,
per the pattern already established in `schema.rs`.

## Summary of recommendations

| Question | Recommendation |
|---|---|
| Tool names | `index_docs`, `search_docs`, `list_indexed_sources`, `remove_indexed_source` — verb_noun, snake_case, matches existing 4 tools |
| Search key | Human-readable `source` name (like docs-mcp-server's `library`), not raw URL and not an opaque ID (unlike Context7) |
| Async job queue | Skip for v1 — reuse the existing 50-page crawl ceiling, keep `index_docs` synchronous like `read_website`/`download_website` |
| Re-indexing an already-indexed URL | Re-index in place, report what changed (pages added/removed/unchanged) — no separate `refresh` tool, no silent skip, no hard error |
| Missing/never-indexed source on search | Error string includes the current source list inline, saving a round trip |
| Partial 404s on re-index | Drop the missing pages, report the diff, don't fail the whole call |
| Seed URL 404s | Fail clearly with URL + status, same shape as existing `fetch_page`/`read_website` failures |
| Embedding model load failure | Fail identically on every call with a specific, actionable message — daemon-health-check category, not per-call |
| `search_docs` response fields | `text`, `score`, `sourceUrl`, `heading`, `sourceTitle` — extends `ReadWebsitePage`'s existing `{url, title, markdown}` vocabulary |
