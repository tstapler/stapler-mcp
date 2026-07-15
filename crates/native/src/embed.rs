//! `Embedder` port adapter backed by `fastembed`'s local ONNX inference
//! (`sentence-transformers/all-MiniLM-L6-v2`, 384-dim). See docs-index's
//! ADR-0001/ADR-0002 — native-only by design, no wasm counterpart.

use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use stapler_mcp_core::ports::{Embedder, PortError};
use std::cell::RefCell;

pub struct NativeEmbedder {
    // Safe to borrow_mut() across the whole `embed()` call only because that
    // function never `.await`s while holding the borrow (fastembed's `embed`
    // is synchronous) — the single-threaded executor can't preempt mid-borrow.
    // If `embed()` ever gains an internal yield point, this becomes a
    // `BorrowMutError` risk.
    model: RefCell<Option<TextEmbedding>>,
    cache_dir: String,
}

impl NativeEmbedder {
    pub fn new(cache_dir: String) -> Self {
        Self {
            model: RefCell::new(None),
            cache_dir,
        }
    }
}

impl Embedder for NativeEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError> {
        let mut model_slot = self.model.borrow_mut();

        if model_slot.is_none() {
            eprintln!("NativeEmbedder: loading all-MiniLM-L6-v2 model (first embed() call)...");
            let load_start = std::time::Instant::now();
            let init = TextInitOptions::new(EmbeddingModel::AllMiniLML6V2)
                .with_cache_dir(std::path::PathBuf::from(&self.cache_dir));
            // Re-attempting init on every call after a failure is deliberate
            // (not cached as a permanent "poisoned" state): a fresh attempt
            // failing identically each time is the same observable behavior
            // as caching the failure, and is simpler to reason about.
            let loaded = TextEmbedding::try_new(init).map_err(|e| {
                eprintln!("NativeEmbedder: model load failed: {e}");
                PortError::Other(format!("failed to load embedding model: {e}"))
            })?;
            eprintln!("NativeEmbedder: model loaded in {:?}", load_start.elapsed());
            *model_slot = Some(loaded);
        }

        let model = model_slot
            .as_mut()
            .expect("model was just initialized above");

        // Deliberately synchronous/blocking here — this daemon runs a
        // single-threaded `current_thread` Tokio runtime with `!Send` state,
        // so `spawn_blocking` (which requires `Send`) doesn't cleanly fit.
        // The caller (`docs::index_source`) bounds the resulting stall by
        // capping batch size (`SUB_BATCH_SIZE`) and yielding between batches
        // rather than eliminating the stall — see plan.md's concurrency
        // Pattern Decisions row and research/pitfalls.md.
        model
            .embed(texts, None)
            .map_err(|e| PortError::Other(format!("failed to compute embeddings: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cache_dir(label: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "stapler-mcp-native-embed-test-{label}-{}",
            std::process::id()
        ));
        dir
    }

    /// Real model load + inference. Downloads the ~90MB ONNX model to a
    /// fresh cache dir on first run. Run explicitly:
    /// `cargo test -p stapler-mcp-native -- --ignored should_return_384_dim_vector_and_reuse_loaded_model_when_embed_called_twice`
    #[tokio::test]
    #[ignore]
    async fn should_return_384_dim_vector_and_reuse_loaded_model_when_embed_called_twice() {
        let cache_dir = temp_cache_dir("basic");
        let embedder = NativeEmbedder::new(cache_dir.to_str().unwrap().to_string());

        let first = embedder
            .embed(&["hello world".to_string()])
            .await
            .expect("first embed call should succeed");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].len(), 384);

        let start = std::time::Instant::now();
        let second = embedder
            .embed(&["hello world".to_string()])
            .await
            .expect("second embed call should succeed");
        let second_elapsed = start.elapsed();

        assert_eq!(second, first, "embedding should be deterministic");
        // Manual timing sanity check, not a strict assertion (per plan): a
        // reused model should answer far faster than a fresh model load.
        eprintln!("second embed() call took {second_elapsed:?} (model reuse expected to be fast)");

        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    #[tokio::test]
    async fn should_fail_fast_identically_on_every_call_when_model_fails_to_load() {
        // Point at a path that cannot possibly hold a valid model cache
        // (a file, not a directory) to force `try_new` to fail deterministically
        // without a network dependency.
        let mut bogus = std::env::temp_dir();
        bogus.push(format!(
            "stapler-mcp-native-embed-bogus-cache-{}",
            std::process::id()
        ));
        std::fs::write(&bogus, b"not a directory").unwrap();

        let embedder = NativeEmbedder::new(bogus.to_str().unwrap().to_string());

        let first = embedder.embed(&["hello".to_string()]).await;
        assert!(matches!(first, Err(PortError::Other(_))));

        let second = embedder.embed(&["hello".to_string()]).await;
        assert!(matches!(second, Err(PortError::Other(_))));

        if let (Err(PortError::Other(m1)), Err(PortError::Other(m2))) = (&first, &second) {
            assert!(!m1.is_empty());
            assert!(!m2.is_empty());
        }

        let _ = std::fs::remove_file(&bogus);
    }

    /// Real model load into a fresh `cache_dir`; asserts the cache dir is
    /// non-empty afterward, proving `cache_dir` was actually threaded into
    /// `fastembed::TextInitOptions` rather than `fastembed`'s own default
    /// cache location. Downloads the model on first run.
    #[tokio::test]
    #[ignore]
    async fn should_cache_model_under_configured_cache_dir_when_embed_called_first_time() {
        let cache_dir = temp_cache_dir("cachedir");
        std::fs::create_dir_all(&cache_dir).unwrap();

        let embedder = NativeEmbedder::new(cache_dir.to_str().unwrap().to_string());
        embedder
            .embed(&["hello".to_string()])
            .await
            .expect("embed should succeed");

        let has_entries = std::fs::read_dir(&cache_dir)
            .expect("cache dir should exist")
            .next()
            .is_some();
        assert!(has_entries, "expected fastembed to populate cache_dir");

        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    /// Throughput benchmark for `MAX_CHUNKS_PER_SOURCE` sizing (plan Task
    /// 2.1.2c). Downloads the model on first run; run explicitly:
    /// `cargo test -p stapler-mcp-native -- --ignored bench_embed_throughput`
    #[tokio::test]
    #[ignore]
    async fn bench_embed_throughput() {
        let cache_dir = temp_cache_dir("bench");
        let embedder = NativeEmbedder::new(cache_dir.to_str().unwrap().to_string());

        // Warm up: load the model outside the timed region.
        embedder
            .embed(&["warmup".to_string()])
            .await
            .expect("warmup embed should succeed");

        let chunks: Vec<String> = (0..100)
            .map(|i| {
                if i % 3 == 0 {
                    format!(
                        "## Section {i}\n\nThis chunk discusses async runtimes in Rust, covering \
                         topics like task scheduling, cooperative yielding, and how `tokio::spawn` \
                         differs from a raw OS thread. Chunk index {i} of 100 in this benchmark run, \
                         representative of a heading-aware markdown split from real documentation."
                    )
                } else if i % 3 == 1 {
                    let code_lines = [
                        "```rust".to_string(),
                        format!("async fn handle_{i}(conn: TcpStream) -> Result<(), Error> {{"),
                        "    let (mut reader, mut writer) = conn.into_split();".to_string(),
                        "    let mut buf = vec![0u8; 4096];".to_string(),
                        "    loop {".to_string(),
                        "        let n = reader.read(&mut buf).await?;".to_string(),
                        "        if n == 0 { break; }".to_string(),
                        "        writer.write_all(&buf[..n]).await?;".to_string(),
                        "    }".to_string(),
                        "    Ok(())".to_string(),
                        "}".to_string(),
                        "```".to_string(),
                        format!(
                            "Example handler function {i}, representative of code-mixed documentation content."
                        ),
                    ];
                    code_lines.join("\n")
                } else {
                    format!(
                        "Chunk {i}: a shorter paragraph of prose describing configuration options, \
                         error handling conventions, and cross-references to other sections of the \
                         documentation, kept under the chunk-size budget of a few hundred characters."
                    )
                }
            })
            .collect();

        let start = std::time::Instant::now();
        let result = embedder
            .embed(&chunks)
            .await
            .expect("benchmark embed call should succeed");
        let elapsed = start.elapsed();

        assert_eq!(result.len(), 100);
        let chunks_per_second = 100.0 / elapsed.as_secs_f64();
        eprintln!(
            "bench_embed_throughput: 100 chunks in {elapsed:?} => {chunks_per_second:.2} chunks/sec"
        );

        let _ = std::fs::remove_dir_all(&cache_dir);
    }
}

// Measured throughput (Task 2.1.2c), for use by Phase 4's
// `MAX_CHUNKS_PER_SOURCE = floor(chunks_per_second * 8)`:
//   ~97.7-100.4 chunks/sec measured via `bench_embed_throughput` (two runs),
//   on this dev machine, 2026-07-15 — AllMiniLML6V2, warm model, 100 chunks
//   of 200-800 char prose/code-mixed content in a single `embed()` call.
//   => MAX_CHUNKS_PER_SOURCE = floor(~98 * 8) = ~784 (well above the
//   SUB_BATCH_SIZE=100 floor, so sub-batching still yields multiple batches).
//   Use the conservative end (~97.7 chunks/sec => 781) if a single fixed
//   constant is needed; re-measure on CI/target hardware before treating this
//   as final per Task 2.1.2c's guidance.
