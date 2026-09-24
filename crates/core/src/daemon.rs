//! Tool dispatch, independently testable via `handle_request_bytes` with no
//! socket involved. `run` drives an accept loop against a `SocketFactory`.
//! Deliberately single-threaded (no `Send` bounds anywhere) — the native
//! binary runs this on a `current_thread` tokio runtime + `LocalSet`, which is
//! what lets the exact same handler-registry code also satisfy a `!Send`
//! wasm-bindgen adapter.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use futures::future::LocalBoxFuture;
#[cfg(not(target_arch = "wasm32"))]
use tokio_util::sync::CancellationToken;

use crate::ports::{Conn, Listener, PortError, SocketFactory};
use crate::protocol::{Request, Response, PING_TOOL, SHUTDOWN_TOOL};

pub type HandlerResult = Result<serde_json::Value, String>;
pub type Handler = Box<dyn Fn(Option<serde_json::Value>) -> LocalBoxFuture<'static, HandlerResult>>;

const CONN_TIMEOUT: Duration = Duration::from_secs(120);

/// Bounds one registered handler call so a hung tool can never block the
/// accept loop (or, in a later phase, the channel bridge consumer) forever.
/// Shortened under `#[cfg(test)]` so the timeout tests don't need to wait
/// out a real 30s. Native-only: wasm32's single JS-driven call per handler
/// has no accept loop to protect, so there's nothing to bound.
#[cfg(all(not(test), not(target_arch = "wasm32")))]
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(all(test, not(target_arch = "wasm32")))]
const REQUEST_TIMEOUT: Duration = Duration::from_millis(50);

pub struct Daemon {
    handlers: RefCell<HashMap<&'static str, Handler>>,
    shutdown: Rc<Cell<bool>>,
    // wasm32 has no `tokio`/`tokio_util` dependency (see crates/core/Cargo.toml)
    // and no caller that needs cancellation there — see `request_shutdown`,
    // `cancellation_token`, `run_cancellable`.
    #[cfg(not(target_arch = "wasm32"))]
    cancel: CancellationToken,
    /// Extra fields merged into every `ping` response's `result` object —
    /// e.g. `browserProfileMode`/`browserProfileWarning` (native `run_daemon`
    /// only; the wasm adapter never calls `set_status_extra`, so its `ping`
    /// response stays `{"pong": true}`). Set at most once, before `run()`'s
    /// accept loop starts — single-writer, read-only afterward.
    status_extra: RefCell<serde_json::Value>,
}

impl Daemon {
    pub fn new() -> Self {
        Daemon {
            handlers: RefCell::new(HashMap::new()),
            shutdown: Rc::new(Cell::new(false)),
            #[cfg(not(target_arch = "wasm32"))]
            cancel: CancellationToken::new(),
            status_extra: RefCell::new(serde_json::json!({})),
        }
    }

    pub fn register(&self, name: &'static str, handler: Handler) {
        self.handlers.borrow_mut().insert(name, handler);
    }

    /// Stores fields to merge into every future `ping` response's `result`
    /// object. Intended to be called at most once, at startup.
    pub fn set_status_extra(&self, extra: serde_json::Value) {
        *self.status_extra.borrow_mut() = extra;
    }

    /// Sets the shutdown flag `run`/`run_cancellable` poll after each
    /// connection, and cancels `cancellation_token()` so a bridge consumer
    /// (or `run_cancellable`'s accept loop) unblocks immediately rather than
    /// waiting for the next connection/message. Both the `shutdown` RPC and
    /// any future SIGTERM path funnel through this one method.
    pub fn request_shutdown(&self) {
        self.shutdown.set(true);
        #[cfg(not(target_arch = "wasm32"))]
        self.cancel.cancel();
    }

    /// Cheap clone — `CancellationToken` is internally `Arc`-backed. Native-only:
    /// nothing on wasm32 waits on cancellation (see the `cancel` field's doc
    /// comment).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Pure function: JSON request bytes in, JSON response bytes out. No
    /// socket, no framing — the easiest layer to unit test.
    pub async fn handle_request_bytes(&self, bytes: &[u8]) -> Vec<u8> {
        let resp = match serde_json::from_slice::<Request>(bytes) {
            Ok(req) => self.handle_request(req).await,
            Err(e) => Response::err(format!("invalid request: {e}")),
        };
        // A Response is always representable as JSON; unwrap is safe.
        serde_json::to_vec(&resp).expect("Response always serializes")
    }

    pub async fn handle_request(&self, req: Request) -> Response {
        match req.tool.as_str() {
            PING_TOOL => {
                let mut pong = serde_json::json!({"pong": true});
                if let (serde_json::Value::Object(extra), serde_json::Value::Object(pong_obj)) =
                    (&*self.status_extra.borrow(), &mut pong)
                {
                    pong_obj.extend(extra.clone());
                }
                Response::ok(pong)
            }
            SHUTDOWN_TOOL => {
                self.request_shutdown();
                Response::ok(serde_json::json!({}))
            }
            other => {
                // Extract the future while holding the borrow only briefly —
                // never hold a RefCell borrow across an .await.
                let fut = {
                    let handlers = self.handlers.borrow();
                    handlers.get(other).map(|h| h(req.params.clone()))
                };
                match fut {
                    #[cfg(not(target_arch = "wasm32"))]
                    Some(fut) => match tokio::time::timeout(REQUEST_TIMEOUT, fut).await {
                        Ok(Ok(v)) => Response::ok(v),
                        Ok(Err(e)) => Response::err(e),
                        Err(_elapsed) => Response::err(format!(
                            "tool call {other:?} timed out after {REQUEST_TIMEOUT:?}"
                        )),
                    },
                    // wasm32 has no `tokio` dependency to bound the call with a
                    // timeout — a single JS-driven call per handler, not an
                    // accept loop that a hung tool could block forever.
                    #[cfg(target_arch = "wasm32")]
                    Some(fut) => match fut.await {
                        Ok(v) => Response::ok(v),
                        Err(e) => Response::err(e),
                    },
                    None => Response::err(format!("unknown tool {other:?}")),
                }
            }
        }
    }

    /// Binds `sock_path` (removing any stale socket file first — safe because
    /// the caller only reaches here after winning the exclusive lock) and
    /// serves one request/response per connection until a `shutdown` call
    /// sets the flag.
    pub async fn run<S: SocketFactory>(
        &self,
        socket: &S,
        sock_path: &str,
    ) -> Result<(), PortError> {
        socket.remove_stale(sock_path).await?;
        let mut listener = socket.bind(sock_path).await?;
        loop {
            let conn = listener.accept().await?;
            self.serve_connection(conn).await?;
            if self.shutdown.get() {
                return Ok(());
            }
        }
    }

    /// Cancellation-aware sibling of `run`: identical per-connection
    /// behavior, but unblocks on `cancellation_token()` firing instead of
    /// only ever being able to return by waiting for one more connection.
    /// Lets a later phase's `Send`-bounded HTTP server share this same
    /// `!Send` daemon's accept loop lifecycle without ever calling it
    /// directly. Native-only: wasm32's `run_daemon` (crates/wasm/src/lib.rs)
    /// calls the plain `run` above, never this.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn run_cancellable<S: SocketFactory>(
        &self,
        socket: &S,
        sock_path: &str,
    ) -> Result<(), PortError> {
        socket.remove_stale(sock_path).await?;
        let mut listener = socket.bind(sock_path).await?;
        loop {
            let conn = tokio::select! {
                _ = self.cancel.cancelled() => return Ok(()),
                accept_result = listener.accept() => accept_result?,
            };
            self.serve_connection(conn).await?;
            if self.shutdown.get() {
                return Ok(());
            }
        }
    }

    /// Shared per-connection body for `run`/`run_cancellable`: apply the
    /// connection timeout, read one frame, dispatch it, write the response.
    async fn serve_connection<C: Conn>(&self, mut conn: C) -> Result<(), PortError> {
        conn.set_timeout(CONN_TIMEOUT);
        if let Some(bytes) = conn.read_frame().await? {
            let resp_bytes = self.handle_request_bytes(&bytes).await;
            conn.write_frame(&resp_bytes).await?;
        }
        Ok(())
    }
}

impl Default for Daemon {
    fn default() -> Self {
        Self::new()
    }
}

/// Wraps a typed `In -> Result<Out, String>` async function as a `Handler`
/// operating on opaque JSON, so each tool's `register` call only has to name
/// its typed signature once instead of hand-rolling marshal/unmarshal.
pub fn json_handler<In, Out, F, Fut>(f: F) -> Handler
where
    In: serde::de::DeserializeOwned + 'static,
    Out: serde::Serialize + 'static,
    F: Fn(In) -> Fut + 'static,
    Fut: std::future::Future<Output = Result<Out, String>> + 'static,
{
    Box::new(move |params| {
        // Omitted params (`None`, e.g. a caller that skips `arguments`
        // entirely for a no-fields `In`) becomes `{}`, not `null` — a
        // zero-field struct's derived `Deserialize` accepts an empty object
        // but rejects `null` outright ("invalid type: null, expected struct
        // ..."), so substituting `null` here made every empty-input tool
        // (e.g. `BrowserListSessionsInput`) fail on an omitted-params call
        // even though it needs nothing from the caller.
        match serde_json::from_value::<In>(params.unwrap_or_else(|| serde_json::json!({}))) {
            Ok(input) => {
                let fut = f(input);
                Box::pin(async move {
                    let out = fut.await?;
                    serde_json::to_value(out).map_err(|e| e.to_string())
                }) as LocalBoxFuture<'static, HandlerResult>
            }
            Err(e) => {
                let msg = format!("invalid params: {e}");
                Box::pin(async move { Err(msg) }) as LocalBoxFuture<'static, HandlerResult>
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, serde::Deserialize)]
    struct EmptyInput {}

    #[derive(Debug, serde::Serialize)]
    struct EmptyOutput {
        ok: bool,
    }

    /// Regression test for a real bug found while verifying browser-tool
    /// parity for issue #15: a caller that omits `params` entirely (as
    /// `crates/cli/tests/browser_session.rs`'s
    /// `list_sessions_should_report_open_sessions_and_close_all_sessions_should_drain_them`
    /// does for `stapler_browser_list_sessions`) used to fail with "invalid
    /// type: null, expected struct ..." for any zero-field `In`, even though
    /// the tool needs nothing from the caller.
    #[tokio::test]
    async fn json_handler_should_accept_omitted_params_for_zero_field_input() {
        let handler = json_handler(|_input: EmptyInput| async { Ok(EmptyOutput { ok: true }) });

        let result = handler(None)
            .await
            .expect("omitted params should deserialize into a zero-field input");

        assert_eq!(result, serde_json::json!({ "ok": true }));
    }

    #[tokio::test]
    async fn handle_request_should_round_trip_ping_without_bytes_serialization() {
        let daemon = Daemon::new();

        let resp = daemon
            .handle_request(Request {
                tool: PING_TOOL.to_string(),
                params: None,
            })
            .await;

        assert_eq!(resp.result, Some(serde_json::json!({"pong": true})));
        assert!(resp.error.is_none());
    }

    #[tokio::test]
    async fn handle_request_should_merge_status_extra_fields_into_ping_response() {
        let daemon = Daemon::new();
        daemon.set_status_extra(serde_json::json!({
            "browserProfileMode": "persistent at /tmp/x",
            "browserProfileWarning": serde_json::Value::Null,
        }));

        let resp = daemon
            .handle_request(Request {
                tool: PING_TOOL.to_string(),
                params: None,
            })
            .await;

        assert_eq!(
            resp.result,
            Some(serde_json::json!({
                "pong": true,
                "browserProfileMode": "persistent at /tmp/x",
                "browserProfileWarning": null,
            }))
        );
    }

    #[tokio::test]
    async fn request_shutdown_should_set_the_shutdown_flag_and_cancel_the_token() {
        let daemon = Daemon::new();

        daemon.request_shutdown();

        assert!(daemon.shutdown.get());
        assert!(daemon.cancellation_token().is_cancelled());
    }

    #[tokio::test]
    async fn shutdown_tool_dispatch_should_cancel_the_same_token_request_shutdown_does() {
        let daemon = Daemon::new();
        let token = daemon.cancellation_token();
        // Doesn't capture `daemon` itself (`!Send`) — only the cloned,
        // `Arc`-backed `CancellationToken`, which is `Send`.
        let blocked = tokio::spawn(async move {
            token.cancelled().await;
        });

        let resp = daemon
            .handle_request(Request {
                tool: SHUTDOWN_TOOL.to_string(),
                params: None,
            })
            .await;
        assert!(resp.error.is_none());

        tokio::time::timeout(Duration::from_millis(200), blocked)
            .await
            .expect("blocked task should resolve promptly after the shutdown RPC")
            .expect("blocked task should not panic");
    }

    struct NeverConn;

    impl Conn for NeverConn {
        async fn read_frame(&mut self) -> Result<Option<Vec<u8>>, PortError> {
            unreachable!("no connection is ever accepted in this test")
        }
        async fn write_frame(&mut self, _bytes: &[u8]) -> Result<(), PortError> {
            unreachable!("no connection is ever accepted in this test")
        }
        fn set_timeout(&mut self, _dur: Duration) {
            unreachable!("no connection is ever accepted in this test")
        }
    }

    struct PendingListener;

    impl Listener for PendingListener {
        type C = NeverConn;
        async fn accept(&mut self) -> Result<Self::C, PortError> {
            // Never resolves — the only way `run_cancellable`'s accept loop
            // can return is via the cancellation branch of its `select!`.
            std::future::pending().await
        }
    }

    struct PendingSocketFactory;

    impl SocketFactory for PendingSocketFactory {
        type L = PendingListener;
        type C = NeverConn;
        async fn bind(&self, _path: &str) -> Result<Self::L, PortError> {
            Ok(PendingListener)
        }
        async fn connect(&self, _path: &str, _timeout: Duration) -> Result<Self::C, PortError> {
            unreachable!("run_cancellable never connects, only binds/accepts")
        }
        async fn remove_stale(&self, _path: &str) -> Result<(), PortError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn run_cancellable_should_return_ok_promptly_when_cancelled_with_no_pending_connection() {
        let daemon = Daemon::new();
        let socket = PendingSocketFactory;

        // `tokio::join!` (not `tokio::spawn`) runs both futures concurrently
        // within this single task, without requiring `Send` — `Daemon` is
        // deliberately `!Send`.
        let cancel_once_polling_starts = async {
            tokio::task::yield_now().await;
            daemon.request_shutdown();
        };

        let (run_result, ()) = tokio::time::timeout(Duration::from_millis(200), async {
            tokio::join!(
                daemon.run_cancellable(&socket, "irrelevant"),
                cancel_once_polling_starts
            )
        })
        .await
        .expect("run_cancellable should return promptly once cancelled");

        assert!(matches!(run_result, Ok(())));
    }

    #[tokio::test]
    async fn handle_request_should_time_out_a_hung_handler_instead_of_blocking_forever() {
        let daemon = Daemon::new();
        daemon.register(
            "hang",
            Box::new(|_params| {
                Box::pin(std::future::pending()) as LocalBoxFuture<'static, HandlerResult>
            }),
        );

        let resp = tokio::time::timeout(
            REQUEST_TIMEOUT * 10,
            daemon.handle_request(Request {
                tool: "hang".to_string(),
                params: None,
            }),
        )
        .await
        .expect("handle_request should itself return once its internal timeout fires");

        let err = resp
            .error
            .expect("a hung handler should produce an error response");
        assert!(err.contains("timed out"), "unexpected error message: {err}");
    }

    #[tokio::test]
    async fn handle_request_should_still_serve_subsequent_calls_after_a_timeout() {
        let daemon = Daemon::new();
        daemon.register(
            "hang",
            Box::new(|_params| {
                Box::pin(std::future::pending()) as LocalBoxFuture<'static, HandlerResult>
            }),
        );

        let timed_out = daemon
            .handle_request(Request {
                tool: "hang".to_string(),
                params: None,
            })
            .await;
        assert!(timed_out.error.is_some());

        let resp = daemon
            .handle_request(Request {
                tool: PING_TOOL.to_string(),
                params: None,
            })
            .await;

        assert_eq!(resp.result, Some(serde_json::json!({"pong": true})));
        assert!(resp.error.is_none());
    }
}
