# Implementation Plan: docs-index

**Feature**: Native Rust semantic search over indexed HTML/Markdown doc sources, reusing `webcrawl.rs`'s fetch/crawl pipeline and local `fastembed` embeddings, replacing the Node-based `docs-mcp-server` for the `stapler-mcp` daemon.
**Date**: 2026-07-14
**Status**: Ready for implementation
**ADRs**: ADR-0001 (local embeddings via `fastembed` + brute-force cosine similarity, native-only for v1), ADR-0002 (new `Embedder` port trait)

**Note on requirements.md traceability**: `requirements.md`'s Must-Have list names one literal
item, "a semantic-search MCP tool." This plan ships four tools (`index_docs`, `search_docs`,
`list_indexed_sources`, `remove_indexed_source`) plus two new port primitives
(`FileStore::delete_file`, the `Embedder` trait) and a cross-source `sources.json` manifest. This
is process-defensible — `requirements.md`'s own "Features" research dimension explicitly posed
"whether a 'list indexed sources' tool is needed alongside search" as an open question, and the
research/ux phase answered yes. **Resolved during Phase 4's Product Triad Review**:
`requirements.md` has been updated (see its "Actual delivered scope" note under Must Have) to
record this scope explicitly, closing the traceability gap that was flagged three times
(architecture review, adversarial review, triad review) before finally being fixed at its root
rather than re-noted in `plan.md` alone each time.

Two of the "required pre-work" fixes (Epic 1.2's `HttpResponse.final_url`, Story 1.3.2's atomic
`write_file`) modify **shared ports** (`HttpClient`, `FileStore`) consumed by already-shipped
tools (`fetch_page`, `read_website`, `download_website`), not just new docs-index code — the
atomic-write change in particular alters runtime write behavior for every existing `FileStore`
caller, not only docs-index's own new writes. Both are defensible as "free" correctness
improvements docs-index's own requirements genuinely need (durable writes, accurate redirect
metadata), but they are cross-cutting infrastructure changes with effects beyond this feature's
boundary, called out here explicitly rather than left implicit in the per-epic goals below.

---

## Step 0.5 — Creative Pass: Alternatives Considered at the Feature Level

Three distinct high-level shapes for where `docs-index`'s logic lives, before committing to one:

**Approach A — Extend `webcrawl.rs` in place.** Add chunking/embedding calls directly inside
`read_website`'s existing pipeline, storing vectors alongside its page cache.
- *Strength*: maximum code reuse, zero new files, no visibility changes needed to `Crawler`.
- *Weakness*: conflates two different concerns (transient page-fetch caching vs. a persistent,
  named, removable search index with its own lifecycle) inside an already-286-line, 2-tool file
  — `read_website`'s cache has no TTL/removal semantics today, but a search index needs
  `list`/`remove` operations that don't belong bolted onto a cache.

**Approach B — New sibling module `crates/core/src/tools/docs.rs`, composing `Crawler`.**
Bump `Crawler` and its helper functions from private to `pub(crate)`, build chunking/embedding/
storage as new functions in a new file, following the exact `tools/fetch.rs` /
`tools/search.rs` / `tools/webcrawl.rs` module-per-tool-family pattern already established.
- *Strength*: clean separation of concerns; reuses proven, already-tested BFS/`robots.txt`/
  link-extraction logic with a small, mechanical, low-risk visibility diff to an existing file.
- *Weakness*: requires touching `webcrawl.rs` (a working, tested file) and means three call
  sites (`read_website`, `download_website`, `index_source`) now share one `Crawler` loop shape
  — slightly more coupling than a fully standalone module.

**Approach C — Fully separate ingestion pipeline with its own fetch loop, no `Crawler` reuse.**
`docs.rs` implements its own minimal BFS/fetch logic independent of `webcrawl.rs`, touching
nothing in that file.
- *Strength*: zero risk of regressing `read_website`/`download_website` — no shared-code surface
  at all.
- *Weakness*: duplicates already-correct `robots.txt` parsing, BFS traversal, and same-host link
  extraction — two crawlers to keep in sync forever, directly contradicting requirements.md's
  explicit "reuse the existing fetch/crawl/`robots.txt` machinery" ask, and doubling the
  maintenance burden for behavior that should be identical.

**Chosen: Approach B.** It matches both `research/architecture.md`'s explicit recommendation and
requirements.md's explicit reuse instruction, and the risk introduced by touching `webcrawl.rs`
is small and mechanical (a `pub(crate)` visibility bump only — no logic changes to `Crawler`
itself, verified by a passing `cargo test -p stapler-mcp-core` immediately after the bump, before
any new code is added on top of it). Approaches A and C are recorded as rejected alternatives in
the Pattern Decisions table below.

---

## Domain Glossary
*(Ubiquitous language — every domain term that appears as a type, method, or variable name. Exact names here must be used consistently in code, tests, and comments.)*

| Term | Definition | Notes |
|------|-----------|-------|
| `Embedder` | Port trait (`crates/core/src/ports.rs`): `async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError>`. Converts text chunks into embedding vectors. | New port, native-only for v1 (ADR-0002). |
| `NativeEmbedder` | Native adapter (`crates/native/src/embed.rs`) implementing `Embedder`, wrapping a lazily-initialized `fastembed::TextEmbedding` cached for the daemon's process lifetime. | ADR-0001. |
| `EMBEDDING_MODEL_ID` | Constant string `"all-MiniLM-L6-v2"` identifying the pinned embedding model. Stored in every `SourceMeta` and checked by `search_docs` before ranking. | Lives in `crates/core/src/tools/docs.rs`, not `ports.rs` (ports.rs stays model-agnostic). |
| `Chunk` | An in-memory unit of markdown text produced by splitting one page's markdown: `{ text: String, heading: Option<String>, chunk_index: u32 }`. | Produced by `chunk_markdown`. |
| `ChunkRecord` | The JSONL-serializable persisted form of one chunk: `{ chunk_text, embedding: Vec<f32>, source_url, chunk_index, content_hash, heading, page_title }`. | One line per chunk in `chunks.jsonl`. `page_title` (the source page's `<title>`) is included from the start so `search_docs` can populate `DocsSearchResult.source_title` without a second lookup — see Task 3.4.2a. |
| `SourceMeta` | The `meta.json` sidecar for one indexed source: `{ source_id, source_name, seed_url, page_urls: Vec<String>, indexed_at_millis, page_count, chunk_count, embedding_model }`. | `page_urls` lets re-indexing report removed pages without loading `chunks.jsonl`. |
| `SourceSummary` | The manifest entry shape stored in `sources.json` and returned by `list_indexed_sources`: a subset of `SourceMeta`'s fields (no `page_urls`). | See "Source enumeration" Pattern Decision. |
| `SourceId` | Newtype `SourceId(String)` wrapping the filesystem-safe, slugified directory key for a source. Derived from `source_name` (not from the seed URL — see Pattern Decisions), via `SourceId::from_name`. | Distinct from `source_name` to prevent misuse — see Pattern Decisions. |
| `source_name` | The human-chosen or URL-derived public identifier (e.g. `"tokio-tutorial"`) used across `index_docs`/`search_docs`/`list_indexed_sources`/`remove_indexed_source`'s wire schema. | Wire field is `source` on every tool's **input** (all four use the same name, per the Product Triad Review's naming-consistency fix); output structs (`IndexDocsOutput`, `IndexedSourceSummary`, `RemoveIndexedSourceOutput`) return it as `sourceName` — a distinct, non-conflicting role (resolved display name, not a lookup key). |
| `content_hash` | SHA-256 hex digest of one page's fetched HTML. Used for within-crawl dedup (two URLs resolving to identical content). | Reuses the same hashing approach as `webcrawl.rs::cache_key_for`. |
| `cosine_similarity` | Pure function in `crates/core/src/tools/docs.rs`: `fn cosine_similarity(a: &[f32], b: &[f32]) -> f32`. The ranking function for `search_docs`. | Unit-tested with hand-computed vectors. |
| `DocsSearchResult` | One `search_docs` output element: `{ text, score, source_url, heading, source_title }`. | Wire: `text`, `score`, `sourceUrl`, `heading`, `sourceTitle`. |
| `IndexDocsInput` / `IndexDocsOutput` | Schema types (`crates/core/src/schema.rs`) for the `index_docs` tool. | See §3.1. |
| `SearchDocsInput` / `SearchDocsOutput` | Schema types for `search_docs`. | |
| `ListIndexedSourcesInput` / `ListIndexedSourcesOutput` / `IndexedSourceSummary` | Schema types for `list_indexed_sources`. | `ListIndexedSourcesInput` is an empty struct (no params), matching the "no input" convention noted in ux.md. |
| `RemoveIndexedSourceInput` / `RemoveIndexedSourceOutput` | Schema types for `remove_indexed_source`. | |
| `docs_index_dir` | Path helper (`crates/core/src/paths.rs`): `{base_dir}/docs-index`, mirroring `cache_dir`. | |
| `embedding_cache_dir` | Path helper: `{base_dir}/models`, where `fastembed` caches its downloaded ONNX model weights. | |
| `source_dir(source_id)` | Path helper (in `docs.rs`): `{docs_index_dir}/<source_id>/`. | Contains `chunks.jsonl` + `meta.json`. |
| `sources_manifest_path` | Path helper: `{docs_index_dir}/sources.json` — the cross-source enumeration manifest. | See "Source enumeration" Pattern Decision. |
| `MarkdownSplitter` | Type from the `text-splitter` crate (v0.32), used by `chunk_markdown` to split a page's markdown at structural (heading/paragraph) boundaries within a character-length budget. | Native-only dependency (target-specific in `Cargo.toml`). |
| `MAX_CHUNKS_PER_SOURCE` | Constant hard-capping total chunks embedded/stored per `index_docs` call, bounding worst-case inline-blocking duration on the single-threaded daemon. **Provisional value: `2000`, pending Task 2.1.2c's real `fastembed` throughput benchmark** — the final value is `floor(measured_chunks_per_second × 8)` (8s = the accepted worst-case inline-blocking budget for an explicit, rare, user-triggered indexing call; see Pattern Decisions — concurrency row). Task 2.1.2c must run and this constant must be set from its result before Task 4.1.2a is implemented for real; `2000` is only a placeholder used in this plan's illustrative examples. | See Pattern Decisions — concurrency row; Task 2.1.2c. |
| `SUB_BATCH_SIZE` | Constant (`100`) — `index_source` calls `Embedder::embed` in sub-batches of at most this many chunks, `tokio::task::yield_now().await`-ing between sub-batches, instead of one giant call covering all of `MAX_CHUNKS_PER_SOURCE`. Lets the daemon interleave other tool calls during a long indexing operation. | See Pattern Decisions — concurrency row; Story 4.1.3. |
| `SourceLocks` | `crates/core/src/tools/docs.rs` type: `pub(crate) struct SourceLocks { active: RefCell<HashSet<SourceId>> }`. An in-memory, per-`source_id` operation guard shared (via `Rc`) between `index_source` and `remove_indexed_source`'s daemon registrations, preventing two mutating operations on the same source from interleaving their `.await`-yielding writes. | See Pattern Decisions — concurrency-guard row; Story 3.4.3. |
| `slugify` / `slugify_from_url` | Helpers turning a user-supplied `source_name` (or, if omitted, a URL's host+path) into a filesystem-safe, lowercase, hyphenated string — the basis for `SourceId`. | `https://tokio.rs/tokio/tutorial` → `tokio-tutorial`. |

---

## Pattern Decisions

| Component | Pattern Chosen | Source | Alternative Rejected | Reason |
|-----------|---------------|--------|---------------------|--------|
| Module placement | New sibling module `crates/core/src/tools/docs.rs`, Transaction Script functions | architecture.md §4; Step 0.5 Approach B | Approach A: extend `webcrawl.rs` in place | Conflates transient page-cache concerns with a persistent, named, removable search index — `webcrawl.rs` is already 286 lines serving 2 tools |
| Crawl reuse | Bump `Crawler`/`next_url`/`fetch_and_expand`/`extract_title_and_markdown`/`cache_key_for`/`resolve_limits` to `pub(crate)` in `webcrawl.rs`, reuse from `docs.rs` | architecture.md §4 | Approach C: hand-roll a second, independent fetch loop in `docs.rs` | Duplicates already-correct BFS/`robots.txt`/link-extraction logic — two crawlers to maintain for identical behavior, contradicts requirements.md's explicit reuse ask |
| Vector storage | JSONL chunk records + JSON meta sidecar via `FileStore`, brute-force cosine similarity in `core` | architecture.md §3; build-vs-buy.md §3; ADR-0001 | `sqlite-vec` + `rusqlite` | Adds this codebase's first-ever DB dependency, unverified wasm32 compile path, solves a 10k+-vector scale problem this project doesn't have |
| Embeddings | `fastembed` 5.17.2 + `all-MiniLM-L6-v2`, native-only | stack.md §1; ADR-0001 | `candle` + `candle-transformers` (wasm-capable) | More integration code; unproven Node-hosted-wasm sentence-embedding track record; native-only matches requirements.md's accepted fallback framing |
| `Embedder` shape | GoF Adapter/Strategy: port trait wrapping the concrete embedding crate | architecture.md §2; ADR-0002 | Call `fastembed::TextEmbedding` directly from `docs.rs` | Breaks the ports-and-adapters architecture every OS/hardware-touching capability in this codebase uses; makes `docs.rs` untestable without a real ONNX model load |
| `docs::index_source` / `docs::search_docs` control flow | Transaction Script (PoEAA) — top-level `async fn`s over plain data, matching `read_website`/`download_website` | features.md §3; user instructions | Domain Model (`Source`/`Chunk` objects with `reindex()`/`embed()` methods) | Over-engineers a linear fetch→chunk→embed→store pipeline; every existing tool in this codebase is a plain function, not model objects with behavior |
| Function naming: `index_source` (not `index_docs`) | Deliberate deviation from "tool name = function name" (the other three: `search_docs`, `list_indexed_sources`, `remove_indexed_source` do match their MCP tool names exactly) | Consistency check flagged this during Phase 4 planning | Rename to `index_docs` for literal 1:1 naming | `index_source` operates on one already-resolved `SourceId`/seed URL — "source" is the noun the function actually acts on; "docs" is the tool-surface framing for the MCP-facing verb-object pair (`index_docs` the tool call). Kept as documented divergence rather than renamed, so a reader isn't misled into expecting every internal fn to mirror its tool name 1:1 when the concepts genuinely differ (operating-on-a-source vs. the doc-indexing capability as a whole) |
| `SourceId` | Newtype `SourceId(String)`, type-driven design | user instructions Step 3 | Bare `String` used interchangeably for both `source_id` and `source_name` | `source_id` (slugified directory key) and `source_name` (display slug) are easy to conflate; the newtype makes passing one where the other is expected a compile error, not a runtime path bug |
| `SourceId` derivation | Slugify **`source_name`** directly (`SourceId::from_name`), not a hash of the seed URL | Resolved during this planning pass — see "Resolved" note below | Hash the seed URL (`cache_key_for(seed_url)`), as architecture.md §3 originally suggested | architecture.md's URL-hash suggestion predates ux.md's finalized name-based lookup design (`search_docs`/`remove_indexed_source` take `source` by name, not URL); hashing the URL would force a directory scan or a name→hash index just to resolve a name-based lookup. Slugifying the name directly gives O(1) lookup with no extra indirection. |
| Index-exists-or-not state | `Option<SourceMeta>` via `FileStore::read_file`'s existing `Ok(None)` = miss convention | ports.rs established convention | Bespoke `enum IndexState { Exists(SourceMeta), Missing }` | `Option` already expresses this; every other tool in the codebase treats `Ok(None)` as the miss case — a bespoke enum is a redundant, inconsistent wrapper |
| Concurrency / blocking inference | Bounded inline blocking, with the bound set from measured evidence, not asserted: Task 2.1.2c benchmarks real `fastembed`/`all-MiniLM-L6-v2` throughput (100 representative chunks, wall-clock timed); `MAX_CHUNKS_PER_SOURCE = floor(measured_chunks_per_sec × 8)`, where 8s is the chosen worst-case-inline-blocking budget. Additionally, `index_source` embeds in `SUB_BATCH_SIZE`-chunk (100) sub-batches with `tokio::task::yield_now().await` between sub-batches, so the daemon can interleave other tool calls during a long indexing run instead of blocking for the entire cap's worth of chunks in one uninterruptible call | pitfalls.md §1; Task 2.1.2c; Story 4.1.3 | (a) Dedicated `std::thread` + channel handoff for `Send`-safe embedding calls; (b) one single unbatched `embed()` call for up to `MAX_CHUNKS_PER_SOURCE` chunks | (a) Hand-rolling a `Send`-safe boundary around `!Send` handler state is real, ongoing complexity for a feature invoked rarely and explicitly by one user; `read_website`/`download_website` already block the daemon for their full crawl duration at this codebase's scale with no complaint — matching that precedent is more consistent than introducing the daemon's only threaded code path. (b) A single giant `embed()` call over up to `MAX_CHUNKS_PER_SOURCE` chunks — even a benchmark-derived, defensible cap — would still stall every other in-flight tool call for the full 8s budget in one uninterruptible chunk; sub-batching with a `yield_now().await` between batches is a small, low-risk change (no new `Send` boundary, no new thread) that turns one long stall into many short ones the scheduler can interleave around, which is worth the modest added complexity of a loop instead of one call |
| Concurrent same-source operations | In-memory `SourceLocks` guard (`RefCell<HashSet<SourceId>>`, shared via `Rc`), checked at the start of `index_source`/`remove_indexed_source`; second concurrent call on the same source gets a clear "already in progress" error instead of interleaving on-disk writes | Blocker found in adversarial review — see Story 3.4.3, Phase 4 tasks | No guard (accept interleaving as part of the existing "eventual consistency" risk) | The interleaving failure mode (a `remove_indexed_source` racing an in-flight `index_docs` for the same source) requires no crash and no unusual timing — any LLM caller firing two related tool calls back-to-back can trigger it during completely normal operation, unlike the daemon-crash-only risk the plan originally accepted; a `RefCell<HashSet<SourceId>>` guard is a few lines and fully compatible with this daemon's existing `!Send`, single-threaded, `Rc`/`RefCell` state-management style (see `crates/cli/src/main.rs`'s `Rc::new(...)`-then-`.clone()`-into-closure pattern) |
| Chunk-file write durability | `NativeFs::write_file` writes to a temp file in the same directory, then `tokio::fs::rename`s it into place (atomic on the same filesystem); temp filename includes both `std::process::id()` **and** a per-call `AtomicU64` counter (Task 1.3.2a) | Blocker found in adversarial review — see Story 1.3.2 | Leave `tokio::fs::write` (truncate-then-write) as-is; separately, a PID-only temp-filename scheme (`format!("{path}.tmp-{}", std::process::id())`) was this fix's own first draft and was rejected after a second adversarial pass | A failed/interrupted write (e.g. disk full) mid-`chunks.jsonl` write currently leaves a truncated file on disk with no signal to `index_source`'s caller beyond the propagated `Err`, while `sources.json`/`meta.json` may still reference it as valid; temp-file-plus-rename makes a partial write simply never appear at the final path at all — a small, general fix to an existing shared primitive that also benefits `read_website`'s cache, not just docs-index. **Why PID alone is insufficient**: the daemon's PID is constant for its entire lifetime, so every `write_file` call to the *same target path* over the daemon's whole run produces an identical PID-only temp filename. `SourceLocks` only guards per-`source_id` writes to `chunks.jsonl`/`meta.json` — it does not serialize writes to the shared `sources.json` manifest, which every `index_docs`/`remove_indexed_source` call rewrites regardless of source. Two such calls for two *different* sources can legitimately run concurrently and race on that shared temp filename: one call's `rename` silently consumes the other's temp file, and the loser's own `rename` then fails with a spurious `ENOENT` — worse than the crash-only "eventual consistency" risk this plan accepts elsewhere (see Unresolved Questions), since it needs no crash, only ordinary concurrent load. A per-call-unique counter closes this regardless of how many concurrent callers write the same path. |
| `search_docs` malformed-JSONL-line handling | Skip the malformed line, log a warning, continue scoring the remaining valid chunks | Blocker found in adversarial review — matches `webcrawl.rs`'s existing "skip one bad page, don't fail the whole crawl" precedent (`fetch_and_expand` returns `None` on a single page's fetch failure rather than aborting the crawl) | Hard-error the whole `search_docs` call on any malformed line | A single truncated last line (e.g. from an interrupted write under the old non-atomic `write_file` — now mitigated by the row above, but still possible from other on-disk corruption) shouldn't make an otherwise-healthy index totally unsearchable; this also matches the codebase's existing tolerance-for-partial-failure precedent instead of inventing a stricter new one just for this file format |
| Redirect / final-URL tracking | Add `final_url: String` to `ports::HttpResponse`, captured from `reqwest::Response::url()` before consuming the body | features.md "Redirects" edge case; `PageExtract.final_url` precedent | Add a `followRedirects: bool` toggle mirroring `docs-mcp-server` | reqwest's default client already follows redirects (max 10 hops) with no per-call override surface in `HttpClient::get`'s current signature; neither `read_website` nor `download_website` expose this knob today — adding one only for `docs-index` would be an inconsistent, unused surface |
| Source enumeration (`list_indexed_sources`, unknown-source error listing) | Maintain a `{docs_index_dir}/sources.json` manifest (`Vec<SourceSummary>`), updated by `index_docs`/`remove_indexed_source` | Gap identified in this planning pass — `FileStore` has no directory-listing primitive | Add `list_dir` to the `FileStore` port trait | Would require new native (`tokio::fs::read_dir`) *and* wasm (new JS glue) implementations for a capability only this one native-only v1 feature needs — disproportionate blast radius on a port shared by every tool and both adapters, versus one small manifest file using the existing `read_file`/`write_file` |
| Source deletion | Add `delete_file(&self, path: &str) -> Result<(), PortError>` to the `FileStore` trait (idempotent: `NotFound` → `Ok`) | Gap identified in this planning pass — `FileStore` has no delete primitive, needed by `remove_indexed_source` | Add a `list_dir`-style recursive directory-delete primitive instead | `delete_file` is the natural third primitive of a blob-store CRUD surface (symmetric with `read_file`/`write_file`), a smaller and more broadly reusable extension than directory enumeration; `remove_indexed_source` calls it twice (once per known file) rather than needing recursive delete |
| `wasm32` buildability of `tools::docs` | `#[cfg(not(target_arch = "wasm32"))]` the entire module in `tools/mod.rs`, and declare `text-splitter` as a `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]` entry in `crates/core/Cargo.toml` | Gap identified in this planning pass — `text-splitter`'s wasm32 compileability was flagged "unverified" in stack.md, and `crates/core` is compiled for both native and wasm32 targets | Attempt to verify/force `text-splitter` (and the whole module) to compile for wasm32 | No `WasmEmbedder` exists for v1 (ADR-0001/ADR-0002) — there is nothing productive `docs.rs` could do under wasm32 even if every dependency compiled there, so cfg-gating removes both the compile-risk and the dead-code surface in one move |

**Resolved note on `SourceId` derivation**: architecture.md §3 (written before ux.md's tool-surface
pass was finalized) suggested deriving `source-id` from `sha256(seed_url)`, matching
`webcrawl.rs::cache_key_for`'s pattern. ux.md's tool surface, finalized after architecture.md,
makes `source` (the human name) the primary lookup key for `search_docs`/`remove_indexed_source`
— a URL-hash-based `source_id` would require either a directory scan or a secondary name→hash
index just to resolve that lookup. This plan resolves the tension by deriving `SourceId` from
`source_name` directly (see Domain Glossary and Phase 3 below), which is simpler and matches
ux.md's finalized design without loss of any property architecture.md actually needed (a stable,
collision-resistant directory key).

---

## Migration Plan

Greenfield feature — no existing `~/.stapler-mcp/docs-index/` data to migrate. The one
forward-looking "migration" concern is the embedding-model-pin invariant: every `SourceMeta`
stores `embedding_model: EMBEDDING_MODEL_ID`, and `search_docs` refuses to rank a source whose
stored `embedding_model` doesn't match the daemon's current `EMBEDDING_MODEL_ID` (see Phase 4,
Story 4.2.2). If `EMBEDDING_MODEL_ID` is ever bumped (a different model or model version), that
is the "migration" — existing sources become unsearchable until explicitly re-indexed
(`index_docs` again, full replace) via a clear, actionable error message rather than silently
wrong similarity scores. No automatic migration/re-embedding path exists or is planned for v1.

## Observability Plan
- **Logs**: `eprintln!` to the daemon's existing `daemon.log` (via the same stderr-redirect
  `ProcessSpawner` already uses for every other tool) on: `NativeEmbedder` model load start/
  duration/failure (first `embed()` call only), one summary line per `index_docs` call
  (`source_name`, pages fetched, pages removed, chunks indexed, elapsed millis), and one warning
  line per malformed `chunks.jsonl` line encountered during `search_docs` (source name, 1-indexed
  line number — see Task 4.2.1c) — mirrors the level of logging already present (or absent)
  elsewhere in this codebase; no new logging framework introduced.
- **Metrics**: not applicable — this codebase has no metrics-collection infrastructure today.
  The `index_docs`/`search_docs` tool *responses themselves* carry the closest equivalent
  (`chunksIndexed`, `pagesRemoved`, per-result `score`), which is sufficient for a single-user
  daemon with no dashboard.
- **Alerts**: not applicable — single-user, single-machine, no monitoring infrastructure exists
  or is planned, consistent with every other tool in this daemon.

## Risk Control
- **Feature flag**: none — this codebase has no flag system. The four new tools are purely
  additive `daemon.register(...)` calls and `thin_client.rs` endpoints; nothing about existing
  tools (`fetch_page`, `brave_web_search`, `read_website`, `download_website`) changes behavior.
- **Rollback procedure**: remove the four `daemon.register("index_docs"|"search_docs"|
  "list_indexed_sources"|"remove_indexed_source", ...)` calls from `crates/cli/src/main.rs` and
  the four `#[tool]` methods from `crates/cli/src/thin_client.rs`; optionally `rm -rf
  ~/.stapler-mcp/docs-index ~/.stapler-mcp/models`. No data migration or schema rollback needed
  since no other tool reads `docs-index/` or `models/`.
- **Staged rollout**: single user — ship directly to the native daemon binary. Informal canary:
  the first real-world `index_docs` call (against a real small doc source, e.g.
  `https://tokio.rs/tokio/tutorial`) doubles as a manual smoke test before broader reliance,
  matching how every prior `stapler-mcp` phase in `NOTES.md` was validated (manual use, not a
  staged-percentage rollout, appropriate at this scale).
- **`docs-mcp-server` coexistence**: `docs-mcp-server` (still connected as of this planning pass,
  per `~/.claude.json`'s `"docs"` entry) already exposes live MCP tools named `search_docs` and
  `list_libraries` — this plan's `search_docs`/`list_indexed_sources` tool names don't collide on
  the exact string with `list_libraries`, but `search_docs` is an **exact name collision** with
  `docs-mcp-server`'s existing tool. Two MCP servers registering the same tool name in one Claude
  Code session is undefined/ambiguous behavior for the calling LLM, not a compile-time or
  daemon-level conflict this plan's code can resolve. **Action required before shipping**:
  disconnect/remove the `"docs"` entry from `~/.claude.json` (or rename `docs-mcp-server`'s
  `search_docs` tool if it must temporarily coexist during a side-by-side comparison) before
  `stapler-mcp`'s `search_docs` is registered — this is a manual config step outside this plan's
  code changes, tracked here so it isn't forgotten at ship time. Separately, requirements.md's
  Success Criteria ("No separate Node process needed for this at all") implies full decommission
  of `docs-mcp-server` once this feature ships — no task in this plan performs that decommission
  step; it is scoped as a manual, post-implementation action the user (Tyler) takes once satisfied
  with `docs-index`'s results, not an automated migration.

## Unresolved Questions
- **`sources.json` eventual-consistency risk**: `index_docs`/`remove_indexed_source` write
  `chunks.jsonl`, then `meta.json`, then rewrite `sources.json` last (best-effort, matching the
  non-transactional write pattern `webcrawl.rs`'s cache already uses). If the daemon crashes
  between these three writes, `sources.json` could list a source whose `meta.json`/`chunks.jsonl`
  don't (yet) match, or omit one that exists. **Narrowed and accepted risk for v1** — the
  in-memory `SourceLocks` guard (Story 3.4.3) fully covers the *concurrent-tool-call-interleaving*
  version of this risk (e.g. a `remove_indexed_source` racing an in-flight `index_docs` for the
  same source, which was found during adversarial review to be a normal-operation risk, not a
  crash-only one — see the "Concurrent same-source operations" Pattern Decisions row). What
  remains accepted is genuinely only the **daemon-crash-mid-write** case, which the guard cannot
  help with (a crash discards all in-memory state, including the guard itself). That residual risk
  is no worse than the cache-invalidation risk already accepted elsewhere in this codebase
  (`pitfalls.md`: "no TTL/staleness infra exists anywhere yet"). Must be resolved (e.g.
  write-ahead ordering guarantee, or a `list_indexed_sources` fallback that reconciles against
  actual `source_dir` contents) only if this is ever observed in practice.
- **Exact `fastembed` 5.17.2 API surface** (`TextEmbedding::try_new`, `InitOptions`,
  `EmbeddingModel::AllMiniLML6V2` exact method/builder names) was assessed from crates.io
  metadata during research, not verified against the installed crate's own rustdoc. **Must be
  verified** during Task 2.1.2a, before writing `NativeEmbedder`'s body — resolve before that
  task starts.
- **Exact `text-splitter` 0.32 `MarkdownSplitter` constructor signature** (character-`usize` vs.
  `Range<usize>` capacity argument) similarly unverified against installed-crate rustdoc. **Must
  be verified** during Task 3.2.1b, before that task starts.
- **Default `source_name` collision** when two different seed URLs slugify to the same default
  name (e.g. two different sites both under a path ending in `/tutorial` with the same host
  stem) is not specially handled in v1 — the second `index_docs` call silently overwrites the
  first under that name (same behavior as an intentional re-index). **Accepted risk** — very
  unlikely at "a handful of sources" scale; the caller can always pass an explicit `source` to
  disambiguate. Not blocking.
- Content-hash skip-unchanged-page re-indexing (a v1.1 optimization noted in architecture.md §3)
  is explicitly **not** part of this plan. None of the tasks below implement it.

## Dependency Visualization

```
Phase 1 (Foundation / pre-work)
  Epic 1.1 (Crawler visibility bump) ─┐
  Epic 1.2 (HttpResponse.final_url) ──┼─→ Phase 4 (docs.rs tool functions)
  Epic 1.3 (FileStore.delete_file) ───┤        ↑
  Epic 1.4 (deps + path helpers) ─────┤        │
  Epic 1.5 (wasm32 cfg-gate) ─────────┘        │
                                                │
Phase 2 (Embedder port + NativeEmbedder) ───────┤
                                                │
Phase 3 (Core domain logic:                    │
  schema types, chunking, cosine similarity,   │
  SourceId, storage types) ───────────────────→┤
                                                │
                                       Phase 4 (index_docs, search_docs,
                                                list_indexed_sources,
                                                remove_indexed_source)
                                                │
                                                ↓
                                       Phase 5 (daemon wiring:
                                                cli/main.rs + thin_client.rs)
                                                │
                                                ↓
                                       Phase 6 (unit + integration tests)
```

Phases 1–3 have no dependencies on each other and can be implemented in any order (or in
parallel by separate work sessions) as long as all of Phase 1–3 land before Phase 4 begins.
Phase 4's three stories (`index_docs`, `search_docs`, `list_indexed_sources`/
`remove_indexed_source`) can also proceed in parallel once Phase 3 lands, but Phase 5 requires
all of Phase 4. Phase 6's unit tests can be written alongside Phase 3/4 (test-first is fine);
Phase 6's integration test requires Phase 5.

---

## Phase 1: Foundation (Pre-work)

### Epic 1.1: Crawler & extraction visibility bump
**Goal**: Make `webcrawl.rs`'s proven BFS/`robots.txt`/extraction logic callable from a new
sibling module, with zero behavior change.

#### Story 1.1.1: Bump `Crawler` and its helpers to `pub(crate)`
**As a** `docs-index` implementer, **I want** `Crawler` and its supporting functions visible
within the crate, **so that** `docs.rs` can reuse the exact same crawl loop `read_website`/
`download_website` already use, instead of duplicating it.
**Acceptance Criteria**:
- `Crawler` (the struct), `Crawler::new`, `Crawler::next_url`, `Crawler::fetch_and_expand`,
  `extract_title_and_markdown`, `cache_key_for`, and `resolve_limits` are `pub(crate)` in
  `crates/core/src/tools/webcrawl.rs`, and every existing caller (`read_website`,
  `download_website`) still compiles and passes unchanged.
  - *Given* `crates/core/src/tools/webcrawl.rs` before this change (all of the above private),
    *When* each is changed from no visibility modifier to `pub(crate)` and nothing else in the
    file is edited, *Then* `cargo test -p stapler-mcp-core` and `cargo test -p stapler-mcp` (which
    runs `crates/cli/tests/webcrawl.rs`'s `webcrawl_respects_robots_and_caches` test) both still
    pass with identical output to before the change.
**Files**: `crates/core/src/tools/webcrawl.rs`

##### Task 1.1.1a: Bump `Crawler` struct and impl block (~3 min)
- Change `struct Crawler<'a, H: HttpClient>` to `pub(crate) struct Crawler<'a, H: HttpClient>`.
- Change `impl<'a, H: HttpClient> Crawler<'a, H>` method signatures `async fn new(...)`, `fn
  next_url(...)`, `async fn fetch_and_expand(...)` to `pub(crate) async fn` / `pub(crate) fn`.
  Leave `fn allowed(&self, ...)` private (only called internally by `next_url`).
- Files: `crates/core/src/tools/webcrawl.rs`

##### Task 1.1.1b: Bump helper functions to `pub(crate)` (~2 min)
- Change `fn extract_title_and_markdown(...)`, `fn cache_key_for(...)`, `fn
  resolve_limits(...)` to `pub(crate) fn`. Leave `extract_links`, `fetch_robots`, `fetch_ok`,
  `same_host` private (internal-only, `docs.rs` doesn't need them directly).
- Files: `crates/core/src/tools/webcrawl.rs`

##### Task 1.1.1c: Verify no regression (~2 min)
- Run `cargo test -p stapler-mcp-core -p stapler-mcp` and confirm all existing tests
  (`crates/core` unit tests, `crates/cli/tests/webcrawl.rs`, `crates/cli/tests/daemon_ping.rs`)
  still pass unchanged.
- Files: none (verification only)

### Epic 1.2: Redirect / final-URL tracking
**Goal**: `HttpClient::get` callers can learn the final URL after redirects, needed so a chunk's
`source_url` metadata records the page actually served, not the requested URL.

#### Story 1.2.1: Add `final_url` to `HttpResponse`
**As a** `docs-index` implementer, **I want** `HttpClient::get` to report the post-redirect URL,
**so that** `index_docs` can store the correct `sourceUrl` per chunk even when a crawled link
redirects.
**Acceptance Criteria**:
- `ports::HttpResponse` has a new `final_url: String` field; `NativeHttp::get` populates it from
  `reqwest::Response::url()`; `WasmHttp::get` populates it with the requested `url` as a
  best-effort placeholder (wasm/Node isn't wired to docs-index, so exact redirect tracking there
  is out of scope for v1, but the struct must still compile).
  - *Given* a request to `http://127.0.0.1:PORT/old-page` that the test mock server 301-redirects
    to `http://127.0.0.1:PORT/new-page`, *When* `NativeHttp::get("http://127.0.0.1:PORT/old-page",
    &[])` is called, *Then* the returned `HttpResponse.final_url` equals
    `"http://127.0.0.1:PORT/new-page"`, not the originally requested URL.
**Files**: `crates/core/src/ports.rs`, `crates/native/src/http.rs`, `crates/wasm/src/http.rs`

##### Task 1.2.1a: Add the field to the port type (~2 min)
- In `crates/core/src/ports.rs`, add `pub final_url: String` to `pub struct HttpResponse`
  (after `body`), with a one-line doc comment: "the URL actually served, after following any
  redirects."
- Files: `crates/core/src/ports.rs`

##### Task 1.2.1b: Populate `final_url` in `NativeHttp` (~3 min)
- In `crates/native/src/http.rs`, capture `let final_url = resp.url().to_string();` immediately
  after `let status = resp.status().as_u16();` and *before* `let body = resp.bytes().await...`
  (order matters: `.bytes()` consumes `resp`, `.url()` borrows it). Add `final_url` to the
  returned `HttpResponse { status, body, final_url }`.
- Files: `crates/native/src/http.rs`

##### Task 1.2.1c: Populate `final_url` in `WasmHttp` (best-effort) (~2 min)
- In `crates/wasm/src/http.rs`, set `final_url: url.to_string()` in the returned
  `HttpResponse { status, body, final_url }`, with a `// TODO(docs-index-wasm): capture the
  real post-redirect URL via response.url on the JS side if/when a wasm Embedder exists.`
  comment.
- Files: `crates/wasm/src/http.rs`

##### Task 1.2.1d: Fix any other `HttpResponse` construction sites (~2 min)
- `sg --pattern 'HttpResponse { $$$ }' --lang rust` across `crates/`; fix any additional
  construction site (e.g. test fakes in `crates/cli/tests/`) to include `final_url`.
- Files: as discovered (expected: none beyond 1.2.1b/c based on current codebase)

### Epic 1.3: `FileStore::delete_file` primitive and atomic `write_file`
**Goal**: `remove_indexed_source` needs to delete a source's `chunks.jsonl`/`meta.json`;
`FileStore` currently has no delete primitive. Separately, adversarial review found
`NativeFs::write_file` is confirmed non-atomic (plain `tokio::fs::write`, truncate-then-write, no
temp+rename), which for `docs-index` means a disk-full/interrupted write mid-`chunks.jsonl` can
leave a truncated file on disk while `meta.json`/`sources.json` still claim the source is fully
indexed. Both fixes touch the same file (`crates/native/src/fs.rs`), so Story 1.3.2 folds into
this Epic alongside Story 1.3.1 rather than opening a new one.

#### Story 1.3.1: Add `delete_file` to the `FileStore` port
**As a** `docs-index` implementer, **I want** an idempotent `delete_file` on `FileStore`, **so
that** `remove_indexed_source` can actually remove a source's on-disk data.
**Acceptance Criteria**:
- `FileStore` has `async fn delete_file(&self, path: &str) -> Result<(), PortError>`; deleting a
  file that doesn't exist returns `Ok(())`, not an error (idempotent, matching `read_file`'s
  `Ok(None)`-for-missing convention).
  - *Given* a file at `~/.stapler-mcp/docs-index/tokio-tutorial/meta.json` exists on disk, *When*
    `NativeFs::delete_file("~/.stapler-mcp/docs-index/tokio-tutorial/meta.json")` is called,
    *Then* the file no longer exists on disk and the call returns `Ok(())`.
  - *Given* no file exists at `~/.stapler-mcp/docs-index/does-not-exist/meta.json`, *When*
    `NativeFs::delete_file(...)` is called on that path, *Then* the call returns `Ok(())` (not
    `Err`).
**Files**: `crates/core/src/ports.rs`, `crates/native/src/fs.rs`, `crates/wasm/src/fs.rs`

##### Task 1.3.1a: Add trait method (~2 min)
- Add `async fn delete_file(&self, path: &str) -> Result<(), PortError>;` to `trait FileStore`
  in `crates/core/src/ports.rs`, with a doc comment: "Idempotent — deleting a path that doesn't
  exist is `Ok(())`, not an error."
- Files: `crates/core/src/ports.rs`

##### Task 1.3.1b: Implement `NativeFs::delete_file` (~3 min)
- In `crates/native/src/fs.rs`, implement using `tokio::fs::remove_file(path).await`, mapping
  `Err(e) if e.kind() == std::io::ErrorKind::NotFound` to `Ok(())`, other errors to
  `PortError::Io(e.to_string())`.
- Files: `crates/native/src/fs.rs`

##### Task 1.3.1c: Implement `WasmFs::delete_file` + JS glue stub (~3 min)
- Add `#[wasm_bindgen(js_name = jsDeleteFile)] fn js_delete_file(path: &str) -> js_sys::Promise;`
  extern binding in `crates/wasm/src/fs.rs`, and implement `WasmFs::delete_file` calling it (same
  `JsFuture::from(...).await.map_err(...)` shape as `write_file`). Add the corresponding
  `jsDeleteFile` export to `crates/wasm/src/glue/fs.js` (Node `fs.promises.unlink`, catching and
  swallowing `ENOENT`).
- Files: `crates/wasm/src/fs.rs`, `crates/wasm/src/glue/fs.js`

#### Story 1.3.2: Make `NativeFs::write_file` atomic (temp file + rename)
**As a** `docs-index` user (and every other `FileStore::write_file` caller), **I want** writes to
never leave a partially-written file at the final path, **so that** a disk-full or interrupted
write mid-`chunks.jsonl` can't leave `sources.json`/`meta.json` pointing at broken chunk data
(blocker found in adversarial review).
**Scope note**: this is a narrow, general fix to the one existing native `write_file`
implementation — it is not a `FileStore` redesign, and every existing caller (including
`read_website`'s page cache) benefits for free, with no interface change and no caller-visible
behavior change on the success path.
**Acceptance Criteria**:
- `NativeFs::write_file` writes to a temp file in the same directory as the target path, then
  `tokio::fs::rename`s it into place; a failed/interrupted write never leaves a partially-written
  file at the final path.
  - *Given* a target path `/tmp/test-home/docs-index/tokio-tutorial/chunks.jsonl` that does not
    yet exist, *When* `NativeFs::write_file(path, bytes)` is called and succeeds, *Then* the file
    at `path` contains exactly `bytes` (unchanged observable behavior on the success path) and no
    stray temp file (e.g. `chunks.jsonl.tmp-*`) remains in that directory afterward.
  - *Given* a target path that already has valid content on disk, *When* `write_file` is called
    with new content but the write is interrupted before the rename step completes (simulated in a
    test by writing to the temp file and asserting the rename step is the only thing that changes
    what's visible at the final path — e.g. by checking the temp file's existence and the final
    path's unchanged content immediately before the rename call), *Then* the original file at
    `path` is untouched (still has its old, valid content) — a reader concurrently opening `path`
    at any point during the write never observes a truncated or partial file, only the old
    complete content or the new complete content, never something in between.
  - *Given* two overlapping `write_file` calls to the **same** `path` (e.g. two `index_docs`/
    `remove_indexed_source` calls for different sources, both rewriting the shared
    `sources.json` manifest around the same time — `SourceLocks` guards per-`source_id` writes to
    `chunks.jsonl`/`meta.json` but does not serialize writes to the shared manifest path), *When*
    both calls build their temp filenames concurrently, *Then* the two temp filenames are
    guaranteed distinct (not just distinct-with-high-probability) within the daemon's process
    lifetime, so neither call's `rename` can consume the other's temp file — each call's own
    `write_file` either fully succeeds or fully fails on its own terms, never with a spurious
    `ENOENT` caused by the other call's rename. (`std::process::id()` alone does **not** satisfy
    this — the daemon's PID is constant for its entire lifetime, so it does not distinguish
    between two calls to the same path within one daemon run.)
**Files**: `crates/native/src/fs.rs`

##### Task 1.3.2a: Implement temp-file + rename in `write_file` (~4 min)
- Replace the current body:
  ```rust
  tokio::fs::write(path, bytes).await.map_err(|e| PortError::Io(e.to_string()))
  ```
  with a temp-file-then-rename implementation: build a temp path in the same parent directory as
  `path`, with a **per-call-unique** suffix — `std::process::id()` alone is insufficient, since it
  is constant for the daemon's entire lifetime, so two concurrent `write_file` calls to the *same*
  `path` (e.g. two `index_docs`/`remove_indexed_source` calls for different sources both rewriting
  the shared `sources.json` manifest, which `SourceLocks` does not guard) would otherwise collide
  on an identical temp filename — whichever call renames first "wins," and the loser's own rename
  then fails with a spurious `ENOENT` (found in adversarial review). Add a module-level counter to
  `crates/native/src/fs.rs`:
  ```rust
  static TMP_FILE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
  ```
  and build the temp path as `format!("{path}.tmp-{}-{}", std::process::id(),
  TMP_FILE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed))` — same-directory is
  required for `rename` to be atomic, since cross-filesystem renames are not; `Ordering::Relaxed`
  is sufficient because the counter only needs to yield a distinct value per call within this
  process, not establish a happens-before relationship with any other memory. A monotonic counter
  is used rather than a random suffix deliberately: this codebase has no randomness dependency
  today, and uniqueness only needs to hold within one process's lifetime (temp files are
  same-directory, same-process by construction), so a counter is simpler and dependency-free. Then
  `tokio::fs::write(&tmp_path, bytes).await`, then `tokio::fs::rename(&tmp_path, path).await`,
  mapping either step's `Err` to `PortError::Io(e.to_string())`. On write failure, best-effort
  clean up the temp file (`let _ = tokio::fs::remove_file(&tmp_path).await;`) before returning the
  error, so a failed write doesn't leave stray `.tmp-*` files behind either.
- Files: `crates/native/src/fs.rs`

##### Task 1.3.2b: Unit test — atomic write leaves no partial file (~4 min)
- Add the three acceptance-criteria tests to `crates/native/src/fs.rs`'s existing test module (or
  create one following this crate's established test-module convention), including the
  concurrent-same-path-distinct-temp-filenames case (e.g. spawn two `write_file` calls to the same
  path via `tokio::join!` and assert both complete without an `ENOENT`/`PortError::Io`, or more
  narrowly, call the temp-path-building logic twice in a row and assert the two resulting paths
  differ).
- Files: `crates/native/src/fs.rs`

### Epic 1.4: New dependencies & path helpers
**Goal**: Get `fastembed` and `text-splitter` declared correctly (native-only where required),
and the `docs-index`/model-cache directory paths defined.

#### Story 1.4.1: Declare new crate dependencies
**As a** `docs-index` implementer, **I want** `fastembed` and `text-splitter` available, **so
that** Phase 2/3 code compiles.
**Acceptance Criteria**:
- `crates/native/Cargo.toml` gains `fastembed = "=5.17.2"` (exact pin, not a caret range —
  ADR-0001's Consequences section requires treating any `fastembed`/`ort` bump as a full
  re-verification pass, not a routine `cargo update`; `"5.17.2"` alone resolves to `^5.17.2` and
  would let a plain `cargo update -p fastembed` silently pull in a newer, unreviewed `fastembed`
  and transitively a newer `ort` RC with zero `Cargo.toml` diff); `crates/core/Cargo.toml` gains
  `text-splitter` as a `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]` entry (not an
  unconditional dependency), version `"0.32"` with the `markdown` feature.
  - *Given* `crates/core/Cargo.toml` before this change (no `text-splitter` dependency), *When*
    the target-specific dependency block is added, *Then* `cargo build -p stapler-mcp-core`
    (native, default target) succeeds and pulls in `text-splitter`, while a hypothetical
    `cargo build -p stapler-mcp-core --target wasm32-unknown-unknown` never attempts to compile
    `text-splitter` at all (confirmed by its absence from that build's dependency graph, e.g. via
    `cargo tree --target wasm32-unknown-unknown -p stapler-mcp-core -i text-splitter` reporting
    "package not found").
**Files**: `crates/native/Cargo.toml`, `crates/core/Cargo.toml`

##### Task 1.4.1a: Add `fastembed` to native (~2 min)
- Add `fastembed = "=5.17.2"` (exact pin — see Story 1.4.1's acceptance criteria for why a caret
  range doesn't satisfy ADR-0001) under `[dependencies]` in `crates/native/Cargo.toml`.
- Files: `crates/native/Cargo.toml`

##### Task 1.4.1b: Add `text-splitter` as a target-specific core dependency (~3 min)
- In `crates/core/Cargo.toml`, add:
  ```toml
  [target.'cfg(not(target_arch = "wasm32"))'.dependencies]
  text-splitter = { version = "0.32", features = ["markdown"] }
  ```
  below the existing `[dependencies]` block (do not add it to `[dependencies]` directly).
- Files: `crates/core/Cargo.toml`

##### Task 1.4.1c: Confirm native build picks it up (~2 min)
- Run `cargo build -p stapler-mcp-core -p stapler-mcp-native`; confirm it succeeds and
  `Cargo.lock` gains `fastembed`/`text-splitter` and their transitive deps.
- Files: `Cargo.lock` (generated)

#### Story 1.4.2: `docs_index_dir` / `embedding_cache_dir` path helpers
**As a** `docs-index` implementer, **I want** shared path helpers, **so that** `docs.rs`,
`cli/main.rs`, and `crates/native/src/embed.rs` agree on where index and model data live.
**Acceptance Criteria**:
- `paths::docs_index_dir(&env)` returns `{base_dir}/docs-index`; `paths::embedding_cache_dir(&env)`
  returns `{base_dir}/models`.
  - *Given* `STAPLER_MCP_HOME=/tmp/test-home`, *When* `paths::docs_index_dir(&env)` is called,
    *Then* it returns `"/tmp/test-home/docs-index"`.
**Files**: `crates/core/src/paths.rs`

##### Task 1.4.2a: Add the two helper functions (~2 min)
- Add `pub fn docs_index_dir<E: EnvPort>(env: &E) -> String { format!("{}/docs-index",
  base_dir(env)) }` and `pub fn embedding_cache_dir<E: EnvPort>(env: &E) -> String {
  format!("{}/models", base_dir(env)) }` to `crates/core/src/paths.rs`, following the exact
  style of the existing `cache_dir` function.
- Files: `crates/core/src/paths.rs`

### Epic 1.5: `wasm32` scope containment
**Goal**: Prevent `tools::docs` (and its native-only dependency `text-splitter`) from ever
becoming a wasm32 build blocker, per the Pattern Decisions table.

#### Story 1.5.1: cfg-gate the `docs` module
**As a** `docs-index` implementer, **I want** `tools::docs` compiled out of `wasm32-unknown-unknown`
builds entirely, **so that** the wasm crate keeps compiling regardless of `text-splitter`'s (or
`fastembed`'s) wasm32 status, which was never going to be exercised there anyway (no
`WasmEmbedder` exists — ADR-0002).
**Acceptance Criteria**:
- `crates/core/src/tools/mod.rs` declares `pub mod docs;` behind
  `#[cfg(not(target_arch = "wasm32"))]`.
  - *Given* `crates/core/src/tools/mod.rs` after this change, *When* `crates/wasm` (which depends
    on `stapler-mcp-core`) is built for `wasm32-unknown-unknown`, *Then* `stapler_mcp_core::tools::docs`
    is simply not part of the compiled crate for that target — no reference to it exists anywhere
    reachable from `crates/wasm/src/lib.rs`, so its absence causes no compile error.
**Files**: `crates/core/src/tools/mod.rs`

##### Task 1.5.1a: Add the cfg-gated module declaration (~1 min)
- In `crates/core/src/tools/mod.rs`, add:
  ```rust
  #[cfg(not(target_arch = "wasm32"))]
  pub mod docs;
  ```
  (Phase 3 creates the actual `docs.rs` file this refers to — this task only adds the
  declaration; it's fine for `docs.rs` not to exist yet as long as the module isn't referenced
  elsewhere before Phase 3 lands. If sequencing implementation strictly in order, defer this
  exact task until immediately before Task 3.2.1a, or create an empty placeholder file now.)
- Files: `crates/core/src/tools/mod.rs`

---

## Phase 2: `Embedder` Port + Native Adapter

### Epic 2.1: Port trait and native implementation
**Goal**: A working, testable `Embedder` abstraction over `fastembed`, per ADR-0002.

#### Story 2.1.1: `Embedder` port trait
**As a** `docs-index` implementer, **I want** an `Embedder` trait in `ports.rs`, **so that**
`docs.rs`'s tool functions can be generic over it (real `NativeEmbedder` in production, a
`FakeEmbedder` in tests).
**Acceptance Criteria**:
- `crates/core/src/ports.rs` defines `pub trait Embedder { async fn embed(&self, texts:
  &[String]) -> Result<Vec<Vec<f32>>, PortError>; }` with a doc comment stating it is
  intentionally native-only for v1 (no wasm implementation exists) and why.
  - *Given* `crates/core/src/ports.rs` after this change, *When* a test defines `struct
    FakeEmbedder; impl Embedder for FakeEmbedder { async fn embed(&self, texts: &[String]) ->
    Result<Vec<Vec<f32>>, PortError> { Ok(texts.iter().map(|_| vec![0.0f32; 384]).collect()) } }`,
    *Then* it compiles without needing any other change to `ports.rs`.
**Files**: `crates/core/src/ports.rs`

##### Task 2.1.1a: Add the trait (~2 min)
- Add `pub trait Embedder { async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>,
  PortError>; }` to `crates/core/src/ports.rs`, placed after `FileStore`, with the doc comment
  described above (reference ADR-0002 by decision, not by filename, e.g. "see docs-index's
  ADR-0002" is fine as a one-line pointer).
- Files: `crates/core/src/ports.rs`

#### Story 2.1.2: `NativeEmbedder` adapter
**As a** the native daemon, **I want** a real `Embedder` backed by `fastembed`, **so that**
`index_docs`/`search_docs` can actually produce embeddings.
**Acceptance Criteria**:
- `crates/native/src/embed.rs` defines `pub struct NativeEmbedder { ... }` implementing
  `Embedder`, lazily loading `fastembed::TextEmbedding` (model `AllMiniLML6V2`, cache dir =
  `embedding_cache_dir`) on first `embed()` call, reusing the loaded model on subsequent calls.
  - *Given* a fresh `NativeEmbedder::new(cache_dir)` that has never called `embed()`, *When*
    `embedder.embed(&["hello world".to_string()]).await` is called, *Then* it returns
    `Ok(vec![v])` where `v.len() == 384` (the model's embedding dimension), and a second call
    with the same input returns a vector with the same values (deterministic) without re-loading
    the model from disk (verified by the second call completing in a fraction of the first
    call's wall time in a manual timing check, not a strict automated assertion).
  - *Given* the model fails to load (e.g. corrupted cache file), *When* `embed()` is called,
    *Then* it returns `Err(PortError::Other(...))` with a message naming the actual failure (not
    a generic "internal error"), and every subsequent `embed()` call on the same `NativeEmbedder`
    fails identically (fail-fast, not retried-and-hidden) — matching ux.md §4's "embedding model
    fails to load" requirement.
  - *Given* an empty `cache_dir` directory (no prior model files), *When*
    `NativeEmbedder::new(cache_dir.clone())` is constructed and `embed(&["hello".to_string()])`
    is called for the first time, *Then* at least one file exists under `cache_dir` afterward
    (e.g. via `std::fs::read_dir(&cache_dir)` returning a non-empty iterator) — proving
    `embedding_cache_dir` was actually threaded into `fastembed`'s `InitOptions` and the model
    landed under `~/.stapler-mcp/models/`, not `fastembed`'s own default cache location (e.g.
    `~/.cache/fastembed`). This closes a real coverage gap flagged in adversarial review: "second
    call is fast" alone says nothing about *where* the model was cached.
**Files**: `crates/native/src/embed.rs`, `crates/native/src/lib.rs`

##### Task 2.1.2a: Verify exact `fastembed` 5.17.2 API and scaffold the struct (~4 min)
- Check `fastembed` 5.17.2's rustdoc (via `cargo doc -p fastembed --no-deps --open` or
  docs.rs) for the exact `TextEmbedding::try_new`/`InitOptions`/`EmbeddingModel::AllMiniLML6V2`
  signatures (flagged as unverified in `research/stack.md` — resolve here). Create
  `crates/native/src/embed.rs` with `pub struct NativeEmbedder { model:
  std::cell::RefCell<Option<fastembed::TextEmbedding>>, cache_dir: String }` and `pub fn
  new(cache_dir: String) -> Self`.
- Files: `crates/native/src/embed.rs`

##### Task 2.1.2b: Implement lazy model load + `embed()` (~5 min)
- Implement `impl Embedder for NativeEmbedder`: on `embed()`, if `self.model.borrow().is_none()`,
  initialize `TextEmbedding::try_new(...)` with `EmbeddingModel::AllMiniLML6V2` and the cache
  directory, mapping init failure to `PortError::Other(format!("failed to load embedding model:
  {e}"))`; store the result in the `RefCell` (note: on init *failure*, do not cache a "poisoned"
  `None` state permanently in a way that silently retries every call — re-attempting init on
  every call is acceptable and matches "fail fast and identically on every call" from ux.md,
  since a fresh attempt failing identically each time is the same observable behavior as caching
  the failure). Then call `.embed(texts.to_vec(), None)` (or the verified-correct method name/
  signature from Task 2.1.2a) and map its `Vec<Vec<f32>>` result (or convert from
  `ndarray`/other numeric type if `fastembed`'s actual return type differs — verify in Task
  2.1.2a) directly to the trait's `Result<Vec<Vec<f32>>, PortError>`.
- Files: `crates/native/src/embed.rs`

##### Task 2.1.2c: Benchmark real `fastembed` throughput and set `MAX_CHUNKS_PER_SOURCE` (~8 min)
- **Why this task exists**: adversarial review found `MAX_CHUNKS_PER_SOURCE = 2000` (used
  elsewhere in this plan, e.g. Story 4.1.2) was asserted with no measurement, and is arithmetically
  inconsistent with pitfalls.md §1's own "sub-10ms per call is tolerable inline blocking" framing —
  2000 chunks in one blocking call could plausibly take anywhere from seconds to over a minute,
  stalling the entire single-threaded daemon for every other in-flight tool call during that
  window. This task replaces the guess with a real measurement before the cap is used for real.
- Using the now-working `NativeEmbedder` from Task 2.1.2b, write a small throwaway benchmark
  (a `#[test] #[ignore]`d test in `crates/native/src/embed.rs`, or a `cargo run --example`
  scratch binary — either is fine, it does not need to be a permanent artifact) that: builds 100
  representative-length chunks (200–800 characters each, matching Story 3.2.1's chunk-size budget,
  with realistic prose/code-mixed content rather than repeated filler text), calls
  `embedder.embed(&chunks).await` once, and measures wall-clock elapsed time around that single
  call (`std::time::Instant`). Compute `chunks_per_second = 100.0 / elapsed.as_secs_f64()`.
- Run it once, record the measured `chunks_per_second` in a code comment next to the
  `MAX_CHUNKS_PER_SOURCE` constant definition (Task 4.1.2a) for future reference (e.g. `// measured
  ~85 chunks/sec on <hardware note>, 2026-07-14`).
- Set `MAX_CHUNKS_PER_SOURCE = floor(chunks_per_second × 8)`. The `8` (seconds) is this plan's
  chosen worst-case-inline-blocking budget: long enough to comfortably batch-embed a realistic
  "handful of doc sources" corpus in one `index_docs` call, short enough that the daemon stalling
  for it — during an explicit, rare, user-triggered indexing action, not a live query — is
  annoying but not catastrophic for a solo-dev tool with no other concurrent users. If the
  benchmark comes back low enough that `floor(chunks_per_second × 8)` would be uncomfortably small
  for realistic doc sources (e.g. under a few hundred), revisit this budget (raise it, e.g. to
  15–20s) rather than silently accepting a cap too small to be useful — but do not raise it past a
  point where a single `index_docs` call would perceptibly hang the daemon for a live user; if no
  budget in a reasonable range (up to ~20s) yields a workable cap, escalate to reconsidering the
  worker-thread option (rejected in the Pattern Decisions table) rather than shipping an
  unworkably small cap. As part of "is this cap workable," explicitly check `measured_cap >=
  SUB_BATCH_SIZE` (100) — a cap below `SUB_BATCH_SIZE` degenerates sub-batching (Story 4.1.3) to a
  single batch, silently losing the yield-and-interleave benefit for that hardware profile.
- Update every place in this plan that currently states `MAX_CHUNKS_PER_SOURCE = 2000` as a fixed
  fact (Domain Glossary, Story 4.1.2's acceptance criteria, Task 4.1.2a) to the value actually
  measured here before Task 4.1.2a is implemented for real; `2000` remains only as this plan's
  illustrative placeholder.
- Files: `crates/native/src/embed.rs` (benchmark test/example), `crates/core/src/tools/docs.rs`
  (constant + comment, once Task 3.4.x/4.1.2a defines it)

##### Task 2.1.2d: Export from `crates/native/src/lib.rs` (~1 min)
- Add `mod embed; pub use embed::NativeEmbedder;` (matching the existing `pub use` pattern for
  `NativeFs`, `NativeHttp`, etc.) to `crates/native/src/lib.rs`.
- Files: `crates/native/src/lib.rs`

---

## Phase 3: Core Domain Logic

### Epic 3.1: Schema types
**Goal**: Wire-format `Input`/`Output` structs for all four new tools, following `schema.rs`'s
existing `camelCase` / `///`-doc-comment / defaults-stated-inline conventions.

#### Story 3.1.1: `index_docs` and `search_docs` schema types
**As a** thin-client/daemon wiring implementer, **I want** typed, `schemars`-derived I/O for
`index_docs` and `search_docs`, **so that** the MCP tool schema an LLM caller sees matches
ux.md's field names exactly.
**Acceptance Criteria**:
- `IndexDocsInput { url: String, source: Option<String>, max_depth: Option<u32>, max_pages:
  Option<u32> }` serializes with `#[serde(rename_all = "camelCase")]` to `{url, source, maxDepth,
  maxPages}`. The wire field is named `source` (not `sourceName`) deliberately, matching
  `SearchDocsInput`/`RemoveIndexedSourceInput`'s `source` field for the same "which doc source"
  concept — a Product Triad Review pass flagged the original `sourceName` naming as an undocumented
  inconsistency with `ux.md`'s recommendation of one consistent field name across all four tools'
  inputs. (`IndexDocsOutput`/`IndexedSourceSummary`/`RemoveIndexedSourceOutput`'s *output* field
  stays `source_name`/`sourceName` — a different role, the resolved display name being returned,
  not a source-selecting input, so no inconsistency there.)
  - *Given* `IndexDocsInput { url: "https://tokio.rs/tokio/tutorial".into(), source:
    Some("tokio-tutorial".into()), max_depth: Some(2), max_pages: Some(20) }`, *When* serialized
    via `serde_json::to_value`, *Then* the resulting JSON is
    `{"url":"https://tokio.rs/tokio/tutorial","source":"tokio-tutorial","maxDepth":2,"maxPages":20}`.
- `IndexDocsOutput { source_name: String, source_id: String, pages_indexed: u32, pages_removed:
  Vec<String>, chunks_indexed: u32, embedding_model: String, truncated: bool }` — `truncated`
  is `true` when `MAX_CHUNKS_PER_SOURCE` was hit.
  - *Given* indexing `https://tokio.rs/tokio/tutorial` produces 12 pages and 340 chunks under
    the cap, *When* `index_docs` returns, *Then* `IndexDocsOutput { source_name:
    "tokio-tutorial", source_id: "tokio-tutorial", pages_indexed: 12, pages_removed: vec![],
    chunks_indexed: 340, embedding_model: "all-MiniLM-L6-v2", truncated: false }`.
- `SearchDocsInput { source: String, query: String, limit: Option<u32> }`,
  `SearchDocsOutput { results: Vec<DocsSearchResult> }`,
  `DocsSearchResult { text: String, score: f32, source_url: String, heading: Option<String>,
  source_title: String }`.
  - *Given* `SearchDocsInput { source: "tokio-tutorial", query: "how do I spawn a task",
    limit: Some(3) }` against an index containing a chunk whose text discusses `tokio::spawn`,
    *When* `search_docs` returns, *Then* `SearchDocsOutput.results` contains at most 3
    `DocsSearchResult`s sorted by descending `score`, and the top result's `heading` is
    `Some("Spawning")` (or similar section title) if that heading exists in the source markdown,
    `sourceUrl` is `"https://tokio.rs/tokio/tutorial/spawning"` (the specific sub-page, not the
    seed URL), and `sourceTitle` is that page's `<title>`.
**Files**: `crates/core/src/schema.rs`

##### Task 3.1.1a: `IndexDocsInput`/`IndexDocsOutput` (~4 min)
- Add both structs to `crates/core/src/schema.rs` with `#[derive(Debug, Clone, Serialize,
  Deserialize, JsonSchema)] #[serde(rename_all = "camelCase")]`, `///` doc comments on every
  field stating defaults/caps exactly as `ReadWebsiteInput`/`DownloadWebsiteInput` do (e.g.
  `maxDepth`/`maxPages` docs identical in wording to `ReadWebsiteInput`'s, since `index_docs`
  reuses the same `resolve_limits`).
- Files: `crates/core/src/schema.rs`

##### Task 3.1.1b: `SearchDocsInput`/`SearchDocsOutput`/`DocsSearchResult` (~4 min)
- Add all three structs, same derive/rename pattern. `limit` doc comment: "Maximum number of
  results to return, defaults to 5." `score` doc comment: "Cosine similarity to the query, higher
  is more relevant (range roughly -1.0 to 1.0)."
- Files: `crates/core/src/schema.rs`

#### Story 3.1.2: `list_indexed_sources` and `remove_indexed_source` schema types
**As a** thin-client/daemon wiring implementer, **I want** typed I/O for the remaining two
tools, **so that** the full 4-tool surface from ux.md is representable.
**Acceptance Criteria**:
- `ListIndexedSourcesInput {}` (empty struct, no fields), `ListIndexedSourcesOutput { sources:
  Vec<IndexedSourceSummary> }`, `IndexedSourceSummary { source_name: String, source_id: String,
  seed_url: String, page_count: u32, chunk_count: u32, indexed_at_millis: u64, embedding_model:
  String }`.
  - *Given* two sources previously indexed (`tokio-tutorial`, `serde-guide`), *When*
    `list_indexed_sources` is called with `{}`, *Then* `ListIndexedSourcesOutput.sources` has
    exactly 2 entries, each with a non-empty `sourceName` and `seedUrl` matching what was passed
    to the corresponding `index_docs` call.
- `RemoveIndexedSourceInput { source: String }`, `RemoveIndexedSourceOutput { removed: bool,
  source_name: String }`.
  - *Given* a previously-indexed source `"tokio-tutorial"`, *When*
    `remove_indexed_source({"source": "tokio-tutorial"})` is called, *Then* it returns
    `RemoveIndexedSourceOutput { removed: true, source_name: "tokio-tutorial" }`, and a
    subsequent `search_docs({"source": "tokio-tutorial", "query": "anything"})` call fails with
    an error listing it as no longer indexed.
**Files**: `crates/core/src/schema.rs`

##### Task 3.1.2a: `ListIndexedSourcesInput`/`Output`/`IndexedSourceSummary` (~3 min)
- Add all three structs, same conventions as 3.1.1.
- Files: `crates/core/src/schema.rs`

##### Task 3.1.2b: `RemoveIndexedSourceInput`/`Output` (~2 min)
- Add both structs, same conventions.
- Files: `crates/core/src/schema.rs`

### Epic 3.2: Chunking
**Goal**: Turn a page's markdown into structurally-sensible `Chunk`s with a small header prepended
before embedding (features.md §1's "prepend a small metadata header" recommendation), bounded by
a character budget.

#### Story 3.2.1: `chunk_markdown`
**As a** `docs::index_source` implementer, **I want** a pure function that splits one page's
markdown into `Chunk`s, **so that** indexing produces retrieval-friendly units instead of one
giant blob per page.
**Acceptance Criteria**:
- `chunk_markdown(markdown: &str, page_title: &str) -> Vec<Chunk>` splits at heading/paragraph
  boundaries within a `200..800`-character budget (using `text-splitter::MarkdownSplitter`), and
  each returned `Chunk`'s `heading` is the nearest preceding Markdown heading line found within
  that chunk's text (or the page's `#`-level title heading if none is found), never `panic`s on
  empty input (returns `vec![]` for `""`).
  - *Given* markdown `"# Tokio Tutorial\n\n## Spawning\n\nUse `tokio::spawn` to run an async
    task...\n\n## Async I/O\n\nTokio provides async versions of..."` (each section body under
    800 chars), *When* `chunk_markdown(markdown, "Tokio Tutorial")` is called, *Then* it returns
    at least 2 `Chunk`s, one with `heading == Some("Spawning".to_string())` whose `text` contains
    `"tokio::spawn"`, and one with `heading == Some("Async I/O".to_string())`.
  - *Given* `markdown = ""`, *When* `chunk_markdown("", "Empty Page")` is called, *Then* it
    returns `vec![]`.
**Files**: `crates/core/src/tools/docs.rs`

##### Task 3.2.1a: Define `Chunk` struct (~2 min)
- In `crates/core/src/tools/docs.rs` (create the file, add the module's top-of-file doc comment
  describing its purpose and its relationship to `webcrawl.rs`, per this file's own convention),
  define `pub(crate) struct Chunk { pub text: String, pub heading: Option<String>, pub
  chunk_index: u32 }`.
- Files: `crates/core/src/tools/docs.rs`

##### Task 3.2.1b: Verify `text-splitter` 0.32 `MarkdownSplitter` API and implement `chunk_markdown` (~5 min)
- Check `text-splitter` 0.32's rustdoc for the exact `MarkdownSplitter::new(...)` constructor
  signature (character-count `usize` vs. `Range<usize>` capacity — flagged unverified in
  `research/stack.md`; resolve here). Implement `pub(crate) fn chunk_markdown(markdown: &str,
  page_title: &str) -> Vec<Chunk>` using `MarkdownSplitter::new(200..800).chunks(markdown)`,
  tracking a simple regex/line-scan for the most recent `#`/`##`/`###` heading line seen so far
  in the source markdown as each chunk is produced (a single forward pass suffices — track
  "current heading" as you iterate the splitter's chunks in order, since chunks are yielded in
  document order).
- Files: `crates/core/src/tools/docs.rs`

##### Task 3.2.1c: Unit test — heading-aware chunking (~4 min)
- Add `#[cfg(test)] mod tests` in `docs.rs` (or a colocated test) exercising the Given-When-Then
  from Story 3.2.1's first acceptance criterion.
- Files: `crates/core/src/tools/docs.rs`

##### Task 3.2.1d: Unit test — empty input (~2 min)
- Add the empty-markdown test from Story 3.2.1's second acceptance criterion.
- Files: `crates/core/src/tools/docs.rs`

### Epic 3.3: Cosine similarity
**Goal**: A hand-computed-verified similarity function — the entire "vector search" half of this
feature per ADR-0001.

#### Story 3.3.1: `cosine_similarity` + unit tests
**As a** `docs::search_docs` implementer, **I want** a correct, tested cosine-similarity function,
**so that** ranking is trustworthy without needing a vector-search library (build-vs-buy.md §3's
correctness-risk mitigation).
**Acceptance Criteria**:
- `cosine_similarity(a: &[f32], b: &[f32]) -> f32` returns `1.0` for identical vectors, `0.0`
  for orthogonal vectors, and correctly ranks a known 3-vector set.
  - *Given* `a = [1.0, 0.0, 0.0]`, `b = [1.0, 0.0, 0.0]`, *When* `cosine_similarity(&a, &b)` is
    called, *Then* the result is `1.0` (within `1e-6` floating-point tolerance).
  - *Given* `a = [1.0, 0.0]`, `b = [0.0, 1.0]`, *When* `cosine_similarity(&a, &b)` is called,
    *Then* the result is `0.0` (within `1e-6` tolerance).
  - *Given* query `q = [1.0, 0.0]`, and three candidates `near = [0.9, 0.1]`, `orthogonal = [0.0,
    1.0]`, `opposite = [-1.0, 0.0]`, *When* each is scored against `q` and sorted descending,
    *Then* the order is `near, orthogonal, opposite` (scores approximately `0.994`, `0.0`,
    `-1.0` respectively).
**Files**: `crates/core/src/tools/docs.rs`

##### Task 3.3.1a: Implement `cosine_similarity` (~3 min)
- Add `pub(crate) fn cosine_similarity(a: &[f32], b: &[f32]) -> f32` to `docs.rs`: dot product
  divided by the product of both vectors' L2 norms; return `0.0` if either norm is `0.0` (guard
  against division by zero for an all-zero vector, which shouldn't occur in practice but must
  not panic).
- Files: `crates/core/src/tools/docs.rs`

##### Task 3.3.1b: Unit tests — identical, orthogonal, ranking (~4 min)
- Add the three test cases from Story 3.3.1's acceptance criteria, using
  `assert!((result - expected).abs() < 1e-6)` for float comparisons.
- Files: `crates/core/src/tools/docs.rs`

### Epic 3.4: `SourceId` and storage types
**Goal**: The newtype, slugification, and serde storage-record types the rest of Phase 4 builds
on.

#### Story 3.4.1: `SourceId`, `slugify`, `slugify_from_url`
**As a** `docs::index_source`/`search_docs` implementer, **I want** a safe, deterministic way to
turn a `source_name` (explicit or URL-derived) into a filesystem-safe directory key, **so that**
`SourceId` and `source_name` can never be accidentally interchanged (Pattern Decisions).
**Acceptance Criteria**:
- `SourceId::from_name(name: &str) -> SourceId` lowercases, replaces any run of
  non-alphanumeric characters with a single `-`, and trims leading/trailing `-`.
  - *Given* `"Tokio Tutorial!!"`, *When* `SourceId::from_name("Tokio Tutorial!!")` is called,
    *Then* the result's inner string is `"tokio-tutorial"`.
- `slugify_from_url(url: &Url) -> String` derives a default name from host+path, deduplicating a
  leading path segment that matches the host's first label.
  - *Given* `Url::parse("https://tokio.rs/tokio/tutorial").unwrap()`, *When*
    `slugify_from_url(&url)` is called, *Then* the result is `"tokio-tutorial"` (host stem
    `"tokio"` + path segments `["tokio", "tutorial"]` with the duplicate leading `"tokio"`
    segment collapsed).
  - *Given* `Url::parse("https://doc.rust-lang.org/book/").unwrap()`, *When*
    `slugify_from_url(&url)` is called, *Then* the result is `"doc-rust-lang-org-book"` (no
    matching leading segment to dedupe, so host+path are simply joined).
**Files**: `crates/core/src/tools/docs.rs`

##### Task 3.4.1a: `SourceId` newtype + `from_name` (~3 min)
- Add `#[derive(Debug, Clone, PartialEq, Eq, Hash)] pub(crate) struct SourceId(String)` (`Hash` is
  required so `SourceId` can key `SourceLocks`'s `HashSet<SourceId>` — see Story 3.4.3) with
  `pub(crate) fn from_name(name: &str) -> Self` (implements the slugify logic directly, or
  delegates to a shared `slugify(s: &str) -> String` free function used by both `from_name` and
  `slugify_from_url`) and `pub(crate) fn as_str(&self) -> &str`.
- Files: `crates/core/src/tools/docs.rs`

##### Task 3.4.1b: `slugify_from_url` (~3 min)
- Implement `pub(crate) fn slugify_from_url(url: &url::Url) -> String` per the acceptance
  criteria's dedup logic.
- Files: `crates/core/src/tools/docs.rs`

##### Task 3.4.1c: Unit tests — `SourceId::from_name` and `slugify_from_url` (~4 min)
- Add the three test cases from Story 3.4.1's acceptance criteria.
- Files: `crates/core/src/tools/docs.rs`

#### Story 3.4.2: `ChunkRecord`, `SourceMeta`, `SourceSummary` + path helpers
**As a** `docs::index_source`/`search_docs` implementer, **I want** serde-derived storage record
types and the path functions that locate them, **so that** Phase 4's read/write logic has
concrete types to serialize.
**Acceptance Criteria**:
- `ChunkRecord { chunk_text: String, embedding: Vec<f32>, source_url: String, chunk_index: u32,
  content_hash: String, heading: Option<String>, page_title: String }` round-trips through
  `serde_json::to_string`/`from_str` (JSONL: one such object per line). `page_title` is included
  in the canonical struct from the start (not added later) since `search_docs`'s
  `DocsSearchResult.source_title` needs it directly from each `ChunkRecord` — see Task 4.2.1d,
  which consumes this field rather than adding it.
  - *Given* a `ChunkRecord` with `chunk_text: "Use tokio::spawn...".into(), embedding: vec![0.1,
    0.2, 0.3], source_url: "https://tokio.rs/tokio/tutorial/spawning".into(), chunk_index: 0,
    content_hash: "abc123".into(), heading: Some("Spawning".into()), page_title: "Tokio
    Tutorial".into()`, *When* serialized then deserialized, *Then* the round-tripped value equals
    the original.
- `SourceMeta { source_id: String, source_name: String, seed_url: String, page_urls:
  Vec<String>, indexed_at_millis: u64, page_count: u32, chunk_count: u32, embedding_model:
  String }` and `SourceSummary` (same fields minus `page_urls`) both round-trip.
- `source_dir(docs_index_dir: &str, id: &SourceId) -> String`, `chunks_path(...)`,
  `meta_path(...)`, `sources_manifest_path(docs_index_dir: &str) -> String` produce the paths
  described in the Domain Glossary.
  - *Given* `docs_index_dir = "/home/tstapler/.stapler-mcp/docs-index"` and `id =
    SourceId::from_name("tokio-tutorial")`, *When* `chunks_path(docs_index_dir, &id)` is called,
    *Then* it returns
    `"/home/tstapler/.stapler-mcp/docs-index/tokio-tutorial/chunks.jsonl"`.
**Files**: `crates/core/src/tools/docs.rs`

##### Task 3.4.2a: `ChunkRecord` + `SourceMeta` + `SourceSummary` structs (~4 min)
- Add all three with `#[derive(Debug, Clone, Serialize, Deserialize)]` (no `JsonSchema` needed —
  these are internal storage records, not wire-facing MCP schema types, so they stay in `docs.rs`
  rather than `schema.rs`).
- Files: `crates/core/src/tools/docs.rs`

##### Task 3.4.2b: Path helper functions (~3 min)
- Add `pub(crate) fn source_dir`, `pub(crate) fn chunks_path`, `pub(crate) fn meta_path`,
  `pub(crate) fn sources_manifest_path` per the acceptance criteria.
- Files: `crates/core/src/tools/docs.rs`

##### Task 3.4.2c: Round-trip unit tests (~3 min)
- Add serde round-trip tests for `ChunkRecord` and `SourceMeta`, and the `chunks_path` example
  test.
- Files: `crates/core/src/tools/docs.rs`

#### Story 3.4.3: `SourceLocks` — in-memory per-source operation guard
**As a** `docs-index` user, **I want** a concurrent `index_docs` and `remove_indexed_source` call
on the same source to be rejected rather than silently interleaved, **so that** my on-disk index
data can't be corrupted by two related tool calls firing back-to-back (a normal-operation risk,
not just a daemon-crash one — found in adversarial review; see the "Concurrent same-source
operations" Pattern Decisions row).

**Placement note**: this lives here in Phase 3 (alongside `SourceId`, which `SourceLocks` is keyed
on) rather than in Phase 4 alongside the tool functions, because it's a small, self-contained data
structure with its own unit tests, no dependency on `HttpClient`/`Embedder`, and Phase 4's
`index_source`/`remove_indexed_source` tasks only need to *use* an already-defined type — same
shape as how `SourceId` itself (also just a type Phase 4 consumes) is defined here.

**Root cause being fixed**: `index_source`'s persistence step (Task 4.1.3c) is three sequential
awaited writes (`chunks.jsonl` → `meta.json` → `sources.json`). Because this daemon is
cooperatively scheduled (`.await` yield points exist at every `tokio::fs` call, even though it's
single-threaded), a `remove_indexed_source` call for the *same* source dispatched while
`index_docs` is between those awaits can delete files `index_docs` just wrote or is about to
write, leaving `sources.json`/`meta.json` claiming a source is fully indexed while `chunks.jsonl`
is missing. This requires no daemon crash — just two related tool calls arriving close together,
which is a plausible, normal thing for an LLM caller to do.

**Acceptance Criteria**:
- `SourceLocks::try_acquire(&self, id: &SourceId) -> Option<SourceLockGuard<'_>>` returns `None`
  if `id` is already held, `Some(guard)` otherwise; dropping the returned guard (success, error, or
  early-return — any exit path) releases the lock.
  - *Given* an empty `SourceLocks`, *When* `try_acquire(&SourceId::from_name("tokio-tutorial"))`
    is called twice in a row without dropping the first guard, *Then* the first call returns
    `Some(_)` and the second call returns `None`.
  - *Given* a `SourceLocks` with an active guard for `"tokio-tutorial"`, *When* that guard is
    dropped (goes out of scope), *Then* a subsequent `try_acquire(&SourceId::from_name(
    "tokio-tutorial"))` call returns `Some(_)` again.
- `index_source` and `remove_indexed_source` (Phase 4) both check-and-insert into a shared
  `SourceLocks` at the very start of their work (before any file I/O for that source) and release
  automatically via RAII on every exit path (success, any `Err` return, including early returns —
  guaranteed by the guard's `Drop` impl, not by manual cleanup at each return site).
  - *Given* two overlapping calls on the same `source`, one `index_docs` and one
    `remove_indexed_source` (or two `index_docs` calls), where the second is dispatched while the
    first is still in flight (e.g. between its `chunks.jsonl` and `meta.json` writes), *When* the
    second call attempts to acquire the same source's lock, *Then* it returns
    `Err("source 'tokio-tutorial' is already being indexed or removed; try again shortly")`
    immediately, without performing any file I/O, and the first call completes normally and
    unaffected.
- **Read path (`search_docs`) is deliberately *not* guarded** — documented decision, not an
  oversight: Story 1.3.2's atomic `write_file` (temp file + rename) means a concurrent reader of
  `chunks.jsonl`/`meta.json` always sees either the fully-old or fully-new version of a file, never
  a torn/partial one, so `search_docs` racing an `index_docs`/`remove_indexed_source` call cannot
  observe corrupted file *contents*. The one remaining edge case — `search_docs` reading `meta.json`
  (still present) right as a concurrent `remove_indexed_source` has deleted `chunks.jsonl` but not
  yet `meta.json` — surfaces as an empty-or-error result for that one call, is not itself
  data-corrupting, and self-resolves on the next call once the remove finishes; guarding every read
  to close this narrow, self-healing, non-corrupting window was judged not worth the added
  complexity for a single-user tool.
**Files**: `crates/core/src/tools/docs.rs`

##### Task 3.4.3a: `SourceLocks` struct + `try_acquire`/RAII guard (~5 min)
- Add to `crates/core/src/tools/docs.rs`:
  ```rust
  pub(crate) struct SourceLocks {
      active: RefCell<HashSet<SourceId>>,
  }

  pub(crate) struct SourceLockGuard<'a> {
      locks: &'a SourceLocks,
      id: SourceId,
  }

  impl SourceLocks {
      pub(crate) fn new() -> Self { ... }
      pub(crate) fn try_acquire(&self, id: &SourceId) -> Option<SourceLockGuard<'_>> { ... }
  }

  impl Drop for SourceLockGuard<'_> {
      fn drop(&mut self) {
          self.locks.active.borrow_mut().remove(&self.id);
      }
  }
  ```
  `try_acquire` inserts into `active` and returns `Some(SourceLockGuard { locks: self, id:
  id.clone() })` only if the insert indicates the id wasn't already present (`HashSet::insert`
  returns `bool`); otherwise returns `None` without mutating `active`. This mirrors the daemon's
  existing `!Send`, `Rc`/`RefCell` state-management style (see `crates/cli/src/main.rs`'s
  `Rc::new(NativeFs)`-then-`.clone()`-into-closure pattern for `http`/`fs`/`browser`) — `SourceLocks`
  is constructed once in `run_daemon()` as `Rc::new(SourceLocks::new())` and `.clone()`d into the
  `index_docs`/`remove_indexed_source` registration closures the same way `fs`/`http` already are
  (Phase 5, Task 5.1.1a/b/d).
- Files: `crates/core/src/tools/docs.rs`

##### Task 3.4.3b: Unit tests — acquire/release/reacquire and double-acquire rejection (~4 min)
- Add the two `SourceLocks`-level test cases from Story 3.4.3's first acceptance criterion (no
  `index_source`/`remove_indexed_source` involvement yet — that's covered by Phase 4's own tests
  in Task 4.1.1c/4.3.2b).
- Files: `crates/core/src/tools/docs.rs`

---

## Phase 4: `docs.rs` Tool Functions

### Epic 4.1: `index_docs`
**Goal**: The full crawl → dedup → chunk → embed → persist pipeline (Transaction Script).

#### Story 4.1.1: Crawl, seed-failure handling, within-crawl content-hash dedup, and the `SourceLocks` guard
**As a** `docs-index` user, **I want** `index_docs` to crawl a seed URL, skip duplicate-content
pages within the same crawl, and refuse to run if the same source is already mid-operation, **so
that** trailing-slash/query-param variants of the same page don't get embedded twice, and a
concurrent `remove_indexed_source`/second `index_docs` call on the same source can't corrupt
on-disk state (Story 3.4.3).
**Acceptance Criteria**:
- Before any file I/O, `index_source` resolves `SourceId` from the (explicit or URL-derived)
  `source_name` and calls `SourceLocks::try_acquire`; if it returns `None` (already held), return
  `Err("source '{source_name}' is already being indexed or removed; try again shortly")`
  immediately, holding the guard for the rest of the function's body on success (dropped on every
  exit path via RAII).
  - *Given* a `SourceLocks` that already holds a guard for `"tokio-tutorial"` (simulating an
    in-flight operation), *When* `index_source` is called with `source_name:
    Some("tokio-tutorial".into())`, *Then* it returns
    `Err("source 'tokio-tutorial' is already being indexed or removed; try again shortly")`
    without calling `http.get` or `fs.write_file` at all.
- If the seed URL itself fails to fetch (e.g. 404), `index_docs` returns `Err` naming the URL
  and status, without attempting to write any files.
  - *Given* `IndexDocsInput { url: "https://example.com/docs-that-dont-exist".into(), ... }`
    where that URL 404s, *When* `index_source` is called, *Then* it returns `Err("failed to
    index https://example.com/docs-that-dont-exist: 404 Not Found. Check the URL is still
    correct.")` and `~/.stapler-mcp/docs-index/` gains no new subdirectory.
- Two crawled URLs whose fetched HTML hashes identically (e.g. `/tutorial` and `/tutorial/`
  serving byte-identical content) contribute only one page's worth of chunks.
  - *Given* a crawl seeded at `https://example.com/docs` that discovers both
    `https://example.com/docs/page` and `https://example.com/docs/page/` (both returning
    byte-identical HTML), *When* `index_source` processes both, *Then* only the first-visited
    URL's content is chunked/embedded — the second is skipped via its `content_hash` matching an
    already-seen hash in this crawl, and `pages_indexed` in the eventual output counts it as 1,
    not 2.
**Files**: `crates/core/src/tools/docs.rs`

##### Task 4.1.1a: `SourceLocks` guard acquisition, seed validation + fetch-failure detection (~5 min)
- Begin `pub async fn index_source<H, F, E, C>(http: &H, fs: &F, embedder: &E, clock: &C,
  locks: &SourceLocks, docs_index_dir: &str, input: IndexDocsInput) -> Result<IndexDocsOutput,
  String> where H: HttpClient, F: FileStore, E: Embedder, C: ClockPort`. Validate `input.url`
  non-empty, parse via `Url::parse`. Resolve `source_name`/`SourceId` from `input.source` (falling
  back to `slugify_from_url(&url)` when `input.source` is `None`) — same logic Task 4.1.3b already
  needs; hoist it to the top of the function now that the lock needs it before any I/O, rather
  than duplicating it later in Task 4.1.3b. Call `locks.try_acquire(&id)`; on `None`,
  return the "already being indexed or removed" `Err` immediately. On `Some(guard)`, keep `guard`
  alive in a local binding for the rest of the function (it releases automatically via `Drop` on
  every return path, including the early-return `?`s introduced by later tasks — no manual
  cleanup needed at each exit point). Then construct the `Crawler` (reusing `pub(crate)
  Crawler::new`), call `next_url`/`fetch_and_expand` for the seed (depth 0) explicitly first; if
  that fails, return the `Err` described in the acceptance criteria before entering the main loop
  (the guard still drops correctly here since it's a local variable, not manually released).
- Files: `crates/core/src/tools/docs.rs`

##### Task 4.1.1b: Main crawl loop + content-hash dedup set (~5 min)
- Continue the loop via `crawler.next_url`/`fetch_and_expand` (reusing the exact loop shape from
  `webcrawl.rs::read_website`, per architecture.md §1.2). For each successfully fetched page,
  compute `content_hash = sha256_hex(&html)` (reuse `pub(crate) cache_key_for`-style hashing, or
  a small local helper doing the identical `Sha256::new().update(...).finalize()` pattern);
  maintain a `HashSet<String>` of seen hashes for this crawl; skip (but still count toward
  `max_pages` bookkeeping, since `fetch_and_expand` already ran) any page whose hash was already
  seen.
- Files: `crates/core/src/tools/docs.rs`

##### Task 4.1.1c: Unit tests — lock-guard rejection, seed 404, and content-hash dedup (~7 min)
- Using a `FakeHttpClient` test double (matching the style of any existing core-level test
  fakes, or newly hand-rolled per this file's `#[cfg(test)]` module) that serves a fixed
  URL→response map, write all three acceptance-criteria tests from Story 4.1.1, including the
  `SourceLocks`-rejection test: pre-acquire a guard for `"tokio-tutorial"` on a `SourceLocks`
  instance, pass that same `SourceLocks` (and a `FakeHttpClient` that would panic/fail the test if
  `.get` were called) to `index_source`, and assert the "already being indexed or removed" `Err`
  comes back without any fetch attempt.
- Files: `crates/core/src/tools/docs.rs`

#### Story 4.1.2: Chunk collection, `MAX_CHUNKS_PER_SOURCE` ceiling, and embedding-input headers
**As a** `docs-index` user, **I want** indexing bounded and each chunk embedded with a small
contextual header, **so that** a single huge reference page can't blow the daemon-blocking
budget, and retrieval quality benefits from title/heading context (features.md §1).
**Acceptance Criteria**:
- Total chunks collected across the whole crawl never exceed `MAX_CHUNKS_PER_SOURCE` (value set
  by Task 2.1.2c's real throughput benchmark, `floor(measured_chunks_per_sec × 8)`; this plan uses
  the illustrative placeholder `2000` below — substitute the real, benchmarked value once Task
  2.1.2c has run); if the raw total would exceed it, chunks beyond the cap are dropped and
  `IndexDocsOutput.truncated = true`. The check happens **per-chunk, not per-page**: the cap is
  tested after every individual chunk is produced during a page's chunking (not only once per
  page, and not only at the very end), so a single unusually large page cannot itself overshoot
  the cap before truncation takes effect — chunking of the *current* page stops mid-page the
  instant the cap is hit, rather than finishing that page's chunking first.
  - *Given* a crawl that would otherwise produce 2500 chunks across 50 pages (placeholder
    `MAX_CHUNKS_PER_SOURCE = 2000`), *When* `index_source` runs, *Then* exactly 2000
    `ChunkRecord`s are written and `IndexDocsOutput.truncated == true`.
  - *Given* a single page that alone would chunk into 3000 chunks (placeholder
    `MAX_CHUNKS_PER_SOURCE = 2000`, cap not yet reached by any prior page), *When* `index_source`
    processes that page, *Then* chunking of that page stops at chunk 2000 (mid-page) rather than
    producing all 3000 chunks and truncating afterward — confirmed by the `FakeHttpClient`/chunker
    test double recording that chunk production for that page halted early, not merely that the
    final stored count is 2000.
- Each chunk's text sent to `Embedder::embed` is `"{page_title} — {heading_or_page_title}\n\n
  {chunk.text}"`, while the *stored* `ChunkRecord.chunk_text` remains the raw `chunk.text` (no
  header) so `search_docs`'s returned `text` field is clean, quotable prose.
  - *Given* a chunk with `heading: Some("Spawning")` on a page titled `"Tokio Tutorial"` and
    `text: "Use tokio::spawn to run..."`, *When* it's embedded, *Then* the string passed to
    `Embedder::embed` is `"Tokio Tutorial — Spawning\n\nUse tokio::spawn to run..."`, but the
    `ChunkRecord.chunk_text` written to disk is exactly `"Use tokio::spawn to run..."`.
**Files**: `crates/core/src/tools/docs.rs`

##### Task 4.1.2a: Per-page chunking + collection with truncation (~5 min)
- Define `const MAX_CHUNKS_PER_SOURCE: usize = <value from Task 2.1.2c's benchmark>;` (with the
  `// measured ~N chunks/sec on <hardware note>, <date>` comment from Task 2.1.2c) in
  `crates/core/src/tools/docs.rs`.
- For each deduped, fetched page: `extract_title_and_markdown` (reused `pub(crate)`), then
  `chunk_markdown(&markdown, &title)`. Append each produced `Chunk` (plus its page's `url`/`title`/
  `content_hash`) to a running `Vec` **one chunk at a time**, checking the running total against
  `MAX_CHUNKS_PER_SOURCE` after every single chunk appended (per-chunk check, not per-page) —
  stop appending and set `truncated = true` the moment the cap is hit, even if that happens
  partway through one page's `chunk_markdown` output. This is a correctness requirement, not an
  optimization: `chunk_markdown` itself still returns a page's full `Vec<Chunk>` (it has no
  knowledge of the running cross-page total), so the truncation loop must consume that page's
  chunks incrementally and be able to stop mid-page — do not collect a whole page's chunks and
  then decide whether to truncate afterward, since that would let one large page overshoot the cap
  before truncation takes effect.
- Files: `crates/core/src/tools/docs.rs`

##### Task 4.1.2b: Build embedding-input strings with the header (~2 min)
- Build a parallel `Vec<String>` of `"{title} — {heading_or_title}\n\n{chunk.text}"` strings for
  the `Embedder::embed` call, keeping the plain `chunk.text` separately for the eventual
  `ChunkRecord.chunk_text`.
- Files: `crates/core/src/tools/docs.rs`

##### Task 4.1.2c: Unit tests — truncation and header construction (~5 min)
- Test the truncation acceptance criterion (construct a `FakeHttpClient` serving enough
  large pages to exceed the cap, or more simply, unit-test the truncation logic in isolation by
  feeding a pre-built `Vec<Chunk>` of >2000 entries directly to the truncation helper if it's
  factored as its own function) and the header-string-construction acceptance criterion.
- Files: `crates/core/src/tools/docs.rs`

#### Story 4.1.3: Embed in yielding sub-batches, persist (full replace), and report removed pages
**As a** `docs-index` user, **I want** embedding done in small sub-batches that yield to the
daemon between each one, and an atomic-per-file full replace of the source's stored data, **so
that** a long `index_docs` call doesn't stall every other in-flight daemon tool call for its
entire duration, and re-indexing `tokio-tutorial` a second time cleanly supersedes the first while
telling me which previously-indexed pages disappeared.

**Design note (resolves a blocker from adversarial review)**: the original plan called for
"exactly one `Embedder::embed` call... batched across all collected chunks," reasoning that
batching minimizes call overhead. Adversarial review correctly pointed out this makes the
`MAX_CHUNKS_PER_SOURCE` cap's "bounds worst-case inline blocking" claim misleading in practice:
even with a benchmark-derived cap (Task 2.1.2c), one single call covering the full cap's worth of
chunks is still one uninterruptible blocking span for the whole budget (up to ~8s per Task
2.1.2c), during which *zero* other tool calls can be interleaved, not even briefly. Instead,
`index_source` embeds in `SUB_BATCH_SIZE` (100)-chunk sub-batches, calling
`tokio::task::yield_now().await` between each sub-batch's `embed()` call. This was weighed against
just keeping one giant call: sub-batching adds a small loop and one extra `.await` point per
sub-batch — trivial complexity, no new `Send` boundary, no new thread — in exchange for turning
one long uninterruptible stall into several shorter ones the single-threaded scheduler can
interleave other tool calls around. That trade is clearly worth it here, so sub-batching is the
chosen implementation, not a single unbatched call.
**Acceptance Criteria**:
- Chunks are embedded in sub-batches of at most `SUB_BATCH_SIZE` (100) chunks each, in original
  order, with `tokio::task::yield_now().await` called between sub-batches (but not after the
  last one).
  - *Given* a crawl producing 340 chunks across 12 pages, *When* `index_source` runs, *Then* the
    `FakeEmbedder` test double records exactly 4 calls to `embed` (`ceil(340 / 100)`), with
    `texts.len()` values `[100, 100, 100, 40]` in that order, and the resulting `Vec<ChunkRecord>`
    preserves original chunk order across the sub-batch boundaries (chunk 150's embedding is not
    swapped with chunk 250's).
- Re-indexing a source whose previous `page_urls` included a URL absent from the new crawl
  reports it in `pages_removed`.
  - *Given* `tokio-tutorial` was previously indexed with `page_urls =
    ["https://tokio.rs/tokio/tutorial", "https://tokio.rs/tokio/tutorial/old-page"]`, and a
    re-index crawl only discovers `"https://tokio.rs/tokio/tutorial"` (the `old-page` URL no
    longer linked/reachable), *When* `index_source` completes, *Then*
    `IndexDocsOutput.pages_removed == vec!["https://tokio.rs/tokio/tutorial/old-page"]`.
- `chunks.jsonl` and `meta.json` are fully overwritten (not appended/merged) on every
  `index_docs` call for a given source.
  - *Given* `tokio-tutorial`'s `chunks.jsonl` previously had 200 lines, *When* a re-index
    produces 340 chunks, *Then* the file on disk after the call has exactly 340 lines, not 540.
**Files**: `crates/core/src/tools/docs.rs`

##### Task 4.1.3a: Sub-batched `embed()` calls + assemble `ChunkRecord`s (~6 min)
- Add `const SUB_BATCH_SIZE: usize = 100;` near `MAX_CHUNKS_PER_SOURCE` (Task 4.1.2a).
- For each `chunk(SUB_BATCH_SIZE)` slice of `embed_inputs` (via `.chunks(SUB_BATCH_SIZE)`), call
  `embedder.embed(sub_batch).await.map_err(|e| e.to_string())?`, appending the returned
  `Vec<Vec<f32>>` to a running `Vec<Vec<f32>>` of all embeddings in order; after each sub-batch
  except the last, call `tokio::task::yield_now().await` before continuing to the next one. Zip
  the final concatenated embeddings vector with the collected `(Chunk, url, content_hash,
  page_title)` tuples (same original order, `page_title` carried from each page's
  `extract_title_and_markdown` result) to build `Vec<ChunkRecord>`, setting each record's
  `page_title` field directly (no separate lookup at search time — see Task 4.2.1d).
- Files: `crates/core/src/tools/docs.rs`

##### Task 4.1.3b: Load old `SourceMeta` for removed-page diffing (~3 min)
- `source_name`/`id` were already resolved in Task 4.1.1a (needed there for the `SourceLocks`
  guard) — reuse those bindings rather than re-deriving them here. Read the existing `meta_path`
  via `fs.read_file` (`Option<SourceMeta>`); compute `pages_removed` as
  `old_meta.map(|m| m.page_urls).unwrap_or_default()` minus the URLs actually fetched this crawl.
- Files: `crates/core/src/tools/docs.rs`

##### Task 4.1.3c: Write `chunks.jsonl`, `meta.json`, update `sources.json` manifest (~5 min)
- Serialize `Vec<ChunkRecord>` as newline-joined JSON objects (JSONL) and `fs.write_file(&
  chunks_path(...), ...)`. Build and write `SourceMeta` to `meta_path(...)`. Read
  `sources_manifest_path`, deserialize `Vec<SourceSummary>` (empty if `Ok(None)`), replace/insert
  the entry for this `source_id`, write it back.
- Files: `crates/core/src/tools/docs.rs`

##### Task 4.1.3d: Assemble `IndexDocsOutput`, return (~2 min)
- Build and return `IndexDocsOutput { source_name, source_id: id.as_str().to_string(),
  pages_indexed, pages_removed, chunks_indexed, embedding_model: EMBEDDING_MODEL_ID.to_string(),
  truncated }`.
- Files: `crates/core/src/tools/docs.rs`

##### Task 4.1.3e: Unit tests — sub-batched embed calls, removed-pages diff, full replace (~6 min)
- Write the three acceptance-criteria tests using `FakeHttpClient`/`InMemoryFileStore`/
  `FakeEmbedder` test doubles (the first test asserts on `FakeEmbedder`'s recorded per-call
  `texts.len()` sequence and total call count, per Story 4.1.3's updated sub-batching acceptance
  criterion, not a single call).
- Files: `crates/core/src/tools/docs.rs`

### Epic 4.2: `search_docs`
**Goal**: Load a source's stored chunks, embed the query, rank by cosine similarity.

#### Story 4.2.1: Load, unknown-source error, embedding-model guard, rank, return top-k
**As a** `docs-index` user, **I want** `search_docs` to rank stored chunks against my query and
fail helpfully when the source doesn't exist or was indexed with a different model, **so that**
I get either a correct ranked result set or an actionable error (ux.md §4).
**Acceptance Criteria**:
- Searching a never-indexed source name returns an error listing the currently-indexed sources.
  - *Given* only `tokio-tutorial` and `serde-guide` are indexed, *When* `search_docs({"source":
    "tokio", "query": "spawn"})` is called, *Then* it returns `Err("no indexed source named
    'tokio'; currently indexed: tokio-tutorial, serde-guide. Call list_indexed_sources for
    details, or index_docs to add a new source.")`.
- Searching a source whose stored `embedding_model` doesn't match `EMBEDDING_MODEL_ID` fails
  clearly rather than returning meaningless scores.
  - *Given* `tokio-tutorial`'s `meta.json` has `embedding_model: "some-older-model-v1"` while the
    daemon's current `EMBEDDING_MODEL_ID` is `"all-MiniLM-L6-v2"`, *When* `search_docs({"source":
    "tokio-tutorial", "query": "spawn"})` is called, *Then* it returns `Err("tokio-tutorial was
    indexed with embedding model 'some-older-model-v1', but this daemon now uses
    'all-MiniLM-L6-v2'. Re-run index_docs to rebuild the index with the current model.")`
    without attempting to compute any similarity scores.
- A successful search returns results sorted descending by `score`, capped at `input.limit`
  (default 5).
  - *Given* `tokio-tutorial` has 340 stored chunks and `SearchDocsInput { source:
    "tokio-tutorial", query: "how do I spawn a task", limit: None }`, *When* `search_docs` is
    called, *Then* `SearchDocsOutput.results.len() <= 5`, and for every adjacent pair `results[i]`,
    `results[i+1]`, `results[i].score >= results[i+1].score`.
- A malformed/truncated line in `chunks.jsonl` (e.g. from an interrupted write, or on-disk
  corruption) is skipped with a logged warning, not a hard error for the whole search — matching
  `webcrawl.rs::fetch_and_expand`'s existing "skip one bad page, don't fail the whole crawl"
  precedent (blocker found in adversarial review; this was previously unspecified).
  - *Given* a `chunks.jsonl` with 10 valid `ChunkRecord` lines and 1 additional truncated/malformed
    line (e.g. `{"chunk_text": "incomplete...` with no closing brace) appended after them, *When*
    `search_docs` is called against that source, *Then* it returns `Ok(SearchDocsOutput)` with
    results computed from the 10 valid chunks (ranked/capped normally), does not error, and emits
    one `eprintln!` warning line naming the source and the (1-indexed) line number that failed to
    parse.
**Files**: `crates/core/src/tools/docs.rs`

##### Task 4.2.1a: Resolve source, load meta+chunks, unknown-source error path (~5 min)
- `pub async fn search_docs<F, E>(fs: &F, embedder: &E, docs_index_dir: &str, input:
  SearchDocsInput) -> Result<SearchDocsOutput, String> where F: FileStore, E: Embedder`. Compute
  `id = SourceId::from_name(&input.source)`; `fs.read_file(&meta_path(...))`; on `Ok(None)`,
  build the unknown-source error by reading `sources_manifest_path` and joining its
  `source_name`s.
- Files: `crates/core/src/tools/docs.rs`

##### Task 4.2.1b: Embedding-model mismatch guard (~2 min)
- After successfully loading `SourceMeta`, compare `meta.embedding_model` to
  `EMBEDDING_MODEL_ID`; return the mismatch `Err` described above if they differ, before reading
  `chunks.jsonl` or calling `embedder.embed`.
- Files: `crates/core/src/tools/docs.rs`

##### Task 4.2.1c: Embed query, score, sort, truncate to `limit` (~5 min)
- Read+parse `chunks.jsonl` line by line (not one `serde_json::from_str` call over the whole
  file — parsing must be per-line so one bad line doesn't abort the rest). For each line, call
  `serde_json::from_str::<ChunkRecord>(line)`; on `Ok(record)`, keep it; on `Err(e)`, `eprintln!`
  a warning (`"search_docs: skipping malformed chunk on {source_name} line {line_number}: {e}"`)
  and continue to the next line rather than propagating the error — matching
  `webcrawl.rs::fetch_and_expand`'s existing skip-one-bad-item-not-the-whole-operation precedent.
  Call `embedder.embed(&[input.query.clone()])`, take the single resulting vector as `query_vec`.
  Score every successfully-parsed `ChunkRecord` via `cosine_similarity(&query_vec,
  &record.embedding)`, sort descending by score, truncate to `input.limit.unwrap_or(5) as usize`.
- Files: `crates/core/src/tools/docs.rs`

##### Task 4.2.1d: Map to `DocsSearchResult`, assemble output (~2 min)
- Map each surviving `(ChunkRecord, score)` to `DocsSearchResult { text: record.chunk_text,
  score, source_url: record.source_url, heading: record.heading, source_title:
  record.page_title.clone() }` — `ChunkRecord.page_title` is already part of the canonical struct
  (Task 3.4.2a), populated at write time by Task 4.1.3a, so this is a direct field read, not a
  second lookup or a schema change made here.
- Files: `crates/core/src/tools/docs.rs`

##### Task 4.2.1e: Unit tests — unknown source, model mismatch, ranked+capped results, malformed-line skip (~8 min)
- Write all four acceptance-criteria tests using `InMemoryFileStore`/`FakeEmbedder` test doubles
  pre-seeded with known `ChunkRecord`s and embeddings chosen so the expected ranking is
  unambiguous (e.g. reuse the "near/orthogonal/opposite" vectors from Story 3.3.1). For the
  malformed-line test, seed the `InMemoryFileStore`'s `chunks.jsonl` bytes directly with 10 valid
  JSONL lines plus 1 hand-crafted malformed line, rather than going through the normal
  `ChunkRecord`-serialization path (which would never produce malformed output).
- Files: `crates/core/src/tools/docs.rs`

### Epic 4.3: `list_indexed_sources` / `remove_indexed_source`
**Goal**: The two lightweight lifecycle tools from ux.md's 4-tool surface.

#### Story 4.3.1: `list_indexed_sources`
**As a** `docs-index` user, **I want** to see what I've indexed, **so that** I can resolve a
loosely-remembered name ("my tokio docs") to its exact stored `sourceName`.
**Acceptance Criteria**:
- Returns every entry currently in `sources.json`, or an empty list if nothing has been indexed
  yet (not an error).
  - *Given* an empty `~/.stapler-mcp/docs-index/` (no `sources.json` file exists yet), *When*
    `list_indexed_sources({})` is called, *Then* it returns `Ok(ListIndexedSourcesOutput {
    sources: vec![] })`, not an error.
  - *Given* `tokio-tutorial` and `serde-guide` are indexed, *When* `list_indexed_sources({})` is
    called, *Then* it returns both, each with a correct `pageCount`/`chunkCount`/
    `indexedAtMillis`/`embeddingModel` matching what `index_docs` last wrote for that source.
**Files**: `crates/core/src/tools/docs.rs`

##### Task 4.3.1a: Implement `list_indexed_sources` (~3 min)
- `pub async fn list_indexed_sources<F: FileStore>(fs: &F, docs_index_dir: &str, _input:
  ListIndexedSourcesInput) -> Result<ListIndexedSourcesOutput, String>`: read
  `sources_manifest_path`, `Ok(None)` → `Ok(ListIndexedSourcesOutput { sources: vec![] })`,
  otherwise deserialize and map `SourceSummary` → `IndexedSourceSummary`.
- Files: `crates/core/src/tools/docs.rs`

##### Task 4.3.1b: Unit tests — empty and populated manifest (~3 min)
- Write both acceptance-criteria tests.
- Files: `crates/core/src/tools/docs.rs`

#### Story 4.3.2: `remove_indexed_source`
**As a** `docs-index` user, **I want** to delete a source I no longer need, **so that** my index
doesn't grow unboundedly with stale content (features.md §1's "unbounded growth" edge case), and
without racing a concurrent `index_docs` call on the same source (Story 3.4.3).
**Acceptance Criteria**:
- Before any file I/O, `remove_indexed_source` resolves `SourceId` from `input.source` and calls
  `SourceLocks::try_acquire`, same guard mechanism and error message as `index_source` (Story
  4.1.1) — this is the other half of the "already in progress" acceptance criterion covered there
  (an `index_docs` call in flight blocks a `remove_indexed_source` on the same source, and
  vice versa).
  - *Given* a `SourceLocks` that already holds a guard for `"tokio-tutorial"` (simulating an
    in-flight `index_docs` call), *When* `remove_indexed_source({"source": "tokio-tutorial"})` is
    called with that same `SourceLocks`, *Then* it returns
    `Err("source 'tokio-tutorial' is already being indexed or removed; try again shortly")`
    without calling `fs.delete_file` or `fs.read_file` at all.
- Removing a source deletes its `chunks.jsonl` and `meta.json`, and removes its entry from
  `sources.json`; a subsequent `search_docs`/`list_indexed_sources` no longer sees it.
  - *Given* `tokio-tutorial` is indexed, *When* `remove_indexed_source({"source":
    "tokio-tutorial"})` is called, *Then* `~/.stapler-mcp/docs-index/tokio-tutorial/chunks.jsonl`
    and `.../meta.json` no longer exist on disk, `sources.json` no longer contains an entry with
    `sourceName == "tokio-tutorial"`, and the call returns `RemoveIndexedSourceOutput { removed:
    true, source_name: "tokio-tutorial" }`.
- Removing a never-indexed source name is a clear error, not a silent no-op success.
  - *Given* no source named `"nonexistent"` was ever indexed, *When*
    `remove_indexed_source({"source": "nonexistent"})` is called, *Then* it returns
    `Err("no indexed source named 'nonexistent'; currently indexed: ...")` — same message shape
    as `search_docs`'s unknown-source error (Story 4.2.1's first criterion), for consistency.
**Files**: `crates/core/src/tools/docs.rs`

##### Task 4.3.2a: Implement `remove_indexed_source` (~6 min)
- `pub async fn remove_indexed_source<F: FileStore>(fs: &F, locks: &SourceLocks, docs_index_dir:
  &str, input: RemoveIndexedSourceInput) -> Result<RemoveIndexedSourceOutput, String>`: resolve
  `SourceId` first, then immediately call `locks.try_acquire(&id)` before any `fs` call — on
  `None`, return the "already being indexed or removed" `Err`; keep the guard alive in a local
  binding for the rest of the function (RAII release on every exit path). Then read `meta_path` to
  confirm existence (reusing the same unknown-source error builder as `search_docs` — factor it
  into a shared `pub(crate) fn unknown_source_error(fs: &impl FileStore, docs_index_dir: &str,
  requested: &str) -> String` helper used by both), then `fs.delete_file(&chunks_path(...))`,
  `fs.delete_file(&meta_path(...))`, rewrite `sources.json` with that entry removed.
- Files: `crates/core/src/tools/docs.rs`

##### Task 4.3.2b: Unit tests — successful remove, unknown-source error, lock-guard rejection (~6 min)
- Write all three acceptance-criteria tests, including the `SourceLocks`-rejection test (mirrors
  Task 4.1.1c's version: pre-acquire a guard for the target source, assert the "already in
  progress" `Err` comes back with zero `fs.delete_file`/`fs.read_file` calls).
- Files: `crates/core/src/tools/docs.rs`

---

## Phase 5: Wiring

### Epic 5.1: Daemon registration
**Goal**: Make the four new tools callable over the daemon's socket, matching every existing
tool's `daemon.register(name, json_handler(...))` shape exactly.

#### Story 5.1.1: Register `index_docs`, `search_docs`, `list_indexed_sources`, `remove_indexed_source`
**As a** the daemon, **I want** the four new tools registered with the correct captured ports,
**so that** `thin_client.rs` (Phase 5.2) has something to call.
**Acceptance Criteria**:
- All four tools are registered in `run_daemon()` before `daemon.run(...)` is called, each
  wrapping its `docs::` function with the exact ports it needs (`http`+`fs`+`embedder`+`clock`
  for `index_docs`; `fs`+`embedder` for `search_docs`; `fs` only for the other two).
  - *Given* the daemon has started with a `docs-index`-capable build, *When* a raw
    `client::call(&socket, &sock_path, "index_docs", Some(json!({"url": "http://127.0.0.1:PORT/",
    "source": "test-source"})), timeout)` is issued, *Then* it returns
    `Ok(serde_json::Value)` shaped like `IndexDocsOutput`, not `Err("unknown tool \"index_docs\"")`.
**Files**: `crates/cli/src/main.rs`

##### Task 5.1.1a: Construct `NativeEmbedder`, `SourceLocks`, and `docs_index_dir`, import types (~3 min)
- In `run_daemon()`, after the existing `http`/`fs`/`browser` construction, add: `let embedder =
  Rc::new(NativeEmbedder::new(paths::embedding_cache_dir(&env)));`, `let source_locks =
  Rc::new(docs::SourceLocks::new());` (Story 3.4.3 — shared, `.clone()`d into the `index_docs`/
  `remove_indexed_source` registration closures the same way `fs`/`http` already are, matching
  this function's existing `Rc::new(...)`-then-`.clone()` pattern), and `let docs_index_dir =
  paths::docs_index_dir(&env);`. Add the new imports (`stapler_mcp_core::schema::{IndexDocsInput,
  SearchDocsInput, ListIndexedSourcesInput, RemoveIndexedSourceInput}`,
  `stapler_mcp_core::tools::docs`, `stapler_mcp_native::NativeEmbedder`).
- Files: `crates/cli/src/main.rs`

##### Task 5.1.1b: Register `index_docs` (~4 min)
- `daemon.register("index_docs", json_handler({ let http = http.clone(); let fs = fs.clone();
  let embedder = embedder.clone(); let source_locks = source_locks.clone(); let clock = /*
  NativeClock or similar ClockPort impl, constructed alongside http/fs above if not already
  present */; let docs_index_dir = docs_index_dir.clone(); move |input: IndexDocsInput| { ... async
  move { docs::index_source(&*http, &*fs, &*embedder, &clock, &source_locks, &docs_index_dir,
  input).await } } }));` — matching the exact clone-into-closure shape every other registration
  already uses, with `source_locks` cloned in alongside `fs`/`http`/`embedder`. Note: a
  `ClockPort` implementation must be constructed here; check whether `NativeEnv`/an existing
  native clock type already exists in `crates/native/src/lib.rs` (used elsewhere for
  `EnsureOptions`/`client::ensure_daemon` in `thin_client.rs` — `NativeClock` is already imported
  there) and reuse it rather than inventing a second one.
- Files: `crates/cli/src/main.rs`

##### Task 5.1.1c: Register `search_docs` (~2 min)
- Same pattern, capturing `fs`+`embedder`+`docs_index_dir`.
- Files: `crates/cli/src/main.rs`

##### Task 5.1.1d: Register `list_indexed_sources` and `remove_indexed_source` (~4 min)
- `list_indexed_sources`: same pattern, capturing `fs`+`docs_index_dir` only (no `embedder`/
  `http`/`source_locks` needed — read-only, per Story 3.4.3's read-path decision). `
  remove_indexed_source`: same pattern, capturing `fs`+`source_locks`+`docs_index_dir` (no
  `embedder`/`http` needed), calling `docs::remove_indexed_source(&*fs, &source_locks,
  &docs_index_dir, input).await`.
- Files: `crates/cli/src/main.rs`

### Epic 5.2: Thin-client MCP endpoints
**Goal**: Expose the four tools to Claude Code via the `rmcp` `#[tool]` macro, matching ux.md's
exact tool-level description density.

#### Story 5.2.1: Four `#[tool]` methods on `ThinClient`
**As a** Claude Code (or Tyler, typing a natural-language request), **I want** `index_docs`/
`search_docs`/`list_indexed_sources`/`remove_indexed_source` visible in the MCP tool list with
clear descriptions, **so that** I can index and search doc sources without knowing the daemon's
internal wiring.
**Acceptance Criteria**:
- All four tools appear in the MCP server's `tools/list` response with descriptions matching
  ux.md's density convention (what it does, key parameter behavior, caching/state note).
  - *Given* the `stapler-mcp` thin client is connected via stdio, *When* an MCP `tools/list`
    request is sent, *Then* the response includes `index_docs`, `search_docs`,
    `list_indexed_sources`, and `remove_indexed_source`, each with a non-empty `description` and
    a `inputSchema` matching their respective `*Input` struct's `schemars`-derived JSON Schema.
**Files**: `crates/cli/src/thin_client.rs`

##### Task 5.2.1a: `index_docs` endpoint (~3 min)
- Add `#[tool(name = "index_docs", description = "Crawl a URL (reusing the same same-host/
  robots.txt-respecting crawler as read_website, up to maxDepth/maxPages) and build a local
  semantic search index over it under the given source name (or a name derived from the URL).
  Re-running index_docs on an already-indexed source fully re-indexes it in place. Indexing runs
  in yielding sub-batches so other daemon calls can interleave during long indexing runs, rather
  than blocking for the full duration.")] async fn index_docs(&self,
  params: Parameters<IndexDocsInput>) -> Result<Json<IndexDocsOutput>, String> {
  call_daemon("index_docs", params.0).await.map(Json) }`.
- Files: `crates/cli/src/thin_client.rs`

##### Task 5.2.1b: `search_docs` endpoint (~2 min)
- Add `#[tool(name = "search_docs", description = "Semantically search a previously index_docs'd
  source by name, returning the top-scoring text chunks ranked by relevance to query.")]`
  method, same `call_daemon` forwarding shape.
- Files: `crates/cli/src/thin_client.rs`

##### Task 5.2.1c: `list_indexed_sources` endpoint (~2 min)
- Add `#[tool(name = "list_indexed_sources", description = "List every doc source currently
  indexed via index_docs, with page/chunk counts and when each was last indexed.")]` method.
- Files: `crates/cli/src/thin_client.rs`

##### Task 5.2.1d: `remove_indexed_source` endpoint (~2 min)
- Add `#[tool(name = "remove_indexed_source", description = "Permanently delete a previously
  indexed doc source and all its stored chunks. Use only if explicitly instructed — there is no
  undo.")]` method (description language deliberately mirrors `docs-mcp-server`'s `remove_docs`
  caution per ux.md §3).
- Files: `crates/cli/src/thin_client.rs`

---

## Phase 6: Tests

### Epic 6.1: Additional cross-cutting unit tests
**Goal**: Cover the acceptance criteria not already exercised by Phase 3/4's per-story unit
tests: redirect final-URL propagation and the full happy-path shape end-to-end at the `docs.rs`
function level (still no real daemon/socket/model involved).

#### Story 6.1.1: Redirect final-URL propagation into `ChunkRecord.source_url`
**As a** `docs-index` user, **I want** a chunk's `sourceUrl` to reflect the page actually served,
**so that** a `search_docs` result's link isn't a dead/redirecting URL.
**Acceptance Criteria**:
- A page reached via a redirect has its `ChunkRecord.source_url` set to the final URL, not the
  originally-crawled one.
  - *Given* a `FakeHttpClient` where fetching `"https://example.com/old"` returns `HttpResponse {
    status: 200, body: <html>, final_url: "https://example.com/new".to_string() }`, *When*
    `index_source` processes that page, *Then* every `ChunkRecord` produced from it has
    `source_url == "https://example.com/new"`.
**Files**: `crates/core/src/tools/docs.rs`

##### Task 6.1.1a: Thread `final_url` through `index_source`'s page-processing (~3 min)
- Confirm (or adjust, if Task 4.1.1b didn't already do this) that `index_source` uses
  `resp.final_url` rather than the crawled `url` when building each `ChunkRecord.source_url` and
  the `SourceMeta.page_urls` entry for that page. Note: `fetch_and_expand` currently returns just
  `Option<String>` (the HTML body) per `webcrawl.rs`'s existing signature — if `final_url` isn't
  threaded through `fetch_and_expand`'s return value, this task must extend it (a small,
  `pub(crate)`-only signature change: return `Option<(String, String)>` — `(html, final_url)` —
  or a small named struct instead of a bare tuple for clarity) and update its two other callers
  (`read_website`, `download_website`) to ignore the new second field. Flag this as a slightly
  larger-than-typical task since it touches a shared, already-`pub(crate)` function from Phase 1.
- Files: `crates/core/src/tools/webcrawl.rs`, `crates/core/src/tools/docs.rs`

##### Task 6.1.1b: Unit test — redirect final-URL propagation (~3 min)
- Write the acceptance-criteria test.
- Files: `crates/core/src/tools/docs.rs`

### Epic 6.2: End-to-end daemon integration test
**Goal**: One thin, real-daemon smoke test proving the wiring (Phase 5) is correct, mirroring
`crates/cli/tests/webcrawl.rs`'s structure — explicitly **not** the primary correctness
verification (that's Phase 3/4's fast, offline `FakeEmbedder`-based unit tests).

#### Story 6.2.1: `index_docs` → `search_docs` → `list_indexed_sources` → `remove_indexed_source` round trip
**As a** the project maintainer, **I want** one integration test proving all four tools are
correctly registered and reachable through the real daemon socket, **so that** a wiring mistake
in Phase 5 (e.g. a typo'd tool name, a missed `daemon.register` call) is caught by `cargo test`.
**Acceptance Criteria**:
- Marked `#[ignore]` by default with a doc comment explaining why: the real `NativeEmbedder`
  downloads the ~90MB `all-MiniLM-L6-v2` ONNX model from Hugging Face Hub on first use, which
  requires network access and is too slow/flaky for a default `cargo test` run — matching this
  project's precedent of keeping default test runs fast and hermetic (`webcrawl.rs`'s test uses
  an in-process mock HTTP server specifically to avoid any real network dependency; this test
  cannot fully avoid one for the embedder, so it's opted out of the default run instead).
  - *Given* `cargo test -p stapler-mcp` is run without `-- --ignored`, *When* the test suite
    completes, *Then* `docs_index_round_trip` is reported as `ignored`, not run.
  - *Given* `cargo test -p stapler-mcp -- --ignored docs_index_round_trip` is run with network
    access available, *When* the test executes, *Then* it: spawns a mock HTTP site (same pattern
    as `webcrawl.rs`'s test), calls `index_docs` for it, calls `search_docs` against the result
    and gets back at least 1 result, calls `list_indexed_sources` and sees the new source, calls
    `remove_indexed_source` and then confirms a subsequent `search_docs` call errors as
    unknown-source.
**Files**: `crates/cli/tests/docs_index.rs`

##### Task 6.2.1a: Scaffold the test file + mock site (~4 min)
- Create `crates/cli/tests/docs_index.rs`, reusing `webcrawl.rs`'s `TestEnv`/`spawn_mock_site`-
  style helpers (either by duplicating the small amount of needed boilerplate, since
  `crates/cli/tests/` files don't share code between each other by default in this workspace's
  current layout — confirm via `Glob` whether a shared `tests/common/mod.rs` already exists
  before duplicating; if not, duplicating ~30 lines is acceptable and consistent with
  `daemon_ping.rs`/`webcrawl.rs` apparently already each having their own version).
- Files: `crates/cli/tests/docs_index.rs`

##### Task 6.2.1b: `#[ignore]`d round-trip test body (~5 min)
- Write the full round trip described in the acceptance criteria, using `client::call` exactly
  as `webcrawl.rs`'s test does, ending with the daemon `"shutdown"` call.
- Files: `crates/cli/tests/docs_index.rs`

##### Task 6.2.1c: Manual verification note (~2 min)
- Since this test is `#[ignore]`d by default, manually run `cargo test -p stapler-mcp --
  --ignored docs_index_round_trip` once during implementation (accepting the one-time ~90MB
  model download) to confirm it actually passes before considering Phase 6 complete — an
  `#[ignore]`d test that has never been run is not verified.
- Files: none (verification only)

### Epic 6.3: Retrieval-quality spot check (pre-mortem P1 mitigation)
**Goal**: `all-MiniLM-L6-v2` is a general-purpose sentence-similarity model trained on prose/NLI
data, not code/API-reference content — it is entirely possible for `search_docs` to pass every
mechanical test in Phases 3–6 (correct types, correct ranking-by-score, correct error handling)
while still returning poor-quality results for Tyler's actual primary use case (Rust crate docs,
technical/code-heavy content), because no unit or integration test in this plan assesses semantic
*relevance* — only mechanical correctness (does the code run, does it rank by score correctly).
This is a real, plausible failure mode identified in `pre-mortem.md`'s #1 P1 item: the feature
could ship "working" and Tyler could quietly stop using it, with nothing in this plan's test
suite ever catching that. This epic is a deliberately human-judgment step, not an automatable
one — semantic relevance quality is not something a hand-computed unit test can assert.

#### Story 6.3.1: Manual relevance spot check against real Rust documentation
**As a** the project maintainer, **I want** to manually judge search-result relevance against a
real, representative doc source before considering `docs-index` done, **so that** a
mechanically-correct-but-practically-useless embedding choice is caught before Tyler starts
relying on the tool, not discovered weeks later by disuse.
**Acceptance Criteria**:
- After Phase 6's automated tests pass, `index_docs` is run once against a real, representative,
  code-heavy doc source (e.g. `https://doc.rust-lang.org/book/ch16-00-concurrency.html` or
  `https://tokio.rs/tokio/tutorial`), then `search_docs` is run against it for 3-5 realistic
  queries a real usage session would plausibly issue (e.g. "how do I spawn a task", "what's the
  difference between Mutex and RwLock", "async function that returns a Result").
  - *Given* `tokio-tutorial` indexed from `https://tokio.rs/tokio/tutorial`, *When* `search_docs`
    is called with `query: "how do I spawn a background task"`, *Then* the maintainer manually
    confirms the top 1-2 results are genuinely about `tokio::spawn`/task spawning (not merely
    non-empty and correctly *sorted* by score, but actually *relevant* by human judgment) — if
    the top results are off-topic despite high cosine scores, that is a signal `all-MiniLM-L6-v2`
    is a poor fit for this content and the `Embedder` choice should be revisited (e.g. a
    code-aware embedding model, or a hybrid lexical+semantic ranking) before considering v1 done,
    not silently shipped.
**Files**: none (manual verification only — no code artifact; the outcome is a go/no-go judgment
call, documented as a one-line note added to `NOTES.md`'s eventual docs-index phase entry once
implementation completes, recording whether relevance quality was judged acceptable)

##### Task 6.3.1a: Run the spot check and record the verdict (~10 min)
- Perform the manual check described in Story 6.3.1's acceptance criteria. If results are
  judged relevant: proceed, note "relevance spot-checked, acceptable" in the eventual `NOTES.md`
  entry. If results are judged poor: do not ship v1 as-is — file this as a following-phase
  concern (revisit `Embedder`/model choice) rather than silently accepting a search tool that
  doesn't actually help.
- Files: none (manual verification only)

#### Story 6.3.2: `docs-mcp-server` name-collision go/no-go gate
**As a** the project maintainer, **I want** an explicit, checked step confirming the
`docs-mcp-server` tool-name collision (Risk Control's "`docs-mcp-server` coexistence" note) is
resolved, **so that** shipping `search_docs` doesn't silently create two same-named MCP tools in
one Claude Code session — a Product Triad Review pass flagged that a Risk Control *note* alone is
easy to forget at ship time, unlike a task with its own explicit pass/fail check.
**Acceptance Criteria**:
- Before `stapler-mcp`'s `search_docs` tool is exercised for real (i.e. before or as part of Task
  6.3.1a's spot check), `~/.claude.json`'s `"docs"` entry (the `docs-mcp-server` connection) is
  confirmed either removed, or `docs-mcp-server`'s own `search_docs` tool is confirmed renamed/
  disabled — not merely "will do later."
  - *Given* `~/.claude.json` currently has a `"docs"` MCP server entry pointing at
    `docs-mcp-server`'s persistent SSE service, *When* this task is performed, *Then* either that
    entry is removed from `~/.claude.json` (full cutover) or the maintainer has explicitly
    verified no name collision exists for the duration `docs-mcp-server` and `stapler-mcp`'s
    `search_docs` coexist, and records which choice was made in `NOTES.md`.
**Files**: none (manual verification/config-file step only — `~/.claude.json` is outside this
repo)

##### Task 6.3.2a: Confirm or resolve the `search_docs` name collision (~5 min)
- Check `~/.claude.json` for the `"docs"` entry per Risk Control's "`docs-mcp-server`
  coexistence" note. Remove it (preferred, matches requirements.md's "no separate Node process
  needed at all" success criterion) or explicitly document why it's being kept alongside
  `stapler-mcp`'s `search_docs` for now. Record the outcome in `NOTES.md`'s eventual docs-index
  phase entry.
- Files: none (manual verification only)
