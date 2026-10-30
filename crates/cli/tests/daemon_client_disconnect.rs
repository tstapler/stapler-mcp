//! AC2: clients that hang up mid-request are logged by the daemon, which keeps
//! serving later connections instead of exiting.

mod common;

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

#[tokio::test]
async fn abandoned_connections_should_not_kill_daemon_and_should_be_logged() {
    let h = common::Harness::new();
    h.ensure(Duration::from_secs(45))
        .await
        .expect("daemon start");
    let pid = h.daemon_pid();

    // Send a full request, then hang up without reading the response: the
    // daemon's reply hits a closed socket (EPIPE).
    for _ in 0..20 {
        let mut stream = UnixStream::connect(&h.sock_path).expect("connect");
        stream.write_all(b"{\"tool\":\"ping\"}\n").unwrap();
        drop(stream);
    }

    // The daemon is serial, so once this ping is answered the abandoned
    // connections ahead of it have all been processed.
    h.ping(Duration::from_secs(10))
        .await
        .expect("daemon should still answer after abandoned connections");
    assert_eq!(h.daemon_pid(), pid, "daemon must not have been replaced");
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        0,
        "daemon process must be alive"
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let log = std::fs::read_to_string(&h.log_path).unwrap_or_default();
        if log.contains("connection error (continuing)") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "daemon.log never recorded a connection error:\n{log}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
