//! The `DaemonTransport` seam: how an `McpRouter` reaches the shared
//! stapler-mcp daemon. `SocketTransport` is today's (and the stdio server's)
//! only implementation, reproducing the exact `ensure_daemon` + `client::call`
//! sequence the old `ThinClient` inlined. `ChannelTransport` backs the
//! Streamable HTTP transport instead, via `run_bridge_consumer` below.

use std::rc::Rc;
use std::time::Duration;

use futures::FutureExt;
use stapler_mcp_core::client::{self, EnsureOptions};
use stapler_mcp_core::daemon::Daemon;
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
            EnsureOptions::default(),
        )
        .await
        .map_err(|e| format!("ensure daemon: {e}"))?;

        client::call(&socket, &sock_path, tool, Some(params), CALL_TIMEOUT)
            .await
            .map_err(|e| e.to_string())
    }
}

/// One in-process daemon call: the request plus a channel to deliver its
/// response back to the caller. The bridge consumer in `main.rs`'s
/// `run_daemon` reads these off `bridge_rx` and drives them through the same
/// `!Send` `Daemon::handle_request` the Unix-socket accept loop uses.
pub type BridgeMessage = (
    stapler_mcp_core::protocol::Request,
    tokio::sync::oneshot::Sender<stapler_mcp_core::protocol::Response>,
);

pub const BRIDGE_CHANNEL_CAPACITY: usize = 64;

/// Downcasts a `catch_unwind` payload to a loggable string. Panic payloads
/// are conventionally `&str` (from a `panic!("literal")`) or `String` (from
/// `panic!("{}", ...)`); anything else (a custom payload type) falls back to
/// a fixed message rather than failing to log at all.
// Used by `run_bridge_consumer` below and by `main.rs`; `#[allow]`ed because
// `tests/tool_schema.rs` pulls this file in via `#[path]` into a separate
// compilation unit that only calls `McpRouter::registered_tools()`, where
// dead-code analysis can't see this function's real callers.
#[allow(dead_code)]
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

/// Drives the bridge channel: reads `(Request, reply_tx)` pairs off `rx` and
/// dispatches each through `daemon.handle_request`, replying on `reply_tx`.
/// A panicking tool handler is caught via `catch_unwind` and turned into a
/// clean error reply plus a distinct "bridge consumer panicked" log line,
/// rather than taking this task (and every future HTTP-originated call, since
/// nothing else drains `rx`) down with it. Returns once `cancel` fires or
/// `rx` closes, draining any messages left queued with a "daemon shutting
/// down" error so no caller is left hung on a oneshot that will never
/// resolve. Extracted from `main.rs::run_daemon` so integration tests (see
/// `crates/cli/tests/http_transport.rs`) can construct this exact consumer
/// loop against a test-only `Daemon` without spawning the real binary.
// See `panic_message`'s comment above for why this needs `#[allow(dead_code)]`.
#[allow(dead_code)]
pub async fn run_bridge_consumer(
    daemon: Rc<Daemon>,
    mut rx: tokio::sync::mpsc::Receiver<BridgeMessage>,
    cancel: tokio_util::sync::CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            msg = rx.recv() => {
                match msg {
                    Some((req, reply_tx)) => {
                        let tool = req.tool.clone();
                        let resp = match std::panic::AssertUnwindSafe(daemon.handle_request(req))
                            .catch_unwind()
                            .await
                        {
                            Ok(resp) => resp,
                            Err(payload) => {
                                eprintln!(
                                    "stapler-mcp: bridge consumer panicked handling tool call {tool:?}: {}",
                                    panic_message(&payload)
                                );
                                stapler_mcp_core::protocol::Response::err(
                                    "internal error: tool handler panicked".to_string(),
                                )
                            }
                        };
                        let _ = reply_tx.send(resp);
                    }
                    None => break,
                }
            }
        }
    }

    while let Ok((_, reply_tx)) = rx.try_recv() {
        let _ = reply_tx.send(stapler_mcp_core::protocol::Response::err(
            "daemon shutting down".to_string(),
        ));
    }
}

/// `DaemonTransport` for the in-process bridge channel: hands the request to
/// `run_bridge_consumer` via `tx` and awaits the reply on a fresh oneshot.
/// Constructed per HTTP session by `http_server::build_mcp_service`'s
/// `service_factory`, which is why `call`'s returned future must stay `Send`
/// even though the consumer on the other end of `tx` is not.
#[derive(Clone)]
pub struct ChannelTransport {
    tx: tokio::sync::mpsc::Sender<BridgeMessage>,
}

impl ChannelTransport {
    pub fn new(tx: tokio::sync::mpsc::Sender<BridgeMessage>) -> Self {
        Self { tx }
    }
}

impl DaemonTransport for ChannelTransport {
    fn call(
        &self,
        tool: &'static str,
        params: serde_json::Value,
    ) -> impl std::future::Future<Output = Result<serde_json::Value, String>> + Send {
        let tx = self.tx.clone();
        async move {
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            let req = stapler_mcp_core::protocol::Request {
                tool: tool.to_string(),
                params: Some(params),
            };

            // Race the send against a 250ms timer so a saturated bridge
            // channel (the daemon core falling behind) shows up in logs
            // instead of silently stalling the HTTP request. The message is
            // never dropped or duplicated: `send_fut` is pinned once and
            // polled by both branches of `select!`, so if the timer wins
            // first we just log and keep polling the *same* in-flight send
            // future to completion rather than constructing a new one (which
            // `mpsc::Sender::send`'s by-value `msg` wouldn't allow anyway).
            let send_fut = tx.send((req, reply_tx));
            tokio::pin!(send_fut);
            tokio::select! {
                result = &mut send_fut => {
                    result.map_err(|_| "daemon bridge channel closed".to_string())?;
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {
                    eprintln!("stapler-mcp: HTTP request queued >250ms waiting on daemon core");
                    send_fut
                        .await
                        .map_err(|_| "daemon bridge channel closed".to_string())?;
                }
            }

            let resp = reply_rx
                .await
                .map_err(|_| "daemon bridge channel closed before reply".to_string())?;
            if let Some(err) = resp.error {
                return Err(err);
            }
            Ok(resp.result.unwrap_or(serde_json::Value::Null))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stapler_mcp_core::daemon::Daemon;

    #[tokio::test]
    async fn channel_transport_call_should_complete_independently_when_two_clones_call_concurrently(
    ) {
        tokio::task::LocalSet::new()
            .run_until(async {
                let daemon = Daemon::new();
                let (tx, mut rx) =
                    tokio::sync::mpsc::channel::<BridgeMessage>(BRIDGE_CHANNEL_CAPACITY);

                tokio::task::spawn_local(async move {
                    while let Some((req, reply_tx)) = rx.recv().await {
                        let resp = daemon.handle_request(req).await;
                        let _ = reply_tx.send(resp);
                    }
                });

                let a = ChannelTransport::new(tx.clone());
                let b = ChannelTransport::new(tx);

                let (result_a, result_b) = tokio::join!(
                    a.call("ping", serde_json::json!({})),
                    b.call("ping", serde_json::json!({})),
                );

                assert_eq!(result_a, Ok(serde_json::json!({"pong": true})));
                assert_eq!(result_b, Ok(serde_json::json!({"pong": true})));
            })
            .await;
    }

    /// Deterministically exercises the >250ms backpressure branch without a
    /// real wall-clock wait: `start_paused` gives this test a virtual clock,
    /// so `tokio::time::advance` fires `call`'s internal `sleep` instantly.
    /// The property under test is the one that matters operationally — a
    /// slow-to-drain bridge channel never loses or duplicates the message —
    /// not the log line itself, which isn't practical to assert on from
    /// inside the test binary (see this task's manual e2e verification for
    /// that).
    #[tokio::test(start_paused = true)]
    async fn channel_transport_call_should_still_deliver_message_when_send_blocks_past_250ms() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<BridgeMessage>(1);

        // Saturate the channel's one slot with a message nobody's draining
        // yet, so `transport.call`'s own `send` below has to wait.
        let (blocker_reply_tx, _blocker_reply_rx) = tokio::sync::oneshot::channel();
        let blocking_req = stapler_mcp_core::protocol::Request {
            tool: "blocker".to_string(),
            params: None,
        };
        tx.try_send((blocking_req, blocker_reply_tx))
            .expect("fill the channel's single slot");

        let transport = ChannelTransport::new(tx);
        let call_task =
            tokio::spawn(async move { transport.call("ping", serde_json::json!({})).await });

        // Let `call`'s `send` future register as pending against the full
        // channel before advancing the virtual clock past the warning
        // threshold — `select!` should take the timer branch and then keep
        // waiting on the same `send`, not abandon it.
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(300)).await;

        // Drain the channel: first the blocker (freeing the slot `call`'s
        // send was waiting on), then the real call's own message.
        let (blocker_req, _blocker_tx) = rx.recv().await.expect("recv blocker message");
        assert_eq!(blocker_req.tool, "blocker");

        let (real_req, real_reply_tx) = rx
            .recv()
            .await
            .expect("recv real message after backpressure");
        assert_eq!(real_req.tool, "ping");
        real_reply_tx
            .send(stapler_mcp_core::protocol::Response {
                result: Some(serde_json::json!({"pong": true})),
                error: None,
            })
            .expect("reply to the unblocked call");

        let result = call_task.await.expect("call task should not panic");
        assert_eq!(result, Ok(serde_json::json!({"pong": true})));

        // The channel had capacity 1 and only ever received these two
        // messages — nothing lost, nothing duplicated.
        assert!(rx.try_recv().is_err());
    }
}
