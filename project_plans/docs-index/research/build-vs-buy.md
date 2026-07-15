# Research: Build vs. Buy — docs-index

**Dimension**: Build-vs-buy / sourcing strategy
**Date**: 2026-07-14
**Question**: Should local semantic doc search be built from primitives in Rust, or sourced from/wrap an existing solution?

This is a companion to `research/stack.md` (which already picks specific crate *versions* for
embeddings/chunking/vector-search) and `research/features.md` (feature landscape, comparable
tools). This document answers a different question: for each layer of the system, is there an
existing near-complete solution worth adopting instead of assembling primitives, and — a step up
from that — is building this project at all the right call given the operational pain is already
fixed elsewhere.

## 1. Existing OSS library or framework

### Near-complete "fetch + chunk + embed + vector search" solutions

**`EmbedAnything` (StarlightSearch/EmbedAnything)** is the closest thing to an all-in-one match:
ingests PDFs/TXT/MD/images/audio/websites, does semantic/late chunking, embeds via Candle/ONNX/
cloud backends, and streams to vector-DB adapters (Qdrant, Weaviate, Pinecone, Milvus, Chroma,
Elastic). Apache-2.0, 1.3k stars, 912 commits, latest release v0.7.1 (2026-07-10) — genuinely
active and well-maintained.

- **Pros**: covers the whole pipeline in one dependency; active maintenance; permissive license;
  usable as a pure Rust crate (92.1% Rust), not just via its PyO3 bindings.
- **Cons**: built around *streaming to an external vector database* — every listed backend
  (Qdrant, Weaviate, Pinecone, Milvus, Chroma, Elastic) is a networked service, which is a poor
  fit for "single user, local-only, no separate process" (the exact problem this project exists
  to get *away* from — replacing one heavy external process with another external process is a
  wash, not a win). No embedded/flat-file vector store is mentioned. Pulls in a much wider
  dependency surface (Candle *and* ONNX *and* multiple vector-DB client SDKs) than this project
  needs when the target corpus is a handful of doc sources. Would still need custom glue for the
  ports-and-adapters `HttpClient`/`FileStore` architecture this workspace already commits to —
  EmbedAnything has its own fetch/IO model, not one that plugs into `ports.rs`.
- **Verdict**: **Not recommended.** Solves a bigger problem (multi-backend production RAG
  ingestion) than this project has, and its "vector search" half assumes infrastructure this
  project explicitly doesn't want. Not worth the dependency weight or the impedance mismatch with
  the existing port traits.

**`rag-toolchain` (JackMatthewRimmer/rust-rag-toolchain)** — actively released, but embeddings and
generation are wired to OpenAI/Anthropic-style cloud APIs by design. Violates the "no external API
key or per-query cost" constraint outright.

- **Verdict**: **Not recommended** — wrong shape (cloud-embedding-first) for a local-only
  requirement.

**`wg-ragsmith`** — chunking + vector storage utilities aimed at RAG. Still v0.1.x, explicitly
documented as unstable ("APIs are unstable and will change between minor versions, breaking
changes may arrive without fanfare").

- **Verdict**: **Not recommended** for a dependency this project would need to live with
  long-term in a single-maintainer codebase — too early to trust for anything beyond
  cherry-picking implementation ideas.

### Single-piece crates (embedding-only / vector-search-only)

Already evaluated in depth in `research/stack.md` §1–2; summarized here for the build-vs-buy
framing:

| Crate | Piece | Maturity (as of 2026-07) | License | Verdict |
|---|---|---|---|---|
| `fastembed` (Anush008/fastembed-rs) | Embedding | v5.17.2 (2026-06-15), 962 stars, 90 releases, active | Apache-2.0 | **Recommended** — thin, purpose-built ONNX wrapper; least glue code of any embedding option |
| `candle`/`candle-transformers` (huggingface) | Embedding | v0.11.0 (2026-06-26), HF-maintained, active | MIT/Apache-2.0 | **Viable** — only path with real wasm32 support; more integration code than `fastembed` |
| `ort` (pykeio/ort) | ONNX runtime binding underlying `fastembed` | 2.0.0-rc.12 (pre-1.0), wasm32 support explicitly abandoned by maintainer | MIT/Apache-2.0 | **Viable as a transitive dep only** — don't depend on directly |
| `sqlite-vec` | Vector search | v0.1.9 (2026-05-18), Mozilla Builders + Fly.io/Turso/SQLite Cloud sponsorship, active | MIT/Apache-2.0 | **Viable, not needed** — mature, but overkill at this corpus scale (see §3) |
| `hora` (hora-search/hora) | Vector search (ANN) | No published releases, original crate effectively stale; community forks of unclear status | Apache-2.0 | **Not recommended** — unmaintained, no version to pin |
| `instant-distance` | Vector search (HNSW, pure Rust) | Last published 0.6.1 in 2023, 347 stars, only 2 open issues (low-traffic, not necessarily healthy) | MIT/Apache-2.0 | **Not recommended** — stale, and solves a scale problem this project doesn't have |
| `usearch` (unum-cloud) | Vector search | Actively released (2.25.2), wraps native C++ core via FFI, SIMD-accelerated, multi-metric | Apache-2.0 | **Viable, not needed** — well-maintained but adds a C++ FFI boundary for no benefit at this scale |

None of the single-piece crates need to be ruled out on maturity grounds except `hora` and
`instant-distance` (both stale). The real question for the vector-search half isn't "which crate
is best" — it's "is a crate needed at all" (§3).

## 2. SaaS / managed API

Requirements.md rules this out explicitly ("no external API key or per-query cost"), and the
research here mostly confirms that's correct rather than surfacing a reason to reconsider:

- Every mainstream embedding API (OpenAI, Cohere, Voyage AI, Google Vertex, AWS Bedrock —
  the exact set `docs-mcp-server`'s LangChain layer already wires up) requires an account, an API
  key, and has per-token cost. This is precisely the dependency this project exists to eliminate;
  adopting one would just relocate the problem docs-mcp-server already has, not solve it.
- "Free tier" options that superficially look like they'd satisfy "no cost" (HF Inference API
  free tier, Cloudflare Workers AI free allotment) still require account signup/API keys and are
  rate-limited, revocable, and networked — they fail "no external API dependency," not just "no
  cost." A local model with no network call at inference time is a categorically different
  guarantee than a free-tier network call, and requirements.md's framing ("local-only embeddings,
  no external API dependency") reads as intentionally ruling out both, not just the paid ones.
- The one edge case worth naming: a *local* server like Ollama technically isn't a "SaaS," but it
  reintroduces exactly the "separate long-running process" shape this whole project exists to
  collapse into the single `stapler-mcp` daemon (the `rust-local-rag` project surveyed in §4 uses
  this pattern). It's local-first but not local-*in-process*, so it doesn't satisfy the
  architectural goal even though it satisfies the "no cloud cost" one.
- **Verdict**: **Not recommended**, full stop. Nothing found changes the calculus requirements.md
  already landed on. In-process local inference (`fastembed`/`candle`, both already evaluated in
  §1 and `stack.md`) is the only option consistent with both the cost constraint *and* the
  single-daemon architectural constraint.

## 3. LLM-generated implementation vs. battle-tested library (vector similarity specifically)

Given the expected corpus ("a handful of doc sources," tens to low hundreds of pages, low
thousands of 384-dim vectors at most — per `stack.md`/`features.md`'s scale framing):

**Brute-force cosine similarity over an in-memory `Vec<f32>`**:
- **Pros**: ~20 lines of code, zero new dependencies, zero wasm-compatibility risk (pure Rust
  arithmetic has no platform-specific FFI/threading concerns the way `sqlite-vec`/`usearch` do —
  see `stack.md`'s unresolved wasm32 question for `sqlite-vec`), trivially auditable/testable
  (a handful of unit tests against known vectors fully specify correctness — cosine similarity is
  a closed-form, well-understood formula with no tuning parameters or index-construction edge
  cases to get subtly wrong), and it slots directly into the existing `FileStore`-JSON cache
  pattern `webcrawl.rs` already established rather than introducing a new storage engine.
  Performance at this scale (low thousands of vectors × single query) is sub-millisecond to
  low-single-digit-millisecond — nowhere near the regime where an ANN index's approximate-recall
  tradeoff pays for itself.
- **Cons**: doesn't scale — becomes the wrong answer somewhere in the tens-of-thousands-of-vectors
  range (well past this project's stated scope), and a hand-rolled implementation *could* still
  contain a bug (e.g., forgetting to normalize, an off-by-one in top-K selection) even at 20 lines.
- **Correctness risk**: low, and cheaply mitigated — cosine similarity is simple enough that a
  handful of unit tests with hand-computed expected values (e.g., orthogonal vectors → 0,
  identical vectors → 1, a known 3-vector ranking) close essentially all the risk. This is a much
  lower-risk "20 lines" than, say, hand-rolling HTML parsing or an HNSW graph — it's a dot product
  and a norm, not a data structure with invariants to maintain.
- **Maintenance burden**: near-zero. No upstream crate to track for breaking changes, security
  advisories, or wasm32-support regressions (`sqlite-vec`'s wasm32 story is explicitly unverified
  per `stack.md`; brute-force Rust arithmetic has no such open question at all).

**Adopting `sqlite-vec` or `hora`**:
- **Pros**: `sqlite-vec` specifically is mature, actively maintained, well-sponsored, and would
  give real headroom if corpus size grows unexpectedly; also gives free persistence/querying via
  SQL if the storage layer ever needs more than flat JSON.
- **Cons**: `hora` is disqualified on maintenance grounds alone (§1). `sqlite-vec` adds a new
  storage engine (`rusqlite` + a native SQLite extension) to a codebase that currently has *no*
  database dependency anywhere, adds an unverified wasm32 compile question (`stack.md` flags this
  directly — `sqlite-vec`'s FFI bindings target `libsqlite3-sys`, not confirmed to work through
  `rusqlite`'s newer `sqlite-wasm-rs` wasm32 path), and solves a scale problem ("tens of thousands
  to millions of vectors, concurrent access, complex filtering") this project doesn't have and
  isn't likely to have given it's explicitly single-user with a handful of sources.
- **Maintenance burden**: nonzero even for a "just works" dependency — new major version to track,
  a native-extension-loading step (`sqlite3_auto_extension`) to keep working across SQLite
  versions, and a whole new failure mode (extension load failure) that brute-force cosine
  similarity simply cannot have.

- **Verdict**: **Brute-force in-memory scan is recommended for v1.** This is the rare case where
  the "just write it" instinct is actually correct rather than a false-economy shortcut — the
  problem is small, closed-form, and well-understood enough that the "battle-tested library"
  premium buys protection against failure modes (index corruption, concurrent-access bugs,
  approximate-recall tuning) that don't exist in a linear scan. Revisit only if corpus size grows
  by an order of magnitude or more, at which point `sqlite-vec` (not `hora`/`instant-distance`) is
  the fallback already vetted and ready to swap in.

## 4. Fork or adapt

### Close-enough existing projects

**`rust-local-rag` (ksaritek/rust-local-rag)** — a local RAG MCP server in Rust, structurally the
closest analog found: uses the official `rmcp` SDK (same MCP crate this workspace already
depends on per `stack.md`'s existing-deps table), local embeddings via Ollama
(`nomic-embed-text`), Poppler-based PDF text extraction, and a custom in-memory vector store.

- **Pros**: proves the shape (`rmcp` + local embeddings + custom in-memory store) is viable and
  matches this project's target architecture closely; MIT-licensed, permissive; swapping its
  PDF/Poppler ingestion for this project's existing `webcrawl.rs` HTML/Markdown pipeline is
  plausible in principle.
- **Cons**: 33 stars, 5 forks, effectively one commit of visible history, one contributor, no
  releases — reads as a proof-of-concept/demo project, not production code to build on.
  Embeddings come from Ollama (a separate local server process), which is the exact
  "separate-process" shape this project's constraints are trying to eliminate — adopting it as-is
  would trade `docs-mcp-server`'s Node process for an Ollama process, not net a win. It would need
  its embedding layer replaced (Ollama → in-process `fastembed`/`candle`), its ingestion layer
  replaced (Poppler/PDF → the existing `Crawler`/`dom_smoothie`/`htmd` pipeline), and its port
  architecture doesn't exist — there's nothing here that maps onto `ports.rs`'s `HttpClient`/
  `FileStore` traits, so "forking" it means keeping essentially none of its code, just its rough
  shape as a reference.
- **Verdict**: **Not recommended as a fork target** — worth a skim for implementation ideas (how
  it structures its `rmcp` tool handlers), but at this size/maturity there's no meaningful amount
  of working, reusable code to inherit. Building fresh inside the existing `stapler-mcp-core`
  module (as `webcrawl.rs` already models) is less work than adapting this, not more, because so
  much of this project's hard parts (fetch/crawl/robots.txt/caching) are *already done* in this
  repo and `rust-local-rag` would need to be gutted to reuse them.

No other candidate surfaced across this research pass (`EmbedAnything`, `rag-toolchain`,
`wg-ragsmith` — all covered in §1) is a plausible fork target either: each is either shaped for a
different problem (networked vector DBs, cloud embeddings) or too immature to trust (v0.1.x).
There is no "local RAG over web docs" Rust project close enough to this project's specific
combination (in-process embeddings + ports-and-adapters architecture + reuse of an existing
crawler) to fork rather than build on the primitives already evaluated in §1 and `stack.md`.

### Is building this at all still the right call?

Requirements.md frames this explicitly as still open ("whether to also replace the tool's
functionality"), and it's worth surfacing directly rather than assuming the answer is yes because
research was commissioned:

- The single concrete operational pain point that originally motivated replacing
  `docs-mcp-server` — per-session process spawn instead of one shared instance — is **already
  fixed**, separately, via a persistent systemd service + SSE transport (stated directly in
  requirements.md's Context section: "that fix stands regardless of this project"). That means
  the remaining case for `docs-index` is not "fix broken/annoying behavior" but purely
  "architectural consistency" (last non-Rust tool in an otherwise fully-consolidated toolset) and
  "reduced footprint" (drop the Node/LangChain/tree-sitter/`better-sqlite3` stack entirely).
- Those are legitimate reasons — consistent with the pattern of every other tool already migrated
  into `stapler-mcp` per `README.md`/`NOTES.md` — but they're maintenance/aesthetic/footprint
  reasons, not urgency-driven ones. There's no user-facing capability gap today: `docs-mcp-server`
  still works, semantic search still functions, nothing is broken.
- Against that: this project has unusually strong "build" economics *if* it proceeds, precisely
  because so much of the hard, error-prone part (fetch, crawl, `robots.txt`, HTML extraction,
  caching) is already built, tested, and proven in `webcrawl.rs`, and §1–3 above establish that
  the *remaining* new pieces (embedding via `fastembed`, chunking via `text-splitter`, similarity
  via a ~20-line brute-force scan) are each either a thin, well-maintained single-purpose crate or
  intentionally-trivial hand-rolled code — not a large from-scratch undertaking. The "full product,
  not a trivial wrapper" caution in `NOTES.md`'s original deferral has been substantially narrowed
  by this research: the 90+-format-parser and multi-provider-embedding surface area that made
  `docs-mcp-server` "a full product" is explicitly out of scope here (HTML/Markdown only, one
  local embedding model), which is most of what made it large in the first place.
- **Verdict on the meta-question**: **Viable, not urgent.** "Don't build this, the pain is already
  resolved" is a legitimate and defensible verdict — there is no functional or operational
  emergency forcing this. But it is not the strongest verdict available: the scoped-down v1 (no
  UI, no multi-format, no cloud embeddings, reusing `webcrawl.rs`) is small enough, and sits on
  enough already-proven code, that the effort-to-value ratio is now favorable in a way it wasn't
  when `NOTES.md` originally deferred it as "a full product." This reads as a reasonable
  discretionary project to schedule (matching requirements.md's "no hard deadline... follows the
  phased build log" framing) rather than either an urgent build or a "don't bother."

## Summary

| # | Option | Verdict |
|---|---|---|
| 1 | Adopt an existing all-in-one Rust RAG framework (`EmbedAnything`, `rag-toolchain`, `wg-ragsmith`) | Not recommended — wrong shape (networked vector DBs / cloud embeddings) or too immature |
| 1 | Adopt single-purpose crates for embedding (`fastembed`) and skip vector search entirely | Recommended (embedding) / see #3 (vector search) |
| 2 | SaaS/managed embedding API, including local-server options like Ollama | Not recommended — violates cost/dependency and in-process-daemon constraints respectively |
| 3 | Brute-force cosine similarity over `Vec<f32>` vs. `sqlite-vec`/`hora`/`instant-distance` | Brute-force recommended for v1; `sqlite-vec` is the vetted fallback if scale grows; `hora`/`instant-distance` not recommended (stale) |
| 4 | Fork an existing "local RAG over web docs" project (`rust-local-rag`) | Not recommended — too immature to meaningfully inherit code from; build on this repo's existing `webcrawl.rs` instead |
| 4 (meta) | Build `docs-index` at all, given the operational pain is already fixed | Viable, not urgent — legitimate to defer indefinitely, but the scoped-down v1 has favorable build economics if/when picked up |
