//! AC0: after the daemon is SIGKILLed, the next `ensure_daemon` restarts it
//! within 30s (cold headless Chrome included).

mod common;

use std::time::{Duration, Instant};

use stapler_mcp_core::ports::PortError;

#[tokio::test]
async fn sigkill_should_be_recovered_by_next_ensure_daemon_within_30s() {
    let h = common::Harness::new();
    h.ensure(Duration::from_secs(45))
        .await
        .expect("initial daemon start");
    let old_pid = h.daemon_pid();

    assert!(common::signal(old_pid, libc::SIGKILL));
    assert!(
        common::wait_for_exit(old_pid, Duration::from_secs(5)),
        "daemon should die after SIGKILL"
    );

    let err = h
        .ping(Duration::from_secs(2))
        .await
        .expect_err("killed daemon must not answer");
    assert!(
        !matches!(err, PortError::Timeout),
        "a dead daemon must look refused, not busy: {err:?}"
    );

    let started = Instant::now();
    h.ensure(Duration::from_secs(45))
        .await
        .expect("next ensure_daemon should restart the daemon");
    let took = started.elapsed();
    assert!(took < Duration::from_secs(30), "restart took {took:?}");

    h.ping(Duration::from_secs(5))
        .await
        .expect("ping after restart");
    assert_ne!(h.daemon_pid(), old_pid, "should be a new daemon process");
}
