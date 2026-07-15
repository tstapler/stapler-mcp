# Research: Stack — docs-index

**Date**: 2026-07-14
**Question**: Which libraries/frameworks/versions/patterns should `docs-index` use? What's new vs. already a workspace dependency?

## Existing workspace state (read before researching externally)

Root `Cargo.toml` is a 4-member workspace: `crates/core` (pure logic, no OS calls — behind `ports.rs` traits), `crates/native` (tokio/reqwest/chromiumoxide adapter), `crates/wasm` (`wasm-bindgen`/`cdylib`, targets `wasm32-unknown-unknown`, delegates to Node), `crates/cli` (rmcp server binary).

Relevant deps already present:

| Crate | Where | Version | Relevance to docs-index |
|---|---|---|---|
| `dom_smoothie` | core | 0.18 | Readability extraction — already used by `webcrawl.rs::read_website` to pull article content out of raw HTML |
| `dom_query` | core | 0.28 | DOM querying, used alongside `dom_smoothie` |
| `htmd` | core | 0.5 | HTML→Markdown conversion — `webcrawl.rs` line ~56 calls `htmd::convert(article.content)` to produce the markdown that's returned/cached today. **This is the natural input to a chunker** — no new HTML-parsing dependency needed for ingestion. |
| `texting_robots` | core | 0.2 | `robots.txt` parsing, reused as-is |
| `sha2` | core | 0.11 | Already used for cache keys; reusable for content-hash-based staleness detection |
| `serde`/`serde_json`/`schemars` | core | 1 / 1 / 1.0 | Tool I/O schema, reusable |
| `reqwest` | native | 0.13 (rustls, no default features) | `HttpClient` adapter |
| `tokio` | native/cli | 1 | async runtime, native-only |
| `rmcp` | cli | 2 | MCP server framework |

`ports.rs` defines `HttpClient`, `FileStore` (`read_file`/`write_file`, byte-oriented, `Ok(None)` = cache miss), `BrowserDriver`, and process/env/clock/lock ports — each with a native and (partial) wasm implementation. No embedding, vector-search, or tokenization crate is present anywhere in the workspace yet — this is a fully new dependency category.

`webcrawl.rs` (`read_website`, `download_website`, ~286 lines) is generic over `H: HttpClient` and `F` (the fetch closure), and already does fetch → `Readability` extract → `htmd` markdown conversion → `FileStore`-backed caching. This is the pipeline `docs-index` ingestion should sit downstream of, not duplicate.

## 1. Local-embedding crates

### `fastembed` (Anush008/fastembed-rs) — recommend for native-only v1

- **crates.io**: `fastembed` 5.17.2 (updated 2026-06-15), MIT-licensed, actively maintained.
- Thin, purpose-built wrapper: bundles ONNX Runtime inference (via `ort`) + tokenization + pooling behind one `TextEmbedding::try_new(...).embed(texts, batch_size)` call. This is by far the least integration code of any option.
- Default model is `BAAI/bge-small-en-v1.5`, but `sentence-transformers/all-MiniLM-L6-v2` is a first-class supported enum variant (`EmbeddingModel::AllMiniLML6V2`) — small (~80–90 MB ONNX fp32, smaller quantized), Apache-2.0 licensed, 384-dim, the de facto default "good enough, cheap" sentence embedding model. Models auto-download from HF Hub on first use and cache locally — cache path is configurable, so it can be pointed at `~/.stapler-mcp/`.
- **wasm32-unknown-unknown: does not compile.** Confirmed via multiple sources (Anush008/fastembed-rs, GitHub issues on downstream wrappers like `rig-fastembed`) — it depends on `ort`'s native ONNX Runtime linkage and threading, which has no path to `wasm32-unknown-unknown`. Native-only.

### `ort` (pykeio/ort) — the ONNX Runtime binding fastembed sits on

- **crates.io**: latest is `2.0.0-rc.12` (pre-1.0, updated 2026-03-05) — "2.0" has been in RC for a long time; the 1.x line (`ort` 1.14 the last search hit for) is the stable-if-older alternative. Description: "A safe Rust wrapper for ONNX Runtime 1.24."
- **wasm32: explicitly abandoned by the maintainer.** Direct quote from the pykeio/ort GitHub releases page: *"Getting wasm32-unknown-unknown working in the first place was basically a miracle... it's no longer feasible for [the maintainer] to work on WASM support for ort."* Treat `ort`-based approaches (including `fastembed`) as permanently native-only, not a temporary gap.

### `candle` (huggingface/candle) — recommend if wasm parity for embeddings is ever wanted

- **crates.io**: `candle-core` / `candle-nn` / `candle-transformers` all at **0.11.0** (updated 2026-06-26), dual MIT/Apache-2.0.
- **wasm32-unknown-unknown: works today, not aspirational.** HuggingFace ships and maintains `candle-wasm-examples/bert` in the main repo specifically for browser-based sentence embeddings (mean-pooling BERT/MiniLM-family models), with a live public demo ("Candle BERT Semantic Similarity Wasm" on HF Spaces). This is the one embedding path with real, current wasm32 evidence — not a forum claim.
- Tradeoff vs. `fastembed`: no ONNX-runtime convenience layer — you load `safetensors`/GGUF weights + a tokenizer JSON directly and write the forward pass call yourself (candle-transformers ships a `bert` model module, so this is "wire it up," not "write BERT from scratch"). More integration code than `fastembed`, but it is the only embedding option that can plausibly satisfy the wasm/Node adapter in the same code path as native, since model bytes can be fetched via the existing `HttpClient` port and cached via `FileStore` — no filesystem/thread assumptions baked in the way `ort` has.
- A community "sentence-transformers on Candle" port exists that wraps `candle_core` with a `SentenceTransformerBuilder` API supporting `all-MiniLM-L6-v2`/`L12-v2`, `bge-small-en-v1.5`, multilingual E5, etc. — worth evaluating in the planning phase as a way to avoid hand-writing pooling/normalization, but it's a smaller, less-established crate than `candle` itself; pin exact name/version during planning rather than here.

### Recommendation

- **v1 (native-only), lowest integration cost**: `fastembed` 5.17.2 + `all-MiniLM-L6-v2`. Matches the requirements doc's stated fallback framing ("embeddings native-only... whole tool ships native-only for now") directly — this is the cheapest way to get there.
- **If wasm/Node parity is later required**: swap to `candle` + `candle-transformers` 0.11, using the HF `candle-wasm-examples/bert` pattern as the reference implementation for both native and wasm adapters (one model-forward-pass implementation, two `HttpClient`/`FileStore`-backed model-loading adapters). This is strictly more work than `fastembed` but is the only path that doesn't hit a hard wasm wall.
- Either way: **do not evaluate `ort` directly** — its wasm support is dead per the maintainer, and `fastembed` already gets you its native benefits without hand-rolling the ONNX session/tokenizer plumbing.

## 2. Local vector search / similarity

Given corpus scale ("a few Rust crate docs, some web documentation," "tens to low hundreds of pages" per the requirements doc), an ANN index is very likely unnecessary overhead.

- **`sqlite-vec`** (crates.io 0.1.9, updated 2026-05-18) — FFI bindings to the `sqlite-vec` SQLite extension, depends on `rusqlite ^0.31`. Registers via `sqlite3_auto_extension`. Mature-enough, widely used pattern for local vector search at small-to-medium scale.
  - `rusqlite` itself is at **0.40.1** (2026-06-06). Historically `rusqlite`/`libsqlite3-sys` had **no** `wasm32-unknown-unknown` support (needs a libc SQLite doesn't have on the raw wasm target) — but this has changed recently: rusqlite added an `ffi-sqlite-wasm-rs` feature (default-on) that swaps `libsqlite3-sys` for the `sqlite-wasm-rs` crate specifically on `wasm32-unknown-unknown`, giving rusqlite itself wasm32 support as of recent releases. **However**: `sqlite-vec`'s own FFI bindings still target `libsqlite3-sys` directly per its docs — it is not confirmed to work through the `sqlite-wasm-rs` swap path. Treat `sqlite-vec` under wasm32 as unverified; would need a spike before relying on it there.
- **Pure-Rust ANN indexes** (`instant-distance`, `hora`): both are effectively **stale/unmaintained** — `instant-distance` (HNSW, pure Rust, MIT) last published 0.6.1 in **2023**; `hora` (the original crate) is dead, superseded by community forks like `hora-new` of unclear maintenance status. Neither is a good bet for a project that has to live long-term in this codebase.
- **Brute-force cosine similarity over the existing `FileStore` JSON cache** — recommended. At "tens to low hundreds of pages" × a handful of chunks each (low thousands of 384-dim vectors at most), a linear scan computing cosine similarity is sub-millisecond-to-low-millisecond territory; no index data structure, no new storage engine, no wasm-compatibility question at all. This is also the only option that requires **zero new dependencies** — vectors are just `Vec<f32>` serialized into the same JSON-over-`FileStore` pattern `webcrawl.rs` already established for its cache. Reassess only if corpus size grows by an order of magnitude or more.

### Recommendation

Skip both `sqlite-vec` and the pure-Rust ANN crates for v1. Store chunk text + embedding vector + source metadata as JSON via the existing `FileStore` port (one file per indexed source, or one file per chunk — decide in the architecture/planning phase), and do brute-force cosine similarity in `stapler-mcp-core` at query time. This is the simplest option, has no wasm risk, and matches the project's own stated preference ("reusing the existing `FileStore`-backed JSON cache with brute-force cosine similarity for the expected small corpus size" — already called out as viable in the requirements doc's own research-dimensions section).

## 3. Chunking / tokenization

- **`text-splitter`** (crates.io 0.32.0, updated 2026-06-16) — recommend. Has a purpose-built `MarkdownSplitter` (CommonMark + GFM-aware, splits at semantic boundaries: headings, paragraphs, etc.) that takes markdown text directly — which is exactly the format `webcrawl.rs`'s `htmd::convert` already produces. Supports both character-count and token-count-based chunk sizing. No confirmed wasm32 report found in this pass (worth a quick build check in planning), but it's pure Rust text processing with no native-syscall dependencies, so wasm32 risk is low — same risk class as `htmd`/`dom_query`, which the wasm crate already presumably needs to handle if it shares the same core.
- **Tokenization for chunk sizing**: `text-splitter` supports pluggable tokenizers for token-based chunk length (as opposed to raw character count). If token-accurate chunk sizing is wanted for the embedding model's context window:
  - `tokenizers` (HF, crates.io 0.23.1, 2026-04-27) — the correct choice if pairing with a HF sentence-transformer model (matches whatever tokenizer ships with `all-MiniLM-L6-v2`/BERT-family models used by `fastembed` or `candle`). Has WASM bindings published as a separate `tokenizers-wasm` npm package (built via `wasm-pack`) — the base Rust crate is wasm-buildable, just needs the same `wasm-pack` treatment as the rest of `crates/wasm` presumably already applies.
  - `tiktoken-rs` (crates.io 0.12.0, 2026-06-02) — **wrong tool here**: it's OpenAI/tiktoken-specific (GPT-4/5 family encodings). Not relevant since embeddings come from a local sentence-transformer model, not an OpenAI-format tokenizer. Skip.
- Character-count-based chunking (skip a tokenizer dependency entirely for v1) is a reasonable simplification given `all-MiniLM-L6-v2`'s 256-token context window and short doc pages — a conservative character budget (e.g. ~800-1000 chars ≈ safely under 256 tokens for English prose) avoids pulling in `tokenizers` at all. Revisit if truncation/quality issues show up in practice.

### Recommendation

`text-splitter` 0.32 with `MarkdownSplitter`, character-length mode for v1 (skip `tokenizers` dependency initially — add it later only if character-based budgeting proves too imprecise once the embedding model is chosen).

## 4. Summary table — new dependencies vs. existing

| Purpose | Crate | Version | New or existing | wasm32-unknown-unknown |
|---|---|---|---|---|
| HTML extraction | `dom_smoothie` | 0.18 | existing (core) | used today, presumably OK |
| HTML→Markdown | `htmd` | 0.5 | existing (core) | used today, presumably OK |
| Chunking | `text-splitter` | 0.32.0 | **new** | pure Rust, likely OK — verify in planning |
| Embeddings (native) | `fastembed` | 5.17.2 | **new**, native-only | **no** — confirmed unsupported |
| Embeddings (wasm-capable alt.) | `candle-core`/`candle-nn`/`candle-transformers` | 0.11.0 | **new**, only if wasm parity pursued | **yes** — HF-maintained wasm BERT example |
| Vector storage/search | none — JSON via `FileStore` + brute-force cosine in `stapler-mcp-core` | n/a | **no new dependency** | trivially yes (pure Rust) |
| (rejected) ONNX runtime | `ort` | 2.0.0-rc.12 | avoid direct dep | **no** — maintainer abandoned wasm support |
| (rejected) SQLite vector ext | `sqlite-vec` (+`rusqlite` 0.40.1) | 0.1.9 | avoid for v1 | unverified/unlikely without a spike |
| (rejected) pure-Rust ANN | `instant-distance` / `hora` | 0.6.1 (2023) / stale | avoid | unmaintained, unnecessary at this corpus scale |
| (rejected) tokenizer | `tiktoken-rs` | 0.12.0 | n/a | wrong tokenizer family (OpenAI-specific) |

## Open questions for the planning phase

- Whether to spike `candle` now (to keep wasm/Node parity alive) or ship native-only `fastembed` first and treat wasm embedding support as a follow-up phase — the requirements doc explicitly allows either as an outcome.
- Exact model-caching path under `~/.stapler-mcp/` for the ~80 MB ONNX (or safetensors) model file, and whether it's fetched via the existing `HttpClient` port (consistent with the ports architecture) or via `fastembed`'s own HF Hub downloader (simpler, but bypasses the port abstraction that everything else in this daemon goes through).
- Whether `text-splitter`'s wasm32 buildability needs an explicit CI/build check before committing to it, given no direct evidence was found either way (as opposed to `candle`, where wasm support has a maintained, demoed reference implementation).
