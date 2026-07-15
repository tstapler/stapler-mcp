# Architecture Review: docs-index
**Date**: 2026-07-14
**Verdict**: CONCERNS (0 blockers, 6 concerns, 4 nitpicks)

No `docs/adr/ADR-000-architecture-constitution.md` exists in this repository (confirmed via
filesystem check before this review). Constitution-check section skipped per instructions.

---

## Blockers

None. No element of the plan is structurally broken enough to block starting Phase 1 —
`fastembed`/brute-force-cosine/native-only-v1 is a sound, well-justified stack choice
(ADR-0001, build-vs-buy.md), the `Embedder` port trait correctly follows the existing
ports-and-adapters convention (ADR-0002), and the wire-vs-storage type split (`schema.rs`
types vs. `docs.rs`-local `ChunkRecord`/`SourceMeta`) is exactly right. The items below are
real but each has a cheap, scoped fix that fits inside the existing task breakdown.

## Concerns

- [ ] **Unresolved Questions §1 / Story 4.1.3 / Story 4.3.2 (`sources.json` write ordering)** —
  the plan writes `chunks.jsonl` → `meta.json` → `sources.json` (manifest) last, and
  `list_indexed_sources`/the unknown-source error path (Task 4.2.1a) read only
  `sources.json`, never `meta_path` directly. If the daemon crashes between the `meta.json`
  and `sources.json` writes, a fully-indexed source becomes **silently invisible** —
  `search_docs`/`list_indexed_sources` report it as never-indexed even though its data exists
  on disk — rather than surfacing a loud, actionable error. The plan calls this "accepted
  risk," but the specific failure mode (silent invisibility of real data) is worse than the
  alternative (loud failure). **Remediation**: either (a) have `search_docs`'s
  unknown-source path fall back to checking `meta_path(id)` directly when a name isn't found
  in the manifest, self-healing/rewriting the manifest entry on divergence, or (b) reorder to
  write the manifest entry first (optimistically), so a crash mid-write instead produces a
  loud "indexed but chunks missing" error on the next `search_docs` call. Prefer (a) — it
  doesn't require renegotiating the existing write-order reasoning documented elsewhere in the
  plan.

- [ ] **Task 3.3.1a (`cosine_similarity`)** — the spec (Story 3.3.1) only tests
  equal-length vectors (identical, orthogonal, a 3-vector ranking). Rust's `zip` over
  mismatched-length slices silently truncates to the shorter length rather than erroring, so a
  future embedding-dimension change that isn't caught by the `embedding_model`
  string-equality guard (e.g. a stored `ChunkRecord.embedding` from a corrupted write, or a
  different-dimension model swapped in without bumping `EMBEDDING_MODEL_ID`) would produce a
  plausible-but-wrong score instead of a loud failure. **Remediation**: add an explicit
  length-mismatch guard to `cosine_similarity` (`debug_assert_eq!(a.len(), b.len())` at
  minimum, or return an explicit sentinel/`Result`), plus a fourth unit test for
  mismatched-length inputs alongside Story 3.3.1's three specified cases.

- [ ] **Task 3.4.1a (`SourceId::from_name`)** — the slugify algorithm ("lowercase, collapse
  non-alphanumeric runs to `-`, trim leading/trailing `-`") can produce an **empty string**
  for pathological input (`""`, `"!!!"`, an all-punctuation `source_name`), yielding a
  structurally valid `SourceId("")` that is semantically nonsensical — it collapses to a
  degenerate path like `{docs_index_dir}//meta.json` and silently collides with any other
  `source_name` that also slugifies to empty. This is exactly the "illegal state
  representable" pattern type-driven design flags: the type doesn't prove the invariant it's
  meant to prove (a valid, non-empty directory key). **Remediation**: make `SourceId::from_name`
  return `Result<SourceId, String>`, rejecting empty output at the parse boundary — Task
  4.1.1a's caller already handles a `Result`-returning parse step (`Url::parse`), so this is a
  consistent, mechanical addition, not a new pattern.

- [ ] **Domain Glossary / Task 4.2.1b (`embedding_model: String`, `EMBEDDING_MODEL_ID`)** —
  this field is the one invariant the plan itself calls out as critical (ADR-0001's
  "Negative" consequence: mismatched embeddings must never be silently ranked together), yet
  it's enforced only via ad hoc `String == &str` comparison inside `search_docs`, i.e.
  **runtime-check level, not type level** — directly the question Lens 2 asks to scrutinize.
  `docs.rs` will carry several other raw-`String` fields (`source_name`, `source_url`,
  `content_hash`, `heading`) flowing through the same functions, so a bare `String` for the
  one field that gates a correctness-critical guard is a real primitive-obsession risk, not
  just a style nit. **Remediation**: introduce a small `EmbeddingModelId` newtype (wrapping
  `EMBEDDING_MODEL_ID` and `SourceMeta.embedding_model`) with its own `PartialEq`/`matches()`
  — a mechanical, near-zero-cost change that at least makes an accidental comparison against
  an unrelated `String` a compile-time type mismatch rather than a silently-permitted `==`.

- [ ] **Module size — all of Phase 3/4 targeting `crates/core/src/tools/docs.rs`** — Step
  0.5 of the plan itself rejects "Approach A" partly *because* `webcrawl.rs` is "already
  286 lines serving 2 tools." As planned, `docs.rs` will hold 4 internal storage types
  (`Chunk`, `ChunkRecord`, `SourceMeta`, `SourceSummary`), path helpers, `SourceId`/slugify,
  `chunk_markdown`, `cosine_similarity`, all four tool functions (`index_source`,
  `search_docs`, `list_indexed_sources`, `remove_indexed_source`), and their unit tests —
  substantially more surface area than `webcrawl.rs`'s 2 tools, likely well past the file
  size the plan's own reasoning treats as a red flag. **Remediation**: split into a
  `tools/docs/` submodule directory (e.g. `mod.rs`, `chunking.rs`, `storage.rs`, `index.rs`,
  `search.rs`) before the file grows past `webcrawl.rs`'s own stated threshold, applying the
  plan's own Step 0.5 argument to itself.

- [ ] **Epic 4.1 / Stories 4.1.1–4.1.3 (`index_source`'s internal structure)** — the chosen
  Transaction Script (Pattern Decisions table) is a reasonable top-level choice, but
  `index_source` as specified does 7+ distinct concerns in one function body (seed
  validation, crawl-loop content-hash dedup, per-page chunking with a cross-page truncation
  cap, header-string construction, one batched embed call, old-`SourceMeta` diffing for
  removed-page reporting, three-file persistence, output assembly). Task 4.1.2c hedges this
  explicitly ("...if it's factored as its own function"), leaving decomposition optional
  rather than required — risking a Long Method inside an otherwise-correct pattern choice.
  **Remediation**: make decomposition a required part of Story 4.1.1–4.1.3, not an aside —
  factor `index_source`'s body into named private helpers matching the existing phase
  breakdown (e.g. `crawl_and_dedup`, `collect_and_truncate_chunks`, `persist_source`), each
  independently unit-testable at that granularity, turning Task 4.1.2c's current hedge into
  the mandated structure.

## Nitpicks

- `IndexDocsOutput.source_id` / `IndexedSourceSummary.source_id` expose the internal
  slugified directory key on the wire, but no tool input actually consumes it —
  `search_docs`/`remove_indexed_source` take `source` by display name, not by `source_id`.
  Consider whether it needs to be wire-visible at all, versus being purely a debugging
  convenience that couples external consumers to `SourceId`'s slugification algorithm.
- `ChunkRecord.content_hash` is actually a **page**-level hash duplicated identically across
  every chunk belonging to that page (per Task 4.1.1b), not a per-chunk hash — the field name
  invites confusion with a genuinely per-chunk value; `page_content_hash` would be clearer.
- `SearchDocsInput.limit` has no upper-bound clamp, unlike `max_depth`/`max_pages` (both
  clamped via `resolve_limits`'s existing `MAX_PAGES_CEILING`/`MAX_DEPTH_CEILING` pattern in
  `webcrawl.rs`). For consistency with that established codebase convention, clamp `limit`
  too (e.g. `1..=50`) rather than leaving it unbounded.
- Epic 1.2 adds `final_url: String` to `ports::HttpResponse`, which now expresses the same
  concept `ports::PageExtract.final_url` already carries for `BrowserDriver`. Not wrong (the
  two ports serve different call paths), but worth a one-line doc-comment cross-reference so
  a future reader doesn't wonder why two port response types independently carry `final_url`.

## What's Right (not a finding, context for the verdict)

- Wire/storage separation is correctly modeled: `schema.rs`'s `JsonSchema`-derived
  `IndexDocsOutput`/`DocsSearchResult`/etc. are hand-assembled from, not directly serialized
  from, `docs.rs`-local `ChunkRecord`/`SourceMeta`/`SourceSummary` (explicitly called out in
  Task 3.4.2a) — correct DTO/storage-format decoupling.
- `Embedder` as GoF Adapter+Strategy (ADR-0002) is the right call and matches every other
  OS/hardware-touching capability's port-and-adapter treatment in this codebase.
- The `SourceId` newtype itself (distinct from `source_name`) is a good application of
  type-driven design — the concerns above are about *gaps* in it (empty-string case), not
  about the newtype's existence being wrong.
- Full consistency with `research/build-vs-buy.md`: `fastembed`, brute-force cosine, no
  vector-search crate, no fork target — every recommendation in that research doc is
  reflected exactly in ADR-0001/ADR-0002 and the Pattern Decisions table.
