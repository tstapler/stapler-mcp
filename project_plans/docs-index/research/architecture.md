# Architecture Research: docs-index

**Status**: Draft | **Phase**: 2 — Research
**Input**: `../requirements.md`

## 1. How the existing architecture actually works

### 1.1 Ports (`crates/core/src/ports.rs`, 122 lines, read in full)

`core` never touches `std::net`/`fs`/`process`/`env`/`time::Instant` directly — every
OS-touching capability is a trait, each with exactly one native adapter
(`crates/native/src/*`, real tokio/reqwest/fs4/chromiumoxide) and one wasm adapter
(`crates/wasm/src/*`, wasm-bindgen glue delegating to a Node host). Current ports:
`Conn`/`Listener`/`SocketFactory` (daemon IPC), `LockGuard`/`ProcessLock` (single-instance
flock), `ProcessSpawner` (detach daemon), `EnvPort`, `ClockPort`, `SleepPort`, `HttpClient`
(`get(url, headers) -> HttpResponse{status, body}`), `BrowserDriver`
(`navigate_and_extract(url, timeout) -> PageExtract`), and `FileStore`:

```rust
pub trait FileStore {
    async fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), PortError>;
    async fn read_file(&self, path: &str) -> Result<Option<Vec<u8>>, PortError>; // None = cache miss
}
```

`FileStore` is deliberately minimal — whole-blob read/write keyed by an opaque path
string, nothing SQL-shaped, nothing partial/streaming. Native's `NativeFs` is literally
`tokio::fs::write`/`read` (creating parent dirs on write). Wasm's `WasmFs` calls
`jsWriteFile`/`jsReadFile` through `wasm-bindgen` into `src/glue/fs.js`. Both return
`Ok(None)` for missing files rather than erroring — cache-miss is a first-class case, not
an error path.

The pattern per the file's own header comment: add a port **only** for genuinely
OS/hardware-touching behavior; pure computation stays as plain functions/structs in
`core`, generic over `<H: HttpClient>` etc. so it's testable with fakes and portable to
both adapters unchanged.

### 1.2 `webcrawl.rs` (`crates/core/src/tools/webcrawl.rs`, 286 lines)

One shared BFS crawler (`struct Crawler<'a, H: HttpClient>`) backs both `read_website`
(Readability→Markdown extraction, cached under `{cache_dir}/read-website/<sha256(url)>.json`
as `{title, markdown}`) and `download_website` (raw HTML saved to
`{save_dir}/{host}/{sanitized-path}`). Both are the only `pub` items in the file.

**Important visibility finding**: `Crawler` itself and every one of its methods
(`new`, `allowed`, `next_url`, `fetch_and_expand`) are **private** — no `pub` or
`pub(crate)` — as are the helper fns `extract_title_and_markdown`, `extract_links`,
`fetch_robots`, `fetch_ok`, `same_host`, `cache_key_for`. Rust module privacy means a
sibling module (`tools::docs`) cannot call any of this today, even within the same crate,
despite both being under `stapler-mcp-core`. Requirements' "reusing `Crawler`" is
therefore not free — it requires a deliberate visibility bump (see §5).

Generic loop shape both `pub fn`s share (this is the shape a new indexing function needs
to replicate):

```rust
let mut crawler = Crawler::new(http, seed, max_depth, max_pages).await;
let mut pages = Vec::new();
while let Some((url, depth)) = crawler.next_url(pages.len()) {
    // read_website: check FileStore cache by sha256(url) first, else...
    let Some(html) = crawler.fetch_and_expand(&url, depth).await else { continue };
    // ...extract/transform/persist, push to `pages`
}
```

### 1.3 Wiring: schema → daemon registration → thin client

- `crates/core/src/schema.rs` — plain `#[derive(Serialize, Deserialize)]` Input/Output
  structs per tool (`ReadWebsiteInput`, `ReadWebsiteOutput`, etc.). No enum/registry here;
  it's just typed payloads.
- `crates/cli/src/main.rs` (the daemon binary) — imperative registration:
  `daemon.register("read_website", json_handler(closure capturing Rc<NativeHttp>,
  Rc<NativeFs>, cache_dir, calling webcrawl::read_website(...)))`. Every tool is one
  `daemon.register(name, json_handler(...))` call. `cache_dir` comes from
  `core::paths::cache_dir(&env)` = `~/.stapler-mcp/cache` (overridable via
  `STAPLER_MCP_HOME`, see `crates/core/src/paths.rs`).
- `crates/cli/src/thin_client.rs` — the actual MCP-facing tool endpoints (rmcp), one per
  tool, each just `call_daemon("read_website", params.0).await.map(Json)` — a pure IPC
  forward to the daemon over the `Conn`/socket port. The thin client holds no business
  logic and no ports beyond the socket.

A new docs-index feature needs: new `*Input`/`*Output` structs in `schema.rs`, new
`daemon.register(...)` call(s) in `cli/src/main.rs` wiring up whatever ports the new tool
needs, and new thin-client MCP endpoint(s) in `thin_client.rs` that just forward to
`call_daemon`. This is a well-worn, mechanical path — no surprises here.

## 2. Decision: does the vector index need its own port trait?

**No new `VectorStore`/`Index` port. Reuse `FileStore`.** Cosine similarity over a small,
fully-loaded-into-memory `Vec<f32>` corpus is pure computation, not an OS capability — it
belongs in `core` as plain logic, same as `extract_links`/`same_host` in webcrawl.rs are
plain functions rather than a "LinkExtractorPort". `FileStore::read_file` returning the
whole blob is exactly the right shape for "load one source's index into memory, do
brute-force similarity search, return top-k" at the corpus sizes in scope (a handful of
doc sources — the requirements doc explicitly names this as the assumption that lets
brute-force be acceptable instead of `sqlite-vec`/HNSW). If corpus size ever grows past
what brute-force in-memory search tolerates, that's a future port-trait decision
(`VectorStore` with `upsert`/`search` methods, native impl on `sqlite-vec` via `rusqlite`)
— but it is not justified by today's scope and would be premature abstraction against
unconfirmed scale.

**A new `Embedder` port trait is warranted**, and for a different reason than storage:
running model inference (loading an ONNX/candle model, doing tensor math, possibly a
one-time model-weights download) is exactly the class of OS/hardware-touching capability
every existing port already gate-keeps (network, filesystem, process, browser, clock).
Proposed shape, consistent with `HttpClient`'s style:

```rust
pub trait Embedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError>;
}
```

Native adapter (`crates/native/src/embed.rs`, new): loads a small local
sentence-transformer (specific crate — `candle`, `ort`+ONNX, `fastembed-rs` — is the Stack
research dimension, out of scope here) once, likely lazily on first use and cached for the
daemon's lifetime — the daemon is already a persistent process, so amortizing model-load
cost across calls is free architecture the per-session-stdio `docs-mcp-server` never had.

Wasm adapter: **flagged as the single open architectural risk**, matching requirements'
own pitfalls section. Two honest options, not a shared implementation:
1. **Native-only for v1** (recommended) — `WasmEmbedder` either doesn't exist or returns
   `PortError::Other("embeddings not supported on this adapter")`, and the
   docs-index tool is registered only in `cli/src/main.rs` (native daemon), not exposed
   through any wasm/Node code path at all.
2. Proxy to Node via `wasm-bindgen` glue (same pattern as `fs.js`/browser CDP), calling a
   JS embedding lib (e.g. `transformers.js`) from the Node host process. Rejected for v1:
   this reintroduces the exact category of heavy Node/ML dependency this whole project
   exists to eliminate, on the one adapter (wasm/Node) where it's least escapable — a
   self-defeating trade for a feature whose entire premise is escaping a Node/LangChain
   stack.

Given `stapler-mcp`'s daemon is a native long-running process and the thin client/wasm
side exists mainly for the Node-hosted MCP transport shim, option 1 costs nothing today:
the daemon already runs natively; only the thin client crosses into wasm, and the thin
client has no business logic to begin with (see §1.3) — it will forward `index_docs`/
`search_docs` calls to the native daemon exactly like every other tool, unaffected by
whether wasm has its own `Embedder`.

## 3. Chunk/embedding storage format

Two candidate formats; recommending the first.

**Recommended: JSON Lines, one file per indexed source**, under
`~/.stapler-mcp/docs-index/<source-id>/chunks.jsonl` (mirrors the existing
`{cache_dir}/read-website/<sha256(url)>.json` convention — one file per logical unit, hash
or slug as the key), one JSON object per line:

```json
{"chunk_text": "...", "embedding": [0.0123, -0.0456, ...], "source_url": "https://...", "chunk_index": 3, "content_hash": "sha256-of-source-page-html"}
```

Plus a small sidecar `~/.stapler-mcp/docs-index/<source-id>/meta.json`:

```json
{"source_url": "https://...", "source_id": "...", "indexed_at_millis": 1752... , "chunk_count": 42, "embedding_model": "..."}
```

`meta.json` is what a `list_indexed_sources` tool reads (cheap — no need to load every
`chunks.jsonl` to enumerate sources), and what staleness/re-index decisions key off. JSONL
was chosen over a single big JSON array because `FileStore` has no append primitive
(`write_file` is whole-blob) and no streaming read — JSONL keeps the option open to switch
to append-friendly writes later without a format migration, and keeps failure atomic per
line if a future incremental-embed path is added.

**Alternative considered**: one file per source but as a single JSON object
`{chunks: [...]}`Instead of JSONL. Simpler to reason about, marginally smaller (no
per-line overhead), but forces a full read-modify-write of the whole file for any change
and gives up the future append option for no benefit at today's scale. Not recommended,
but noted since at "a handful of sources" scale either works — this is a low-stakes
choice.

`source-id` derivation: reuse the existing `cache_key_for` pattern
(`sha256(seed_url)`-derived hex string) from webcrawl.rs rather than inventing a second
hashing scheme — again, extend rather than duplicate.

### Cache invalidation / re-indexing strategy

**Full replace, not incremental, for v1.** `read_website`'s own cache has no TTL or
invalidation logic at all today (a page cached once is cached forever until the cache dir
is manually cleared) — there is no existing staleness-detection infrastructure in this
codebase to extend (`ClockPort` exists but nothing currently uses it for expiry). Given
that, the lowest-risk, most consistent-with-existing-patterns behavior is: an explicit
re-index call for a source deletes/overwrites `chunks.jsonl` and `meta.json` wholesale via
`write_file`, rather than diffing old vs. new chunks. This avoids partial-index states (a
half-updated JSONL file) and matches the "cache miss vs. hit" binary already used
everywhere else in this codebase.

A cheap, optional improvement that reuses existing machinery rather than adding new
infrastructure: before re-embedding a page, compare its freshly-fetched HTML's sha256
against the `content_hash` already stored for that page's chunks in the old `chunks.jsonl`
(if unchanged, skip re-embedding that page's chunks and carry them forward unchanged into
the new file). This is a v1.1-shaped optimization, not a v1 requirement — flag it in
planning but don't block v1 on it.

## 4. Module placement: `tools/docs.rs` vs. separate ingestion path

**Recommend a new `crates/core/src/tools/docs.rs`, sibling to `webcrawl.rs`**, that
composes (not duplicates) webcrawl's crawl loop, exactly as `read_website`/
`download_website` already do for their own two output modes. Rationale:

- The requirements doc itself frames this as "extend or sit alongside" webcrawl.rs, not
  invent new ingestion — a third sibling module keeps that intent literal without
  entangling docs-index's embedding/chunking concerns into webcrawl.rs's file (which is
  already at 286 lines serving two tools; a third tool's worth of chunking/embedding logic
  belongs in its own file, not appended there).
- `tools/mod.rs` already declares `pub mod fetch; pub mod search; pub mod webcrawl;` — one
  more `pub mod docs;` is the established pattern, zero new architectural surface.
- Fits the daemon-registration and thin-client wiring pattern in §1.3 without
  modification — `docs::index_source`/`docs::search_docs` become two more
  `daemon.register(...)` calls and two more thin-client endpoints, identical shape to
  every existing tool.

**Concrete follow-on requirement this creates**: bump `Crawler` and the private helpers it
needs (`new`, `next_url`, `fetch_and_expand`, and `extract_title_and_markdown` for getting
clean Markdown out of fetched HTML before chunking) from private to `pub(crate)` in
webcrawl.rs. `pub(crate)` (not `pub`) is sufficient and correct — nothing outside this
crate needs to construct a `Crawler`, this is purely a same-crate, cross-module visibility
fix. This is a small, mechanical, low-risk change but it is a real diff to an existing
file and should be called out explicitly in the implementation plan rather than discovered
mid-implementation.

`docs::index_source` should follow the exact loop shape from §1.2: `Crawler::new` →
`next_url`/`fetch_and_expand` loop → `extract_title_and_markdown` per page (reuse, don't
reimplement Readability extraction) → chunk the returned Markdown (chunking strategy is a
Features-dimension research question, not decided here — e.g. split on heading boundaries
then re-split long sections to a token/char budget) → `Embedder::embed` the chunks → append
to the in-memory `Vec` for that source → `FileStore::write_file` the final JSONL/meta pair
once the crawl completes (full-replace per §3, not per-page incremental writes, to avoid
partial state on a failed/interrupted crawl).

## 5. Summary of concrete architectural decisions

| Question | Decision |
|---|---|
| New port trait for the vector index itself? | No — reuse `FileStore`, brute-force cosine similarity is plain `core` logic |
| New port trait for embeddings? | Yes — `Embedder::embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError>` |
| Wasm `Embedder` impl? | None for v1 — docs-index tools registered/reachable natively only |
| Storage format | JSONL chunks + JSON meta sidecar, one dir per source under `~/.stapler-mcp/docs-index/<source-id>/` |
| Re-index strategy | Full replace (matches existing no-TTL cache pattern); content-hash skip-unchanged is an optional v1.1 |
| Module | New `crates/core/src/tools/docs.rs`, sibling to `webcrawl.rs`, composing it |
| Required pre-work | Bump `Crawler`/`next_url`/`fetch_and_expand`/`extract_title_and_markdown` in `webcrawl.rs` from private to `pub(crate)` |
| Wiring | New schema structs in `schema.rs`, new `daemon.register(...)` in `cli/src/main.rs`, new thin-client endpoints in `thin_client.rs` — identical mechanical pattern to every existing tool |

## 6. Data flow (index path)

```
MCP client → thin_client::index_docs (rmcp endpoint)
           → call_daemon("index_docs", params)  [IPC over Conn/socket]
           → daemon dispatch → docs::index_source(&http, &fs, &embedder, input)
               → Crawler::new/next_url/fetch_and_expand  (pub(crate), reused from webcrawl.rs)
               → extract_title_and_markdown per page      (pub(crate), reused)
               → chunk(markdown) -> Vec<Chunk>             (new, core, pure fn)
               → embedder.embed(chunk_texts)               (new Embedder port, native adapter)
               → assemble Vec<{chunk_text, embedding, source_url, chunk_index, content_hash}>
               → fs.write_file(".../docs-index/<source-id>/chunks.jsonl", ...)
               → fs.write_file(".../docs-index/<source-id>/meta.json", ...)
```

Search path is symmetric and simpler — no `HttpClient`/`Crawler` involved at all:

```
MCP client → thin_client::search_docs → call_daemon → docs::search_docs(&fs, &embedder, input)
    → fs.read_file(".../docs-index/<source-id>/chunks.jsonl")   [None => "source not indexed" error]
    → embedder.embed(&[query])
    → cosine_similarity(query_embedding, each chunk.embedding), sort, take top-k
    → return ranked chunks
```

Consistency requirement: since search reads whatever `chunks.jsonl` currently contains and
index does a full-file replace (§3), a search that races an in-flight re-index sees either
the fully-old or fully-new index, never a torn mix — as long as `write_file` writes are
effectively atomic at the OS level for files this size, which `NativeFs`'s
`tokio::fs::write` provides (single write syscall path for small files; no explicit
temp-file-then-rename here today, worth a plan-time note but not a blocker at this file
size).
