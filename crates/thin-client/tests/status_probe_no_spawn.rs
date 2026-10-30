//! `SocketTransport::status_probe` must report a down daemon without starting
//! one (ADR-002) and must name the socket and log paths.
//! One test per file: it sets a process-global env var.

use stapler_mcp::transport::{DaemonTransport, SocketTransport};

#[tokio::test]
async fn status_probe_should_report_not_running_with_paths_and_leave_no_daemon_behind() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::env::set_var("STAPLER_MCP_HOME", tmp.path());

    let status = SocketTransport.status_probe().await;

    assert!(!status.pong);
    assert!(
        status.error.as_deref().unwrap().contains("not running"),
        "{status:?}"
    );
    let sock = status.socket_path.expect("socket path");
    let log = status.log_path.expect("log path");
    assert!(sock.starts_with(tmp.path().to_str().unwrap()), "{sock}");
    assert!(log.ends_with("daemon.log"), "{log}");
    assert!(
        !std::path::Path::new(&sock).exists(),
        "probe must not start a daemon"
    );
    assert!(
        !tmp.path().join("daemon.lock").exists(),
        "probe must not start a daemon"
    );
}
