# Research: Stack — MCP over Streamable HTTP

Grounds itself in `project_plans/mcp-streamable-http/requirements.md`, especially the
verified finding that `rmcp`'s `StreamableHttpService` requires `S: Send`, in direct
tension with the daemon core's deliberate `!Send` design
(`crates/core/src/daemon.rs:1-6`). All claims below are sourced from direct reads of
`rmcp` 2.2.0's vendored source at
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rmcp-2.2.0` and this repo's own
`Cargo.lock`/source, not general knowledge of the crate (which may be stale for this
exact version).

## 1. Cargo features

Confirmed by reading the `[features]` block directly
(`rmcp-2.2.0/Cargo.toml:63-176`, normalized/generated form — original is
`Cargo.toml.orig`):

```
transport-streamable-http-server = [
    "transport-streamable-http-server-session",
    "server-side-http",
    "transport-worker",
]
transport-streamable-http-server-session = [
    "transport-async-rw",
    "dep:tokio-stream",
]
server-side-http = [
    "uuid",
    "dep:rand",
    "dep:tokio-stream",
    "dep:http",
    "dep:http-body",
    "dep:http-body-util",
    "dep:bytes",
    "dep:sse-stream",
    "tower",
]
transport-worker = ["dep:tokio-stream"]
tower = ["dep:tower-service"]
transport-async-rw = ["tokio/io-util", "tokio-util/codec"]
```

So enabling a single feature, `transport-streamable-http-server`, transitively pulls in
all three of `transport-streamable-http-server-session`, `server-side-http`, and
`transport-worker` — exactly the four features named in the requirements doc's research
question. No need to list them individually in `crates/cli/Cargo.toml`; one feature
flag suffices. Net new transitive deps introduced (not currently used anywhere in the
workspace, confirmed against `Cargo.lock` — see §3): `uuid`, `rand`, `sse-stream`,
`http`/`http-body`/`http-body-util`/`bytes` (these four are already present in the lock
transitively via `reqwest`, see §3), `tower-service`, `tokio-stream`.

Current `crates/cli/Cargo.toml:14`: `rmcp = { version = "2", features = ["server",
"macros", "transport-io", "schemars"] }` — needs `"transport-streamable-http-server"`
added to that list.

## 2. Does rmcp provide axum integration, or is it BYO hyper/axum?

**BYO.** `rmcp` does not depend on `axum` as a library dependency — `axum` appears only
in `rmcp`'s own `[dev-dependencies]` (`rmcp-2.2.0/Cargo.toml.orig:191`, used for the
crate's own test suite) and nowhere in `[dependencies]`. Confirmed by grepping `axum`
across `rmcp-2.2.0/src`: the only non-test hits are two doc-comment mentions in
`tower.rs` (lines 404, 524-526) describing how to *use* it, not importing it.

What `rmcp` actually exposes is a plain `tower_service::Service`:

- `StreamableHttpService<S, M>` (`rmcp-2.2.0/src/transport/streamable_http_server/tower.rs:547-558`)
  implements `tower_service::Service<http::Request<RequestBody>>`
  (`tower.rs:571-595`), with:
  ```rust
  impl<RequestBody, S, M> tower_service::Service<Request<RequestBody>> for StreamableHttpService<S, M>
  where
      RequestBody: Body + Send + 'static,
      S: crate::Service<RoleServer> + Send + 'static,
      M: SessionManager,
      RequestBody::Error: Display,
      RequestBody::Data: Send + 'static,
  {
      type Response = BoxResponse;   // = Response<BoxBody<Bytes, Infallible>>
      type Error = Infallible;
      ...
  }
  ```
  (`tower.rs:579-580`; `BoxResponse` alias at
  `rmcp-2.2.0/src/transport/common/server_side_http.rs:22`.) This confirms the
  requirements doc's cited `Send` bound verbatim, at the same line range (571-577).

- Constructed via `StreamableHttpService::new(service_factory, session_manager,
  config)` (`tower.rs:631-648`), where `service_factory: impl Fn() -> Result<S,
  std::io::Error> + Send + Sync + 'static`. **Important, not called out in the
  requirements doc**: the factory is called once *per session* (`get_service()` at
  `tower.rs:649-651`, invoked from the POST/init handling path) — `StreamableHttpService`
  does not hold one shared `S` instance; it manufactures a fresh `S` for every new
  session. Any design that bridges to shared daemon state has to route through
  something *outside* `S` (e.g. a cloned channel handle captured by the factory
  closure), not rely on `S` itself accumulating state across sessions.

- `session_manager: Arc<M>` where `M: SessionManager` — `LocalSessionManager`
  (`rmcp-2.2.0/src/transport/streamable_http_server/session/local.rs:32-35`) is the
  provided in-memory implementation, keyed by `SessionId` with a `tokio::sync::RwLock<
  HashMap<SessionId, LocalSessionHandle>>`. Despite the name, "Local" refers to
  in-memory/single-process session storage, **not** `!Send`/`LocalSet` affinity — it's
  itself `Send + Sync` (`RwLock`-guarded `HashMap`, no `Rc`/`RefCell`).

- Binding to a listener: rmcp's own tests show the pattern —
  `rmcp-2.2.0/tests/test_streamable_http_connection_reuse.rs:66-77`:
  ```rust
  let service: StreamableHttpService<SumServer, LocalSessionManager> = StreamableHttpService::new(
      || Ok(SumServer::new()),
      Default::default(),
      StreamableHttpServerConfig::default()...,
  );
  let router = axum::Router::new().nest_service("/mcp", service);
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  axum::serve(listener, router).with_graceful_shutdown(...).await;
  ```
  `axum::Router::nest_service` accepts anything implementing `tower_service::Service`
  with a compatible body, so `StreamableHttpService` mounts directly — but the caller
  supplies `axum` (or raw `hyper` + a manual accept loop) as its own dependency. All ten
  `test_streamable_http_*.rs` files under `rmcp-2.2.0/tests/` follow this same
  axum-`Router`-plus-`axum::serve` pattern; none use bare hyper directly for the
  *server* side (bare `hyper` does appear, but only inside `client-side` reqwest-adjacent
  test helpers, e.g. `src/transport/auth.rs`).

**Confirms**: the daemon binary will need a direct `axum` dependency (currently absent
from the workspace entirely, see §3) unless the plan phase chooses to write a manual
`hyper::server` accept loop against the raw `tower_service::Service` — extra ceremony
for no benefit given `rmcp`'s own tests establish axum as the idiomatic pairing.

## 3. `!Send` bridge: realistic integration shape

### The constraint is unconditional, not a runtime-flavor artifact

A subtlety worth flagging precisely because it changes which "obvious" workaround is
actually available: the `Send` bound on `S` is not just the trait bound on
`StreamableHttpService::new` — it's *exercised* internally. `spawn_session_worker`
(`tower.rs:660-693`) drives each session with a bare `tokio::spawn`:

```rust
fn spawn_session_worker(..., service: S, transport: M::Transport, ...) {
    tokio::spawn(async move {
        let svc = serve_server::<S, M::Transport, _, TransportAdapterIdentity>(service, transport).await;
        ...
    });
}
```

`tokio::spawn` (as opposed to `tokio::task::spawn_local`/`LocalSet::spawn_local`)
requires `F: Send + 'static` **regardless of whether the runtime is `current_thread` or
`multi_thread`** — this is a property of the `tokio::spawn` API surface itself, not a
scheduler choice. So switching the daemon's own runtime flavor, or wrapping the daemon
core in a `LocalSet`, does not relax this: `rmcp` has already committed to `tokio::spawn`
internally, so `S` must be `Send` no matter what runtime the *caller* uses. This rules
out any design where the existing `!Send` `Daemon`/`Rc<RefCell<...>>` core is passed
directly as `S` — confirmed, not merely inferred from the trait bound alone.

### Wasm-shared-code constraint: confirmed still live (answers an Open Question)

The requirements doc flags as an open question whether the `!Send`/wasm-shared-code
rationale in `daemon.rs`'s doc comment is "still load-bearing" or whether `crates/wasm`
has diverged enough that relaxing `!Send` in the native daemon specifically would be
free. Direct read of `crates/wasm/src/lib.rs:17,52`:

```rust
use stapler_mcp_core::daemon::{json_handler, Daemon};
...
let daemon = Daemon::new();
```

confirms `crates/wasm` directly instantiates and depends on the exact same `Daemon`
struct from `crates/core::daemon` (not a divergent copy). Relaxing `Daemon`'s internal
`!Send` state (e.g. `Rc<RefCell<...>>` → `Arc<Mutex<...>>`) would still compile for wasm
(wasm-bindgen doesn't care about `Send`), but it is not "free": it would mean every
`Handler` closure and every `NativeBrowser`/embedder/etc. state cell pays
`Arc`/`Mutex` overhead and lock-ordering risk on the native path even though the daemon
is still fundamentally single-threaded there, purely to satisfy `rmcp`'s HTTP-transport
requirement. This is real evidence for planning, not a guess — but it's a cost/tradeoff
call, not a blocking fact by itself.

### Recommended shape: mirror the existing thin-client/daemon channel boundary in-process

The existing architecture already draws exactly this kind of boundary: `ThinClient`
(`Send`-compatible, implements `rmcp::ServerHandler`, runs over stdio) talks to the
`!Send` `Daemon` core over a Unix-socket round trip
(`crates/core/src/client.rs::call`, `crates/core/src/protocol.rs` framing). The
Send/`!Send` seam already exists at the process boundary; Streamable HTTP just needs
that same seam pulled in-process:

- **Recommended: bounded `tokio::mpsc` channel from `Send` HTTP-session tasks into the
  `!Send` `LocalSet`-confined `Daemon` worker.** Concretely: keep running the daemon's
  `current_thread` + `LocalSet` runtime exactly as today
  (`crates/cli/src/main.rs:32-36`). Add one long-lived task spawned via
  `local.spawn_local(...)` that owns the `Daemon` (or the same handler registry) and
  reads `(Request, oneshot::Sender<Response>)` off an `mpsc::Receiver`. The
  `StreamableHttpService`'s `service_factory` closure (§2) captures a `Send`-cloneable
  `mpsc::Sender` and builds a small `Send`-compatible `ServerHandler` (structurally the
  same shape as today's `ThinClient`, minus the socket dial) that, per tool call,
  sends `(Request, oneshot::Sender<Response>)` and awaits the `oneshot::Receiver`. This
  is the smallest structural change from what exists today: it reuses
  `Daemon::handle_request_bytes`/`dispatch` completely unmodified, reuses the
  `ThinClient`-shaped `#[tool_router]` boilerplate (relevant to the "duplicated
  tool-registration" rabbit hole in requirements.md), and needs exactly one new
  runtime primitive (the mpsc bridge task) rather than a second runtime. It also
  answers the "session semantics" open question directly: each HTTP session's
  `Send` handler is stateless itself (just a channel sender clone) so
  `transport-streamable-http-server-session`'s per-client SSE session bookkeeping never
  touches `!Send` state — only the single bridge task does, serialized through the
  channel, matching how the daemon already serializes concurrent Unix-socket clients
  today (one accept loop, one connection processed at a time,
  `crates/core/src/daemon.rs::run:87-97`).

- **Rejected: two separate tokio runtimes with a channel between them.** Strictly more
  moving parts than the single-runtime `mpsc`-bridge above for no additional
  capability — `LocalSet::spawn_local` on the *existing* `current_thread` runtime
  already gives the `!Send` worker task a place to live without a second runtime's
  lifecycle (startup, shutdown coordination, panics-cross-runtime handling) to manage.
  A second runtime would only be justified if the HTTP-serving side needed genuine
  multi-core parallelism, which the requirements doc's non-functional section
  explicitly disclaims ("a handful of concurrent local sessions ... not a
  multi-tenant/high-concurrency server").

- **Rejected (for now): `Arc<Mutex<...>>`/`Arc<tokio::sync::Mutex<...>>` rewrite of the
  core.** Would let a single `S` be genuinely `Send` and shared directly (no channel,
  no bridge task), which is architecturally simpler in the abstract — but per the
  confirmed-live wasm-sharing constraint above, this is not a free relaxation; it's a
  real invasive rewrite of every `Rc<RefCell<...>>` state cell in `crates/core`
  (`NativeBrowser`, `Daemon::handlers`/`shutdown`, and whatever else `crates/native`
  holds `Rc`-style) plus lock-ordering review across all 23 tool handlers, for a
  benefit (avoiding one mpsc channel and one bridge task) that's small next to that
  cost. Plan phase should still record this as the rejected alternative with this
  specific reasoning, since it's the most natural-looking option and the item's own
  framing ("should be largely a transport-layer swap") suggests whoever filed it may
  have assumed this was available for free.

This is a design recommendation for the plan phase, not an implementation — the
mpsc-bridge shape above has not been prototyped in this repo; treat the exact channel
message type, backpressure/bound size, and shutdown-ordering (bridge task vs. `Daemon`'s
own `shutdown: Rc<Cell<bool>>` flag) as implementation-spike unknowns, not settled
facts.

## 4. Crate versions: already in `Cargo.lock` vs. net-new

Checked directly against this repo's `Cargo.lock` (not assumed from `rmcp`'s own
`Cargo.toml.orig` versions):

| Crate | In `Cargo.lock` today? | Version if present | Source |
|---|---|---|---|
| `hyper` | Yes | 1.10.1 | `Cargo.lock:1489-1491` |
| `hyper-util` | Yes | 0.1.20 | `Cargo.lock:1541-1543` |
| `tower` | Yes | 0.5.3 | `Cargo.lock:3999-4001` |
| `tower-http` | Yes | 0.6.11 | `Cargo.lock:4014-4016` |
| `tower-service` | Yes | 0.3.3 | `Cargo.lock:4038-4040` |
| `http` / `http-body` / `http-body-util` | Yes | 1.4.2 / 1.1.0 / 0.1.4 | `Cargo.lock:1441,1451,1461` |
| `bytes` | Yes | 1.12.1 | `Cargo.lock:337-339` |
| `axum` | **No** | — net new | not present anywhere in `Cargo.lock` |
| `sse-stream` | **No** | — net new (rmcp's dep, `dep:sse-stream` under `server-side-http`) | not present |
| `uuid` | **No** | — net new (rmcp's dep, `server-side-http` requires it) | not present |
| `rand` | Present (0.9.5 and 0.10.2, two versions) | already resolved, likely satisfies rmcp's `dep:rand` | `Cargo.lock:2755,2765` |

All of `hyper`/`hyper-util`/`tower`/`tower-http`/`tower-service`/`http*`/`bytes` are
already fully resolved in the lockfile — but **transitively**, via `reqwest` (declared
only in `crates/native/Cargo.toml:11`: `reqwest = { version = "0.13", features =
["rustls"], default-features = false }`, whose locked dependency list at
`Cargo.lock:2987-3027` for the `0.12.28` resolution — note two `reqwest` versions
(`0.12.28`, `0.13.4`) are in the lock, presumably from differing native/wasm resolution
— lists `tower`, `tower-http`, `tower-service`, `hyper`, `hyper-util`, `http`,
`http-body`, `http-body-util`, `bytes` directly). No crate in the workspace currently
declares `tower`, `tower-http`, or `hyper` directly in a `[dependencies]` block
(confirmed: `grep -rn "tower-http\|tower_http" --include=Cargo.toml .` returns no hits
in any workspace-member `Cargo.toml`). Practically: adding `rmcp`'s
`transport-streamable-http-server` feature plus a direct `axum` dependency should
resolve to compatible/identical versions for everything except `axum`, `sse-stream`, and
`uuid` (net-new downloads), since Cargo's resolver will prefer the versions already
pinned in the lockfile where semver allows. This needs confirming with an actual `cargo
update -p rmcp --precise ...`/`cargo build` run during the implementation spike — not
claimed as verified here since no `Cargo.toml` edit or resolver run was performed for
this research task.

**tokio features**: current `crates/cli/Cargo.toml:13`: `tokio = { version = "1",
features = ["rt", "macros", "time", "net", "io-util", "sync"] }`. `server-side-http`'s
transitive `transport-async-rw` feature requires `tokio/io-util` (already present) and
`tokio-util/codec`; `transport-streamable-http-server-session`/`transport-worker`
require `dep:tokio-stream` (an `rmcp`-internal dep, not something `crates/cli` itself
needs to add to its own `tokio` feature list). No `tokio/rt-multi-thread` is required by
anything found above — reinforcing §3's point that the `Send` bound is independent of
runtime-flavor choice.

## 5. Bearer-token auth: idiomatic placement

`tower-http` 0.6.11 is already resolvable in the lock (§3) and provides exactly this
primitive, confirmed by reading its source directly
(`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tower-http-0.6.11`):

- Feature `auth = ["base64", "validate-request"]`
  (`tower-http-0.6.11/Cargo.toml:70-73`) gates
  `tower_http::validate_request::ValidateRequestHeaderLayer`
  (`src/validate_request.rs`) and the `Bearer` validator
  (`src/auth/require_authorization.rs`).
- `ValidateRequestHeaderLayer::bearer(token: &str) -> Self` is a real constructor
  (`require_authorization.rs:124-129`) and `ValidateRequestHeaderLayer<T>` implements
  `tower::Layer<S>` (`validate_request.rs:178`), so it composes via
  `ServiceBuilder::new().layer(ValidateRequestHeaderLayer::bearer(&token)).service(...)`
  — exactly the "does it fit as a `tower::Layer`" question, answered yes.

**One real friction point, verified, not hypothetical**: `ValidateRequestHeaderLayer::
bearer` is generic over the wrapped service's response body type and requires `ResBody:
Default` (`require_authorization.rs:108-113,124-129`) — it constructs a `401`
rejection response using `ResBody::default()`. `StreamableHttpService`'s own response
type is `BoxResponse = Response<BoxBody<Bytes, Infallible>>`
(`rmcp-2.2.0/src/transport/common/server_side_http.rs:22`, `http_body_util`'s
`BoxBody`), and `http_body_util::combinators::BoxBody` does not implement `Default` (it
wraps a boxed trait object with no zero-value construction) — so applying
`ValidateRequestHeaderLayer::bearer` **directly** around the `StreamableHttpService`
tower `Service` will not typecheck as written.

The working integration point instead: apply the layer at the **`axum::Router`** level
(`Router::layer(...)`, or equivalently `axum::middleware::from_fn` with a small custom
check), since `axum`'s own body type (`axum::body::Body`) does implement `Default`, and
`Router::nest_service("/mcp", streamable_http_service)` is already the established
mounting pattern (§2). This wasn't independently verified against `axum`'s own source
(not vendored locally — `find ~/.cargo/registry/src -maxdepth 1 -iname "axum-*"`
returned nothing, confirming it's genuinely net-new to this machine's cache, consistent
with §3's "not in `Cargo.lock`" finding) — flagging this specific sub-claim
(`axum::body::Body: Default`) as **needs confirmation during the implementation spike**,
not verified from primary source, though it matches well-established public `axum`
behavior.

Either way, the auth layer belongs in front of/around the whole `axum::Router` (or at
minimum wrapping the `/mcp` route specifically), not woven into the `!Send` bridge or
the `ServerHandler` implementation itself — keeps auth rejection a pure HTTP-layer
concern, consistent with the observability requirement to log rejected auth attempts
distinctly from tool-call errors (a `Router`-level layer is also the natural place to
emit that log line, since it runs before any session/tool-dispatch logic).

## Summary of what still needs an implementation-spike, not settled here

- Exact `mpsc` channel message shape and backpressure bound for the `!Send` bridge
  (§3) — no prototype was built for this research task.
- Confirming `cargo build` actually resolves `hyper`/`tower`/etc. to the already-locked
  versions once `axum` + `rmcp`'s new feature are added, rather than forcing an upgrade
  cascade (§3 — resolver behavior wasn't executed).
- `axum::body::Body: Default` (§5) — inferred from public `axum` knowledge, not read
  from vendored source (not present locally).
- Whether `LocalSessionManager`'s per-session `Arc<...>`/`RwLock` bookkeeping introduces
  any lock contention against the `!Send` bridge task under concurrent sessions — the
  requirements doc's own concurrency success metric (≥2 concurrent HTTP sessions against
  a stateful tool) needs an actual test against the recommended shape, not just this
  design read-through.
