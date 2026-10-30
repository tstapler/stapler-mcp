//! The `DaemonTransport` seam: how an `McpRouter` reaches the shared
//! stapler-mcp daemon. `SocketTransport` (the stdio thin client's transport)
//! runs the `ensure_daemon` + `client::call` sequence, locating the daemon
//! binary via `daemon_locator`. The daemon's in-process `ChannelTransport`
//! (Streamable HTTP) lives in `stapler-mcp-daemon`.

use std::time::Duration;

use crate::daemon_locator;
use stapler_mcp_core::client::{self, EnsureOptions};
use stapler_mcp_core::paths;
use stapler_mcp_native::{
    NativeClock, NativeEnv, NativeSleeper, NativeSocketFactory, NativeSpawner,
};

const CALL_TIMEOUT: Duration = Duration::from_secs(120);

pub trait DaemonTransport {
    fn call(
        &self,
        tool: &'static str,
        params: serde_json::Value,
    ) -> impl std::future::Future<Output = Result<serde_json::Value, String>> + Send;
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
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            format!(
                "ensure daemon: {e} (socket={sock_path}, log={log_path}; {})",
                daemon_locator::lookup_hint()
            )
        })?;

        client::call(&socket, &sock_path, tool, Some(params), CALL_TIMEOUT)
            .await
            .map_err(|e| e.to_string())
    }
}
