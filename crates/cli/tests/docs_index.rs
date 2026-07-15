//! End-to-end smoke test for the docs-index feature's daemon wiring
//! (plan.md Epic 6.2 / Story 6.2.1): proves `index_docs`, `search_docs`,
//! `list_indexed_sources`, and `remove_indexed_source` are all correctly
//! `daemon.register`ed and reachable through the real Unix-socket protocol,
//! mirroring `webcrawl.rs`'s and `daemon_ping.rs`'s real-daemon integration
//! pattern.
//!
//! This is deliberately NOT the primary correctness verification for the
//! docs-index tools — that's `crates/core/src/tools/docs.rs`'s fast, offline
//! unit tests using `FakeHttpClient`/`InMemoryFileStore`/`FakeEmbedder`. This
//! test exists solely to catch a Phase-5 wiring mistake (e.g. a typo'd tool
//! name, a missed `daemon.register` call) that only shows up when the tools
//! are driven through the real daemon socket.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use stapler_mcp_core::client::{self, EnsureOptions};
use stapler_mcp_core::paths;
use stapler_mcp_core::ports::EnvPort;
use stapler_mcp_native::{NativeClock, NativeSleeper, NativeSocketFactory, NativeSpawner};

struct TestEnv {
    home: String,
}

impl EnvPort for TestEnv {
    fn var(&self, key: &str) -> Option<String> {
        if key == "STAPLER_MCP_HOME" {
            Some(self.home.clone())
        } else {
            None
        }
    }

    fn home_dir(&self) -> Option<String> {
        Some(self.home.clone())
    }
}

/// Hand-rolled multi-route HTTP mock, same pattern as `webcrawl.rs`'s
/// `spawn_mock_site` — parses just the request line's path and serves a
/// small synthetic two-page site with real (if silly) prose content, so
/// `search_docs`'s embedding-based ranking has something meaningful to
/// distinguish between.
async fn spawn_mock_site() -> (String, tokio::sync::oneshot::Sender<()>) {
    let routes: HashMap<&str, String> = HashMap::from([
        (
            "/",
            "<html><head><title>Rust Async Guide</title></head><body>\
             <p>Rust's async/await syntax lets you write asynchronous code that \
             looks like ordinary synchronous code. Futures are lazy: they do \
             nothing until polled by an executor such as Tokio.</p>\
             <a href=\"/page2\">Page 2</a></body></html>"
                .to_string(),
        ),
        (
            "/page2",
            "<html><head><title>Sourdough Bread Recipe</title></head><body>\
             <p>To bake sourdough bread, first feed your starter with equal \
             parts flour and water and let it rise overnight. Fold the dough \
             gently to build gluten strength before its long, slow proof.</p>\
             </body></html>"
                .to_string(),
        ),
    ]);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock site");
    let addr = listener.local_addr().expect("mock site addr");
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => return,
                accepted = listener.accept() => {
                    let Ok((mut stream, _)) = accepted else { return };
                    let routes = routes.clone();
                    tokio::spawn(async move {
                        let mut buf = [0u8; 4096];
                        let n = stream.read(&mut buf).await.unwrap_or(0);
                        let request = String::from_utf8_lossy(&buf[..n]);
                        let path = request
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("/")
                            .to_string();
                        let body = routes.get(path.as_str()).cloned();
                        let resp = match body {
                            Some(b) => format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                b.len(),
                                b
                            ),
                            None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                        };
                        let _ = stream.write_all(resp.as_bytes()).await;
                        let _ = stream.shutdown().await;
                    });
                }
            }
        }
    });

    (format!("http://{addr}"), shutdown_tx)
}

/// Ignored by default: the real `NativeEmbedder` downloads the ~90MB
/// `all-MiniLM-L6-v2` ONNX model from Hugging Face Hub on first use, which
/// requires network access and is too slow/flaky for a default `cargo test`
/// run — matching this project's precedent of keeping default test runs
/// fast and hermetic (`webcrawl.rs`'s test uses an in-process mock HTTP
/// server specifically to avoid any real network dependency; this test
/// cannot fully avoid one for the embedder, so it's opted out of the
/// default run instead). Run explicitly with:
///   cargo test -p stapler-mcp -- --ignored docs_index_round_trip
#[tokio::test]
#[ignore]
async fn docs_index_round_trip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().to_string_lossy().to_string();
    std::env::set_var("STAPLER_MCP_HOME", &home);
    let env = TestEnv { home };
    std::fs::create_dir_all(paths::base_dir(&env)).unwrap();

    let (site_url, shutdown_site) = spawn_mock_site().await;

    let sock_path = paths::socket_path(&env);
    let log_path = paths::log_path(&env);
    let socket = NativeSocketFactory;
    let spawner = NativeSpawner;
    let sleeper = NativeSleeper;
    let clock = NativeClock;
    let exe = env!("CARGO_BIN_EXE_stapler-mcp").to_string();

    client::ensure_daemon(
        &socket,
        &spawner,
        &sleeper,
        &clock,
        &sock_path,
        &log_path,
        EnsureOptions {
            // Generous timeout: first run also downloads the ~90MB ONNX
            // embedding model from Hugging Face Hub.
            startup_timeout: Some(Duration::from_secs(300)),
            exe_hint: Some(exe),
        },
    )
    .await
    .expect("daemon should auto-start");

    // 1. index_docs: crawl the mock site (seed + linked page2) and embed it
    //    with the real NativeEmbedder.
    let index_result = client::call(
        &socket,
        &sock_path,
        "stapler_index_docs",
        Some(json!({
            "url": site_url,
            "source": "mock-docs",
            "maxDepth": 1,
            "maxPages": 10,
        })),
        Duration::from_secs(180),
    )
    .await
    .expect("index_docs should succeed");

    assert_eq!(index_result["sourceName"], "mock-docs", "got: {index_result:?}");
    let pages_indexed = index_result["pagesIndexed"].as_u64().expect("pagesIndexed");
    assert_eq!(pages_indexed, 2, "expected seed + page2 to be indexed, got: {index_result:?}");
    let chunks_indexed = index_result["chunksIndexed"].as_u64().expect("chunksIndexed");
    assert!(chunks_indexed >= 1, "expected at least one chunk indexed, got: {index_result:?}");

    // 2. search_docs: a query aimed squarely at the async/Tokio page should
    //    come back with at least one result.
    let search_result = client::call(
        &socket,
        &sock_path,
        "stapler_search_docs",
        Some(json!({
            "source": "mock-docs",
            "query": "how does async await work in Rust with Tokio futures",
        })),
        Duration::from_secs(60),
    )
    .await
    .expect("search_docs should succeed");

    let results = search_result["results"].as_array().expect("results array");
    assert!(!results.is_empty(), "expected at least 1 search result, got: {search_result:?}");

    // 3. list_indexed_sources: the newly indexed source shows up.
    let list_result = client::call(
        &socket,
        &sock_path,
        "stapler_list_indexed_sources",
        Some(json!({})),
        Duration::from_secs(10),
    )
    .await
    .expect("list_indexed_sources should succeed");

    let sources = list_result["sources"].as_array().expect("sources array");
    assert!(
        sources.iter().any(|s| s["sourceName"] == "mock-docs"),
        "expected 'mock-docs' in list_indexed_sources output, got: {list_result:?}"
    );

    // 4. remove_indexed_source: removes the source...
    let remove_result = client::call(
        &socket,
        &sock_path,
        "stapler_remove_indexed_source",
        Some(json!({"source": "mock-docs"})),
        Duration::from_secs(10),
    )
    .await
    .expect("remove_indexed_source should succeed");
    assert_eq!(remove_result["removed"], true, "got: {remove_result:?}");
    assert_eq!(remove_result["sourceName"], "mock-docs", "got: {remove_result:?}");

    // ...and a subsequent search_docs call against it now errors as an
    // unknown source.
    let search_after_removal = client::call(
        &socket,
        &sock_path,
        "stapler_search_docs",
        Some(json!({
            "source": "mock-docs",
            "query": "anything",
        })),
        Duration::from_secs(30),
    )
    .await;

    let err = search_after_removal.expect_err("search_docs should error for a removed source");
    let err_message = err.to_string();
    assert!(
        err_message.contains("no indexed source named"),
        "expected an unknown-source error, got: {err_message}"
    );

    let _ = shutdown_site.send(());

    client::call(&socket, &sock_path, "shutdown", None, Duration::from_secs(2))
        .await
        .expect("shutdown call should succeed");
}
