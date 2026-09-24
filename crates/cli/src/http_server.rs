//! MCP Streamable HTTP transport: an axum `Router` that mounts rmcp's
//! `StreamableHttpService` at `/mcp`, backed by a fresh `McpRouter<ChannelTransport>`
//! per HTTP session. Each session's tool calls cross the bridge channel into
//! the same `!Send` `Daemon` the Unix-socket path uses (see `main.rs`'s
//! `run_daemon` bridge consumer) — this module never touches `Rc`/`RefCell`
//! state directly, which is what lets it be `Send` and run on a plain
//! `tokio::task::spawn`.
//!
//! Graceful shutdown: `run_http_server` takes the daemon's
//! `CancellationToken` and passes it to axum's `with_graceful_shutdown`, so
//! SIGTERM/`SHUTDOWN_TOOL` (which both cancel that token — see
//! `Daemon::request_shutdown`) stop this server as part of the same joined
//! shutdown sequence `main.rs`'s `run_daemon` drives. Auth is a single shared
//! bearer token (see `require_bearer_token`): every request to `/mcp` must
//! present `Authorization: Bearer <token>`, checked in constant time via
//! `stapler_mcp_core::http_auth::BearerToken`'s `PartialEq`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::json;
use stapler_mcp_core::http_auth::BearerToken;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::mcp_router::McpRouter;
use crate::transport::{BridgeMessage, ChannelTransport};

/// Binds `127.0.0.1:{port}` and serves MCP-over-HTTP at `/mcp` until either
/// the listener errors or `cancel` fires. A bind failure (e.g. `AddrInUse`)
/// is never fatal to the daemon as a whole — the caller logs it and keeps
/// running stdio/socket transports, so this returns `Err` rather than
/// exiting the process.
pub async fn run_http_server(
    bridge_tx: mpsc::Sender<BridgeMessage>,
    port: u16,
    token: BearerToken,
    cancel: CancellationToken,
) -> Result<(), std::io::Error> {
    let router = build_router(bridge_tx, token);

    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!("stapler-mcp: HTTP port {port} already in use — continuing with stdio/socket transport only");
            return Err(e);
        }
        Err(e) => {
            eprintln!("stapler-mcp: HTTP transport failed to bind 127.0.0.1:{port}: {e}");
            return Err(e);
        }
    };

    // `into_make_service_with_connect_info` is what populates the
    // `ConnectInfo<SocketAddr>` extension `log_http_connection` reads —
    // without it that extractor always sees `None`.
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move { cancel.cancelled_owned().await })
    .await
}

fn build_router(bridge_tx: mpsc::Sender<BridgeMessage>, token: BearerToken) -> axum::Router {
    let service = build_mcp_service(bridge_tx);
    // Layers added later wrap outside layers added earlier (last `.layer()`
    // call = outermost = first to see the request). `log_http_connection` is
    // added last so it wraps `require_bearer_token`: it must run for every
    // request, including ones the auth layer rejects, and it needs the
    // *final* response (401 or otherwise) to log an accurate status.
    //
    // The auth layer must wrap the whole `Router`, not the bare
    // `StreamableHttpService` passed to `nest_service` — `from_fn_with_state`
    // produces a `tower::Layer` whose body type has friction with
    // `StreamableHttpService`'s own `Service` impl when applied directly to
    // it. Layering at the `Router` level (outside-in for requests) means
    // this middleware runs before the request ever reaches `/mcp`.
    axum::Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(token, require_bearer_token))
        .layer(middleware::from_fn(log_http_connection))
}

/// Assigns each incoming request a monotonically increasing correlation id
/// and logs its start/end (with status and elapsed time) to stderr. Uses a
/// plain counter rather than a UUID — this repo doesn't otherwise depend on
/// the `uuid` crate, and uniqueness within one daemon's log file is all that
/// matters here. Requires `ConnectInfo<SocketAddr>` (populated by
/// `run_http_server`'s `into_make_service_with_connect_info`, not by this
/// `Router` itself); tests that drive the router directly via `oneshot`
/// insert the extension manually rather than going through a real TCP accept
/// loop — see `test_request`'s `extensions_mut().insert(...)` below.
/// (`Option<ConnectInfo<T>>` isn't usable here: axum only auto-derives an
/// `Option`-wrapped extractor for types implementing `OptionalFromRequestParts`,
/// which `ConnectInfo` does not.)
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

async fn log_http_connection(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let peer = addr.to_string();

    // Deliberately logs only the id and peer address here — never headers or
    // the request body, which is where a bearer token could leak (see
    // `require_bearer_token`'s own comment on the same concern).
    eprintln!("stapler-mcp: http request start {id} from {peer}");
    let start = Instant::now();

    let response = next.run(req).await;

    let elapsed_ms = start.elapsed().as_millis();
    let status = response.status();
    eprintln!("stapler-mcp: http request end {id} status={status} elapsed_ms={elapsed_ms}");

    response
}

/// Rejects any request that doesn't present `Authorization: Bearer <token>`
/// matching the daemon's generated token. The two rejection reasons ("missing
/// header" vs. "wrong/invalid token") produce textually distinct log lines
/// and response bodies — deliberately, so they're distinguishable in tests
/// and operator logs. Never logs the presented (possibly attacker-supplied)
/// token value.
async fn require_bearer_token(
    State(token): State<BearerToken>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let Some(header_value) = headers.get(axum::http::header::AUTHORIZATION) else {
        eprintln!(
            "stapler-mcp: rejected unauthenticated HTTP request — missing Authorization header"
        );
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "missing Authorization header" })),
        )
            .into_response();
    };

    let presented = header_value
        .to_str()
        .ok()
        .and_then(|v| v.strip_prefix("Bearer "));

    let matches = presented.is_some_and(|p| BearerToken::new(p.to_string()) == token);

    if !matches {
        eprintln!("stapler-mcp: rejected unauthenticated HTTP request — invalid bearer token");
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "invalid bearer token" })),
        )
            .into_response();
    }

    next.run(req).await
}

fn build_mcp_service(
    bridge_tx: mpsc::Sender<BridgeMessage>,
) -> StreamableHttpService<McpRouter<ChannelTransport>, LocalSessionManager> {
    let service_factory = move || {
        Ok(McpRouter::with_transport(ChannelTransport::new(
            bridge_tx.clone(),
        )))
    };

    // `StreamableHttpServerConfig` is `#[non_exhaustive]`, so it can't be
    // built with struct-update syntax outside rmcp's own crate — start from
    // `Default` and override the two fields this phase cares about.
    let mut config = StreamableHttpServerConfig::default();
    config.stateful_mode = false;
    config.json_response = true;

    StreamableHttpService::new(
        service_factory,
        Arc::new(LocalSessionManager::default()),
        config,
    )
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use tower::ServiceExt;

    use super::*;

    fn test_bridge() -> mpsc::Sender<BridgeMessage> {
        let (bridge_tx, _bridge_rx) =
            mpsc::channel::<BridgeMessage>(crate::transport::BRIDGE_CHANNEL_CAPACITY);
        bridge_tx
    }

    /// `oneshot`-driven tests bypass the real TCP accept loop, so unlike
    /// production traffic (see `run_http_server`) nothing populates
    /// `ConnectInfo<SocketAddr>` automatically — `log_http_connection`
    /// requires it, so tests insert a fake one themselves.
    fn with_fake_connect_info(mut request: axum::http::Request<Body>) -> axum::http::Request<Body> {
        let peer: SocketAddr = "127.0.0.1:9999".parse().expect("parse test peer addr");
        request.extensions_mut().insert(ConnectInfo(peer));
        request
    }

    #[test]
    fn build_router_should_construct_without_panicking_given_a_fake_bridge_tx() {
        let router = build_router(test_bridge(), BearerToken::new("test-token".to_string()));

        // Constructing the router is the whole assertion here: `nest_service`
        // panics at call time (not just type-check time) if the service's
        // trait bounds don't line up with what axum expects, so reaching this
        // point proves the `StreamableHttpService` really does satisfy
        // `nest_service`'s `Service<Request, Error = Infallible>` bound.
        let _: axum::Router = router;
    }

    #[test]
    fn build_mcp_service_should_use_stateless_json_response_config() {
        let service = build_mcp_service(test_bridge());

        assert!(!service.config.stateful_mode);
        assert!(service.config.json_response);
    }

    #[tokio::test]
    async fn require_bearer_token_should_call_next_when_correct_token_presented() {
        let router = build_router(test_bridge(), BearerToken::new("correct-token".to_string()));
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("authorization", "Bearer correct-token")
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#))
            .expect("build request");
        let request = with_fake_connect_info(request);

        let response = router.oneshot(request).await.expect("call router");

        assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn require_bearer_token_should_return_401_when_authorization_header_missing() {
        let router = build_router(test_bridge(), BearerToken::new("correct-token".to_string()));
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .body(Body::empty())
            .expect("build request");
        let request = with_fake_connect_info(request);

        let response = router.oneshot(request).await.expect("call router");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn require_bearer_token_should_return_401_when_token_is_wrong() {
        let router = build_router(test_bridge(), BearerToken::new("correct-token".to_string()));
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("authorization", "Bearer wrong-token")
            .body(Body::empty())
            .expect("build request");
        let request = with_fake_connect_info(request);

        let response = router.oneshot(request).await.expect("call router");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn log_http_connection_should_assign_increasing_correlation_ids_across_requests() {
        let router = build_router(test_bridge(), BearerToken::new("correct-token".to_string()));

        let make_request = || {
            with_fake_connect_info(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("authorization", "Bearer correct-token")
                    .header("accept", "application/json, text/event-stream")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#))
                    .expect("build request"),
            )
        };

        let id_before = NEXT_REQUEST_ID.load(Ordering::Relaxed);
        let response = router
            .clone()
            .oneshot(make_request())
            .await
            .expect("call router");
        let id_after_first = NEXT_REQUEST_ID.load(Ordering::Relaxed);
        let _response2 = router
            .clone()
            .oneshot(make_request())
            .await
            .expect("call router");
        let id_after_second = NEXT_REQUEST_ID.load(Ordering::Relaxed);

        // The middleware consumed exactly one id per request, and never
        // reused one: the counter strictly increases across calls.
        assert!(id_after_first > id_before);
        assert!(id_after_second > id_after_first);
        assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
