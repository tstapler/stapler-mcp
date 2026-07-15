//! One-time manual relevance spot-check (plan.md Story 6.3.1 / pre-mortem.md's
//! #1 P1 item): index a real, representative, code-heavy doc source and issue
//! a handful of realistic queries against it, printing the ranked results for
//! human judgment. `all-MiniLM-L6-v2` is a general-purpose sentence-similarity
//! model, not trained specifically on code/API-reference content — this is
//! the only check in the whole test suite that looks at semantic *relevance*
//! rather than mechanical correctness (ranking order, error handling, etc.).
//!
//! Not a test — this makes a real network call (crawls a real site, and on
//! first run downloads the ~90MB `all-MiniLM-L6-v2` ONNX model). Run
//! explicitly:
//!   cargo run -p stapler-mcp --example relevance_spot_check

use std::time::Duration;

use serde_json::json;

use stapler_mcp_core::client::{self, EnsureOptions};
use stapler_mcp_core::paths;
use stapler_mcp_native::{
    NativeClock, NativeEnv, NativeSleeper, NativeSocketFactory, NativeSpawner,
};

const SEED_URL: &str = "https://tokio.rs/tokio/tutorial";
const SOURCE_NAME: &str = "tokio-tutorial-spot-check";

const QUERIES: &[&str] = &[
    "how do I spawn a background task",
    "what's the difference between Mutex and RwLock",
    "async function that returns a Result",
    "how do I share state between tasks",
    "how do channels work in tokio",
];

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let env = NativeEnv;
    let sock_path = paths::socket_path(&env);
    let log_path = paths::log_path(&env);
    let socket = NativeSocketFactory;
    let spawner = NativeSpawner;
    let sleeper = NativeSleeper;
    let clock = NativeClock;
    // `CARGO_BIN_EXE_*` is only set by cargo for integration tests, not
    // examples — fall back to the standard workspace target-dir layout.
    let exe = std::env::var("STAPLER_MCP_BIN").unwrap_or_else(|_| {
        format!(
            "{}/../../target/debug/stapler-mcp",
            env!("CARGO_MANIFEST_DIR")
        )
    });

    println!("Starting/ensuring daemon (first run downloads the ~90MB embedding model)...");
    client::ensure_daemon(
        &socket,
        &spawner,
        &sleeper,
        &clock,
        &sock_path,
        &log_path,
        EnsureOptions {
            startup_timeout: Some(Duration::from_secs(300)),
            exe_hint: Some(exe),
        },
    )
    .await
    .expect("daemon should auto-start");

    println!("Indexing {SEED_URL} as '{SOURCE_NAME}'...");
    let index_result = client::call(
        &socket,
        &sock_path,
        "stapler_index_docs",
        Some(json!({
            "url": SEED_URL,
            "source": SOURCE_NAME,
            "maxDepth": 2,
            "maxPages": 20,
        })),
        Duration::from_secs(180),
    )
    .await
    .expect("stapler_index_docs should succeed");
    println!(
        "Indexed: {} pages, {} chunks\n",
        index_result["pagesIndexed"], index_result["chunksIndexed"]
    );

    for query in QUERIES {
        println!("=== Query: \"{query}\" ===");
        let search_result = client::call(
            &socket,
            &sock_path,
            "stapler_search_docs",
            Some(json!({
                "source": SOURCE_NAME,
                "query": query,
                "limit": 3,
            })),
            Duration::from_secs(60),
        )
        .await
        .expect("stapler_search_docs should succeed");

        let results = search_result["results"].as_array().expect("results array");
        if results.is_empty() {
            println!("  (no results)");
        }
        for (i, r) in results.iter().enumerate() {
            let heading = r["heading"].as_str().unwrap_or("(no heading)");
            let score = r["score"].as_f64().unwrap_or(0.0);
            let text = r["text"].as_str().unwrap_or("");
            let snippet: String = text.chars().take(220).collect();
            println!("  [{i}] score={score:.4} heading={heading:?}");
            println!("      {snippet}...");
        }
        println!();
    }

    println!("Cleaning up: removing '{SOURCE_NAME}'...");
    let _ = client::call(
        &socket,
        &sock_path,
        "stapler_remove_indexed_source",
        Some(json!({"source": SOURCE_NAME})),
        Duration::from_secs(10),
    )
    .await
    .expect("stapler_remove_indexed_source should succeed");

    println!("Done. Judge the results above for relevance, then record the verdict.");
}
