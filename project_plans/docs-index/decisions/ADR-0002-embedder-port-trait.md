# ADR-0002: New `Embedder` port trait

**Status**: Accepted
**Date**: 2026-07-14
**Deciders**: Tyler Stapler (solo project)
**Related**: `project_plans/docs-index/research/architecture.md` §2, ADR-0001

## Context

`crates/core/src/ports.rs` gate-keeps every OS/hardware-touching capability this daemon uses
(`HttpClient`, `FileStore`, `BrowserDriver`, `ProcessSpawner`, `EnvPort`, `ClockPort`,
`SleepPort`, socket/lock ports) behind a trait, each with one native adapter
(`crates/native/src/*`) and one wasm adapter (`crates/wasm/src/*`). Running local embedding
inference (loading an ONNX model, doing tensor math, a one-time model-weights download/cache)
is new to this codebase — no such capability exists in `ports.rs` today. The question: does
embedding inference get its own port trait, or does `crates/core/src/tools/docs.rs` call
`fastembed` directly?

## Decision

Add a new port trait to `crates/core/src/ports.rs`:

```rust
pub trait Embedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError>;
}
```

Native adapter: `NativeEmbedder` in `crates/native/src/embed.rs`, wrapping a lazily-initialized
`fastembed::TextEmbedding` (see ADR-0001) cached for the daemon's process lifetime. No wasm
adapter is implemented for v1 (see ADR-0001's "platform scope" decision and `plan.md`'s
"wasm32 buildability" Pattern Decision row) — `tools::docs` (the only caller of `Embedder`) is
compiled out of the `wasm32-unknown-unknown` target entirely, so the absence of a
`WasmEmbedder` is not a missing-trait-impl compile error, it's a module that never asks for one.

## Rationale

- **Consistency with the existing ports-and-adapters architecture.** `ports.rs`'s own header
  comment states the pattern precisely: "add a port only for genuinely OS/hardware-touching
  behavior; pure computation stays as plain functions/structs in `core`." Model inference is
  unambiguously in the OS/hardware-touching category — it loads a file from disk (or downloads
  one), allocates significant memory, and does CPU-bound work — the same class of capability
  every existing port already gate-keeps.
- **Testability.** Every `tools/*.rs` function in this codebase is generic over its port traits
  (`fn read_website<H: HttpClient, F: FileStore>(...)`), which is what lets `crates/cli/tests/`
  exercise real logic against hand-rolled test doubles instead of a real network/filesystem. An
  `Embedder` port lets `docs::index_source`/`docs::search_docs` be unit-tested with a
  `FakeEmbedder` returning deterministic vectors, with zero dependency on `fastembed`'s real
  ONNX model (which takes real wall-clock time to load and, on first run, requires a network
  download) — critical for keeping this feature's own test suite fast and offline-runnable
  (see `plan.md` Phase 6).
- **GoF Adapter/Strategy framing.** `Embedder` is simultaneously a GoF Adapter (it adapts
  `fastembed`'s concrete API to this codebase's own trait shape, matching how `HttpClient`
  adapts `reqwest`) and a Strategy (the embedding algorithm/model is swappable behind the trait
  — relevant if a `candle`-based `WasmEmbedder` is ever added later per ADR-0001's stated
  fallback path). Calling `fastembed::TextEmbedding` directly from `docs.rs` would collapse
  both benefits and hard-wire the concrete crate into business logic.

## Consequences

- **Positive**: `docs.rs`'s tool functions stay pure `core` logic, generic over
  `H: HttpClient, F: FileStore, E: Embedder, C: ClockPort` — same shape as every existing tool.
- **Positive**: swapping `fastembed` for a different embedding crate later (e.g. if `fastembed`
  or `ort` has a breaking/abandoned release) only touches `crates/native/src/embed.rs`, not
  `crates/core/src/tools/docs.rs` or its tests.
- **Negative**: one more trait to keep native-only in mind when reading `ports.rs` — every other
  port trait in this file has (at least a partial) wasm implementation; `Embedder` is the first
  one that doesn't, and that asymmetry should be called out in the trait's own doc comment so a
  future reader doesn't assume a `WasmEmbedder` is merely unwritten rather than intentionally
  out of scope.

## Alternatives Considered

| Alternative | Rejected because |
|---|---|
| Call `fastembed::TextEmbedding` directly from `docs.rs` | Hard-wires a concrete, pre-1.0-adjacent crate into business logic; breaks the ports-and-adapters pattern every other tool follows; makes `docs.rs` untestable without a real ONNX model load |
| Fold embedding into the `HttpClient` or `FileStore` trait (e.g. `FileStore::embed`) | Category error — embedding is CPU-bound inference, not a blob-store or network operation; would violate single-responsibility on an already-minimal trait |
| A `VectorStore` port trait bundling both embedding *and* storage/search | Rejected by `research/architecture.md` §2 already — storage/search is plain computation over `FileStore`-backed JSON (no OS capability involved), only the embedding half needs a port |
