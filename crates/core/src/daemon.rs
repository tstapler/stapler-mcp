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

use crate::ports::{Conn, Listener, PortError, SocketFactory};
use crate::protocol::{Request, Response, PING_TOOL, SHUTDOWN_TOOL};

pub type HandlerResult = Result<serde_json::Value, String>;
pub type Handler = Box<dyn Fn(Option<serde_json::Value>) -> LocalBoxFuture<'static, HandlerResult>>;

const CONN_TIMEOUT: Duration = Duration::from_secs(120);

pub struct Daemon {
    handlers: RefCell<HashMap<&'static str, Handler>>,
    shutdown: Rc<Cell<bool>>,
}

impl Daemon {
    pub fn new() -> Self {
        Daemon {
            handlers: RefCell::new(HashMap::new()),
            shutdown: Rc::new(Cell::new(false)),
        }
    }

    pub fn register(&self, name: &'static str, handler: Handler) {
        self.handlers.borrow_mut().insert(name, handler);
    }

    /// Pure function: JSON request bytes in, JSON response bytes out. No
    /// socket, no framing — the easiest layer to unit test.
    pub async fn handle_request_bytes(&self, bytes: &[u8]) -> Vec<u8> {
        let resp = match serde_json::from_slice::<Request>(bytes) {
            Ok(req) => self.dispatch(req).await,
            Err(e) => Response::err(format!("invalid request: {e}")),
        };
        // A Response is always representable as JSON; unwrap is safe.
        serde_json::to_vec(&resp).expect("Response always serializes")
    }

    async fn dispatch(&self, req: Request) -> Response {
        match req.tool.as_str() {
            PING_TOOL => Response::ok(serde_json::json!({"pong": true})),
            SHUTDOWN_TOOL => {
                self.shutdown.set(true);
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
            let mut conn = listener.accept().await?;
            conn.set_timeout(CONN_TIMEOUT);
            if let Some(bytes) = conn.read_frame().await? {
                let resp_bytes = self.handle_request_bytes(&bytes).await;
                conn.write_frame(&resp_bytes).await?;
            }
            if self.shutdown.get() {
                return Ok(());
            }
        }
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
        match serde_json::from_value::<In>(params.unwrap_or(serde_json::Value::Null)) {
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
