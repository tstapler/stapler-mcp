//! Docs indexing/search tools (native-only; see ADR-0002).
//!
//! Builds on `webcrawl.rs`'s fetch/crawl/`robots.txt` pipeline (its
//! `Crawler` and helper functions are `pub(crate)` for reuse here) to turn
//! crawled Markdown pages into a locally-embedded, brute-force-searchable
//! index: chunk -> embed -> store as JSONL + JSON sidecars under
//! `docs_index_dir`, then rank via `cosine_similarity` at query time. See
//! `project_plans/docs-index/implementation/plan.md` for the full design.
//!
//! This file currently contains Phase 3's pure-logic building blocks
//! (chunking, cosine similarity, `SourceId`/slugification, storage record
//! types + path helpers, and `SourceLocks`) — the `index_docs`/
//! `search_docs`/`list_indexed_sources`/`remove_indexed_source` tool
//! functions that compose these (Phase 4) land separately.

use std::cell::RefCell;
use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use text_splitter::MarkdownSplitter;
use url::Url;

use super::webcrawl::{extract_title_and_markdown, resolve_limits, Crawler};
use crate::ports::{ClockPort, Embedder, FileStore, HttpClient};
use crate::schema::{
    DocsSearchResult, IndexDocsInput, IndexDocsOutput, IndexedSourceSummary,
    ListIndexedSourcesInput, ListIndexedSourcesOutput, RemoveIndexedSourceInput,
    RemoveIndexedSourceOutput, SearchDocsInput, SearchDocsOutput,
};

// ---------------------------------------------------------------------
// Epic 3.2: Chunking
// ---------------------------------------------------------------------

/// One chunk of a page's markdown, produced by [`chunk_markdown`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Chunk {
    pub text: String,
    pub heading: Option<String>,
    pub chunk_index: u32,
}

/// Finds every `#`/`##`/`###` heading line in `markdown`, returning
/// `(byte_offset_of_line_start, heading_text)` pairs in document order.
/// Deeper headings (`####`+) are deliberately not tracked, matching the
/// plan's "most recent `#`/`##`/`###` heading line" scope.
fn heading_marks(markdown: &str) -> Vec<(usize, String)> {
    let mut marks = Vec::new();
    let mut offset = 0usize;
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let hash_count = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=3).contains(&hash_count) {
            let rest = &trimmed[hash_count..];
            if rest.is_empty() || rest.starts_with(' ') {
                let text = rest.trim().to_string();
                if !text.is_empty() {
                    marks.push((offset, text));
                }
            }
        }
        offset += line.len();
    }
    marks
}

/// Splits `markdown` into structurally-sensible [`Chunk`]s within a
/// `200..800`-character budget (`text-splitter::MarkdownSplitter`),
/// tagging each chunk with the nearest preceding `#`/`##`/`###` heading
/// line seen so far in document order — falling back to `page_title` for
/// any text that appears before the first heading. Never panics on empty
/// input; returns `vec![]` for `""`.
pub(crate) fn chunk_markdown(markdown: &str, page_title: &str) -> Vec<Chunk> {
    if markdown.is_empty() {
        return vec![];
    }

    let marks = heading_marks(markdown);
    let splitter = MarkdownSplitter::new(200..800);

    let mut mark_idx = 0usize;
    let mut current_heading: Option<String> = None;
    let mut chunks = Vec::new();

    for (chunk_index, (start, text)) in splitter.chunk_indices(markdown).enumerate() {
        let chunk_end = start + text.len();
        // Advance past every heading that appears at or before this
        // chunk's end (either earlier in the doc, or within this chunk's
        // own text) — chunks are yielded in document order, so a single
        // forward pass over `marks` suffices.
        while mark_idx < marks.len() && marks[mark_idx].0 < chunk_end {
            current_heading = Some(marks[mark_idx].1.clone());
            mark_idx += 1;
        }
        chunks.push(Chunk {
            text: text.to_string(),
            heading: current_heading
                .clone()
                .or_else(|| Some(page_title.to_string())),
            chunk_index: chunk_index as u32,
        });
    }

    chunks
}

// ---------------------------------------------------------------------
// Epic 3.3: Cosine similarity
// ---------------------------------------------------------------------

/// Cosine similarity between two vectors: dot product divided by the
/// product of both vectors' L2 norms. Returns `0.0` (rather than
/// panicking or producing `NaN`) if either vector's norm is `0.0`.
pub(crate) fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

// ---------------------------------------------------------------------
// Epic 3.4: SourceId, storage types, SourceLocks
// ---------------------------------------------------------------------

/// Turns `s` into a filesystem-safe, lowercase, hyphenated slug: lowercases,
/// collapses any run of non-alphanumeric characters into a single `-`, and
/// trims a leading/trailing `-`.
fn slugify(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut last_was_dash = true; // suppresses a leading '-'
    for c in s.chars() {
        if c.is_alphanumeric() {
            result.extend(c.to_lowercase());
            last_was_dash = false;
        } else if !last_was_dash {
            result.push('-');
            last_was_dash = true;
        }
    }
    if result.ends_with('-') {
        result.pop();
    }
    result
}

/// Filesystem-safe, slugified directory key for one indexed source.
/// Distinct from `source_name` (the human-facing wire field) to prevent the
/// two from being accidentally interchanged — see plan.md's Pattern
/// Decisions ("`SourceId` vs. `source_name`" row).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceId(String);

impl SourceId {
    pub(crate) fn from_name(name: &str) -> Self {
        SourceId(slugify(name))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Derives a default source name from a seed URL's host+path, used when the
/// caller doesn't supply an explicit `source_name`.
///
/// If the URL's first path segment duplicates the host's first label (e.g.
/// `tokio.rs/tokio/tutorial` — `"tokio"` appears in both the host and the
/// path), the base is just that shared label and the duplicate path
/// segment is dropped (`"tokio" + "tutorial"` -> `"tokio-tutorial"`).
/// Otherwise, there's nothing to dedupe, so the full host and path are
/// simply joined (`"doc.rust-lang.org" + "book"` ->
/// `"doc-rust-lang-org-book"`).
pub(crate) fn slugify_from_url(url: &url::Url) -> String {
    let host = url.host_str().unwrap_or("source");
    let host_first_label = host.split('.').next().unwrap_or(host);
    let segments: Vec<String> = url
        .path_segments()
        .map(|segs| segs.filter(|s| !s.is_empty()).map(str::to_string).collect())
        .unwrap_or_default();

    let (base, remaining): (&str, &[String]) = match segments.first() {
        Some(first) if slugify(first) == slugify(host_first_label) => {
            (host_first_label, &segments[1..])
        }
        _ => (host, segments.as_slice()),
    };

    let mut parts = vec![base.to_string()];
    parts.extend(remaining.iter().cloned());
    slugify(&parts.join("-"))
}

/// The JSONL-serializable persisted form of one chunk — one line per chunk
/// in `chunks.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChunkRecord {
    pub chunk_text: String,
    pub embedding: Vec<f32>,
    pub source_url: String,
    pub chunk_index: u32,
    pub content_hash: String,
    pub heading: Option<String>,
    pub page_title: String,
}

/// The `meta.json` sidecar for one indexed source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SourceMeta {
    pub source_id: String,
    pub source_name: String,
    pub seed_url: String,
    pub page_urls: Vec<String>,
    pub indexed_at_millis: u64,
    pub page_count: u32,
    pub chunk_count: u32,
    pub embedding_model: String,
}

/// The `sources.json` manifest entry shape returned by
/// `list_indexed_sources` — the same fields as `SourceMeta` minus
/// `page_urls`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SourceSummary {
    pub source_id: String,
    pub source_name: String,
    pub seed_url: String,
    pub indexed_at_millis: u64,
    pub page_count: u32,
    pub chunk_count: u32,
    pub embedding_model: String,
}

/// `{docs_index_dir}/<source_id>/`.
pub(crate) fn source_dir(docs_index_dir: &str, id: &SourceId) -> String {
    format!("{docs_index_dir}/{}", id.as_str())
}

/// `{docs_index_dir}/<source_id>/chunks.jsonl`.
pub(crate) fn chunks_path(docs_index_dir: &str, id: &SourceId) -> String {
    format!("{}/chunks.jsonl", source_dir(docs_index_dir, id))
}

/// `{docs_index_dir}/<source_id>/meta.json`.
pub(crate) fn meta_path(docs_index_dir: &str, id: &SourceId) -> String {
    format!("{}/meta.json", source_dir(docs_index_dir, id))
}

/// `{docs_index_dir}/sources.json` — the cross-source enumeration manifest.
pub(crate) fn sources_manifest_path(docs_index_dir: &str) -> String {
    format!("{docs_index_dir}/sources.json")
}

/// In-memory, per-`SourceId` operation guard preventing two mutating
/// operations (`index_docs`/`remove_indexed_source`) on the same source
/// from interleaving their `.await`-yielding writes. Deliberately does
/// *not* guard reads (`search_docs`) — see plan.md's Story 3.4.3
/// acceptance criteria for why that's safe given atomic `write_file`.
pub struct SourceLocks {
    active: RefCell<HashSet<SourceId>>,
}

/// RAII guard returned by [`SourceLocks::try_acquire`]; releases the lock
/// on every exit path (success, error, or early return) via `Drop`.
pub struct SourceLockGuard<'a> {
    locks: &'a SourceLocks,
    id: SourceId,
}

impl SourceLocks {
    pub fn new() -> Self {
        SourceLocks {
            active: RefCell::new(HashSet::new()),
        }
    }

    /// Returns `Some(guard)` and marks `id` as held if it wasn't already
    /// held; returns `None` without mutating any state otherwise.
    pub(crate) fn try_acquire(&self, id: &SourceId) -> Option<SourceLockGuard<'_>> {
        let inserted = self.active.borrow_mut().insert(id.clone());
        if inserted {
            Some(SourceLockGuard {
                locks: self,
                id: id.clone(),
            })
        } else {
            None
        }
    }
}

impl Drop for SourceLockGuard<'_> {
    fn drop(&mut self) {
        self.locks.active.borrow_mut().remove(&self.id);
    }
}

// ---------------------------------------------------------------------
// Epic 4.1: index_source
// ---------------------------------------------------------------------

/// Identifies the pinned embedding model. Stored in every `SourceMeta` and
/// checked by `search_docs` before ranking (a stored source embedded with a
/// different model can't be meaningfully compared against a freshly-embedded
/// query).
pub(crate) const EMBEDDING_MODEL_ID: &str = "all-MiniLM-L6-v2";

/// Hard cap on total chunks embedded/stored per `index_docs` call, bounding
/// worst-case inline-blocking duration on the single-threaded daemon.
/// `floor(measured_chunks_per_sec * 8)`, 8s being the accepted worst-case
/// inline-blocking budget for this explicit, rare, user-triggered tool call
/// (Task 2.1.2c; plan.md's Pattern Decisions "Concurrency / blocking
/// inference" row).
// measured ~98-100 chunks/sec on this dev machine, 2026-07-15
pub(crate) const MAX_CHUNKS_PER_SOURCE: usize = 780;

/// `index_source` calls `Embedder::embed` in sub-batches of at most this many
/// chunks, `tokio::task::yield_now().await`-ing between sub-batches, instead
/// of one giant call covering all of `MAX_CHUNKS_PER_SOURCE` — see Story
/// 4.1.3's design note for why. `MAX_CHUNKS_PER_SOURCE >= SUB_BATCH_SIZE`
/// (780 >= 100), so sub-batching always has at least one full batch to work
/// with.
pub(crate) const SUB_BATCH_SIZE: usize = 100;

/// One successfully-fetched, not-yet-chunked page collected during the crawl
/// phase of `index_source`.
struct FetchedPage {
    /// The URL actually served after any redirects (`HttpResponse::final_url`),
    /// not necessarily the URL requested — see Epic 6.1.
    final_url: String,
    html: String,
    content_hash: String,
}

fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Maps common HTTP status codes to their reason phrase for `seed_fetch_error`'s
/// message. Falls back to a generic phrase for anything not explicitly listed —
/// this is purely for a human-readable error message, not protocol logic.
fn http_status_reason(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        410 => "Gone",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Error",
    }
}

/// The seed-URL-fetch-failure error message from Story 4.1.1's acceptance
/// criteria: names the URL and status, and tells the caller what to check.
fn seed_fetch_error(url: &str, status: Option<u16>) -> String {
    match status {
        Some(code) => format!(
            "failed to index {url}: {code} {}. Check the URL is still correct.",
            http_status_reason(code)
        ),
        None => format!("failed to index {url}: request failed. Check the URL is still correct."),
    }
}

/// Appends `page_chunks` (one page's full `chunk_markdown` output) onto
/// `entries`, stopping the instant `entries.len()` would exceed
/// `MAX_CHUNKS_PER_SOURCE` — mid-page, not just mid-crawl (Task 4.1.2a).
/// Returns `true` if the cap was hit (either already before this call, or
/// partway through `page_chunks`).
fn append_chunks_with_cap(
    entries: &mut Vec<(Chunk, String, String, String)>,
    page_chunks: Vec<Chunk>,
    final_url: &str,
    content_hash: &str,
    page_title: &str,
) -> bool {
    for chunk in page_chunks {
        if entries.len() >= MAX_CHUNKS_PER_SOURCE {
            return true;
        }
        entries.push((
            chunk,
            final_url.to_string(),
            content_hash.to_string(),
            page_title.to_string(),
        ));
    }
    entries.len() >= MAX_CHUNKS_PER_SOURCE
}

/// Builds the string sent to `Embedder::embed` for one chunk: `"{page_title}
/// — {heading_or_page_title}\n\n{chunk.text}"` (Task 4.1.2b). The *stored*
/// `ChunkRecord.chunk_text` stays the raw `chunk.text` — this header is only
/// for embedding context, never persisted or returned to `search_docs`
/// callers.
fn embedding_input_for(page_title: &str, chunk: &Chunk) -> String {
    let heading = chunk.heading.as_deref().unwrap_or(page_title);
    format!("{page_title} — {heading}\n\n{}", chunk.text)
}

/// Calls `Embedder::embed` in `SUB_BATCH_SIZE`-sized sub-batches (in order),
/// yielding to the scheduler between sub-batches (not after the last one) so
/// a long `index_docs` call doesn't block every other in-flight daemon tool
/// call for its entire duration (Story 4.1.3's design note).
async fn embed_in_sub_batches<E: Embedder>(embedder: &E, inputs: &[String]) -> Result<Vec<Vec<f32>>, String> {
    let mut embeddings = Vec::with_capacity(inputs.len());
    let batches: Vec<&[String]> = inputs.chunks(SUB_BATCH_SIZE).collect();
    let batch_count = batches.len();
    for (i, batch) in batches.into_iter().enumerate() {
        let batch_embeddings = embedder.embed(batch).await.map_err(|e| e.to_string())?;
        embeddings.extend(batch_embeddings);
        if i + 1 < batch_count {
            tokio::task::yield_now().await;
        }
    }
    Ok(embeddings)
}

/// The full crawl -> dedup -> chunk -> embed -> persist pipeline for
/// `index_docs` (see plan.md Epic 4.1). Guarded per-`SourceId` by `locks` so
/// a concurrent `index_docs`/`remove_indexed_source` call on the same source
/// can't interleave on-disk writes (Story 3.4.3/4.1.1).
pub async fn index_source<H, F, E, C>(
    http: &H,
    fs: &F,
    embedder: &E,
    clock: &C,
    locks: &SourceLocks,
    docs_index_dir: &str,
    input: IndexDocsInput,
) -> Result<IndexDocsOutput, String>
where
    H: HttpClient,
    F: FileStore,
    E: Embedder,
    C: ClockPort,
{
    let call_start = std::time::Instant::now();
    if input.url.trim().is_empty() {
        return Err("url must not be empty".to_string());
    }
    let seed = Url::parse(&input.url).map_err(|e| format!("invalid url: {e}"))?;

    // Resolve source_name/SourceId and acquire the lock before any I/O
    // (Task 4.1.1a) — an in-flight operation on the same source must be
    // rejected without ever touching the network or the filesystem.
    let source_name = input.source.clone().unwrap_or_else(|| slugify_from_url(&seed));
    let id = SourceId::from_name(&source_name);
    if id.as_str().is_empty() {
        return Err(format!(
            "source name '{source_name}' has no alphanumeric characters and cannot be used to derive a storage location; pass a source name with at least one letter or digit"
        ));
    }

    let Some(_guard) = locks.try_acquire(&id) else {
        return Err(format!(
            "source '{source_name}' is already being indexed or removed; try again shortly"
        ));
    };

    let (max_depth, max_pages) = resolve_limits(input.max_depth, input.max_pages);
    let mut crawler = Crawler::new(http, seed.clone(), max_depth, max_pages).await;

    // Fetch the seed explicitly first (Task 4.1.1a): a seed-URL failure gets
    // a precise, URL+status-naming error, before entering the main crawl
    // loop's "skip one bad page" tolerance.
    let Some((seed_url, seed_depth)) = crawler.next_url(0) else {
        return Err(format!(
            "failed to index {}: seed URL is not fetchable (blocked by robots.txt).",
            input.url
        ));
    };
    let (seed_html, seed_final_url) = match crawler.fetch_and_expand(&seed_url, seed_depth).await {
        Ok(pair) => pair,
        Err(status) => return Err(seed_fetch_error(&input.url, status)),
    };

    // Main crawl loop + within-crawl content-hash dedup (Task 4.1.1b): two
    // URLs whose fetched HTML hashes identically contribute only the
    // first-visited URL's content. A deduped-away page still consumed a
    // fetch, so it still counts toward `max_pages` bookkeeping — only a
    // *failed* fetch (matching `read_website`'s existing precedent) doesn't.
    let mut seen_hashes: HashSet<String> = HashSet::new();
    let mut pages: Vec<FetchedPage> = Vec::with_capacity(max_pages as usize);
    let mut fetched_count = 1usize;

    let seed_hash = sha256_hex(&seed_html);
    seen_hashes.insert(seed_hash.clone());
    pages.push(FetchedPage {
        final_url: seed_final_url,
        html: seed_html,
        content_hash: seed_hash,
    });

    while let Some((url, depth)) = crawler.next_url(fetched_count) {
        match crawler.fetch_and_expand(&url, depth).await {
            Ok((html, final_url)) => {
                fetched_count += 1;
                let hash = sha256_hex(&html);
                if !seen_hashes.insert(hash.clone()) {
                    // Duplicate content within this crawl — skip, but the
                    // fetch already happened, so it still counts above.
                    continue;
                }
                pages.push(FetchedPage {
                    final_url,
                    html,
                    content_hash: hash,
                });
            }
            // Matches webcrawl.rs's existing "skip one bad page, don't fail
            // the whole crawl" precedent — and, matching `read_website`,
            // doesn't consume a `max_pages` slot for a failed fetch.
            Err(_) => continue,
        }
    }

    // Chunk collection with the per-chunk MAX_CHUNKS_PER_SOURCE cap (Task
    // 4.1.2a) and embedding-input header construction (Task 4.1.2b).
    let mut truncated = false;
    let mut chunk_entries: Vec<(Chunk, String, String, String)> = Vec::new();
    let mut page_urls: Vec<String> = Vec::with_capacity(pages.len());

    for page in &pages {
        let (title, markdown) = extract_title_and_markdown(&page.html, &page.final_url)
            .map_err(|e| format!("failed to extract content from {}: {e}", page.final_url))?;
        page_urls.push(page.final_url.clone());

        let page_chunks = chunk_markdown(&markdown, &title);
        let hit_cap = append_chunks_with_cap(
            &mut chunk_entries,
            page_chunks,
            &page.final_url,
            &page.content_hash,
            &title,
        );
        if hit_cap {
            truncated = true;
            break;
        }
    }

    let embed_inputs: Vec<String> = chunk_entries
        .iter()
        .map(|(chunk, _url, _hash, title)| embedding_input_for(title, chunk))
        .collect();

    // Sub-batched embed + assemble ChunkRecords (Task 4.1.3a).
    let embeddings = embed_in_sub_batches(embedder, &embed_inputs).await?;
    let chunk_records: Vec<ChunkRecord> = chunk_entries
        .into_iter()
        .zip(embeddings)
        .map(|((chunk, url, content_hash, page_title), embedding)| ChunkRecord {
            chunk_text: chunk.text,
            embedding,
            source_url: url,
            chunk_index: chunk.chunk_index,
            content_hash,
            heading: chunk.heading,
            page_title,
        })
        .collect();

    // Load old SourceMeta for removed-page diffing (Task 4.1.3b).
    let old_page_urls: Vec<String> = fs
        .read_file(&meta_path(docs_index_dir, &id))
        .await
        .map_err(|e| e.to_string())?
        .and_then(|bytes| serde_json::from_slice::<SourceMeta>(&bytes).ok())
        .map(|m| m.page_urls)
        .unwrap_or_default();
    let pages_removed: Vec<String> = old_page_urls
        .into_iter()
        .filter(|u| !page_urls.contains(u))
        .collect();

    // Write chunks.jsonl, meta.json, and update the sources.json manifest —
    // full replace, not append/merge (Task 4.1.3c).
    let jsonl = chunk_records
        .iter()
        .map(|r| serde_json::to_string(r).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");
    fs.write_file(&chunks_path(docs_index_dir, &id), jsonl.as_bytes())
        .await
        .map_err(|e| e.to_string())?;

    let pages_indexed = pages.len() as u32;
    let chunks_indexed = chunk_records.len() as u32;
    let indexed_at_millis = clock.now_millis();

    let meta = SourceMeta {
        source_id: id.as_str().to_string(),
        source_name: source_name.clone(),
        seed_url: input.url.clone(),
        page_urls,
        indexed_at_millis,
        page_count: pages_indexed,
        chunk_count: chunks_indexed,
        embedding_model: EMBEDDING_MODEL_ID.to_string(),
    };
    fs.write_file(&meta_path(docs_index_dir, &id), &serde_json::to_vec(&meta).map_err(|e| e.to_string())?)
        .await
        .map_err(|e| e.to_string())?;

    let manifest_path = sources_manifest_path(docs_index_dir);
    let mut manifest: Vec<SourceSummary> = fs
        .read_file(&manifest_path)
        .await
        .map_err(|e| e.to_string())?
        .and_then(|bytes| serde_json::from_slice::<Vec<SourceSummary>>(&bytes).ok())
        .unwrap_or_default();
    manifest.retain(|s| s.source_id != id.as_str());
    manifest.push(SourceSummary {
        source_id: id.as_str().to_string(),
        source_name: source_name.clone(),
        seed_url: input.url.clone(),
        indexed_at_millis,
        page_count: pages_indexed,
        chunk_count: chunks_indexed,
        embedding_model: EMBEDDING_MODEL_ID.to_string(),
    });
    fs.write_file(&manifest_path, &serde_json::to_vec(&manifest).map_err(|e| e.to_string())?)
        .await
        .map_err(|e| e.to_string())?;

    eprintln!(
        "index_docs: source '{source_name}' — {pages_indexed} pages indexed, {} pages removed, \
         {chunks_indexed} chunks indexed, {:?} elapsed",
        pages_removed.len(),
        call_start.elapsed()
    );

    // Assemble IndexDocsOutput, return (Task 4.1.3d).
    Ok(IndexDocsOutput {
        source_name,
        source_id: id.as_str().to_string(),
        pages_indexed,
        pages_removed,
        chunks_indexed,
        embedding_model: EMBEDDING_MODEL_ID.to_string(),
        truncated,
    })
}

// ---------------------------------------------------------------------
// Shared: unknown-source error (Epic 4.2 / Epic 4.3)
// ---------------------------------------------------------------------

/// Builds the "no indexed source named ..." error shared by `search_docs`
/// (Task 4.2.1a) and `remove_indexed_source` (Task 4.3.2a): names the
/// requested source and lists every currently-indexed `source_name` from
/// `sources.json`, so the caller can fix a typo without a separate
/// `list_indexed_sources` round trip.
pub(crate) async fn unknown_source_error<F: FileStore>(
    fs: &F,
    docs_index_dir: &str,
    requested: &str,
) -> String {
    let manifest: Vec<SourceSummary> = fs
        .read_file(&sources_manifest_path(docs_index_dir))
        .await
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<Vec<SourceSummary>>(&bytes).ok())
        .unwrap_or_default();
    let names = manifest
        .iter()
        .map(|s| s.source_name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "no indexed source named '{requested}'; currently indexed: {names}. Call \
         list_indexed_sources for details, or index_docs to add a new source."
    )
}

// ---------------------------------------------------------------------
// Epic 4.2: search_docs
// ---------------------------------------------------------------------

/// Loads a source's stored chunks, embeds the query, and ranks the chunks by
/// cosine similarity (see plan.md Epic 4.2). Malformed lines in
/// `chunks.jsonl` are skipped with a logged warning rather than failing the
/// whole search, matching `webcrawl.rs::fetch_and_expand`'s existing
/// skip-one-bad-item precedent.
pub async fn search_docs<F, E>(
    fs: &F,
    embedder: &E,
    docs_index_dir: &str,
    input: SearchDocsInput,
) -> Result<SearchDocsOutput, String>
where
    F: FileStore,
    E: Embedder,
{
    let id = SourceId::from_name(&input.source);

    let meta_bytes = fs
        .read_file(&meta_path(docs_index_dir, &id))
        .await
        .map_err(|e| e.to_string())?;
    let Some(meta_bytes) = meta_bytes else {
        return Err(unknown_source_error(fs, docs_index_dir, &input.source).await);
    };
    let meta: SourceMeta = serde_json::from_slice(&meta_bytes).map_err(|e| e.to_string())?;

    // Embedding-model guard (Task 4.2.1b) — must happen before reading
    // chunks.jsonl or calling embedder.embed, so a stale index fails fast
    // rather than returning meaningless scores.
    if meta.embedding_model != EMBEDDING_MODEL_ID {
        return Err(format!(
            "{} was indexed with embedding model '{}', but this daemon now uses '{}'. Re-run \
             index_docs to rebuild the index with the current model.",
            input.source, meta.embedding_model, EMBEDDING_MODEL_ID
        ));
    }

    // Read+parse chunks.jsonl line by line (Task 4.2.1c) — one malformed
    // line is skipped with a warning, not a hard error for the whole search.
    let chunks_bytes = fs
        .read_file(&chunks_path(docs_index_dir, &id))
        .await
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let chunks_text = String::from_utf8_lossy(&chunks_bytes);

    let mut records: Vec<ChunkRecord> = Vec::new();
    for (idx, line) in chunks_text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ChunkRecord>(line) {
            Ok(record) => records.push(record),
            Err(e) => {
                eprintln!(
                    "search_docs: skipping malformed chunk on {} line {}: {e}",
                    input.source,
                    idx + 1
                );
            }
        }
    }

    let query_embeddings = embedder
        .embed(&[input.query.clone()])
        .await
        .map_err(|e| e.to_string())?;
    let query_vec = query_embeddings
        .into_iter()
        .next()
        .ok_or_else(|| "embedder returned no vector for the query".to_string())?;

    let mut scored: Vec<(ChunkRecord, f32)> = records
        .into_iter()
        .map(|record| {
            let score = cosine_similarity(&query_vec, &record.embedding);
            (record, score)
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let limit = input.limit.unwrap_or(5) as usize;
    scored.truncate(limit);

    // Map to DocsSearchResult (Task 4.2.1d).
    let results = scored
        .into_iter()
        .map(|(record, score)| DocsSearchResult {
            text: record.chunk_text,
            score,
            source_url: record.source_url,
            heading: record.heading,
            source_title: record.page_title,
        })
        .collect();

    Ok(SearchDocsOutput { results })
}

// ---------------------------------------------------------------------
// Epic 4.3: list_indexed_sources / remove_indexed_source
// ---------------------------------------------------------------------

/// Enumerates every entry in `sources.json` (Story 4.3.1). A missing
/// manifest (nothing indexed yet) returns an empty list, not an error.
pub async fn list_indexed_sources<F: FileStore>(
    fs: &F,
    docs_index_dir: &str,
    _input: ListIndexedSourcesInput,
) -> Result<ListIndexedSourcesOutput, String> {
    let manifest_bytes = fs
        .read_file(&sources_manifest_path(docs_index_dir))
        .await
        .map_err(|e| e.to_string())?;
    let Some(manifest_bytes) = manifest_bytes else {
        return Ok(ListIndexedSourcesOutput { sources: vec![] });
    };
    let manifest: Vec<SourceSummary> =
        serde_json::from_slice(&manifest_bytes).map_err(|e| e.to_string())?;

    let sources = manifest
        .into_iter()
        .map(|s| IndexedSourceSummary {
            source_name: s.source_name,
            source_id: s.source_id,
            seed_url: s.seed_url,
            page_count: s.page_count,
            chunk_count: s.chunk_count,
            indexed_at_millis: s.indexed_at_millis,
            embedding_model: s.embedding_model,
        })
        .collect();

    Ok(ListIndexedSourcesOutput { sources })
}

/// Deletes a source's `chunks.jsonl`/`meta.json` and removes its entry from
/// `sources.json` (Story 4.3.2). Guarded by `locks` the same way
/// `index_source` is — resolving `SourceId` and acquiring the lock happen
/// before any `fs` I/O, so a concurrent `index_docs`/`remove_indexed_source`
/// call on the same source is rejected without touching disk.
pub async fn remove_indexed_source<F: FileStore>(
    fs: &F,
    locks: &SourceLocks,
    docs_index_dir: &str,
    input: RemoveIndexedSourceInput,
) -> Result<RemoveIndexedSourceOutput, String> {
    let id = SourceId::from_name(&input.source);
    if id.as_str().is_empty() {
        return Err(format!(
            "source name '{}' has no alphanumeric characters and cannot be used to derive a storage location; pass a source name with at least one letter or digit",
            input.source
        ));
    }

    let Some(_guard) = locks.try_acquire(&id) else {
        return Err(format!(
            "source '{}' is already being indexed or removed; try again shortly",
            input.source
        ));
    };

    let meta_bytes = fs
        .read_file(&meta_path(docs_index_dir, &id))
        .await
        .map_err(|e| e.to_string())?;
    if meta_bytes.is_none() {
        return Err(unknown_source_error(fs, docs_index_dir, &input.source).await);
    }

    fs.delete_file(&chunks_path(docs_index_dir, &id))
        .await
        .map_err(|e| e.to_string())?;
    fs.delete_file(&meta_path(docs_index_dir, &id))
        .await
        .map_err(|e| e.to_string())?;

    let manifest_path = sources_manifest_path(docs_index_dir);
    let mut manifest: Vec<SourceSummary> = fs
        .read_file(&manifest_path)
        .await
        .map_err(|e| e.to_string())?
        .and_then(|bytes| serde_json::from_slice::<Vec<SourceSummary>>(&bytes).ok())
        .unwrap_or_default();
    manifest.retain(|s| s.source_id != id.as_str());
    fs.write_file(
        &manifest_path,
        &serde_json::to_vec(&manifest).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;

    Ok(RemoveIndexedSourceOutput {
        removed: true,
        source_name: input.source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Epic 3.2: chunk_markdown ---

    #[test]
    fn should_produce_heading_aware_chunks_when_markdown_has_multiple_sections() {
        // Each section body is comfortably under the 800-char max chunk
        // size, but the *document* as a whole is well over it, so the
        // splitter must produce at least 2 chunks (one per section) rather
        // than merging everything into a single chunk.
        let markdown = "# Tokio Tutorial\n\n\
            ## Spawning\n\n\
            Use `tokio::spawn` to run an async task. This lets you run code \
            concurrently with the rest of your program, scheduled onto the \
            Tokio runtime's worker threads for execution. Spawned tasks are \
            executed independently of the task that spawned them, and may \
            run on a different thread. A spawned task can also itself spawn \
            new tasks. Tasks are the unit of execution managed by the \
            scheduler, and each spawned task is roughly analogous to an \
            OS thread, but managed entirely by the Tokio runtime instead of \
            the operating system.\n\n\
            ## Async I/O\n\n\
            Tokio provides async versions of standard library I/O types, \
            such as files, TCP sockets, and UDP sockets, letting your \
            program perform many I/O operations concurrently. Reading from \
            and writing to these resources returns a future that resolves \
            once the operation completes, rather than blocking the calling \
            thread, freeing that thread up to make progress on other tasks \
            while the I/O operation is in flight in the background.";

        let chunks = chunk_markdown(markdown, "Tokio Tutorial");

        assert!(
            chunks.len() >= 2,
            "expected at least 2 chunks, got {}",
            chunks.len()
        );
        assert!(chunks
            .iter()
            .any(|c| c.heading.as_deref() == Some("Spawning") && c.text.contains("tokio::spawn")));
        assert!(chunks
            .iter()
            .any(|c| c.heading.as_deref() == Some("Async I/O")));
    }

    #[test]
    fn should_return_empty_vec_when_markdown_is_empty() {
        assert_eq!(chunk_markdown("", "Empty Page"), Vec::<Chunk>::new());
    }

    // --- Epic 3.3: cosine_similarity ---

    #[test]
    fn should_score_identical_vectors_1_0_and_rank_near_over_orthogonal_over_opposite() {
        let identical = cosine_similarity(&[1.0, 0.0, 0.0], &[1.0, 0.0, 0.0]);
        assert!((identical - 1.0).abs() < 1e-6);

        let orthogonal_2d = cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]);
        assert!((orthogonal_2d - 0.0).abs() < 1e-6);

        let q = [1.0f32, 0.0];
        let near = [0.9f32, 0.1];
        let orthogonal = [0.0f32, 1.0];
        let opposite = [-1.0f32, 0.0];

        let mut scored = vec![
            ("near", cosine_similarity(&q, &near)),
            ("orthogonal", cosine_similarity(&q, &orthogonal)),
            ("opposite", cosine_similarity(&q, &opposite)),
        ];
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        assert_eq!(
            scored.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            vec!["near", "orthogonal", "opposite"]
        );
        assert!((scored[0].1 - 0.994).abs() < 1e-3);
        assert!((scored[1].1 - 0.0).abs() < 1e-6);
        assert!((scored[2].1 - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn should_return_0_0_when_either_vector_has_zero_norm() {
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[0.0, 0.0]), 0.0);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[0.0, 0.0]), 0.0);
    }

    // --- Epic 3.4.1: SourceId / slugify_from_url ---

    #[test]
    fn should_slugify_name_to_lowercase_hyphenated_id_when_from_name_called() {
        assert_eq!(
            SourceId::from_name("Tokio Tutorial!!").as_str(),
            "tokio-tutorial"
        );
    }

    #[test]
    fn should_dedupe_leading_host_stem_segment_when_deriving_slug_from_url() {
        let url = url::Url::parse("https://tokio.rs/tokio/tutorial").unwrap();
        assert_eq!(slugify_from_url(&url), "tokio-tutorial");

        let url = url::Url::parse("https://doc.rust-lang.org/book/").unwrap();
        assert_eq!(slugify_from_url(&url), "doc-rust-lang-org-book");
    }

    // --- Epic 3.4.2: storage types + path helpers ---

    #[test]
    fn should_round_trip_through_json_when_chunk_record_and_source_meta_serialized() {
        let record = ChunkRecord {
            chunk_text: "Use tokio::spawn...".to_string(),
            embedding: vec![0.1, 0.2, 0.3],
            source_url: "https://tokio.rs/tokio/tutorial/spawning".to_string(),
            chunk_index: 0,
            content_hash: "abc123".to_string(),
            heading: Some("Spawning".to_string()),
            page_title: "Tokio Tutorial".to_string(),
        };
        let json = serde_json::to_string(&record).unwrap();
        let round_tripped: ChunkRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped.chunk_text, record.chunk_text);
        assert_eq!(round_tripped.embedding, record.embedding);
        assert_eq!(round_tripped.source_url, record.source_url);
        assert_eq!(round_tripped.chunk_index, record.chunk_index);
        assert_eq!(round_tripped.content_hash, record.content_hash);
        assert_eq!(round_tripped.heading, record.heading);
        assert_eq!(round_tripped.page_title, record.page_title);

        let meta = SourceMeta {
            source_id: "tokio-tutorial".to_string(),
            source_name: "tokio-tutorial".to_string(),
            seed_url: "https://tokio.rs/tokio/tutorial".to_string(),
            page_urls: vec!["https://tokio.rs/tokio/tutorial".to_string()],
            indexed_at_millis: 1_700_000_000_000,
            page_count: 1,
            chunk_count: 3,
            embedding_model: "all-MiniLM-L6-v2".to_string(),
        };
        let json = serde_json::to_string(&meta).unwrap();
        let round_tripped: SourceMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped.source_id, meta.source_id);
        assert_eq!(round_tripped.source_name, meta.source_name);
        assert_eq!(round_tripped.seed_url, meta.seed_url);
        assert_eq!(round_tripped.page_urls, meta.page_urls);
        assert_eq!(round_tripped.indexed_at_millis, meta.indexed_at_millis);
        assert_eq!(round_tripped.page_count, meta.page_count);
        assert_eq!(round_tripped.chunk_count, meta.chunk_count);
        assert_eq!(round_tripped.embedding_model, meta.embedding_model);
    }

    #[test]
    fn should_build_expected_chunks_path_when_source_id_and_docs_index_dir_given() {
        let docs_index_dir = "/home/tstapler/.stapler-mcp/docs-index";
        let id = SourceId::from_name("tokio-tutorial");
        assert_eq!(
            chunks_path(docs_index_dir, &id),
            "/home/tstapler/.stapler-mcp/docs-index/tokio-tutorial/chunks.jsonl"
        );
    }

    // --- Epic 3.4.3: SourceLocks ---

    #[test]
    fn should_return_none_on_second_try_acquire_when_source_id_already_held() {
        let locks = SourceLocks::new();
        let id = SourceId::from_name("tokio-tutorial");
        let first = locks.try_acquire(&id);
        assert!(first.is_some());
        let second = locks.try_acquire(&id);
        assert!(second.is_none());
    }

    #[test]
    fn should_allow_reacquire_when_previous_guard_is_dropped() {
        let locks = SourceLocks::new();
        let id = SourceId::from_name("tokio-tutorial");
        {
            let guard = locks.try_acquire(&id);
            assert!(guard.is_some());
        }
        let reacquired = locks.try_acquire(&id);
        assert!(reacquired.is_some());
    }

    // --- Epic 4.1: index_source ---
    //
    // Test doubles: none of `FakeHttpClient`/`InMemoryFileStore`/`FakeEmbedder`/
    // `FakeClock` exist elsewhere in this codebase yet (docs-index is the first
    // feature to need them) — added here per plan.md's Test Stack note.

    use crate::ports::{HttpResponse, PortError};
    use std::collections::HashMap;

    const FILLER: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod \
        tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud \
        exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor \
        in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur.";

    struct FakeRoute {
        status: u16,
        body: Vec<u8>,
        final_url: String,
    }

    /// Fixed URL -> response map, matching Task 4.1.1c's `FakeHttpClient` ask.
    /// A URL with no configured route fails the request (matches `fetch_robots`'s
    /// existing "no robots.txt reachable -> treat as allow-all" tolerance for the
    /// `robots.txt` case, since it just swallows the error via `.ok()`).
    struct FakeHttpClient {
        routes: HashMap<String, FakeRoute>,
        calls: RefCell<Vec<String>>,
    }

    impl FakeHttpClient {
        fn new(routes: Vec<(String, FakeRoute)>) -> Self {
            FakeHttpClient {
                routes: routes.into_iter().collect(),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.borrow().len()
        }
    }

    impl HttpClient for FakeHttpClient {
        async fn get(&self, url: &str, _headers: &[(String, String)]) -> Result<HttpResponse, PortError> {
            self.calls.borrow_mut().push(url.to_string());
            match self.routes.get(url) {
                Some(route) => Ok(HttpResponse {
                    status: route.status,
                    body: route.body.clone(),
                    final_url: route.final_url.clone(),
                }),
                None => Err(PortError::Other(format!("no fake route for {url}"))),
            }
        }
    }

    /// Plain 200 route where the served URL is also the final (non-redirected)
    /// URL.
    fn route(url: &str, html: &str) -> (String, FakeRoute) {
        (
            url.to_string(),
            FakeRoute {
                status: 200,
                body: html.as_bytes().to_vec(),
                final_url: url.to_string(),
            },
        )
    }

    /// A route whose `final_url` differs from the requested URL, simulating a
    /// redirect (Epic 6.1) — `HttpResponse::final_url` is what `ChunkRecord`/
    /// `SourceMeta` should end up storing, not the requested URL.
    fn redirected_route(requested_url: &str, final_url: &str, html: &str) -> (String, FakeRoute) {
        (
            requested_url.to_string(),
            FakeRoute {
                status: 200,
                body: html.as_bytes().to_vec(),
                final_url: final_url.to_string(),
            },
        )
    }

    fn error_route(url: &str, status: u16) -> (String, FakeRoute) {
        (
            url.to_string(),
            FakeRoute {
                status,
                body: Vec::new(),
                final_url: url.to_string(),
            },
        )
    }

    /// Would fail the test if `.get` were called at all — used to prove the
    /// `SourceLocks`-rejection path never touches the network (Task 4.1.1c).
    struct PanicHttpClient;

    impl HttpClient for PanicHttpClient {
        async fn get(&self, url: &str, _headers: &[(String, String)]) -> Result<HttpResponse, PortError> {
            panic!("http.get should not have been called, but was called with {url}");
        }
    }

    struct InMemoryFileStore {
        files: RefCell<HashMap<String, Vec<u8>>>,
        write_calls: RefCell<Vec<String>>,
        read_calls: RefCell<Vec<String>>,
        delete_calls: RefCell<Vec<String>>,
    }

    impl InMemoryFileStore {
        fn new() -> Self {
            InMemoryFileStore {
                files: RefCell::new(HashMap::new()),
                write_calls: RefCell::new(Vec::new()),
                read_calls: RefCell::new(Vec::new()),
                delete_calls: RefCell::new(Vec::new()),
            }
        }

        fn seed(&self, path: &str, bytes: Vec<u8>) {
            self.files.borrow_mut().insert(path.to_string(), bytes);
        }

        fn get(&self, path: &str) -> Option<Vec<u8>> {
            self.files.borrow().get(path).cloned()
        }

        fn write_call_count(&self) -> usize {
            self.write_calls.borrow().len()
        }

        fn read_call_count(&self) -> usize {
            self.read_calls.borrow().len()
        }

        fn delete_call_count(&self) -> usize {
            self.delete_calls.borrow().len()
        }
    }

    impl FileStore for InMemoryFileStore {
        async fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), PortError> {
            self.write_calls.borrow_mut().push(path.to_string());
            self.files.borrow_mut().insert(path.to_string(), bytes.to_vec());
            Ok(())
        }

        async fn read_file(&self, path: &str) -> Result<Option<Vec<u8>>, PortError> {
            self.read_calls.borrow_mut().push(path.to_string());
            Ok(self.files.borrow().get(path).cloned())
        }

        async fn delete_file(&self, path: &str) -> Result<(), PortError> {
            self.delete_calls.borrow_mut().push(path.to_string());
            self.files.borrow_mut().remove(path);
            Ok(())
        }
    }

    /// Deterministic fixed-dimension embedder that also records each call's
    /// batch size, for asserting sub-batching call count/order (Task 4.1.3e).
    struct FakeEmbedder {
        batch_sizes: RefCell<Vec<usize>>,
    }

    impl FakeEmbedder {
        fn new() -> Self {
            FakeEmbedder {
                batch_sizes: RefCell::new(Vec::new()),
            }
        }

        fn batch_sizes(&self) -> Vec<usize> {
            self.batch_sizes.borrow().clone()
        }
    }

    impl Embedder for FakeEmbedder {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError> {
            self.batch_sizes.borrow_mut().push(texts.len());
            Ok(texts.iter().map(|_| vec![0.0f32; 4]).collect())
        }
    }

    /// Like `FakeEmbedder`, but the embedding it returns encodes the numeric
    /// suffix of the input text (e.g. `"chunk-42"` -> `vec![42.0]`), so a test
    /// can prove chunk order is preserved across sub-batch boundaries rather
    /// than just checking the final count.
    struct IdentityEmbedder {
        batch_sizes: RefCell<Vec<usize>>,
    }

    impl IdentityEmbedder {
        fn new() -> Self {
            IdentityEmbedder {
                batch_sizes: RefCell::new(Vec::new()),
            }
        }

        fn batch_sizes(&self) -> Vec<usize> {
            self.batch_sizes.borrow().clone()
        }
    }

    impl Embedder for IdentityEmbedder {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError> {
            self.batch_sizes.borrow_mut().push(texts.len());
            Ok(texts
                .iter()
                .map(|t| {
                    let n: f32 = t.rsplit('-').next().and_then(|s| s.parse().ok()).unwrap_or(-1.0);
                    vec![n]
                })
                .collect())
        }
    }

    /// Returns the same fixed vector for every input text — used by
    /// `search_docs` tests where only the query's embedding matters (the
    /// per-chunk vectors come straight from the seeded `ChunkRecord`s, not
    /// from a real embed call).
    struct FixedVectorEmbedder {
        vector: Vec<f32>,
    }

    impl FixedVectorEmbedder {
        fn new(vector: Vec<f32>) -> Self {
            FixedVectorEmbedder { vector }
        }
    }

    impl Embedder for FixedVectorEmbedder {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError> {
            Ok(texts.iter().map(|_| self.vector.clone()).collect())
        }
    }

    /// Would fail the test if `.embed` were called at all — used to prove
    /// `search_docs`'s embedding-model-mismatch guard returns before
    /// attempting to compute any similarity scores (Task 4.2.1b).
    struct PanicEmbedder;

    impl Embedder for PanicEmbedder {
        async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, PortError> {
            panic!("embedder.embed should not have been called for a model-mismatch source");
        }
    }

    struct FakeClock(u64);

    impl ClockPort for FakeClock {
        fn now_millis(&self) -> u64 {
            self.0
        }
    }

    fn base_input(url: &str) -> IndexDocsInput {
        IndexDocsInput {
            url: url.to_string(),
            source: None,
            max_depth: Some(1),
            max_pages: Some(10),
        }
    }

    // --- Story 4.1.1 ---

    #[tokio::test]
    async fn should_reject_index_source_without_any_file_io_when_source_already_locked() {
        let locks = SourceLocks::new();
        let held_id = SourceId::from_name("tokio-tutorial");
        let _held = locks.try_acquire(&held_id).expect("first acquire should succeed");

        let http = PanicHttpClient;
        let fs = InMemoryFileStore::new();
        let embedder = FakeEmbedder::new();
        let clock = FakeClock(1_700_000_000_000);

        let mut input = base_input("https://tokio.rs/tokio/tutorial");
        input.source = Some("tokio-tutorial".to_string());

        let result = index_source(&http, &fs, &embedder, &clock, &locks, "/fake/docs-index", input).await;

        match result {
            Err(e) => assert_eq!(
                e,
                "source 'tokio-tutorial' is already being indexed or removed; try again shortly"
            ),
            Ok(_) => panic!("expected an Err when the source is already locked"),
        }
        assert_eq!(fs.write_call_count(), 0);
    }

    #[tokio::test]
    async fn should_reject_index_source_without_any_file_io_when_source_slugifies_to_empty() {
        let locks = SourceLocks::new();
        let http = PanicHttpClient;
        let fs = InMemoryFileStore::new();
        let embedder = FakeEmbedder::new();
        let clock = FakeClock(1_700_000_000_000);

        let mut input = base_input("https://tokio.rs/tokio/tutorial");
        input.source = Some("...".to_string());

        let result = index_source(&http, &fs, &embedder, &clock, &locks, "/fake/docs-index", input).await;

        match result {
            Err(e) => assert!(
                e.contains("no alphanumeric characters"),
                "expected an alphanumeric-required error, got: {e}"
            ),
            Ok(_) => panic!("expected an Err for a source name with no alphanumeric characters"),
        }
        assert_eq!(fs.write_call_count(), 0);
    }

    #[tokio::test]
    async fn should_return_fetch_error_when_seed_url_404s() {
        let url = "https://example.com/docs-that-dont-exist";
        let http = FakeHttpClient::new(vec![error_route(url, 404)]);
        let fs = InMemoryFileStore::new();
        let embedder = FakeEmbedder::new();
        let clock = FakeClock(1_700_000_000_000);
        let locks = SourceLocks::new();

        let result = index_source(
            &http,
            &fs,
            &embedder,
            &clock,
            &locks,
            "/fake/docs-index",
            base_input(url),
        )
        .await;

        match result {
            Err(e) => assert_eq!(e, format!("failed to index {url}: 404 Not Found. Check the URL is still correct.")),
            Ok(_) => panic!("expected an Err for a 404 seed URL"),
        }
        assert_eq!(fs.write_call_count(), 0);

        let id = SourceId::from_name(&slugify_from_url(&url::Url::parse(url).unwrap()));
        assert!(fs.get(&meta_path("/fake/docs-index", &id)).is_none());
        assert!(fs.get(&chunks_path("/fake/docs-index", &id)).is_none());
    }

    #[tokio::test]
    async fn should_dedup_identical_content_when_crawl_discovers_duplicate_urls() {
        let seed_html = format!(
            "<html><head><title>Docs</title></head><body><p>{FILLER}</p>\
             <a href=\"https://example.com/docs/page\">Page</a>\
             <a href=\"https://example.com/docs/page/\">Page Slash</a></body></html>"
        );
        let dup_html = format!("<html><head><title>Page</title></head><body><p>{FILLER}</p></body></html>");

        let http = FakeHttpClient::new(vec![
            route("https://example.com/docs", &seed_html),
            route("https://example.com/docs/page", &dup_html),
            route("https://example.com/docs/page/", &dup_html),
        ]);
        let fs = InMemoryFileStore::new();
        let embedder = FakeEmbedder::new();
        let clock = FakeClock(1_700_000_000_000);
        let locks = SourceLocks::new();

        let mut input = base_input("https://example.com/docs");
        input.max_pages = Some(10);

        let output = index_source(&http, &fs, &embedder, &clock, &locks, "/fake/docs-index", input)
            .await
            .expect("index_source should succeed");

        // Seed + first-visited duplicate only — the second duplicate URL's
        // identical content is skipped.
        assert_eq!(output.pages_indexed, 2);
    }

    // --- Story 4.1.2 ---

    #[test]
    fn should_stop_appending_mid_page_and_report_truncated_when_single_page_exceeds_cap() {
        let mut entries = Vec::new();
        let page_chunks: Vec<Chunk> = (0..(MAX_CHUNKS_PER_SOURCE + 500))
            .map(|i| Chunk {
                text: format!("chunk {i}"),
                heading: None,
                chunk_index: i as u32,
            })
            .collect();

        let hit_cap = append_chunks_with_cap(&mut entries, page_chunks, "https://example.com/big", "hash1", "Big Page");

        assert!(hit_cap);
        assert_eq!(entries.len(), MAX_CHUNKS_PER_SOURCE);
    }

    #[test]
    fn should_report_truncated_when_cap_is_reached_partway_through_a_later_page() {
        let mut entries = Vec::new();

        let first_page_size = MAX_CHUNKS_PER_SOURCE - 3;
        let first_page_chunks: Vec<Chunk> = (0..first_page_size)
            .map(|i| Chunk {
                text: format!("chunk {i}"),
                heading: None,
                chunk_index: i as u32,
            })
            .collect();
        let hit_cap_1 = append_chunks_with_cap(&mut entries, first_page_chunks, "https://example.com/p1", "hash1", "Page 1");
        assert!(!hit_cap_1);
        assert_eq!(entries.len(), first_page_size);

        let second_page_chunks: Vec<Chunk> = (0..50)
            .map(|i| Chunk {
                text: format!("more {i}"),
                heading: None,
                chunk_index: i as u32,
            })
            .collect();
        let hit_cap_2 = append_chunks_with_cap(&mut entries, second_page_chunks, "https://example.com/p2", "hash2", "Page 2");

        assert!(hit_cap_2);
        assert_eq!(entries.len(), MAX_CHUNKS_PER_SOURCE);
    }

    #[test]
    fn should_build_embedding_input_with_title_and_heading_header_while_keeping_raw_chunk_text() {
        let chunk = Chunk {
            text: "Use tokio::spawn to run...".to_string(),
            heading: Some("Spawning".to_string()),
            chunk_index: 0,
        };

        let input = embedding_input_for("Tokio Tutorial", &chunk);

        assert_eq!(input, "Tokio Tutorial — Spawning\n\nUse tokio::spawn to run...");
        // The chunk's own stored text is untouched by header construction.
        assert_eq!(chunk.text, "Use tokio::spawn to run...");
    }

    #[test]
    fn should_fall_back_to_page_title_in_embedding_input_when_chunk_has_no_heading() {
        let chunk = Chunk {
            text: "Intro text before any heading.".to_string(),
            heading: None,
            chunk_index: 0,
        };

        let input = embedding_input_for("Tokio Tutorial", &chunk);

        assert_eq!(input, "Tokio Tutorial — Tokio Tutorial\n\nIntro text before any heading.");
    }

    // --- Story 4.1.3 ---

    #[tokio::test]
    async fn should_embed_in_sub_batches_of_100_and_yield_between_batches_when_340_inputs_given() {
        let embedder = IdentityEmbedder::new();
        let inputs: Vec<String> = (0..340).map(|i| format!("chunk-{i}")).collect();

        let result = embed_in_sub_batches(&embedder, &inputs).await.expect("embed should succeed");

        assert_eq!(embedder.batch_sizes(), vec![100, 100, 100, 40]);
        let expected: Vec<Vec<f32>> = (0..340).map(|i| vec![i as f32]).collect();
        assert_eq!(result, expected, "chunk order must survive sub-batch boundaries");
    }

    #[tokio::test]
    async fn should_report_removed_pages_when_reindex_no_longer_discovers_a_previously_indexed_page() {
        let docs_index_dir = "/fake/docs-index";
        let id = SourceId::from_name("tokio-tutorial");

        let old_meta = SourceMeta {
            source_id: id.as_str().to_string(),
            source_name: "tokio-tutorial".to_string(),
            seed_url: "https://tokio.rs/tokio/tutorial".to_string(),
            page_urls: vec![
                "https://tokio.rs/tokio/tutorial".to_string(),
                "https://tokio.rs/tokio/tutorial/old-page".to_string(),
            ],
            indexed_at_millis: 1_600_000_000_000,
            page_count: 2,
            chunk_count: 10,
            embedding_model: EMBEDDING_MODEL_ID.to_string(),
        };
        let fs = InMemoryFileStore::new();
        fs.seed(&meta_path(docs_index_dir, &id), serde_json::to_vec(&old_meta).unwrap());

        let seed_html = format!("<html><head><title>Tutorial</title></head><body><p>{FILLER}</p></body></html>");
        let http = FakeHttpClient::new(vec![route("https://tokio.rs/tokio/tutorial", &seed_html)]);
        let embedder = FakeEmbedder::new();
        let clock = FakeClock(1_700_000_000_000);
        let locks = SourceLocks::new();

        let mut input = base_input("https://tokio.rs/tokio/tutorial");
        input.source = Some("tokio-tutorial".to_string());

        let output = index_source(&http, &fs, &embedder, &clock, &locks, docs_index_dir, input)
            .await
            .expect("index_source should succeed");

        assert_eq!(
            output.pages_removed,
            vec!["https://tokio.rs/tokio/tutorial/old-page".to_string()]
        );
    }

    #[tokio::test]
    async fn should_fully_replace_chunks_jsonl_not_append_when_reindexing() {
        let docs_index_dir = "/fake/docs-index";
        let id = SourceId::from_name("tokio-tutorial");

        let fs = InMemoryFileStore::new();
        let old_lines: Vec<String> = (0..200).map(|i| format!("{{\"old_line\":{i}}}")).collect();
        fs.seed(&chunks_path(docs_index_dir, &id), old_lines.join("\n").into_bytes());

        let seed_html = format!("<html><head><title>Tutorial</title></head><body><p>{FILLER}</p></body></html>");
        let http = FakeHttpClient::new(vec![route("https://tokio.rs/tokio/tutorial", &seed_html)]);
        let embedder = FakeEmbedder::new();
        let clock = FakeClock(1_700_000_000_000);
        let locks = SourceLocks::new();

        let mut input = base_input("https://tokio.rs/tokio/tutorial");
        input.source = Some("tokio-tutorial".to_string());

        let output = index_source(&http, &fs, &embedder, &clock, &locks, docs_index_dir, input)
            .await
            .expect("index_source should succeed");

        let contents = fs.get(&chunks_path(docs_index_dir, &id)).expect("chunks.jsonl should exist");
        let contents = String::from_utf8(contents).unwrap();
        let line_count = if contents.is_empty() { 0 } else { contents.lines().count() };

        assert_eq!(line_count, output.chunks_indexed as usize);
        assert!(!contents.contains("old_line"), "old chunks.jsonl content must not survive a re-index");
    }

    #[tokio::test]
    async fn should_set_chunk_record_source_url_to_final_url_when_page_reached_via_redirect() {
        let docs_index_dir = "/fake/docs-index";
        let requested = "https://example.com/old-page";
        let final_url = "https://example.com/new-page";
        let html = format!("<html><head><title>New Page</title></head><body><p>{FILLER}</p></body></html>");

        let http = FakeHttpClient::new(vec![redirected_route(requested, final_url, &html)]);
        let fs = InMemoryFileStore::new();
        let embedder = FakeEmbedder::new();
        let clock = FakeClock(1_700_000_000_000);
        let locks = SourceLocks::new();

        let output = index_source(
            &http,
            &fs,
            &embedder,
            &clock,
            &locks,
            docs_index_dir,
            base_input(requested),
        )
        .await
        .expect("index_source should succeed");

        assert!(output.chunks_indexed > 0);
        let id = SourceId::from_name(&output.source_name);
        let contents = fs.get(&chunks_path(docs_index_dir, &id)).unwrap();
        let contents = String::from_utf8(contents).unwrap();
        for line in contents.lines() {
            let record: ChunkRecord = serde_json::from_str(line).unwrap();
            assert_eq!(record.source_url, final_url);
        }

        let meta_bytes = fs.get(&meta_path(docs_index_dir, &id)).unwrap();
        let meta: SourceMeta = serde_json::from_slice(&meta_bytes).unwrap();
        assert_eq!(meta.page_urls, vec![final_url.to_string()]);
        // 2, not 1: `Crawler::new` also probes `robots.txt` (no route
        // configured for it here, so it's treated as allow-all) before the
        // one real page fetch — proving the seed is fetched exactly once,
        // not twice, despite `index_source` handling it explicitly before
        // the main crawl loop.
        assert_eq!(http.call_count(), 2);
    }

    // --- Epic 4.2: search_docs ---

    fn seeded_summary(source_id: &str, source_name: &str, seed_url: &str, page_count: u32, chunk_count: u32) -> SourceSummary {
        SourceSummary {
            source_id: source_id.to_string(),
            source_name: source_name.to_string(),
            seed_url: seed_url.to_string(),
            indexed_at_millis: 1_700_000_000_000,
            page_count,
            chunk_count,
            embedding_model: EMBEDDING_MODEL_ID.to_string(),
        }
    }

    #[tokio::test]
    async fn should_return_error_listing_indexed_sources_when_source_name_unknown() {
        let docs_index_dir = "/fake/docs-index";
        let fs = InMemoryFileStore::new();
        let manifest = vec![
            seeded_summary("tokio-tutorial", "tokio-tutorial", "https://tokio.rs/tokio/tutorial", 1, 3),
            seeded_summary("serde-guide", "serde-guide", "https://serde.rs/", 1, 3),
        ];
        fs.seed(&sources_manifest_path(docs_index_dir), serde_json::to_vec(&manifest).unwrap());

        let embedder = PanicEmbedder;
        let input = SearchDocsInput {
            source: "tokio".to_string(),
            query: "spawn".to_string(),
            limit: None,
        };

        let result = search_docs(&fs, &embedder, docs_index_dir, input).await;

        match result {
            Err(e) => assert_eq!(
                e,
                "no indexed source named 'tokio'; currently indexed: tokio-tutorial, serde-guide. Call \
                 list_indexed_sources for details, or index_docs to add a new source."
            ),
            Ok(_) => panic!("expected an Err for an unknown source name"),
        }
    }

    #[tokio::test]
    async fn should_return_model_mismatch_error_without_scoring_when_stored_embedding_model_differs_from_current() {
        let docs_index_dir = "/fake/docs-index";
        let id = SourceId::from_name("tokio-tutorial");
        let fs = InMemoryFileStore::new();

        let meta = SourceMeta {
            source_id: id.as_str().to_string(),
            source_name: "tokio-tutorial".to_string(),
            seed_url: "https://tokio.rs/tokio/tutorial".to_string(),
            page_urls: vec!["https://tokio.rs/tokio/tutorial".to_string()],
            indexed_at_millis: 1_700_000_000_000,
            page_count: 1,
            chunk_count: 1,
            embedding_model: "some-older-model-v1".to_string(),
        };
        fs.seed(&meta_path(docs_index_dir, &id), serde_json::to_vec(&meta).unwrap());
        // Deliberately garbage — proves the mismatch guard returns before
        // chunks.jsonl is ever read/parsed.
        fs.seed(&chunks_path(docs_index_dir, &id), b"not valid jsonl at all".to_vec());

        let embedder = PanicEmbedder;
        let input = SearchDocsInput {
            source: "tokio-tutorial".to_string(),
            query: "spawn".to_string(),
            limit: None,
        };

        let result = search_docs(&fs, &embedder, docs_index_dir, input).await;

        match result {
            Err(e) => assert_eq!(
                e,
                "tokio-tutorial was indexed with embedding model 'some-older-model-v1', but this \
                 daemon now uses 'all-MiniLM-L6-v2'. Re-run index_docs to rebuild the index with \
                 the current model."
            ),
            Ok(_) => panic!("expected an Err for an embedding-model mismatch"),
        }
    }

    #[tokio::test]
    async fn should_return_results_sorted_descending_by_score_capped_at_limit_when_search_docs_called() {
        let docs_index_dir = "/fake/docs-index";
        let id = SourceId::from_name("tokio-tutorial");
        let fs = InMemoryFileStore::new();

        let meta = SourceMeta {
            source_id: id.as_str().to_string(),
            source_name: "tokio-tutorial".to_string(),
            seed_url: "https://tokio.rs/tokio/tutorial".to_string(),
            page_urls: vec!["https://tokio.rs/tokio/tutorial".to_string()],
            indexed_at_millis: 1_700_000_000_000,
            page_count: 1,
            chunk_count: 4,
            embedding_model: EMBEDDING_MODEL_ID.to_string(),
        };
        fs.seed(&meta_path(docs_index_dir, &id), serde_json::to_vec(&meta).unwrap());

        fn record(text: &str, embedding: Vec<f32>) -> ChunkRecord {
            ChunkRecord {
                chunk_text: text.to_string(),
                embedding,
                source_url: "https://tokio.rs/tokio/tutorial".to_string(),
                chunk_index: 0,
                content_hash: "hash".to_string(),
                heading: Some("Spawning".to_string()),
                page_title: "Tokio Tutorial".to_string(),
            }
        }

        // Reuses the near/orthogonal/opposite vectors from Story 3.3.1's
        // cosine_similarity test, plus a second "near" candidate, so the
        // expected top-2 ranking is unambiguous.
        let records = vec![
            record("near", vec![0.9, 0.1]),
            record("orthogonal", vec![0.0, 1.0]),
            record("opposite", vec![-1.0, 0.0]),
            record("near2", vec![0.8, 0.2]),
        ];
        let jsonl = records.iter().map(|r| serde_json::to_string(r).unwrap()).collect::<Vec<_>>().join("\n");
        fs.seed(&chunks_path(docs_index_dir, &id), jsonl.into_bytes());

        let embedder = FixedVectorEmbedder::new(vec![1.0, 0.0]);
        let input = SearchDocsInput {
            source: "tokio-tutorial".to_string(),
            query: "how do I spawn a task".to_string(),
            limit: Some(2),
        };

        let output = search_docs(&fs, &embedder, docs_index_dir, input)
            .await
            .expect("search_docs should succeed");

        assert_eq!(output.results.len(), 2);
        assert!(output.results[0].score >= output.results[1].score);
        assert_eq!(output.results[0].text, "near");
        assert_eq!(output.results[1].text, "near2");
    }

    #[tokio::test]
    async fn should_skip_malformed_chunk_line_and_log_warning_when_jsonl_has_one_corrupted_line() {
        let docs_index_dir = "/fake/docs-index";
        let id = SourceId::from_name("tokio-tutorial");
        let fs = InMemoryFileStore::new();

        let meta = SourceMeta {
            source_id: id.as_str().to_string(),
            source_name: "tokio-tutorial".to_string(),
            seed_url: "https://tokio.rs/tokio/tutorial".to_string(),
            page_urls: vec!["https://tokio.rs/tokio/tutorial".to_string()],
            indexed_at_millis: 1_700_000_000_000,
            page_count: 1,
            chunk_count: 11,
            embedding_model: EMBEDDING_MODEL_ID.to_string(),
        };
        fs.seed(&meta_path(docs_index_dir, &id), serde_json::to_vec(&meta).unwrap());

        let valid_records: Vec<ChunkRecord> = (0..10)
            .map(|i| ChunkRecord {
                chunk_text: format!("chunk {i}"),
                embedding: vec![1.0, 0.0],
                source_url: "https://tokio.rs/tokio/tutorial".to_string(),
                chunk_index: i,
                content_hash: "hash".to_string(),
                heading: None,
                page_title: "Tokio Tutorial".to_string(),
            })
            .collect();
        let mut lines: Vec<String> = valid_records.iter().map(|r| serde_json::to_string(r).unwrap()).collect();
        // Hand-crafted, truncated JSON — never produced by the normal
        // ChunkRecord-serialization path.
        lines.push("{\"chunk_text\": \"incomplete...".to_string());
        fs.seed(&chunks_path(docs_index_dir, &id), lines.join("\n").into_bytes());

        let embedder = FixedVectorEmbedder::new(vec![1.0, 0.0]);
        let input = SearchDocsInput {
            source: "tokio-tutorial".to_string(),
            query: "spawn".to_string(),
            limit: Some(20),
        };

        let output = search_docs(&fs, &embedder, docs_index_dir, input)
            .await
            .expect("search_docs should succeed despite one malformed line");

        assert_eq!(output.results.len(), 10);
    }

    // --- Epic 4.3: list_indexed_sources / remove_indexed_source ---

    #[tokio::test]
    async fn should_return_empty_list_when_no_sources_manifest_exists() {
        let fs = InMemoryFileStore::new();

        let output = list_indexed_sources(&fs, "/fake/docs-index", ListIndexedSourcesInput {})
            .await
            .expect("list_indexed_sources should succeed with no manifest");

        assert!(output.sources.is_empty());
    }

    #[tokio::test]
    async fn should_return_all_entries_with_correct_counts_when_sources_manifest_populated() {
        let docs_index_dir = "/fake/docs-index";
        let fs = InMemoryFileStore::new();
        let manifest = vec![
            seeded_summary("tokio-tutorial", "tokio-tutorial", "https://tokio.rs/tokio/tutorial", 3, 42),
            seeded_summary("serde-guide", "serde-guide", "https://serde.rs/", 5, 77),
        ];
        fs.seed(&sources_manifest_path(docs_index_dir), serde_json::to_vec(&manifest).unwrap());

        let output = list_indexed_sources(&fs, docs_index_dir, ListIndexedSourcesInput {})
            .await
            .expect("list_indexed_sources should succeed");

        assert_eq!(output.sources.len(), 2);
        let tokio = output.sources.iter().find(|s| s.source_name == "tokio-tutorial").expect("tokio-tutorial present");
        assert_eq!(tokio.page_count, 3);
        assert_eq!(tokio.chunk_count, 42);
        assert_eq!(tokio.indexed_at_millis, 1_700_000_000_000);
        assert_eq!(tokio.embedding_model, EMBEDDING_MODEL_ID);
        let serde_source = output.sources.iter().find(|s| s.source_name == "serde-guide").expect("serde-guide present");
        assert_eq!(serde_source.page_count, 5);
        assert_eq!(serde_source.chunk_count, 77);
    }

    #[tokio::test]
    async fn should_delete_chunks_and_meta_and_remove_manifest_entry_when_source_exists() {
        let docs_index_dir = "/fake/docs-index";
        let id = SourceId::from_name("tokio-tutorial");
        let other_id = SourceId::from_name("serde-guide");
        let fs = InMemoryFileStore::new();

        let meta = SourceMeta {
            source_id: id.as_str().to_string(),
            source_name: "tokio-tutorial".to_string(),
            seed_url: "https://tokio.rs/tokio/tutorial".to_string(),
            page_urls: vec!["https://tokio.rs/tokio/tutorial".to_string()],
            indexed_at_millis: 1_700_000_000_000,
            page_count: 1,
            chunk_count: 3,
            embedding_model: EMBEDDING_MODEL_ID.to_string(),
        };
        fs.seed(&meta_path(docs_index_dir, &id), serde_json::to_vec(&meta).unwrap());
        fs.seed(&chunks_path(docs_index_dir, &id), b"{}".to_vec());

        let manifest = vec![
            seeded_summary(id.as_str(), "tokio-tutorial", "https://tokio.rs/tokio/tutorial", 1, 3),
            seeded_summary(other_id.as_str(), "serde-guide", "https://serde.rs/", 1, 2),
        ];
        fs.seed(&sources_manifest_path(docs_index_dir), serde_json::to_vec(&manifest).unwrap());

        let locks = SourceLocks::new();
        let input = RemoveIndexedSourceInput {
            source: "tokio-tutorial".to_string(),
        };

        let output = remove_indexed_source(&fs, &locks, docs_index_dir, input)
            .await
            .expect("remove_indexed_source should succeed");

        assert!(output.removed);
        assert_eq!(output.source_name, "tokio-tutorial");
        assert!(fs.get(&meta_path(docs_index_dir, &id)).is_none());
        assert!(fs.get(&chunks_path(docs_index_dir, &id)).is_none());

        let manifest_bytes = fs.get(&sources_manifest_path(docs_index_dir)).expect("sources.json should still exist");
        let manifest: Vec<SourceSummary> = serde_json::from_slice(&manifest_bytes).unwrap();
        assert_eq!(manifest.len(), 1);
        assert_eq!(manifest[0].source_id, other_id.as_str());
    }

    #[tokio::test]
    async fn should_return_error_listing_indexed_sources_when_removing_unknown_source_name() {
        let docs_index_dir = "/fake/docs-index";
        let fs = InMemoryFileStore::new();
        let manifest = vec![seeded_summary("tokio-tutorial", "tokio-tutorial", "https://tokio.rs/tokio/tutorial", 1, 3)];
        fs.seed(&sources_manifest_path(docs_index_dir), serde_json::to_vec(&manifest).unwrap());

        let locks = SourceLocks::new();
        let input = RemoveIndexedSourceInput {
            source: "nonexistent".to_string(),
        };

        let result = remove_indexed_source(&fs, &locks, docs_index_dir, input).await;

        match result {
            Err(e) => assert_eq!(
                e,
                "no indexed source named 'nonexistent'; currently indexed: tokio-tutorial. Call \
                 list_indexed_sources for details, or index_docs to add a new source."
            ),
            Ok(_) => panic!("expected an Err for removing an unknown source name"),
        }
    }

    #[tokio::test]
    async fn should_reject_remove_indexed_source_without_any_file_io_when_source_already_locked() {
        let locks = SourceLocks::new();
        let held_id = SourceId::from_name("tokio-tutorial");
        let _held = locks.try_acquire(&held_id).expect("first acquire should succeed");

        let fs = InMemoryFileStore::new();
        let input = RemoveIndexedSourceInput {
            source: "tokio-tutorial".to_string(),
        };

        let result = remove_indexed_source(&fs, &locks, "/fake/docs-index", input).await;

        match result {
            Err(e) => assert_eq!(e, "source 'tokio-tutorial' is already being indexed or removed; try again shortly"),
            Ok(_) => panic!("expected an Err when the source is already locked"),
        }
        assert_eq!(fs.read_call_count(), 0);
        assert_eq!(fs.delete_call_count(), 0);
    }

    #[tokio::test]
    async fn should_reject_remove_indexed_source_without_any_file_io_when_source_slugifies_to_empty() {
        let locks = SourceLocks::new();
        let fs = InMemoryFileStore::new();
        let input = RemoveIndexedSourceInput {
            source: "...".to_string(),
        };

        let result = remove_indexed_source(&fs, &locks, "/fake/docs-index", input).await;

        match result {
            Err(e) => assert!(
                e.contains("no alphanumeric characters"),
                "expected an alphanumeric-required error, got: {e}"
            ),
            Ok(_) => panic!("expected an Err for a source name with no alphanumeric characters"),
        }
        assert_eq!(fs.read_call_count(), 0);
        assert_eq!(fs.delete_call_count(), 0);
    }
}
