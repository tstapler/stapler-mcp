//! The `DaemonTransport` seam: how an `McpRouter` reaches the shared
//! stapler-mcp daemon. `SocketTransport` (the stdio thin client's transport)
//! runs the `ensure_daemon` + `client::call` sequence, locating the daemon
//! binary via `daemon_locator`. The daemon's in-process `ChannelTransport`
//! (Streamable HTTP) lives in `stapler-mcp-daemon`.

use std::time::Duration;

use crate::daemon_locator;
use stapler_mcp_core::client::{self, EnsureOptions};
use stapler_mcp_core::paths;
use stapler_mcp_core::ports::PortError;
use stapler_mcp_core::protocol::PING_TOOL;
use stapler_mcp_core::schema::DaemonStatusOutput;
use stapler_mcp_native::{
    NativeClock, NativeEnv, NativeSleeper, NativeSocketFactory, NativeSpawner,
};

const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Above the core 30s default so a start that lands near the cold-Chrome limit
/// is not cut off by the client.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(45);

/// `status_probe` must answer quickly even when the daemon is busy.
const STATUS_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

pub trait DaemonTransport {
    fn call(
        &self,
        tool: &'static str,
        params: serde_json::Value,
    ) -> impl std::future::Future<Output = Result<serde_json::Value, String>> + Send;

    /// Reports daemon status without ever failing. The default pings through
    /// `call`; `SocketTransport` overrides it so a status check never spawns
    /// a daemon and an unreachable one comes back as `pong: false` + paths.
    fn status_probe(&self) -> impl std::future::Future<Output = DaemonStatusOutput> + Send {
        // Built before the `async` block so `&self` is not held across an await.
        let call = self.call(PING_TOOL, serde_json::json!({}));
        async move {
            let failed = |error: String| DaemonStatusOutput {
                error: Some(error),
                ..Default::default()
            };
            match call.await {
                Ok(value) => {
                    serde_json::from_value(value).unwrap_or_else(|e| failed(e.to_string()))
                }
                Err(e) => failed(e),
            }
        }
    }
}

/// Error text for a failed `ensure_daemon`: names the socket and daemon log so
/// the caller can diagnose without extra tooling.
fn ensure_error_message(err: &PortError, sock_path: &str, log_path: &str, hint: &str) -> String {
    format!("ensure daemon: {err} (socket: {sock_path}, daemon log: {log_path}; {hint})")
}

/// Maps a no-spawn ping result onto the status tool's output. A timeout means
/// the single-threaded daemon is alive but busy; anything else means nothing
/// is listening.
fn status_from_ping(
    result: Result<(), PortError>,
    pong_payload: Option<serde_json::Value>,
    sock_path: &str,
    log_path: &str,
) -> DaemonStatusOutput {
    let unreachable = |msg: String| {
        DaemonStatusOutput::unreachable(msg, sock_path.to_string(), log_path.to_string())
    };
    match result {
        Ok(()) => pong_payload
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or(DaemonStatusOutput {
                pong: true,
                ..Default::default()
            }),
        Err(PortError::Timeout) => unreachable(format!(
            "daemon busy: no reply within {}s (it serves one request at a time, so it is probably alive)",
            STATUS_PROBE_TIMEOUT.as_secs()
        )),
        Err(e) => unreachable(format!("daemon not running: {e}")),
    }
}

#[derive(Debug, Clone, Default)]
pub struct SocketTransport;

impl DaemonTransport for SocketTransport {
    async fn call(
        &self,
        tool: &'static str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let env = NativeEnv;
        let socket = NativeSocketFactory;
        let spawner = NativeSpawner;
        let sleeper = NativeSleeper;
        let clock = NativeClock;

        let sock_path = paths::socket_path(&env);
        let log_path = paths::log_path(&env);

        client::ensure_daemon(
            &socket,
            &spawner,
            &sleeper,
            &clock,
            &sock_path,
            &log_path,
            EnsureOptions {
                exe_hint: Some(daemon_locator::daemon_exe_path()),
                startup_timeout: Some(STARTUP_TIMEOUT),
            },
        )
        .await
        .map_err(|e| {
            ensure_error_message(&e, &sock_path, &log_path, &daemon_locator::lookup_hint())
        })?;

        client::call(&socket, &sock_path, tool, Some(params), CALL_TIMEOUT)
            .await
            .map_err(|e| e.to_string())
    }

    async fn status_probe(&self) -> DaemonStatusOutput {
        let env = NativeEnv;
        let sock_path = paths::socket_path(&env);
        let log_path = paths::log_path(&env);
        match client::call(
            &NativeSocketFactory,
            &sock_path,
            PING_TOOL,
            None,
            STATUS_PROBE_TIMEOUT,
        )
        .await
        {
            Ok(payload) => status_from_ping(Ok(()), Some(payload), &sock_path, &log_path),
            Err(e) => status_from_ping(Err(e), None, &sock_path, &log_path),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_error_message_should_include_socket_and_log_paths() {
        let msg = ensure_error_message(
            &PortError::Io("Connection refused (os error 111)".into()),
            "/run/x/daemon.sock",
            "/home/u/.stapler-mcp/daemon.log",
            "hint",
        );
        assert!(msg.contains("/run/x/daemon.sock"), "{msg}");
        assert!(msg.contains("/home/u/.stapler-mcp/daemon.log"), "{msg}");
        assert!(msg.contains("Connection refused"), "{msg}");
    }

    #[test]
    fn status_from_ping_should_report_not_running_with_paths_when_refused() {
        let out = status_from_ping(
            Err(PortError::Io("Connection refused (os error 111)".into())),
            None,
            "/s",
            "/l",
        );
        assert!(!out.pong);
        assert!(out.error.as_deref().unwrap().contains("not running"));
        assert_eq!(out.socket_path.as_deref(), Some("/s"));
        assert_eq!(out.log_path.as_deref(), Some("/l"));
    }

    #[test]
    fn status_from_ping_should_report_busy_not_down_on_timeout() {
        let out = status_from_ping(Err(PortError::Timeout), None, "/s", "/l");
        assert!(!out.pong);
        assert!(out.error.as_deref().unwrap().contains("busy"));
    }

    #[test]
    fn status_from_ping_should_pass_through_the_daemons_payload_when_up() {
        let payload = serde_json::json!({"pong": true, "browserProfileMode": "ephemeral"});
        let out = status_from_ping(Ok(()), Some(payload), "/s", "/l");
        assert!(out.pong);
        assert!(out.error.is_none() && out.socket_path.is_none());
        assert_eq!(out.browser_profile_mode.as_deref(), Some("ephemeral"));
    }
}
