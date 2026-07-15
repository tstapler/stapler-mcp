# ADR-0001: Local embeddings via `fastembed` + brute-force cosine similarity, native-only for v1

**Status**: Accepted
**Date**: 2026-07-14
**Deciders**: Tyler Stapler (solo project)
**Related**: `project_plans/docs-index/research/stack.md`, `research/pitfalls.md`, `research/build-vs-buy.md`

## Context

`docs-index` needs local (no cloud API key, no per-query cost) text embeddings and a way to
rank stored chunks against a query, entirely inside the existing single-threaded, `!Send`,
`current_thread`-runtime `stapler-mcp` daemon (`crates/core/src/daemon.rs`). Three embedding
crates were evaluated (`research/stack.md` §1, `research/build-vs-buy.md` §1): `ort` (raw ONNX
Runtime bindings), `fastembed` (a thin `ort` wrapper), and `candle`/`candle-transformers`. Two
vector-search options were evaluated (`research/stack.md` §2, `research/build-vs-buy.md` §3):
`sqlite-vec` (via `rusqlite`) and brute-force cosine similarity over an in-memory `Vec<f32>`.

## Decision

1. **Embeddings**: use `fastembed` 5.17.2 with the `EmbeddingModel::AllMiniLML6V2`
   (`sentence-transformers/all-MiniLM-L6-v2`, 384-dim, Apache-2.0) model, loaded once per
   daemon lifetime by a new `NativeEmbedder` adapter (`crates/native/src/embed.rs`).
2. **Vector search**: no vector-database dependency. Store `{chunk_text, embedding: Vec<f32>,
   source_url, chunk_index, content_hash, heading}` records as JSON Lines
   (`crates/core/src/tools/docs.rs`), and rank via a hand-written `cosine_similarity(a: &[f32],
   b: &[f32]) -> f32` pure function with brute-force linear scan at query time.
3. **Platform scope**: native-only for v1. No `WasmEmbedder` is implemented; the entire
   `tools::docs` module is compiled out of the `wasm32-unknown-unknown` target (see the
   "wasm32 buildability" row in `plan.md`'s Pattern Decisions table).
4. **Do not depend on `ort` directly** — only transitively through `fastembed`.
5. **Do not adopt `sqlite-vec`** or any ANN index (`instant-distance`, `hora`) for v1.

## Rationale

- **`ort`'s wasm32 support is maintainer-abandoned**, not merely incomplete — the pykeio/ort
  release notes explicitly state continued WASM support is "no longer feasible" for the
  maintainer. Treating this as permanently native-only (rather than a gap to revisit) is
  correct per `research/pitfalls.md` §2.
- **`fastembed` is the least-integration-code option**: one `TextEmbedding::try_new(...).embed(...)`
  call replaces hand-rolled ONNX session + tokenizer + pooling plumbing that `candle` would
  require. Given this is a solo, single-user project, minimizing integration code for a
  native-only v1 outweighs `candle`'s wasm32 upside, which this ADR explicitly declines to
  pursue in v1 anyway.
- **`candle` was the only path with real wasm32 evidence**, but even its own example suite hits
  documented friction (`huggingface/candle` issues #1032, #2736) getting a sentence-transformer
  model running under `wasm32-unknown-unknown` loaded by a **Node** host (this daemon's actual
  wasm target) rather than a browser. Betting v1 on an unproven path is not justified when the
  requirements doc itself explicitly allows a native-only outcome.
  and `EMBEDDING_MODEL_ID` is stored/checked as an invariant instead.
- **Brute-force cosine similarity is correct at this project's scale.** Expected corpus: "a
  handful of doc sources," realistically hundreds to low thousands of 384-dim vectors.
  Brute-force exact kNN is broadly fine up to ~100k vectors before latency becomes noticeable.
  `sqlite-vec` would add this codebase's first-ever database dependency, an explicitly
  *unverified* wasm32 compile path (its FFI bindings target `libsqlite3-sys` directly, not
  confirmed to work through `rusqlite`'s newer `sqlite-wasm-rs` swap), and solves a scale
  problem this project does not have.
- **A hand-rolled 20-line cosine-similarity function is low correctness risk**, unlike hand-rolled
  HTML parsing or an HNSW graph: it is a closed-form dot-product-and-norm computation, fully
  specified by a handful of hand-computed unit tests (orthogonal → 0, identical → 1, a known
  3-vector ranking) — see `plan.md` Phase 3/6.

## Consequences

- **Positive**: zero new storage-engine dependency; zero wasm32 compile risk for the
  vector-search half; least integration code for the embedding half; consistent with every
  other tool in this daemon being a `Transaction Script` over plain data.
- **Negative**: no wasm/Node-hosted semantic search for `docs-index` in v1 — if the wasm/Node
  adapter later needs feature parity, this requires a second `Embedder` implementation (`candle`based, per `research/stack.md`'s fallback recommendation), not a trivial swap.
- **Negative**: `fastembed` and `ort` are both pre-1.0-adjacent in maturity terms (`ort` is
  literally pre-1.0, `2.0.0-rc.12`; `fastembed` wraps it). This is a real, flagged stability
  risk — pin exact versions in `Cargo.lock`, and treat any `fastembed`/`ort` major-version bump
  as requiring a full re-verification pass, not a routine `cargo update`.
- **Negative**: if the embedding model (or `fastembed`'s bundled model weights) ever changes,
  old and new vectors are not comparable. Mitigated by storing `embedding_model:
  EMBEDDING_MODEL_ID` in each source's `meta.json` and refusing/forcing-reindex on mismatch —
  encoded as an explicit invariant in `plan.md` Phase 4 (`search_docs`'s model-mismatch guard).

## Alternatives Considered

| Alternative | Rejected because |
|---|---|
| `candle` + `candle-transformers` (wasm-capable) | More integration code than `fastembed`; unproven Node-hosted-wasm sentence-embedding track record; not needed since v1 is native-only by requirements.md's own accepted fallback framing |
| `ort` directly (skip `fastembed`) | Hand-rolls ONNX session/tokenizer/pooling plumbing `fastembed` already provides; no benefit for a single-user v1 |
| `sqlite-vec` + `rusqlite` | New DB dependency, unverified wasm32 story, solves a scale problem (10k+ vectors) this project doesn't have |
| `instant-distance` / `hora` (pure-Rust ANN) | Both stale/unmaintained (last release 2023 / no releases) — not a safe long-term dependency in a single-maintainer codebase |
| Cloud embedding API (OpenAI/Cohere/Bedrock/Vertex) | Violates the explicit "no external API key or per-query cost" constraint — the exact thing `docs-mcp-server` is being replaced to eliminate |
| Local server (Ollama) | Not a SaaS, but reintroduces the "separate long-running process" shape this whole project exists to collapse into one daemon |
| Lexical fallback (TF-IDF/BM25, no embedding model at all) | `requirements.md`'s Constraints section explicitly named this as a contingency if "a true local-embedding model proves too heavy to bundle/run" — it did not prove too heavy: `fastembed`'s ~90MB ONNX model bundles and runs cleanly in-process on the native daemon (the only target v1 ships to), so the contingency this alternative existed to cover never materialized. Rejected for v1 on that basis, not by default — semantic (embedding-based) ranking is strictly more useful than lexical keyword matching for the natural-language queries this tool is designed for, and nothing about `fastembed`'s native footprint forces a fallback. Revisit only if a future wasm/Node `Embedder` is pursued and a true local-embedding model turns out to be infeasible there specifically (see the wasm/Node "Negative" consequence above) — a lexical fallback would then be scoped to that one adapter, not v1's native path. |
