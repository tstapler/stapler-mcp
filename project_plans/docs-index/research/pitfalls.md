# Research: Pitfalls & Risks — docs-index

**Dimension**: Pitfalls | **Status**: Complete | **Date**: 2026-07-14

## Question

What commonly goes wrong with local-embedding/RAG-style doc-search features? What are
the risks in the likely chosen stack (`candle`/`ort`/`fastembed-rs` + `wasm32`)? What
should be explicitly designed against?

## Codebase Context (grounding facts)

- `crates/core/src/daemon.rs` — the daemon is **deliberately single-threaded**: no
  `Send` bounds anywhere, handlers are `LocalBoxFuture`, state uses `Rc`/`RefCell`/
  `Cell`. Doc comment states this explicitly: "the native binary runs this on a
  `current_thread` tokio runtime + `LocalSet`, which is what lets the exact same
  handler-registry code also satisfy a `!Send` wasm-bindgen adapter." "Concurrent"
  `fetch_page`/`brave_web_search`/`read_website` calls are cooperatively interleaved
  on **one OS thread**, not run in parallel — concurrency comes from `.await` points
  yielding, not from multithreading.
- `crates/core/src/tools/webcrawl.rs` — `Crawler` fetches `robots.txt` best-effort
  (`fetch_robots`, lines 62–69: unreachable/absent/unparseable robots.txt is treated
  as allow-all, not aborted), restricts link extraction to `http`/`https` schemes and
  same-host (`extract_links`, lines 42–50), and caps `max_depth`/`max_pages` at hard
  ceilings (5 / 50). There is **no IP-range or hostname allow/deny list** — no
  loopback/link-local/RFC1918 blocking. `robots.txt` is a politeness convention, not
  an access-control mechanism (see Security section below).
- `crates/core/src/ports.rs` — `FileStore::write_file`/`read_file` is the existing
  cache port (native: real filesystem under `~/.stapler-mcp/`; wasm: Node `fs` via
  JS glue). Any docs-index storage would reuse this port and inherits its trust
  model: filesystem permissions on `~/.stapler-mcp/` are the only access control,
  same as the rest of the daemon.

## Findings

### 1. Concurrency / long-lived daemon risk — this is the standout, codebase-specific pitfall

The single-threaded, `!Send`, `current_thread` + `LocalSet` architecture is a
**deliberate, load-bearing design choice** (it's what lets one handler-registry
implementation satisfy both the native adapter and the `!Send` wasm-bindgen
adapter). This creates a real conflict with embedding inference:

- Embedding inference (ONNX Runtime via `ort`, or `candle`) is CPU-bound. If it runs
  inline inside a handler `Future` without yielding, it **blocks the single OS
  thread for the full inference duration**, stalling every other in-flight daemon
  tool call — `fetch_page`, `brave_web_search`, `read_website`, browser tools — for
  as long as embedding takes. For a doc-search query embedding a handful of chunks
  this may be tens to hundreds of ms; for bulk re-indexing a full doc source it
  could be seconds, during which the daemon is unresponsive to everything else.
- The standard Tokio fix — `tokio::task::spawn_blocking` — **requires `F: Send`**.
  Tokio's own docs confirm `spawn_blocking` still uses a separate blocking-thread
  pool even under a `current_thread` runtime, but the handler futures in this
  daemon are built `!Send` by design (`Rc`/`RefCell` throughout). That means
  embedding inference can't cleanly be shelled out to `spawn_blocking` without
  either (a) making the embedding call path `Send`-isolated (e.g., own thread with
  channel handoff, not `spawn_blocking` directly touching `!Send` state) or (b)
  accepting the blocking-the-loop behavior for now and scoping inference to small,
  fast operations only.
- `block_in_place` is explicitly unusable here too — Tokio docs state it panics /
  is unsupported on `current_thread` runtimes (no other worker thread to hand off
  to).
- **Design implication**: this needs an explicit decision in the plan phase, not an
  afterthought. Realistic options: (a) run the embedding model in a dedicated
  worker thread (native `std::thread` + `std::sync::mpsc`/oneshot channel from the
  `!Send` handler, i.e. a hand-rolled `Send`-safe boundary around the one truly
  CPU-bound piece), (b) keep chunks/batches small enough that inline blocking is
  tolerable (sub-10ms per call) and document the daemon-wide latency hit during
  indexing, or (c) run indexing as a rare, explicit background operation (not
  triggered inline by a tool call) so any blocking only affects a batch job, not
  live query latency. Do not assume `spawn_blocking` is a drop-in fix without
  addressing the `Send` mismatch first.

### 2. `wasm32-unknown-unknown` embedding inference is very likely infeasible — treat as settled, not open

- **`ort` (ONNX Runtime bindings)**: the maintainer has publicly discontinued WASM
  support. Per the project's own release notes/discussions: recent changes to
  Emscripten and ONNX Runtime made WASM support "exponentially more difficult" to
  maintain; the maintainer stated it's "no longer feasible to work on WASM support
  for `ort`" given the difficulty of debugging ONNX Runtime's C++ internals under
  Emscripten, and explicitly redirected users to alternative WASM-capable ONNX
  crates (`tract`, `wonnx`). This forecloses `ort`+`wasm32` as a viable path.
- **`fastembed-rs`**: does not target `wasm32-unknown-unknown` at all — it wraps
  `ort` natively and is documented/observed as native-only (no WASM build reported
  in the ecosystem). Not viable for the wasm/Node adapter under any configuration.
- **`candle`**: has a real, working WASM story, but specifically via
  `wasm-bindgen`/browser-oriented examples (Whisper, SAM, LLaMA2-in-browser demos)
  — these assume the standard `wasm-bindgen` JS-interop toolchain and, in several
  documented cases, hit friction: `huggingface/candle` issue #1032 notes
  `wasm32-unknown-unknown` isn't supported "by default" and needs feature-flag
  changes (the `"js"` feature, for `getrandom`-style shims); issue #2736 (Jan 2025)
  reports `candle-wasm-examples/whisper` failing to build for
  `wasm32-unknown-unknown` with an unresolved `libloading::Symbol` import — i.e.
  even candle's own example crates hit native-only transitive dependencies that
  don't compile to wasm cleanly. Getting a sentence-transformer model (not just
  Whisper) running via candle under `wasm32-unknown-unknown` loaded by **Node**
  (this daemon's actual wasm host, not a browser) is unproven territory — most
  working candle+wasm demos assume a browser environment with `wasm-bindgen`
  glue and, for larger models, SharedArrayBuffer-based threading that
  `wasm32-unknown-unknown` does not support at all (`std::thread::spawn` panics on
  this target).
- **`tract` (sonos/tract)**: the most promising wasm-compatible ONNX runtime — pure
  Rust, no C++ shared libraries, no internal multithreading requirement, and is
  positioned by its own maintainers as good for "lightweight, portable inference
  without external dependencies." This is the one candidate worth evaluating
  concretely if wasm-side embeddings are pursued at all, but it was not part of the
  three stacks named in requirements.md and needs its own spike (model format
  compatibility with common sentence-transformer ONNX exports, quantization
  support, actual binary size once bundled).
- **Bottom line**: of the three crates named as "likely chosen stack" in
  requirements.md, two (`ort`, `fastembed-rs`) are confirmed infeasible on
  `wasm32-unknown-unknown`, and the third (`candle`) has known compile-time
  friction even in its own example suite and no confirmed track record running a
  sentence-embedding model inside a **Node-hosted** wasm module (as opposed to a
  browser). Recommend treating "wasm32 real-embedding inference" as **not
  feasible for v1** rather than an open research question to re-litigate at
  planning time — pick a fallback now (see #3).

### 3. Recommended fallback: native-only embeddings, ship v1 native-only (don't build the wasm lexical-fallback path yet)

Two fallback shapes were in scope per requirements.md:

- **(a) Native-only embeddings + wasm/Node falls back to lexical search (BM25/TF-IDF).**
  Doable, but doubles the surface area for v1: two search implementations, two
  ranking behaviors to reconcile in the MCP tool's output contract, and a footgun
  where a query against the same index silently returns different quality/ranking
  behavior depending on which host process happens to be running that day (native
  daemon vs Node daemon — both are valid deployment modes here per `NOTES.md`'s
  Phase 2 description of the dual adapter). That inconsistency is a genuine
  correctness/UX pitfall specific to this project's thin-client/daemon design: the
  user should get the same search behavior regardless of which adapter is
  currently active, and BM25-vs-embeddings does not deliver that.
- **(b) Ship native-only for v1, no wasm story at all.**
  Given the single user, single machine, and that `docs-mcp-server` is already
  being replaced primarily to eliminate the Node process — recommend **(b)**. It
  avoids building and maintaining a second retrieval algorithm for a code path
  that may rarely execute (how often does this user actually run the wasm/Node
  adapter for doc-search specifically, versus the native daemon?), keeps the v1
  scope aligned with "handful of doc sources, solo user," and defers the BM25
  fallback to a later phase only if the wasm adapter turns out to matter in
  practice. This also sidesteps pitfall #1's `Send`/blocking problem for the wasm
  side entirely, since wasm-bindgen environments have no threads to begin with —
  inline-blocking concerns would be even sharper there, not easier.

### 4. General RAG/semantic-search pitfalls — applicability to this project's scale

- **Chunk boundary quality.** Splitting HTML/Markdown mid-sentence or mid-code-block
  measurably hurts retrieval — a chunk that's half of one idea and half of the next
  embeds as a blurry average of both and matches neither well. Chunk on structural
  boundaries (headings, paragraphs, list items, code fences) where possible, not
  fixed character/token windows blindly — `webcrawl.rs` already parses HTML via
  `dom_query`/`dom_smoothie`/`htmd`, so structure-aware chunking (split on
  heading/paragraph boundaries from the already-parsed DOM, before the
  HTML→Markdown conversion collapses structure) is cheap to get mostly right by
  reusing that same pipeline rather than chunking the flattened Markdown output
  blind.
- **Embedding drift on model change.** If the embedding model or its version ever
  changes, old vectors and new-query vectors stop being comparable — cosine
  similarity between vectors from different model generations is close to
  meaningless, and the failure mode is silent (results just get steadily worse,
  not an error). Mitigation: store the embedding model name+version as metadata
  alongside each stored vector/chunk; on model change, require a full re-index
  rather than incremental — partial re-embedding (some chunks old-model, some
  new-model) is explicitly called out as the most common way real systems get this
  wrong. Given the FileStore-backed JSON-cache-like storage under discussion, this
  is a single extra field to add to the schema now, cheap insurance later.
- **Silent staleness.** The index can report "up to date" while the live source
  page has changed since last crawl, because nothing re-checks the source unless
  explicitly told to. `webcrawl.rs`'s existing cache (keyed by SHA-256 of URL, per
  `cache_key_for`) already has this same property for `read_website`/
  `download_website` — worth checking how staleness is currently handled there (if
  at all) before designing docs-index's re-indexing story, since the two should
  probably share a policy (e.g., TTL-based re-fetch, or explicit
  `refresh_docs_source` tool call) rather than inventing a second one.
- **Unbounded growth / no eviction.** Not a real risk at this project's declared
  scale ("a handful of doc sources") but worth one guard rail: cap total indexed
  chunks or total on-disk size and fail/refuse loudly rather than silently growing
  `~/.stapler-mcp/` forever, since there's no admin UI to notice and clean it up.
- **Brute-force cosine similarity scaling.** Confirmed **not a real risk** for this
  user's stated scale. Brute-force exact kNN is broadly considered fine up to
  ~100k vectors (some sources put the ANN-necessary threshold even higher, in the
  millions) before latency becomes noticeable; "a handful of doc sources" chunked
  at typical page/section granularity is realistically hundreds to low thousands
  of vectors. No need to build or evaluate an ANN index (`sqlite-vec`, HNSW, etc.)
  for v1 — brute-force cosine similarity over an in-memory `Vec<f32>` (or backed by
  the existing `FileStore` JSON cache) is the right amount of engineering here, and
  matches the "reuse `FileStore`-backed JSON cache with brute-force cosine
  similarity" option requirements.md already floats as sufficient.

### 5. Security

- **robots.txt is not an SSRF control, and requirements.md's framing should be
  corrected.** requirements.md states "SSRF-style risk already mitigated by
  existing Crawler robots.txt handling" — this is not accurate. `robots.txt` is a
  voluntary crawler-politeness convention with no enforcement mechanism; it says
  nothing about whether a URL points at an internal/loopback/link-local address,
  and compliance is purely client-side choice. The actual mitigating factors
  already present in `Crawler` are: scheme restriction to `http`/`https`
  (`extract_links`, webcrawl.rs:48) and same-host-only link following — these
  reduce *accidental* wandering during a crawl, but do nothing to stop the crawl
  from being pointed at `http://169.254.169.254/...` or `http://localhost:PORT/...`
  in the first place, since the seed URL itself is user/caller-supplied and there
  is no IP-range allow/deny list anywhere in the fetch path. This is an existing
  gap shared with `read_website`/`fetch_page` today, not something docs-index
  introduces net-new — but docs-index changes the blast radius: a one-off
  `read_website` SSRF-adjacent fetch returns transient output to the caller and is
  gone; a docs-index ingestion of the same URL **persists the fetched content to
  disk** (chunked, embedded, indexed) where it becomes searchable indefinitely
  later. If this is ever a concern, it's worth flagging as a pre-existing gap to
  fix at the `Crawler`/`HttpClient` level generally, not something to solve
  bespoke inside docs-index.
- **On-disk storage of embeddings/text.** New surface versus `read_website`'s
  existing cache: persistent storage of full page text and embeddings under
  `~/.stapler-mcp/`, same trust boundary as everything else the daemon already
  caches (filesystem permissions only, single-user single-machine posture per
  requirements.md's own constraints). Not a new class of risk given the stated
  threat model, but worth confirming file permissions on written cache files
  match whatever the existing `FileStore` native adapter already sets (check
  `crates/native/src/fs.rs::write_file` mode bits) — no indication from this
  research pass that they don't, just flagging as a one-line verification for the
  planning phase rather than assuming.

## Sources

- [pykeio/ort — Releases (WASM support discontinuation notes)](https://github.com/pykeio/ort/releases)
- [Introduction | ort](https://ort.pyke.io/)
- [Target wasm32-unknown-unkown not supported · Issue #1032 · huggingface/candle](https://github.com/huggingface/candle/issues/1032)
- [candle-wasm-examples/whisper: cargo build --target wasm32-unknown-unknown fails · Issue #2736 · huggingface/candle](https://github.com/huggingface/candle/issues/2736)
- [GitHub - sonos/tract: Tiny, no-nonsense, self-contained, Tensorflow and ONNX inference](https://github.com/sonos/tract)
- [tract-onnx - crates.io](https://crates.io/crates/tract-onnx)
- [GitHub - Anush008/fastembed-rs](https://github.com/anush008/fastembed-rs)
- [fastembed - docs.rs](https://docs.rs/fastembed)
- [spawn_blocking in tokio::task - Rust docs](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)
- [block_in_place in tokio::task - Rust docs](https://docs.rs/tokio/latest/tokio/task/fn.block_in_place.html)
- [Embedding Drift: The Quiet Killer of Retrieval Quality in RAG Systems](https://dev.to/dowhatmatters/embedding-drift-the-quiet-killer-of-retrieval-quality-in-rag-systems-4l5m)
- [The RAG Freshness Problem: How Stale Embeddings Silently Wreck Retrieval Quality](https://tianpan.co/blog/2026-04-10-rag-freshness-problem-stale-embeddings-silent-failure)
- [Vector similarity search: cosine similarity, dot product & ANN](https://www.kunalganglani.com/learning-paths/ai-software-developer/aidev-embeddings-similarity)
- [Scaling Vector Search Performance: From Millions to Billions](https://bigdataboutique.com/blog/scaling-vector-search-performance-from-millions-to-billions-8d50a1)
- [Server-Side Request Forgery (SSRF) | Imperva](https://www.imperva.com/learn/application-security/server-side-request-forgery-ssrf/)
- [How to prevent SSRF attack - Teleport](https://goteleport.com/blog/ssrf-attacks/)

## Codebase References

- `crates/core/src/daemon.rs:1-7` (single-threaded/`!Send`/`current_thread`+`LocalSet` architecture, doc comment)
- `crates/core/src/tools/webcrawl.rs:42-50` (same-host + scheme-restricted link extraction)
- `crates/core/src/tools/webcrawl.rs:60-69` (`fetch_robots` — best-effort allow-all fallback)
- `crates/core/src/tools/webcrawl.rs:33-37` (`cache_key_for` — SHA-256 URL cache keying, precedent for docs-index storage keying)
- `crates/core/src/ports.rs:116-120` (`FileStore` trait — `write_file`/`read_file`)
- `crates/native/src/fs.rs` (native `FileStore` impl — check file permission bits during planning)
- `NOTES.md:28-30` (Phase 2 dual-adapter architecture: native daemon + `wasm32-unknown-unknown`/Node adapter, both first-class)
