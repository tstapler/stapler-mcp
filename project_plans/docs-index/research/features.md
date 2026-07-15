# Research: Feature Landscape for `docs-index`

**Dimension**: Features / prior art / edge cases
**Date**: 2026-07-14

## 1. What a minimal semantic doc-search tool needs beyond fetch+embed+search

### Chunking strategy

Consensus across `docs-mcp-server`, generic RAG guidance, and a first-hand "lessons learned"
write-up converges on the same shape:

- **Structure-aware chunking beats fixed-size for Markdown/HTML.** `docs-mcp-server` splits on
  headings, code blocks, and tables — never mid-code-block, never mid-table — and prepends a
  small metadata header (page title, URL, section path) to each chunk *before* embedding, so the
  embedding itself carries context, not just the raw text. This is directly reusable: v1 scope is
  HTML/Markdown only, and `htmd` (already a dependency, per `webcrawl.rs`) plus the existing
  Readability/`dom_smoothie` extraction gives a DOM/Markdown AST to split on rather than raw
  characters.
- **Fixed-size fallback for prose-heavy content**: recursive character/token splitting at
  roughly 400–512 tokens is the general-purpose default when no clean structural boundary exists
  (e.g., long unstructured paragraphs). The "power-user" pattern found repeatedly: heading-based
  split first, then recursively re-split any resulting chunk that's still oversized.
- **Overlap matters more than expected.** One real-world account (`mcp-local-rag`-adjacent
  Medium post) found 62% overlap (400-token chunks / 250-token overlap) was needed to stop
  document *entries* (resume jobs, in their case; analogous to a doc's function/method
  signature+description) from being severed mid-entry, which silently destroyed retrieval
  quality. General guidance is far lower (5–20% overlap) but that's for generic prose — headed
  technical content with tightly-coupled heading+body pairs may need the higher end. This is a
  concrete tuning knob worth exposing or at least revisiting empirically, not assuming 10% is
  enough.
- **Hierarchical/parent-child retrieval**: `docs-mcp-server` reassembles small chunks into
  larger context at query time — it configures a max `sort_order` gap to merge adjacent chunks,
  max depth for parent-context traversal, and how many sibling/child chunks to pull in. The
  underlying idea (retrieve small precise chunks, but expand to surrounding context before
  returning) is worth carrying into `search_docs`'s design even if the storage format is much
  simpler (flat JSON via `FileStore` rather than a real DB) — e.g., "return this chunk plus its
  immediate siblings from the same page" as a cheap approximation.

### Re-indexing / staleness handling

- `docs-mcp-server` exposes this explicitly as a **separate tool from indexing**:
  `refresh_version` ("re-scrape a previously indexed library version, updating only changed
  pages") is distinct from `scrape_docs` (initial index). There is no evidence it does
  HTTP-conditional (ETag/If-Modified-Since) staleness detection — search results didn't surface
  that mechanism, and the tool description's "updating only changed pages" more likely means
  content-hash diffing per page after a full re-crawl, not conditional GETs. Do not assume
  ETag support exists upstream to copy; if content-hash-based skip-if-unchanged is wanted,
  it can reuse the same SHA-256 cache-key machinery `read_website` already has in
  `webcrawl.rs` (`cache_key_for`) — a natural, already-proven mechanism in this codebase.
- Staleness is **user-triggered, not automatic**, in every surveyed tool (no background poller
  or TTL-based re-crawl found in any of `docs-mcp-server`, Context7, or `mcp-local-rag`). This
  matches the single-user/no-deadline framing in requirements.md — a manual `refresh_docs` /
  `reindex_source` tool call is sufficient for v1; a cron-like auto-refresh is not needed and
  would add daemon-lifecycle complexity (scheduling, backoff, concurrent-reindex races) for no
  clear payoff at this scale.
- **Real, hard constraint surfaced by the "hard way" account**: if the embedding model changes
  (or its dimensionality changes), old and new vectors are **not comparable** — "old vectors and
  new vectors live in different geometric spaces and similarity scores between them are
  meaningless." This means any local-embedding-model choice must be pinned/versioned in the
  index's stored metadata, and a model change (including a version bump of the same
  model) must force a full wipe-and-reindex of all sources, not an incremental patch. Worth
  encoding as an explicit invariant in the storage format (e.g., store `embedding_model_id` per
  index and refuse/warn on mismatch) rather than discovering it as a silent corruption bug later.

### How many doc sources / scale

- Requirements framing ("a handful of doc sources... a few Rust crate docs, some web
  documentation") implies single digits to low tens of sources, corroborating the
  brute-force-cosine-similarity-over-`FileStore`-JSON option flagged as viable in
  requirements.md rather than needing `sqlite-vec`/a real vector DB. `mcp-local-rag` uses
  LanceDB (file-based, no server process) as a middle ground worth comparing against in the
  stack research phase, but for "a handful of sources," brute-force scan is almost certainly fast
  enough and avoids adding a new storage dependency (and its own `wasm32` compile-viability
  question) beyond what's already proven.

### `list_indexed_sources` / `remove_docs`

- Every comparable tool has both. `docs-mcp-server`'s actual live tool surface (introspected
  directly in this session — see §3) is: `scrape_docs`, `refresh_version`, `remove_docs`,
  `find_version`, `list_libraries`, `list_jobs`, `get_job_info`, `cancel_job`, `search_docs`,
  `fetch_url`. That's a **9-tool surface** for what requirements.md scopes as a 2–3 tool v1
  (index/search/list). The gap between those two numbers is almost entirely explainable by
  `docs-mcp-server`'s job-queue-based async indexing (`list_jobs`/`get_job_info`/`cancel_job`
  exist because scraping is a long-running background job with its own lifecycle) and its
  multi-version-per-library model (`find_version` exists because one library can have many
  indexed versions simultaneously — irrelevant to "index a doc page/site," relevant only if this
  tool ever needs to track "this is the docs for crate X at version Y specifically"). For v1
  scope here, a `list_indexed_sources` tool and a `remove_indexed_source` tool are justified
  (cheap, prevents an ever-growing unmanageable index with no visibility/cleanup path) but the
  job-queue apparatus is very likely overkill unless indexing a source can take long enough
  (multi-minute crawls of large sites) to want async job tracking — worth flagging as an open
  question for the architecture research phase rather than deciding here.

## 2. Comparable tools surveyed

| Tool | Chunking | Re-index | Ranking | Storage |
|---|---|---|---|---|
| `docs-mcp-server` (arabold) | Structure-aware (headings/code/tables), metadata header prepended per chunk | Explicit `refresh_version` tool, full+incremental modes, batch embedding config | Hybrid vector + full-text search, parent/sibling chunk reassembly | SQLite (+ vector ext depending on provider) |
| Context7 (Upstash) | Proprietary; likely hybrid vector+BM25 with re-ranking (not publicly documented) | Not user-facing — Context7 is a managed/hosted index of 104k+ libraries, not something the end user re-indexes | Token-budget-capped response (default 5000 tokens), topic-filtered | Hosted, opaque |
| `mcp-local-rag` (shinpr) | Embedding-similarity-based semantic boundary detection (not just headings) | Deletes-then-reinserts all chunks for a re-ingested file — no incremental diffing, no dupes by construction | "Quality-first" filtering: groups by relevance gap instead of fixed top-K | LanceDB (embedded, file-based, no server process) |
| "Hard way" local RAG (Medium, generic) | Fixed-size w/ high overlap (400/250) needed for entry-structured docs | Wipe-and-reingest-all on any embedding model change (vectors from different models aren't comparable) | HNSW vector proximity; explicit advice to verify retrieval quality independent of generation quality | Not specified (generic local vector store) |

Key transferable lessons:
1. Prepending a small metadata header (title/URL/section path) to each chunk before embedding is
   cheap and reportedly meaningfully improves retrieval — worth adopting even in the simplest v1.
2. A model/dimension change is destructive to the whole index, not additive — must be a designed
   invariant, not an afterthought.
3. Async job tracking for indexing exists in `docs-mcp-server` because crawls can be slow/large;
   `mcp-local-rag`'s simpler delete-then-reinsert model works because its ingestion is presumably
   fast/local-file-based. `webcrawl.rs`'s own `Crawler` already has `max_depth`/`max_pages` caps
   for exactly this reason (bounding crawl time) — the same caps should bound indexing time here,
   which may make synchronous (non-job-queue) indexing tractable for v1's "handful of sources"
   scale.

## 3. MCP tool surface design

Existing pattern from `crates/core/src/tools/webcrawl.rs` (read directly, not guessed):

```rust
pub async fn read_website<H, F>(http: &H, fs: &F, cache_dir: &str, input: ReadWebsiteInput)
    -> Result<ReadWebsiteOutput, String>
where H: HttpClient, F: crate::ports::FileStore;

pub async fn download_website<H, F>(http: &H, fs: &F, input: DownloadWebsiteInput)
    -> Result<DownloadWebsiteOutput, String>
where H: HttpClient, F: crate::ports::FileStore;
```

Both: `#[derive(Serialize, Deserialize)]` input/output structs (camelCase over the wire —
`DownloadWebsiteInput.save_dir` → `saveDir`, matching `schemars`-derived JSON schema exported via
`list_tools_json()`), generic over the port traits (`HttpClient`, `FileStore`) rather than
concrete adapters, `Result<_, String>` error type, `resolve_limits(max_depth, max_pages)` shared
helper for crawl bounds, and a `cache_dir` string prefix pattern (`{cache_dir}/read-website/{hash}.json`)
for on-disk caching via `FileStore`.

A `docs-index` tool set should follow the same shape. Proposed minimal surface (naming aligned to
existing `read_website`/`download_website` verb-noun convention, deliberately narrower than
`docs-mcp-server`'s 9-tool surface per the "async job queue is likely overkill at this scale"
finding above):

- **`index_docs`** — `IndexDocsInput { url: String, source_name: String, max_depth: Option<u32>, max_pages: Option<u32> }`
  → crawls (reusing `Crawler`), chunks, embeds, stores under `source_name`. Synchronous for v1
  given bounded `max_depth`/`max_pages`; revisit async/job-based only if real crawl times prove
  too long for a blocking MCP tool call.
- **`search_docs`** — `SearchDocsInput { source_name: String, query: String, limit: Option<u32> }`
  → `SearchDocsOutput { results: Vec<DocsSearchResult> }` where each result carries chunk text,
  source page URL, title/section-path, and a similarity score — mirroring
  `docs-mcp-server`'s per-chunk metadata-header idea.
- **`list_indexed_sources`** — no input (or optional filter) → source names, page counts,
  last-indexed timestamp, embedding-model id (surfacing the model-pin invariant from §1).
- **`remove_indexed_source`** — `{ source_name: String }` → deletes the source's chunks/vectors.
  Matches `docs-mcp-server`'s `remove_docs` guard language ("use only if explicitly instructed")
  — worth carrying the same cautious tool description since this is a destructive op with no undo.
- **`refresh_indexed_source`** (stretch, maps to `docs-mcp-server`'s `refresh_version`) — re-crawls
  and re-diffs a previously indexed source rather than requiring `remove` + `index` again. Could
  be deferred out of v1 if `index_docs` is simply idempotent/overwriting per-source.

`find_version`/`list_jobs`/`get_job_info`/`cancel_job`/`fetch_url` from `docs-mcp-server`'s
surface are explicitly **not** proposed for v1 — `fetch_url` is redundant with the existing
`read_website`, and the job/version-matrix tools solve problems (long async jobs, multi-version
libraries) that don't clearly apply at "a handful of doc sources, single user" scale per
requirements.md's explicit scope.

## 4. Edge cases

- **Duplicate/near-duplicate pages.** No surveyed tool documents explicit near-dup detection at
  the page level; `mcp-local-rag` sidesteps exact duplication only by delete-then-reinsert
  per-file on re-ingest, not cross-source dedup. For `docs-index`: same-URL-content cache-hit
  behavior already exists via `webcrawl.rs`'s SHA-256 `cache_key_for` and can be reused
  directly to skip re-embedding unchanged pages, but that's dedup-on-refresh, not dedup-across-
  different-URLs-with-same-content (e.g., a version-redirect page and its target both getting
  crawled and both embedded). Worth an explicit design decision: at minimum, dedupe by
  content hash within one `index_docs` crawl, since `Crawler`'s BFS could plausibly visit two
  URLs resolving to identical rendered content (trailing slash variants, `?query` params that
  don't affect content, etc.).
- **Redirects.** `webcrawl.rs`'s `Crawler` and `HttpClient::get` weren't read in enough depth
  here to state current redirect-following behavior with certainty — flag for the architecture
  research pass. `docs-mcp-server`'s `scrape_docs`/`fetch_url` both expose an explicit
  `followRedirects: bool` (default `true`) — worth mirroring that same explicit knob rather than
  silently following or silently failing on 3xx, since a redirect changes what URL a chunk's
  "source" metadata should record (final URL, not requested URL — matches `PageExtract.final_url`
  already in `ports.rs`'s `BrowserDriver` trait, a precedent worth reusing for the docs crawler
  path too).
- **Very large single pages.** Reference-doc pages (e.g., a single "all API methods" page) can be
  enormous. Chunking must not assume a page fits in memory/one embedding call trivially; the
  "recursively re-split oversized structural chunks" pattern (§1) is the mitigation. Also affects
  `max_pages` bookkeeping in `Crawler` — one huge page consuming an entire indexing budget is a
  real UX surprise worth a size cap or at least a truncation-with-warning behavior.
- **Doc sources requiring auth.** Explicitly out of scope per requirements.md's constraints
  (single-user, filesystem-permissions-only posture, no OAuth) — but worth a *fail loud*
  requirement: `index_docs` should surface a clear "this page returned 401/403, not indexed" per
  page rather than silently skipping (mirrors `fetch_and_expand`'s existing "returns `None` on
  fetch failure, skipped not fatal to the whole crawl" behavior in `webcrawl.rs`, which is the
  right default for one bad page but should not be silent at the source-summary level after
  `index_docs` returns).
- **Non-English content.** Not addressed by any surveyed tool's docs. Most local sentence-
  transformer embedding models (the kind under evaluation in the stack research dimension) are
  English-optimized or explicitly multilingual variants exist (e.g., multilingual MiniLM) —
  purely a model-selection question for that research dimension, not an architectural one here.
  No special-casing needed in `docs-index`'s own code either way.
- **Malformed HTML.** Already handled at the extraction layer — `webcrawl.rs` reuses
  `dom_smoothie`/`dom_query` (Readability-style, tolerant HTML parsing) for `read_website`, which
  `docs-index` should sit on top of rather than re-solving; no new malformed-HTML handling is
  needed beyond what `extract_title_and_markdown` (already used by `read_website`) provides.
