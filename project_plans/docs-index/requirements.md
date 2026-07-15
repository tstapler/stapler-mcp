# Requirements: docs-index

**Status**: Draft | **Phase**: 1 — Ideation complete
**Created**: 2026-07-14

## Problem Statement

The user runs `docs-mcp-server` (github.com/arabold/docs-mcp-server, via
their `tstapler/docs-mcp-server` fork) for documentation search/indexing in
Claude Code. It works, but pulls in a large Node/LangChain stack (Fastify web
server + UI, LangChain multi-provider embeddings wired to AWS Bedrock/Google
Vertex/OpenAI, `tree-sitter` parsers for 90+ languages, `sqlite-vec`,
`better-sqlite3`) just to fetch, index, and semantically search a handful of
doc sources. Everything else the user relies on for MCP tooling has already
been consolidated into `stapler-mcp`, a single Rust daemon/thin-client
architecture (see `README.md`/`NOTES.md` in this repo) — this is the one
remaining tool still running a separate heavy Node process.

(Separately, and already fixed as of this planning pass: `docs-mcp-server`
itself was being spawned fresh per Claude Code session via stdio instead of
connecting to one shared instance — that specific duplication problem was
resolved with a config change, not new code — see `scripts/ensure-docs-mcp-server.sh`
and the `~/.claude.json` `"docs"` entry now pointing at a persistent SSE
server. That fix stands regardless of whether this project proceeds; this
requirements doc is about whether to also replace the tool's *functionality*
with something narrower, native to this daemon.)

Who has this problem: just the user (Tyler), solo, single-machine.

## Success Criteria

In 3 months: can index a handful of doc sources the user actually
references (e.g. a few Rust crate docs, some web documentation) and
semantically search them from Claude Code via the existing `stapler-mcp`
daemon — full functional replacement for how `docs-mcp-server` is used
today, deliberately narrower in format support (HTML/Markdown only, not the
other 88 formats). No separate Node process needed for this at all.

## Scope

### Must Have (MoSCoW)

- Index HTML/Markdown pages into a local store, reusing the existing
  fetch/crawl/`robots.txt` machinery already built for `read_website`
  (`crates/core/src/tools/webcrawl.rs`'s `Crawler`).
- Local embeddings — no cloud API key or per-query cost required (unlike
  `docs-mcp-server`'s LangChain→AWS Bedrock/Vertex/OpenAI setup).
- A semantic-search MCP tool: given a query and an indexed source, return
  ranked relevant chunks/pages.

### Out of Scope

- Web UI (Fastify/Alpine.js/HTMX-style dashboard) — CLI/MCP-tool-only,
  matching every other `stapler-mcp` tool.
- The other 88+ formats `docs-mcp-server` supports (PDF, Office docs,
  OpenDocument, RTF, EPUB, Jupyter notebooks, `tree-sitter`-parsed source
  code in dozens of languages, archives). HTML/Markdown only for v1.
- Multi-provider cloud embeddings (AWS Bedrock, Google Vertex/GenAI, OpenAI)
  — local-only embeddings, no external API dependency.
- OAuth2/OIDC auth, multi-user/networked access — single-user, single-machine,
  same posture as the rest of `stapler-mcp` (filesystem permissions on
  `~/.stapler-mcp/` are the only access control).

## Constraints

- **Tech stack**: Rust, inside the existing `stapler-mcp` workspace —
  reuse the established ports-and-adapters architecture (`HttpClient`,
  `FileStore` ports already implemented on both the native and
  `wasm32`/Node adapters), not a new standalone service.
- **Embeddings**: must run locally, no external API key or per-query cost.
  Specific approach (a small `candle`/`ort`-based sentence-transformer model
  vs. a simpler lexical fallback like TF-IDF/BM25 if a true local-embedding
  model proves too heavy to bundle/run in this daemon, especially under the
  `wasm32-unknown-unknown` adapter) is an open question for the research
  phase, not decided here.
- **Timeline**: no hard deadline; this follows the existing phased build
  log in `NOTES.md` (Phases 1a/1b/2/3 already shipped; this would be a
  subsequent phase).
- **Dependencies**: none on other teams/systems — solo project, same as
  the rest of `stapler-mcp`.

## Context

### Existing Work

- `stapler-mcp` already has the daemon/thin-client architecture, a
  `HttpClient` port (native: `reqwest`; wasm: Node's `fetch`), a `FileStore`
  port with `read_file`/`write_file` (added in Phase 3 specifically to
  support caching), and a proven fetch/crawl/`robots.txt`/cache pattern in
  `crates/core/src/tools/webcrawl.rs` (`read_website`/`download_website`)
  that this would most likely extend or sit alongside rather than
  duplicate.
- `docs-mcp-server`'s per-session stdio duplication was independently
  diagnosed and fixed via a config change this same session (persistent
  systemd `--user` service + SSE transport in `~/.claude.json`) — that fix
  is unrelated to whether this Rust replacement gets built, and stands on
  its own.
- No Rust local-embedding library has been evaluated yet — this is the
  single biggest open technical question and the reason a research phase
  (not straight to planning) is warranted.

### Stakeholders

Just the user (Tyler), solo — single-user, single-machine, same scope as
every other `stapler-mcp` tool.

## Research Dimensions Needed

- [ ] Stack — Rust crates for local embeddings (e.g. `candle`, `ort`
      +ONNX sentence-transformer models, `fastembed-rs`) and local vector
      search/similarity (e.g. `sqlite-vec` via `rusqlite`, a pure-Rust
      in-memory/on-disk vector index, or reusing the existing
      `FileStore`-backed JSON cache with brute-force cosine similarity for
      the expected small corpus size) — and which of these, if any,
      actually compile cleanly to `wasm32-unknown-unknown` (a real
      constraint every other Phase 3 dependency was checked against and
      several needed adjustment).
- [ ] Features — survey what a minimal semantic doc-search tool actually
      needs beyond fetch+embed+search: chunking strategy for long pages,
      re-indexing/staleness handling, how many doc sources realistically
      need indexing at once, whether a "list indexed sources" tool is
      needed alongside search.
- [ ] Architecture — how this fits alongside `read_website`/`webcrawl.rs`:
      a new `tools/docs.rs` module reusing `Crawler`, or a genuinely
      separate ingestion path; whether the vector index needs its own
      port trait or fits inside `FileStore`; chunk/embedding storage format
      and cache-invalidation strategy.
- [ ] Pitfalls — known failure modes: embedding model size/load time in a
      long-running daemon, cold-start cost under `wasm32`/Node (loading an
      ONNX model via `ort`/`candle` inside a WASM sandbox may not be
      feasible at all — this could kill the "must work on both adapters"
      constraint for the embedding piece specifically, unlike every prior
      Phase 3 dependency which was pure computation), and what happens if
      it isn't (e.g. embeddings native-only, wasm/Node falls back to
      lexical search, or the whole tool ships native-only for now).
