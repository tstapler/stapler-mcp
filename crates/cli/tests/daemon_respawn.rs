mod common;

use std::time::Duration;

use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn ensure_daemon_should_survive_parallel_cold_start_sigkill_and_client_hangups() {
    // Harness's Drop shuts the daemon down even when an assertion below fails.
    let h = common::Harness::new();
    let budget = Duration::from_secs(60);
    let sock_path = h.sock_path.clone();
    let ping = || h.ping(Duration::from_secs(2));

    // AC1: 6 parallel callers against a cold start all succeed.
    let results = futures::future::join_all((0..6).map(|_| h.ensure(budget))).await;
    for (i, r) in results.iter().enumerate() {
        assert!(r.is_ok(), "parallel caller {i} failed: {r:?}");
    }
    ping().await.expect("daemon reachable after cold start");

    // Root cause of #55: clients hanging up mid-response used to end the
    // daemon's accept loop while the process (and lock) lived on.
    for _ in 0..50 {
        let mut s = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        s.write_all(b"{\"tool\":\"ping\"}\n").await.unwrap();
        drop(s);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    ping()
        .await
        .expect("daemon must keep serving after client hang-ups");

    // AC0: SIGKILL the daemon, then the next caller respawns it.
    assert!(common::signal(h.daemon_pid(), libc::SIGKILL));
    for _ in 0..50 {
        if ping().await.is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ping().await.is_err(), "daemon should be down after SIGKILL");
    h.ensure(budget)
        .await
        .expect("ensure_daemon should respawn a killed daemon");
    ping().await.expect("respawned daemon reachable");
}
