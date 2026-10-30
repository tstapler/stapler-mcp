//! AC1: N parallel callers on a cold start (no daemon) all succeed and none
//! sees "Connection refused".

mod common;

use std::time::Duration;

const N: usize = 8;

#[tokio::test]
async fn eight_parallel_cold_starts_should_all_succeed_without_connection_refused() {
    let h = common::Harness::new();

    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..N {
        let (sock, log) = (h.sock_path.clone(), h.log_path.clone());
        tasks.spawn(async move {
            common::ensure(&sock, &log, Duration::from_secs(45))
                .await
                .map_err(|e| e.to_string())
        });
    }
    while let Some(joined) = tasks.join_next().await {
        let result = joined.expect("task panicked");
        assert!(
            result.is_ok(),
            "a parallel cold-start caller failed: {result:?}"
        );
    }

    // Every caller then makes a real call against the single shared daemon.
    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..N {
        let sock = h.sock_path.clone();
        calls.spawn(async move {
            stapler_mcp_core::client::ping(
                &stapler_mcp_native::NativeSocketFactory,
                &sock,
                Duration::from_secs(30),
            )
            .await
            .map_err(|e| e.to_string())
        });
    }
    while let Some(joined) = calls.join_next().await {
        let result = joined.expect("task panicked");
        assert!(result.is_ok(), "follow-up call failed: {result:?}");
    }
}
