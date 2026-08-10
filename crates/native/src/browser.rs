use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use chromiumoxide::cdp::browser_protocol::accessibility::{EnableParams, GetPartialAxTreeParams};
use chromiumoxide::cdp::browser_protocol::dom::{BackendNodeId, DescribeNodeParams, ResolveNodeParams};
use chromiumoxide::cdp::browser_protocol::page::{EventFrameNavigated, EventFrameRequestedNavigation};
use chromiumoxide::cdp::browser_protocol::target::EventTargetCrashed;
use chromiumoxide::cdp::js_protocol::runtime::{CallArgument, CallFunctionOnParams};
use chromiumoxide::{Browser, BrowserConfig, Page};
use futures::StreamExt;

use stapler_mcp_core::ports::{
    AxSnapshot, BrowserDriver, ClockPort, Locator, NavigateResult, PageExtract, PortError,
    SessionId, SleepPort,
};
use stapler_mcp_core::tools::webcrawl::{blocked_host_reason, NetworkPolicy};
use url::Url;

use crate::ax;

/// How long a session may sit idle (no `navigate`/`click`/`type_text`/`snapshot`
/// call bumping its `last_used`) before `SessionIdleReaper` closes its tab and
/// evicts it from the registry.
const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Hard cap on concurrently open sessions. Without this, a caller can spawn
/// unbounded tabs/pages faster than the 300s idle reaper can catch up,
/// exhausting the underlying browser process (DoS). Mirrors the spirit of
/// `ax::MAX_SNAPSHOT_NODES` — a generous but finite ceiling, not a tuned
/// capacity limit.
const MAX_OPEN_SESSIONS: usize = 20;

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as u64
}

/// Actionable error text for an id with no live entry in the session map —
/// either never issued or already evicted (reaper, crash, or explicit close).
/// Deliberately depends on nothing but `id`, so a reaper-evicted session's
/// error text is byte-identical to a never-issued session's (UX AC #10: the
/// caller shouldn't be able to tell "timed out" from "never existed" apart,
/// since both have the exact same fix). Wording matches `research/ux.md`
/// Error 1 exactly (Task 3.2.1/3.3.1/5.1.3) so navigate/click/type's verbatim
/// passthrough and snapshot's reconstruction produce byte-identical text.
fn not_found_message(id: &str) -> String {
    format!("no active browser session named '{id}'; call stapler_browser_navigate to start a new session")
}

fn crashed_message(id: &str) -> String {
    format!(
        "browser session \"{id}\" crashed — call stapler_browser_navigate (without sessionId) to start a fresh session"
    )
}

/// Minimal state the session-registry bookkeeping (idle reaper, TOCTOU-safe
/// lookup, crash detection) needs from a map entry. Factored out of the
/// concrete `BrowserSession` so this bookkeeping is unit-testable without a
/// real `chromiumoxide::Page` — chromiumoxide only ever constructs a `Page`
/// via a live CDP handshake with a running Chromium process, so production
/// code uses `BrowserSession` and tests substitute a `FakeSession`.
trait SessionState {
    fn last_used(&self) -> u64;
    fn set_last_used(&self, now: u64);
    fn crashed(&self) -> bool;
    /// Wired up to a real CDP crash listener in Epic 3, Story 3.2; exercised
    /// directly by `tests::target_crashed_handler_*` until then.
    #[allow(dead_code)]
    fn set_crashed(&self);
    /// True while some other in-flight call currently holds this session's
    /// per-session lock (see `BrowserSession::lock`). `reap_expired` must
    /// never evict a busy session even if its `last_used` looks stale — the
    /// in-flight call hasn't reached its own `last_used` bump yet, so a long
    /// call would otherwise be reaped out from under itself (TOCTOU). Default
    /// `false` for `FakeSession`, which has no real lock to check.
    fn is_busy(&self) -> bool {
        false
    }
}

/// Extends `SessionState` with the ability to consume `self` and close the
/// underlying resource — split out from `SessionState` because the reaper
/// needs to `.await` this *after* dropping the map borrow that removed the
/// entry (see `reap_expired`), while the TOCTOU-safe lookup path
/// (`touch_or_evict`) never needs to close anything itself.
trait CloseableSession: SessionState {
    async fn close(self);
}

/// A single persistent browser tab, keyed by `SessionId` in
/// `NativeBrowser::sessions`.
struct BrowserSession {
    page: Page,
    /// Millis-since-epoch of the start of the most recent
    /// `navigate`/`click`/`type_text`/`snapshot` call against this session —
    /// bumped synchronously, before any `.await`, so an in-flight call is
    /// never mistaken for idle by a concurrent reaper scan (Story 2.3).
    last_used: Cell<u64>,
    /// Refs from the most recent `AxSnapshot` returned for this session,
    /// resolved by `resolve_locator_impl` (Epic 3, Story 3.1). `Rc`-wrapped
    /// (not a bare `RefCell`) so a short synchronous `self.sessions.borrow()`
    /// critical section can clone a handle to this field, drop the map
    /// borrow, then read/write it across `.await` points without holding the
    /// map borrow live.
    latest_refs: Rc<RefCell<HashMap<String, ax::ResolvedRef>>>,
    /// Every ref string ever issued for this session, tagged with the
    /// `nav_generation` it was issued under — lets `resolve_locator_impl`
    /// distinguish "never issued" from "issued, but the page has since
    /// navigated" (Task 3.1.3).
    known_refs: Rc<RefCell<HashMap<String, u64>>>,
    /// Bumped by 1 on every `navigate` call that *reuses* this session (never
    /// on the session's first navigate, and never on `click`/`type_text`/
    /// `snapshot`). Refs issued before a bump are "stale" once the bump
    /// happens.
    nav_generation: Rc<Cell<u64>>,
    /// Session-scoped monotonic counter backing every `ref` string minted for
    /// this session's `AxSnapshot`s — never reset, so a ref string is never
    /// reused even across re-navigations (Task 3.1.1).
    next_ref_id: Rc<Cell<u64>>,
    /// The URL of the most recently installed `AxSnapshot`, used to build
    /// "no element with ref ... (page: {url})" error text without an extra
    /// CDP round trip.
    latest_url: Rc<RefCell<String>>,
    /// Set by the `Page.frameNavigated` listener (Epic 3, Story 3.4) when an
    /// in-page navigation (link click, JS redirect, form submit, ...) lands
    /// on a host the SSRF guard would have blocked at `navigate` time. Every
    /// `BrowserDriver` method checks this on entry (and `click`/`type_text`
    /// additionally poll it for a short grace period right after dispatch,
    /// so a same-call navigation is caught before returning) and cleared back
    /// to `None` at the start of every `navigate` call (the caller's
    /// documented recovery path).
    blocked: Rc<RefCell<Option<String>>>,
    /// Set by the `Target.targetCrashed` listener registered when the
    /// session's page is created (Epic 3, Story 3.2); checked by every
    /// `BrowserDriver` method before use (Story 2.5).
    crashed: Rc<Cell<bool>>,
    /// Serializes every session-mutating `BrowserDriver` call
    /// (`navigate`/`click`/`type_text`/`snapshot`) against this session's
    /// `Page`. Held for the full duration of the call (across all its
    /// `.await` points), so two concurrent calls against the same
    /// `sessionId` can never interleave CDP round-trips against a shared
    /// `Page`/ref maps. Also doubles as the `is_busy()` signal the reaper
    /// consults so it never evicts a session mid-call.
    lock: Rc<tokio::sync::Mutex<()>>,
}

impl SessionState for BrowserSession {
    fn last_used(&self) -> u64 {
        self.last_used.get()
    }

    fn set_last_used(&self, now: u64) {
        self.last_used.set(now);
    }

    fn crashed(&self) -> bool {
        self.crashed.get()
    }

    fn set_crashed(&self) {
        self.crashed.set(true);
    }

    fn is_busy(&self) -> bool {
        self.lock.try_lock().is_err()
    }
}

impl CloseableSession for BrowserSession {
    async fn close(self) {
        // A crashed target's `Page` is never routed through this path (see
        // `touch_or_evict`'s crash branch, which evicts without closing) —
        // only a live-but-idle session reaches here, so `close()` erroring
        // is a best-effort cleanup, not a signal worth propagating.
        let _ = self.page.close().await;
    }
}

/// Look up `id` in `sessions`, bumping `last_used` to `now` in the same
/// synchronous `borrow_mut()` critical section used to check for a crashed
/// flag — no `.await` anywhere in this function, so a concurrent reaper scan
/// (Story 2.2) can never interleave with this lookup (Story 2.3's TOCTOU
/// requirement). A crashed session is evicted from the map right here (no
/// live `Page` to close, unlike the reaper's path) and reported as
/// `PortError::SessionCrashed`; a missing id is `PortError::NotFound`.
fn touch_or_evict<S: SessionState>(
    sessions: &RefCell<HashMap<String, S>>,
    id: &str,
    now: u64,
) -> Result<(), PortError> {
    let mut map = sessions.borrow_mut();
    let crashed = match map.get(id) {
        None => return Err(PortError::NotFound(not_found_message(id))),
        Some(session) => session.crashed(),
    };
    if crashed {
        map.remove(id);
        return Err(PortError::SessionCrashed(crashed_message(id)));
    }
    map.get(id)
        .expect("presence just confirmed above, under the same unreleased borrow")
        .set_last_used(now);
    Ok(())
}

/// One reaper scan: removes every session idle for longer than
/// `SESSION_IDLE_TIMEOUT` from `sessions` in a single synchronous
/// `borrow_mut()` (no `.await` inside it — the same critical-section
/// discipline `touch_or_evict` uses, per Story 2.3.2), then, only after that
/// borrow is dropped, `.await`s each evicted session's `close()`.
async fn reap_expired<S: CloseableSession>(sessions: &Rc<RefCell<HashMap<String, S>>>, now: u64) {
    let expired: Vec<(String, S)> = {
        let mut map = sessions.borrow_mut();
        let expired_ids: Vec<String> = map
            .iter()
            .filter(|(_, s)| {
                !s.is_busy()
                    && now.saturating_sub(s.last_used()) > SESSION_IDLE_TIMEOUT.as_millis() as u64
            })
            .map(|(id, _)| id.clone())
            .collect();
        expired_ids
            .into_iter()
            .filter_map(|id| map.remove(&id).map(|s| (id, s)))
            .collect()
    }; // `map` borrow dropped here, before any `.await` below.

    for (id, session) in expired {
        let idle_ms = now.saturating_sub(session.last_used());
        session.close().await;
        eprintln!("stapler-mcp: reaped idle browser session {id} (idle {idle_ms}ms)");
    }
}

/// Spawns the `SessionIdleReaper` background task: every 30s, scans
/// `sessions` and evicts anything idle past `SESSION_IDLE_TIMEOUT`. Must be
/// spawned via `tokio::task::spawn_local` (not `tokio::spawn`), since
/// `BrowserSession`'s `Page` and this module's `Rc`/`RefCell` fields are
/// `!Send` — the caller (`NativeBrowser::launch`) already runs inside the
/// daemon's `LocalSet`.
fn spawn_reaper(
    sessions: Rc<RefCell<HashMap<String, BrowserSession>>>,
    clock: impl ClockPort + 'static,
    sleeper: impl SleepPort + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_local(async move {
        loop {
            sleeper.sleep(Duration::from_secs(30)).await;
            reap_expired(&sessions, clock.now_millis()).await;
        }
    })
}

/// One shared browser-process allocator for the daemon's whole lifetime.
/// `Page` methods below take `&self`, so a fresh page (chromedp's "fresh tab
/// per call") is created per `navigate_and_extract` call without needing a
/// mutex — concurrent calls get independent tabs on the same browser process.
///
/// `sessions` additionally holds every persistent tab opened by
/// `navigate`/`click`/`type_text`/`snapshot` (the `BrowserDriver` methods
/// added in Epic 1), reaped by the background task in `reaper` when idle.
pub struct NativeBrowser {
    browser: Browser,
    sessions: Rc<RefCell<HashMap<String, BrowserSession>>>,
    next_id: Cell<u64>,
    /// Reservations for new sessions that have passed the `MAX_OPEN_SESSIONS`
    /// check but not yet been inserted into `sessions` — see
    /// `NewSessionSlotGuard`. Without this, concurrent no-`session_id`
    /// `navigate()` calls could all observe `sessions.len() < MAX_OPEN_SESSIONS`
    /// before any of them reaches its own `.insert`, overshooting the cap.
    pending_new_sessions: Cell<usize>,
    /// `pub` so `crates/cli/src/main.rs`'s shutdown sequence can
    /// `take()`/`abort()` it before `close()`ing the browser (Story 2.4).
    pub reaper: RefCell<Option<tokio::task::JoinHandle<()>>>,
}

/// RAII reservation for the `MAX_OPEN_SESSIONS` cap: `navigate()`'s no-`session_id`
/// branch increments `pending_new_sessions` synchronously (no `.await` between
/// the check and this guard's construction), then this guard decrements it on
/// drop — success, early return via `?`, or panic all release the slot exactly
/// once. This closes the race where the check (`sessions.len() >= MAX_OPEN_SESSIONS`)
/// and the eventual `sessions.insert(..)` are separated by several `.await`
/// points (page creation, `goto`, navigation wait, AX capture): without a
/// synchronous reservation, concurrent callers could all pass the check at the
/// limit and overshoot it before any of them inserts.
struct NewSessionSlotGuard<'a> {
    pending: &'a Cell<usize>,
}

impl<'a> NewSessionSlotGuard<'a> {
    fn reserve(pending: &'a Cell<usize>) -> Self {
        pending.set(pending.get() + 1);
        Self { pending }
    }
}

impl Drop for NewSessionSlotGuard<'_> {
    fn drop(&mut self) {
        self.pending.set(self.pending.get().saturating_sub(1));
    }
}

impl NativeBrowser {
    pub async fn launch() -> Result<Self, PortError> {
        // chromiumoxide's own default, when `user_data_dir` is left unset, is
        // a single fixed shared path (`$TMPDIR/chromiumoxide-runner`) rather
        // than a fresh directory per launch. Every daemon process on a
        // machine would then point Chrome at the same profile directory —
        // the second one to start finds it locked (Chrome's own
        // `SingletonLock`) and fails to launch. Each daemon process gets its
        // own directory instead, scoped by pid + timestamp so concurrent
        // daemons (e.g. back-to-back integration tests) never collide.
        let user_data_dir = std::env::temp_dir().join(format!(
            "stapler-mcp-chromium-{}-{}",
            std::process::id(),
            now_millis()
        ));
        std::fs::create_dir_all(&user_data_dir).map_err(|e| PortError::Other(e.to_string()))?;

        let config = BrowserConfig::builder()
            .user_data_dir(&user_data_dir)
            .build()
            .map_err(|e| PortError::Other(e.to_string()))?;
        let (browser, mut handler) = Browser::launch(config)
            .await
            .map_err(|e| PortError::Other(e.to_string()))?;

        // Drains the CDP websocket event stream for the daemon's whole
        // lifetime; dropping this JoinHandle does not stop the task.
        tokio::spawn(async move { while handler.next().await.is_some() {} });

        let sessions: Rc<RefCell<HashMap<String, BrowserSession>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let reaper = spawn_reaper(
            sessions.clone(),
            crate::NativeClock,
            crate::NativeSleeper,
        );

        Ok(NativeBrowser {
            browser,
            sessions,
            next_id: Cell::new(0),
            pending_new_sessions: Cell::new(0),
            reaper: RefCell::new(Some(reaper)),
        })
    }

    /// Generates a session id unique within this process's lifetime:
    /// `sess-<now_millis_hex>-<counter_hex>`.
    fn new_session_id(&self) -> String {
        let n = self.next_id.get();
        self.next_id.set(n + 1);
        format!("sess-{:x}-{:x}", now_millis(), n)
    }
}

impl NativeBrowser {
    /// Must be called once, explicitly, at daemon shutdown (after every
    /// `Rc<NativeBrowser>` clone held by registered tool handlers has been
    /// dropped, so this is the sole remaining reference) — otherwise the
    /// Chrome subprocess and its CDP connection keep the process alive.
    ///
    /// Callers must abort and await `self.reaper` *before* calling this (see
    /// `crates/cli/src/main.rs`'s shutdown sequence) — a still-running
    /// reaper holds its own `Rc<RefCell<HashMap<...>>>` clone (not of
    /// `Browser` itself, so it doesn't block `Rc::get_mut` on `browser` by
    /// itself, but leaving it running past shutdown would let it log/close
    /// sessions concurrently with `Browser::close()` for no benefit).
    pub async fn close(&mut self) {
        let _ = self.browser.close().await;
    }
}

/// Merges `capture`'s refs into this session's bookkeeping: newly-seen ref
/// strings are tagged in `known_refs` with `nav_generation` (already-known
/// refs keep whatever generation they were first seen under, via
/// `.entry(...).or_insert(...)`), `latest_refs` is fully replaced with the
/// new capture's refs, and `latest_url` is updated. Returns the `AxSnapshot`
/// half of `capture` for the caller to hand back to its own caller.
fn install_snapshot(
    latest_refs: &Rc<RefCell<HashMap<String, ax::ResolvedRef>>>,
    known_refs: &Rc<RefCell<HashMap<String, u64>>>,
    latest_url: &Rc<RefCell<String>>,
    nav_generation: u64,
    capture: ax::AxCapture,
) -> AxSnapshot {
    {
        let mut known = known_refs.borrow_mut();
        for r in capture.refs.keys() {
            known.entry(r.clone()).or_insert(nav_generation);
        }
    }
    *latest_url.borrow_mut() = capture.snapshot.url.clone();
    *latest_refs.borrow_mut() = capture.refs;
    capture.snapshot
}

/// Pure resolution logic behind `resolve_locator` (Tasks 3.1.2 + 3.1.3):
/// takes plain borrowed maps rather than a `&BrowserSession` so it can be
/// unit-tested without a live `chromiumoxide::Page`.
fn resolve_locator_impl(
    latest_refs: &HashMap<String, ax::ResolvedRef>,
    known_refs: &HashMap<String, u64>,
    nav_generation: u64,
    locator: &Locator,
    url: &str,
) -> Result<(BackendNodeId, String), PortError> {
    if let Some(r) = latest_refs.get(&locator.0) {
        return Ok((r.backend_node_id, r.role.clone()));
    }
    let stale = known_refs
        .get(&locator.0)
        .is_some_and(|&issued_gen| issued_gen < nav_generation);
    if stale {
        Err(PortError::NotFound(format!(
            "no element with ref '{}' in current snapshot (page: {url}) — the current page has navigated since this ref was issued; call stapler_browser_snapshot for current refs",
            locator.0
        )))
    } else {
        Err(PortError::NotFound(format!(
            "no element with ref '{}' in current snapshot (page: {url}); call stapler_browser_snapshot for current refs",
            locator.0
        )))
    }
}

/// Closes the `BackendNodeId`-reuse TOCTOU gap (Task 3.1.4): called
/// immediately before dispatch, after any `.await`s that happened past
/// `resolve_locator_impl`. `DOM.describeNode` fails outright if the backend
/// node id no longer refers to a live node; `Accessibility.getPartialAXTree`
/// re-derives the node's current role so a node that got recycled into a
/// different, same-id element (rare, but the CDP protocol does not rule it
/// out) is still caught by a role mismatch.
async fn verify_node_live(
    page: &Page,
    backend_node_id: BackendNodeId,
    expected_role: &str,
    locator: &Locator,
) -> Result<(), PortError> {
    let stale = || {
        PortError::NotFound(format!(
            "element for ref '{}' changed or disappeared since the last snapshot; call stapler_browser_snapshot for current refs",
            locator.0
        ))
    };

    page.execute(
        DescribeNodeParams::builder()
            .backend_node_id(backend_node_id)
            .build(),
    )
    .await
    .map_err(|_| stale())?;

    let partial = page
        .execute(
            GetPartialAxTreeParams::builder()
                .backend_node_id(backend_node_id)
                .build(),
        )
        .await
        .map_err(|_| stale())?;

    let role = partial
        .result
        .nodes
        .first()
        .and_then(|n| n.role.as_ref())
        .and_then(ax::ax_value_to_string);

    match role {
        Some(r) if r == expected_role => Ok(()),
        _ => Err(stale()),
    }
}

/// A freshly-navigated page's AX tree can briefly be empty (document still
/// parsing when `page.wait_for_navigation()` resolves) — retries once, after
/// a short backoff, if the first capture's root has no children.
///
/// `previous_refs` is forwarded to `ax::capture_snapshot` so a node that
/// survives this capture unchanged (e.g. everything except whatever a
/// `click`/`type_text` just mutated) keeps the ref string a caller may
/// already be holding, rather than every recapture reassigning fresh ref
/// strings to the whole tree. Callers doing a real navigation (new page or
/// `page.goto`) must pass an empty map — the old page's refs don't apply to
/// the new document.
async fn wait_and_capture(
    page: &Page,
    next_ref_id: &Cell<u64>,
    previous_refs: &HashMap<String, ax::ResolvedRef>,
) -> Result<ax::AxCapture, PortError> {
    let first = ax::capture_snapshot(page, next_ref_id, previous_refs).await?;
    if !first.snapshot.root.children.is_empty() {
        return Ok(first);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    ax::capture_snapshot(page, next_ref_id, previous_refs).await
}

/// Resolves `backend_node_id` to a live `RemoteObject`/`objectId` (`DOM
/// .resolveNode`) and invokes `js_fn` on it via `Runtime.callFunctionOn`,
/// bypassing `chromiumoxide::Element` entirely (its constructor is
/// crate-private). `js_fn` must be a JS function-expression string; `this`
/// inside it is the resolved DOM node.
async fn invoke_on_node(
    page: &Page,
    backend_node_id: BackendNodeId,
    action_name: &str,
    js_fn: &str,
    args: Vec<serde_json::Value>,
) -> Result<(), PortError> {
    let resolved = page
        .execute(
            ResolveNodeParams::builder()
                .backend_node_id(backend_node_id)
                .build(),
        )
        .await
        .map_err(|e| PortError::Other(e.to_string()))?;
    let object_id = resolved
        .result
        .object
        .object_id
        .ok_or_else(|| PortError::Other("resolved node has no remote object id".to_string()))?;

    let call_args: Vec<CallArgument> = args
        .into_iter()
        .map(|v| CallArgument::builder().value(v).build())
        .collect();

    let params = CallFunctionOnParams::builder()
        .object_id(object_id)
        .function_declaration(js_fn)
        .arguments(call_args)
        .await_promise(true)
        .build()
        .map_err(PortError::Other)?;

    let response = page
        .execute(params)
        .await
        .map_err(|e| PortError::Other(e.to_string()))?;

    // Deliberately does not forward `exception.exception.value`/`description`
    // to the caller: `ExceptionDetails`' debug text is whatever the page's own
    // script produced (e.g. a thrown error's message can embed
    // `localStorage`/DOM text the page script had access to), so relaying it
    // verbatim would be an info leak from the target page to the MCP caller.
    // Only the fixed action name and the CDP-assigned `exception_id` (an
    // opaque integer, not page-controlled) are surfaced.
    if let Some(exception) = &response.result.exception_details {
        return Err(PortError::Other(format!(
            "{action_name} raised an exception on the page (exception id {})",
            exception.exception_id
        )));
    }
    Ok(())
}

async fn dispatch_click(page: &Page, backend_node_id: BackendNodeId) -> Result<(), PortError> {
    invoke_on_node(
        page,
        backend_node_id,
        "click",
        "function() { this.click(); }",
        vec![],
    )
    .await
}

async fn dispatch_type(page: &Page, backend_node_id: BackendNodeId, text: &str) -> Result<(), PortError> {
    invoke_on_node(
        page,
        backend_node_id,
        "type",
        "function(text) { this.focus(); this.value = text; \
         this.dispatchEvent(new Event('input', {bubbles: true})); \
         this.dispatchEvent(new Event('change', {bubbles: true})); }",
        vec![serde_json::Value::String(text.to_string())],
    )
    .await
}

/// Pure logic behind the `Page.frameNavigated` listener's SSRF re-check
/// (Task 3.4.2): parses `frame_url`, runs it through the same
/// `blocked_host_reason` guard `navigate`'s own pre-flight check uses, and
/// formats the *recoverable* variant of the block message — deliberately
/// different wording from `navigate`'s own pre-flight SSRF rejection (that
/// one is Epic 5's job and uses the raw `blocked_host_reason` text verbatim).
fn frame_navigated_blocked_message(
    session_id: &str,
    frame_url: &str,
    policy: NetworkPolicy,
) -> Option<String> {
    let parsed = Url::parse(frame_url).ok()?;
    blocked_host_reason(&parsed, policy)?;
    let host = parsed.host_str().unwrap_or(frame_url).to_string();
    Some(format!(
        "session '{session_id}' navigated to a blocked host '{host}' during the last action; call stapler_browser_navigate with this sessionId and a safe URL to recover it, or start a fresh session"
    ))
}

/// Polls `blocked` for up to 200ms in 20ms steps, giving the async
/// `Page.frameNavigated`/`Page.frameRequestedNavigation` listener spawned by
/// `spawn_session_listeners` a chance to observe and flag a same-call
/// navigation to a blocked host before the caller sees a result. Shared by
/// `navigate` (BLOCKER fix: a redirect during the navigation itself must not
/// leak a snapshot of the blocked host) and `dispatch_action` (a click/type
/// that triggers an in-page navigation).
async fn poll_blocked_grace_period(blocked: &Rc<RefCell<Option<String>>>) -> Option<String> {
    let mut waited = Duration::ZERO;
    let step = Duration::from_millis(20);
    while blocked.borrow().is_none() && waited < Duration::from_millis(200) {
        tokio::time::sleep(step).await;
        waited += step;
    }
    blocked.borrow().clone()
}

/// Registers this session's `Page.frameNavigated`/`Page.frameRequestedNavigation`
/// (SSRF re-check, Task 3.4.2) and `Target.targetCrashed` (Story 2.5's
/// deferred crash listener, implemented here in Story 3.2) event listeners.
/// Called exactly once, at new-session creation — never re-registered on a
/// session-reuse `navigate` call, to avoid duplicate event streams
/// accumulating on the same `Page`.
async fn spawn_session_listeners(
    page: &Page,
    session_id: String,
    blocked: Rc<RefCell<Option<String>>>,
    crashed: Rc<Cell<bool>>,
) {
    // `frameRequestedNavigation` fires for the *intended* destination URL the
    // instant the browser decides to navigate there — unlike
    // `frameNavigated`, which only fires once a navigation actually commits.
    // A link-local/private SSRF target is typically unreachable, so the
    // connection attempt fails outright and the frame instead commits
    // Chromium's own `chrome-error://chromewebdata/` page; a check that only
    // ever inspected `frameNavigated`'s committed URL would see that harmless
    // internal URL and never notice the blocked host was ever requested.
    // Checking the *requested* URL here closes that gap regardless of
    // whether the target is reachable.
    let main_frame_id = page.mainframe().await.ok().flatten();
    if let Ok(mut requested_events) = page.event_listener::<EventFrameRequestedNavigation>().await
    {
        let session_id_for_requests = session_id.clone();
        let blocked_for_requests = blocked.clone();
        let main_frame_id = main_frame_id.clone();
        tokio::task::spawn_local(async move {
            while let Some(event) = requested_events.next().await {
                if main_frame_id.as_ref() != Some(&event.frame_id) {
                    continue;
                }
                let policy = NetworkPolicy::from_env(
                    std::env::var("STAPLER_MCP_ALLOW_PRIVATE_NETWORKS").ok(),
                );
                if let Some(msg) = frame_navigated_blocked_message(
                    &session_id_for_requests,
                    &event.url,
                    policy,
                ) {
                    *blocked_for_requests.borrow_mut() = Some(msg);
                }
            }
        });
    }

    if let Ok(mut frame_events) = page.event_listener::<EventFrameNavigated>().await {
        tokio::task::spawn_local(async move {
            while let Some(event) = frame_events.next().await {
                // Only top-level navigations are SSRF-relevant — an iframe
                // pointing at a blocked host doesn't hand the caller control
                // of the top-level page's origin.
                if event.frame.parent_id.is_some() {
                    continue;
                }
                let policy = NetworkPolicy::from_env(
                    std::env::var("STAPLER_MCP_ALLOW_PRIVATE_NETWORKS").ok(),
                );
                if let Some(msg) =
                    frame_navigated_blocked_message(&session_id, &event.frame.url, policy)
                {
                    *blocked.borrow_mut() = Some(msg);
                }
            }
        });
    }

    if let Ok(mut crash_events) = page.event_listener::<EventTargetCrashed>().await {
        tokio::task::spawn_local(async move {
            if crash_events.next().await.is_some() {
                crashed.set(true);
            }
        });
    }
}

impl BrowserDriver for NativeBrowser {
    async fn navigate_and_extract(
        &self,
        url: &str,
        timeout: Duration,
    ) -> Result<PageExtract, PortError> {
        let fut = async {
            let page = self
                .browser
                .new_page(url)
                .await
                .map_err(|e| PortError::Other(e.to_string()))?;
            page.wait_for_navigation()
                .await
                .map_err(|e| PortError::Other(e.to_string()))?;

            let title: String = page
                .evaluate("document.title")
                .await
                .map_err(|e| PortError::Other(e.to_string()))?
                .into_value()
                .map_err(|e| PortError::Other(e.to_string()))?;
            let text: String = page
                .evaluate("document.body ? document.body.innerText : ''")
                .await
                .map_err(|e| PortError::Other(e.to_string()))?
                .into_value()
                .map_err(|e| PortError::Other(e.to_string()))?;
            let html = page
                .content()
                .await
                .map_err(|e| PortError::Other(e.to_string()))?;
            let final_url = page
                .url()
                .await
                .map_err(|e| PortError::Other(e.to_string()))?
                .unwrap_or_else(|| url.to_string());

            Ok(PageExtract {
                title,
                html,
                text,
                final_url,
            })
        };

        tokio::time::timeout(timeout, fut)
            .await
            .map_err(|_| PortError::Timeout)?
    }

    /// Session-registry bookkeeping (Epic 2) is fully wired here: an unknown
    /// `session_id` returns `PortError::NotFound`, a crashed one returns
    /// `PortError::SessionCrashed` (and is evicted), and a live one has its
    /// `last_used` bumped before anything else happens. New-session and
    /// session-reuse paths both end by (re-)capturing the AX tree and
    /// installing it as the session's `latest_refs`/`latest_url`.
    ///
    /// The whole body runs under `tokio::time::timeout` — a stall anywhere
    /// (page creation, `goto`, navigation wait, AX capture) surfaces as the
    /// exact-wording `PortError::Other` below, not `PortError::Timeout`
    /// (whose generic `Display` can't carry the url/seconds detail).
    async fn navigate(
        &self,
        url: &str,
        session_id: Option<&SessionId>,
        timeout: Duration,
    ) -> Result<NavigateResult, PortError> {
        let fut = async {
            match session_id {
                None => {
                    // DoS guard: without this cap a caller can spawn tabs
                    // faster than the 300s idle reaper reclaims them,
                    // exhausting the underlying Chromium process. The check
                    // includes in-flight reservations (not just what's
                    // already inserted) so concurrent callers can't all pass
                    // it simultaneously and overshoot the cap — see
                    // `NewSessionSlotGuard`.
                    if self.sessions.borrow().len() + self.pending_new_sessions.get()
                        >= MAX_OPEN_SESSIONS
                    {
                        return Err(PortError::Other(format!(
                            "too many open browser sessions (limit {MAX_OPEN_SESSIONS}); close an existing session or wait for idle ones to be reclaimed"
                        )));
                    }
                    // Reserved synchronously, before any `.await` below —
                    // released automatically (on success, early `?` return,
                    // or panic) when this guard drops at the end of this
                    // match arm.
                    let _new_session_slot = NewSessionSlotGuard::reserve(&self.pending_new_sessions);

                    let id = self.new_session_id();
                    // Created blank (never `new_page(url)`): `new_page` starts
                    // the navigation to `url` immediately, and a page whose
                    // script redirects to a blocked host right after load can
                    // commit that redirect before `spawn_session_listeners`
                    // below has subscribed to the CDP event stream — the
                    // listener is registered second, but chromiumoxide's
                    // `new_page` doesn't block until it's attached. Starting
                    // from `about:blank` decouples "tab exists" from
                    // "navigation to caller's url has started", so the
                    // listeners are provably live before the first byte of
                    // `url` is requested.
                    let page = self
                        .browser
                        .new_page("about:blank")
                        .await
                        .map_err(|e| PortError::Other(e.to_string()))?;

                    // Turn on the `Accessibility` domain for this target, per
                    // CDP/Puppeteer convention, so `AXNodeId`s stay stable
                    // across the session's `getFullAXTree`/`getPartialAXTree`
                    // calls. A one-time, page-lifetime op, done once here
                    // rather than per capture.
                    page.execute(EnableParams::default())
                        .await
                        .map_err(|e| PortError::Other(e.to_string()))?;

                    let latest_refs = Rc::new(RefCell::new(HashMap::new()));
                    let known_refs = Rc::new(RefCell::new(HashMap::new()));
                    let nav_generation = Rc::new(Cell::new(0u64));
                    let next_ref_id = Rc::new(Cell::new(0u64));
                    let latest_url = Rc::new(RefCell::new(String::new()));
                    let blocked = Rc::new(RefCell::new(None));
                    let crashed = Rc::new(Cell::new(false));

                    // Registered *before* `wait_for_navigation` (BLOCKER fix):
                    // a redirect chained onto this same initial navigation —
                    // server-side or an immediate in-page `location.href` —
                    // can commit before this call ever returns. Attaching the
                    // listeners only after `wait_for_navigation` resolved (the
                    // previous ordering) meant that commit was missed
                    // entirely, so a public URL that redirects to a blocked
                    // host (e.g. the cloud metadata service) would have its
                    // snapshot returned to the caller with no block ever
                    // surfaced.
                    spawn_session_listeners(&page, id.clone(), blocked.clone(), crashed.clone())
                        .await;

                    // Now safe to actually request the caller's URL: the
                    // listeners are attached, so any redirect chained onto
                    // this navigation (server-side or in-page) is observed.
                    page.goto(url)
                        .await
                        .map_err(|e| PortError::Other(e.to_string()))?;
                    page.wait_for_navigation()
                        .await
                        .map_err(|e| PortError::Other(e.to_string()))?;

                    // Give the listener a moment to observe a same-call
                    // redirect before deciding whether to capture a snapshot
                    // at all.
                    if let Some(reason) = poll_blocked_grace_period(&blocked).await {
                        // The session is still inserted (with no snapshot
                        // ever captured) so the caller can recover via the
                        // documented path: re-navigate this sessionId to a
                        // safe URL.
                        let session = BrowserSession {
                            page,
                            last_used: Cell::new(now_millis()),
                            latest_refs,
                            known_refs,
                            nav_generation,
                            next_ref_id,
                            latest_url,
                            blocked,
                            crashed,
                            lock: Rc::new(tokio::sync::Mutex::new(())),
                        };
                        self.sessions.borrow_mut().insert(id, session);
                        return Err(PortError::NotFound(reason));
                    }

                    // Fresh session: `latest_refs` is still empty, so there's
                    // nothing to borrow across the `.await` — pass an empty
                    // map directly rather than holding a `RefCell` borrow
                    // live over an await point (clippy::await_holding_refcell_ref).
                    let capture = wait_and_capture(&page, &next_ref_id, &HashMap::new()).await?;
                    let snapshot = install_snapshot(
                        &latest_refs,
                        &known_refs,
                        &latest_url,
                        nav_generation.get(),
                        capture,
                    );

                    let session = BrowserSession {
                        page,
                        last_used: Cell::new(now_millis()),
                        latest_refs,
                        known_refs,
                        nav_generation,
                        next_ref_id,
                        latest_url,
                        blocked,
                        crashed,
                        lock: Rc::new(tokio::sync::Mutex::new(())),
                    };
                    self.sessions.borrow_mut().insert(id.clone(), session);

                    Ok(NavigateResult {
                        session_id: SessionId(id),
                        final_url: snapshot.url.clone(),
                        snapshot,
                    })
                }
                Some(id) => {
                    touch_or_evict(&self.sessions, &id.0, now_millis())?;

                    let (page, latest_refs, known_refs, nav_generation, next_ref_id, latest_url, blocked, lock) = {
                        let map = self.sessions.borrow();
                        let session = map
                            .get(&id.0)
                            .expect("touch_or_evict just confirmed presence");
                        (
                            session.page.clone(),
                            session.latest_refs.clone(),
                            session.known_refs.clone(),
                            session.nav_generation.clone(),
                            session.next_ref_id.clone(),
                            session.latest_url.clone(),
                            session.blocked.clone(),
                            session.lock.clone(),
                        )
                    };

                    // Serializes this call against any other concurrent
                    // navigate/click/type_text/snapshot on the same
                    // sessionId — held for the rest of this branch, across
                    // every `.await` below, so two calls against the same
                    // `Page` never interleave CDP round-trips.
                    let _session_guard = lock.lock().await;

                    // This call's own navigation is about to supersede
                    // whatever `blocked` may have recorded from a prior
                    // in-page navigation — this is the caller's documented
                    // recovery path (Task 3.4.2).
                    *blocked.borrow_mut() = None;

                    page.goto(url)
                        .await
                        .map_err(|e| PortError::Other(e.to_string()))?;
                    page.wait_for_navigation()
                        .await
                        .map_err(|e| PortError::Other(e.to_string()))?;

                    nav_generation.set(nav_generation.get() + 1);

                    // BLOCKER fix: give the frameNavigated/frameRequestedNavigation
                    // listener a chance to flag a redirect chained onto this
                    // same `goto` before capturing (let alone returning) a
                    // snapshot of whatever page it landed on.
                    if let Some(reason) = poll_blocked_grace_period(&blocked).await {
                        if let Some(session) = self.sessions.borrow().get(&id.0) {
                            session.last_used.set(now_millis());
                        }
                        return Err(PortError::NotFound(reason));
                    }

                    // Real navigation to a new URL: an old ref's
                    // `BackendNodeId` has no relationship to this new
                    // document's nodes (CDP doesn't guarantee ids aren't
                    // reused across navigations), so identity must not carry
                    // over — pass an empty map rather than the (about to be
                    // discarded) old-page `latest_refs`.
                    let capture = wait_and_capture(&page, &next_ref_id, &HashMap::new()).await?;
                    let snapshot = install_snapshot(
                        &latest_refs,
                        &known_refs,
                        &latest_url,
                        nav_generation.get(),
                        capture,
                    );

                    if let Some(session) = self.sessions.borrow().get(&id.0) {
                        session.last_used.set(now_millis());
                    }

                    Ok(NavigateResult {
                        session_id: SessionId(id.0.clone()),
                        final_url: snapshot.url.clone(),
                        snapshot,
                    })
                }
            }
        };

        tokio::time::timeout(timeout, fut).await.map_err(|_| {
            PortError::Other(format!(
                "timeout after {}s waiting for navigation to {url}",
                timeout.as_secs()
            ))
        })?
    }

    /// See `navigate`'s doc comment for the session-registry check. Locator
    /// resolution (`resolve_locator_impl`) and the `BackendNodeId`-reuse
    /// liveness check (`verify_node_live`) both run immediately before
    /// dispatch; after dispatch, `blocked` is polled for a short grace period
    /// so a same-call in-page navigation to a blocked host is caught before
    /// this call returns, rather than surfacing only on the *next* call.
    async fn click(
        &self,
        session_id: &SessionId,
        locator: &Locator,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError> {
        self.dispatch_action(session_id, locator, timeout, Action::Click)
            .await
    }

    /// See `click`'s doc comment.
    async fn type_text(
        &self,
        session_id: &SessionId,
        locator: &Locator,
        text: &str,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError> {
        self.dispatch_action(session_id, locator, timeout, Action::Type(text.to_string()))
            .await
    }

    /// Read-only: captures and installs a fresh AX tree without dispatching
    /// any DOM mutation. Still checks `blocked` on entry, per every
    /// `BrowserDriver` method's obligation to surface a prior in-page
    /// navigation to a blocked host on the next call that touches the
    /// session (Task 3.4.2's "next call" fallback).
    async fn snapshot(
        &self,
        session_id: &SessionId,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError> {
        let fut = async {
            touch_or_evict(&self.sessions, &session_id.0, now_millis())?;

            let (page, latest_refs, known_refs, nav_generation, next_ref_id, latest_url, blocked, lock) = {
                let map = self.sessions.borrow();
                let session = map
                    .get(&session_id.0)
                    .expect("touch_or_evict just confirmed presence");
                (
                    session.page.clone(),
                    session.latest_refs.clone(),
                    session.known_refs.clone(),
                    session.nav_generation.clone(),
                    session.next_ref_id.clone(),
                    session.latest_url.clone(),
                    session.blocked.clone(),
                    session.lock.clone(),
                )
            };

            let _session_guard = lock.lock().await;

            if let Some(reason) = blocked.borrow().clone() {
                return Err(PortError::NotFound(reason));
            }

            // Clone out of the `RefCell` first — holding a live borrow across
            // the `.await` below would trip clippy::await_holding_refcell_ref
            // (and risks a panic if another task borrows `latest_refs` while
            // this future is suspended).
            let previous_refs = latest_refs.borrow().clone();
            let capture = ax::capture_snapshot(&page, &next_ref_id, &previous_refs).await?;
            let snapshot = install_snapshot(
                &latest_refs,
                &known_refs,
                &latest_url,
                nav_generation.get(),
                capture,
            );

            if let Some(session) = self.sessions.borrow().get(&session_id.0) {
                session.last_used.set(now_millis());
            }

            Ok(snapshot)
        };

        tokio::time::timeout(timeout, fut)
            .await
            .map_err(|_| PortError::Timeout)?
    }
}

/// What `dispatch_action` does to the resolved node.
enum Action {
    Click,
    Type(String),
}

impl NativeBrowser {
    /// Shared body of `click`/`type_text` (Tasks 3.3.1 + 3.3.2): session
    /// lookup, locator resolution, liveness re-check, dispatch, an SSRF
    /// grace-period poll, then a fresh AX capture with `navigated_from` set
    /// if the dispatch itself caused a (non-blocked) navigation.
    async fn dispatch_action(
        &self,
        session_id: &SessionId,
        locator: &Locator,
        timeout: Duration,
        action: Action,
    ) -> Result<AxSnapshot, PortError> {
        let fut = async {
            touch_or_evict(&self.sessions, &session_id.0, now_millis())?;

            let (page, latest_refs, known_refs, nav_generation, next_ref_id, latest_url, blocked, lock) = {
                let map = self.sessions.borrow();
                let session = map
                    .get(&session_id.0)
                    .expect("touch_or_evict just confirmed presence");
                (
                    session.page.clone(),
                    session.latest_refs.clone(),
                    session.known_refs.clone(),
                    session.nav_generation.clone(),
                    session.next_ref_id.clone(),
                    session.latest_url.clone(),
                    session.blocked.clone(),
                    session.lock.clone(),
                )
            };

            let _session_guard = lock.lock().await;

            if let Some(reason) = blocked.borrow().clone() {
                return Err(PortError::NotFound(reason));
            }

            let url_before = page
                .url()
                .await
                .map_err(|e| PortError::Other(e.to_string()))?
                .unwrap_or_else(|| latest_url.borrow().clone());

            let (backend_node_id, expected_role) = {
                let refs = latest_refs.borrow();
                let known = known_refs.borrow();
                resolve_locator_impl(&refs, &known, nav_generation.get(), locator, &url_before)?
            };

            verify_node_live(&page, backend_node_id, &expected_role, locator).await?;

            match &action {
                Action::Click => dispatch_click(&page, backend_node_id).await?,
                Action::Type(text) => dispatch_type(&page, backend_node_id, text).await?,
            }

            // Grace period: give the `Page.frameNavigated` listener a chance
            // to observe and flag a same-call in-page navigation before this
            // call returns (Task 3.3.1's SSRF same-call disclosure AC).
            if let Some(reason) = poll_blocked_grace_period(&blocked).await {
                return Err(PortError::NotFound(reason));
            }

            // Same reasoning as `snapshot()` above: clone out of the
            // `RefCell` before the `.await` rather than holding a borrow
            // across it.
            let previous_refs = latest_refs.borrow().clone();
            let capture = wait_and_capture(&page, &next_ref_id, &previous_refs).await?;
            let mut snapshot = install_snapshot(
                &latest_refs,
                &known_refs,
                &latest_url,
                nav_generation.get(),
                capture,
            );
            if snapshot.url != url_before {
                snapshot.navigated_from = Some(url_before);
            }

            if let Some(session) = self.sessions.borrow().get(&session_id.0) {
                session.last_used.set(now_millis());
            }

            Ok(snapshot)
        };

        tokio::time::timeout(timeout, fut).await.map_err(|_| {
            PortError::Other(format!(
                "timeout after {}s waiting for the action to complete",
                timeout.as_secs()
            ))
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell as StdCell;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// A `BrowserSession` stand-in with no real `chromiumoxide::Page` —
    /// chromiumoxide only ever constructs a `Page` via a live CDP handshake,
    /// so every test in this module that exercises session-registry logic
    /// (reaper scan, TOCTOU-safe lookup, crash detection) runs against this
    /// fake rather than the real daemon.
    struct FakeSession {
        last_used: Cell<u64>,
        crashed: Cell<bool>,
        /// Set right before `close()`'s (fake) await point resolves, so a
        /// test can assert the map was already empty *before* this fires
        /// (Task 2.2.1 AC).
        closed: Rc<StdCell<bool>>,
    }

    impl FakeSession {
        fn new(last_used: u64) -> Self {
            FakeSession {
                last_used: Cell::new(last_used),
                crashed: Cell::new(false),
                closed: Rc::new(StdCell::new(false)),
            }
        }
    }

    impl SessionState for FakeSession {
        fn last_used(&self) -> u64 {
            self.last_used.get()
        }
        fn set_last_used(&self, now: u64) {
            self.last_used.set(now);
        }
        fn crashed(&self) -> bool {
            self.crashed.get()
        }
        fn set_crashed(&self) {
            self.crashed.set(true);
        }
    }

    /// Yields exactly once before resolving — stands in for `page.close()`'s
    /// real `.await` point, so tests can observe map state at the instant
    /// between the synchronous removal and the close resolving.
    struct YieldOnce(bool);
    impl Future for YieldOnce {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.0 {
                Poll::Ready(())
            } else {
                self.0 = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    impl CloseableSession for FakeSession {
        async fn close(self) {
            YieldOnce(false).await;
            self.closed.set(true);
        }
    }

    // ---- Story 2.1: session registry data structures ----

    #[tokio::test]
    async fn native_browser_sessions_should_be_empty_when_freshly_constructed() {
        // `NativeBrowser::launch()` needs a real Chromium binary, which this
        // offline unit-test environment doesn't have; the field this AC
        // cares about (`sessions` starts empty) is exercised directly here
        // instead, mirroring the same `Rc<RefCell<HashMap<...>>>` type
        // `launch()` constructs.
        let sessions: Rc<RefCell<HashMap<String, FakeSession>>> = Rc::new(RefCell::new(HashMap::new()));
        assert_eq!(sessions.borrow().len(), 0);
    }

    #[test]
    fn new_session_id_should_differ_when_called_twice_in_a_row() {
        let next_id = Cell::new(0u64);
        let make = |next_id: &Cell<u64>| {
            let n = next_id.get();
            next_id.set(n + 1);
            format!("sess-{:x}-{:x}", now_millis(), n)
        };
        let first = make(&next_id);
        let second = make(&next_id);
        assert_ne!(first, second);
    }

    // ---- Story 2.2: SessionIdleReaper scan ----

    #[tokio::test]
    async fn reap_expired_should_remove_from_map_before_close_await_resolves_when_session_idle_past_timeout(
    ) {
        let now = now_millis();
        let idle_since = now - (SESSION_IDLE_TIMEOUT.as_millis() as u64 + 1_000);
        let sessions: Rc<RefCell<HashMap<String, FakeSession>>> = Rc::new(RefCell::new(HashMap::new()));
        let session = FakeSession::new(idle_since);
        let closed_flag = session.closed.clone();
        sessions.borrow_mut().insert("sess-1".to_string(), session);

        // Drive the scan up to (but not past) the yield point inside
        // `close()`, so we can assert map emptiness before the close future
        // resolves — proving removal happens first, per Task 2.2.1's AC.
        let mut fut = Box::pin(reap_expired(&sessions, now));
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        // First poll: does the synchronous removal, then hits `YieldOnce`'s
        // pending state inside `close().await` and returns `Pending`.
        assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Pending));
        assert_eq!(sessions.borrow().len(), 0, "session must be removed from the map before close() resolves");
        assert!(!closed_flag.get(), "close() must not have finished yet at this point");

        // Second poll: `YieldOnce` resolves, `close()` finishes.
        assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Ready(())));
        assert!(closed_flag.get());
    }

    #[tokio::test]
    async fn reap_expired_should_not_evict_session_when_last_used_within_timeout() {
        let now = now_millis();
        let sessions: Rc<RefCell<HashMap<String, FakeSession>>> = Rc::new(RefCell::new(HashMap::new()));
        sessions
            .borrow_mut()
            .insert("sess-1".to_string(), FakeSession::new(now - 1_000));

        reap_expired(&sessions, now).await;

        assert_eq!(sessions.borrow().len(), 1);
    }

    // ---- Story 2.3: TOCTOU-safe eviction / last_used bump ----

    #[test]
    fn bump_last_used_should_prevent_eviction_when_call_in_flight_during_scan() {
        let t0 = 1_000_000u64;
        let sessions: Rc<RefCell<HashMap<String, FakeSession>>> = Rc::new(RefCell::new(HashMap::new()));
        sessions
            .borrow_mut()
            .insert("sess-1".to_string(), FakeSession::new(t0 - 400_000));

        // Simulates a `BrowserDriver` method's call-entry bump: happens
        // synchronously, before any of the call's own (here, simulated 10s)
        // async work begins.
        touch_or_evict(&sessions, "sess-1", t0).expect("session is present and not crashed");

        // A reaper scan landing mid-call (t0 + 5s) must see the just-bumped
        // last_used and not evict, even though the pre-bump last_used (400s
        // stale) would have been past SESSION_IDLE_TIMEOUT.
        let mid_call = t0 + 5_000;
        let map = sessions.borrow();
        let still_fresh = mid_call.saturating_sub(map.get("sess-1").unwrap().last_used())
            <= SESSION_IDLE_TIMEOUT.as_millis() as u64;
        assert!(still_fresh, "session must not look idle to a scan mid-call");
    }

    #[test]
    fn touch_or_evict_should_return_not_found_when_session_absent() {
        let sessions: RefCell<HashMap<String, FakeSession>> = RefCell::new(HashMap::new());

        let err = touch_or_evict(&sessions, "sess-missing", now_millis()).unwrap_err();

        match err {
            PortError::NotFound(msg) => {
                assert!(msg.contains("sess-missing"));
                assert!(msg.contains("stapler_browser_navigate"));
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn touch_or_evict_should_bump_last_used_when_session_present_and_not_crashed() {
        let sessions: RefCell<HashMap<String, FakeSession>> = RefCell::new(HashMap::new());
        sessions
            .borrow_mut()
            .insert("sess-1".to_string(), FakeSession::new(0));

        touch_or_evict(&sessions, "sess-1", 42).unwrap();

        assert_eq!(sessions.borrow().get("sess-1").unwrap().last_used(), 42);
    }

    // ---- Story 2.4 is covered by an `#[ignore]`d real-daemon integration
    // test (per validation.md); no offline unit test applies since it
    // exercises `cli/main.rs`'s shutdown sequence end to end.

    // ---- Story 2.5: tab-crash detection ----

    #[test]
    fn target_crashed_handler_should_set_crashed_flag_when_event_fires() {
        let session = FakeSession::new(0);
        assert!(!session.crashed());

        // Stands in for the `EventTargetCrashed` listener closure registered
        // in Epic 3's `navigate` (Story 3.2): invoked directly here with a
        // synthetic "event fired" signal rather than a real CDP event.
        let on_target_crashed = |session: &FakeSession| session.set_crashed();
        on_target_crashed(&session);

        assert!(session.crashed());
    }

    #[test]
    fn touch_or_evict_should_return_session_crashed_then_not_found_when_crash_flag_set() {
        let sessions: RefCell<HashMap<String, FakeSession>> = RefCell::new(HashMap::new());
        let session = FakeSession::new(0);
        session.set_crashed();
        sessions.borrow_mut().insert("sess-1".to_string(), session);

        let first = touch_or_evict(&sessions, "sess-1", now_millis()).unwrap_err();
        match first {
            PortError::SessionCrashed(msg) => assert!(msg.contains("sess-1")),
            other => panic!("expected SessionCrashed, got {other:?}"),
        }

        let second = touch_or_evict(&sessions, "sess-1", now_millis()).unwrap_err();
        match second {
            PortError::NotFound(_) => {}
            other => panic!("expected NotFound after eviction, got {other:?}"),
        }
    }

    // ---- UX AC #10: reaped vs never-existed error text is byte-identical ----

    #[test]
    fn ux_ac10_reaped_session_error_should_be_textually_identical_to_never_existed_session_error() {
        let sessions: RefCell<HashMap<String, FakeSession>> = RefCell::new(HashMap::new());
        // "sess-reaped" is simply absent, exactly as it would be after the
        // reaper evicted it — `not_found_message` takes no history-dependent
        // input, so the two cases can't diverge.
        let reaped_like = touch_or_evict(&sessions, "sess-reaped", now_millis()).unwrap_err();
        let never_existed = touch_or_evict(&sessions, "sess-reaped", now_millis()).unwrap_err();

        assert_eq!(format!("{reaped_like}"), format!("{never_existed}"));
    }

    // ---- Epic 3: locator resolution (Tasks 3.1.2 / 3.1.3) ----
    //
    // `resolve_locator_impl` is the pure function behind `dispatch_action`'s
    // locator lookup — deliberately factored out of anything holding a real
    // `chromiumoxide::Page`/`BrowserSession`, so it's testable here without a
    // live CDP connection. Every method that ultimately calls it
    // (`click`/`type_text`) inherits these behaviors unchanged.

    fn fake_resolved_ref(id: u64, role: &str) -> ax::ResolvedRef {
        ax::ResolvedRef {
            backend_node_id: BackendNodeId::new(id as i64),
            role: role.to_string(),
        }
    }

    #[test]
    fn resolve_locator_should_return_not_found_when_ref_missing_from_latest_refs() {
        let latest_refs: HashMap<String, ax::ResolvedRef> = HashMap::new();
        let known_refs: HashMap<String, u64> = HashMap::new();

        let err = resolve_locator_impl(
            &latest_refs,
            &known_refs,
            0,
            &Locator("ref-1".to_string()),
            "https://example.com/",
        )
        .unwrap_err();

        match err {
            PortError::NotFound(msg) => {
                assert!(msg.contains("ref-1"));
                assert!(msg.contains("stapler_browser_snapshot"));
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn resolve_locator_should_return_generic_message_when_ref_never_issued() {
        let latest_refs: HashMap<String, ax::ResolvedRef> = HashMap::new();
        let known_refs: HashMap<String, u64> = HashMap::new();

        let err = resolve_locator_impl(
            &latest_refs,
            &known_refs,
            3,
            &Locator("ref-never-seen".to_string()),
            "https://example.com/",
        )
        .unwrap_err();

        match err {
            PortError::NotFound(msg) => {
                // Generic wording must NOT claim a navigation happened —
                // that phrasing is reserved for the stale-ref case below.
                assert!(!msg.contains("navigated since"));
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn resolve_locator_should_return_stale_ref_message_when_ref_issued_before_navigation() {
        let latest_refs: HashMap<String, ax::ResolvedRef> = HashMap::new();
        let mut known_refs: HashMap<String, u64> = HashMap::new();
        // Issued under generation 0; the session has since navigated to
        // generation 1 (a `navigate` call reusing this session).
        known_refs.insert("ref-1".to_string(), 0);

        let err = resolve_locator_impl(
            &latest_refs,
            &known_refs,
            1,
            &Locator("ref-1".to_string()),
            "https://example.com/next",
        )
        .unwrap_err();

        match err {
            PortError::NotFound(msg) => {
                assert!(msg.contains("ref-1"));
                assert!(msg.contains("navigated since"));
                assert!(msg.contains("stapler_browser_snapshot"));
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn ux_ac3_locator_not_found_error_should_name_ref_and_page_url_and_corrective_call() {
        let latest_refs: HashMap<String, ax::ResolvedRef> = HashMap::new();
        let known_refs: HashMap<String, u64> = HashMap::new();

        let err = resolve_locator_impl(
            &latest_refs,
            &known_refs,
            0,
            &Locator("ref-42".to_string()),
            "https://example.com/page",
        )
        .unwrap_err();

        let msg = format!("{err}");
        assert!(msg.contains("ref-42"), "message must name the ref: {msg}");
        assert!(
            msg.contains("https://example.com/page"),
            "message must name the page url: {msg}"
        );
        assert!(
            msg.contains("stapler_browser_snapshot"),
            "message must name the corrective call: {msg}"
        );
    }

    #[test]
    fn resolve_locator_hit_should_return_backend_node_id_and_role_when_ref_present() {
        let mut latest_refs: HashMap<String, ax::ResolvedRef> = HashMap::new();
        latest_refs.insert("ref-1".to_string(), fake_resolved_ref(7, "button"));
        let known_refs: HashMap<String, u64> = HashMap::new();

        let (backend_node_id, role) = resolve_locator_impl(
            &latest_refs,
            &known_refs,
            0,
            &Locator("ref-1".to_string()),
            "https://example.com/",
        )
        .expect("ref-1 is present in latest_refs");

        assert_eq!(*backend_node_id.inner(), 7);
        assert_eq!(role, "button");
    }

    // ---- Epic 3: `install_snapshot` bookkeeping (Task 3.1.3) ----

    #[test]
    fn install_snapshot_should_tag_new_refs_with_current_generation_and_replace_latest_refs() {
        use stapler_mcp_core::ports::AxNode;

        let latest_refs = Rc::new(RefCell::new(HashMap::new()));
        let known_refs = Rc::new(RefCell::new(HashMap::new()));
        let latest_url = Rc::new(RefCell::new(String::new()));

        let mut refs = HashMap::new();
        refs.insert("ref-1".to_string(), fake_resolved_ref(1, "button"));
        let capture = ax::AxCapture {
            snapshot: AxSnapshot {
                root: AxNode {
                    node_ref: String::new(),
                    role: "WebArea".to_string(),
                    name: String::new(),
                    value: None,
                    children: vec![],
                },
                url: "https://example.com/after".to_string(),
                truncated: false,
                navigated_from: None,
            },
            refs,
        };

        let snapshot = install_snapshot(&latest_refs, &known_refs, &latest_url, 2, capture);

        assert_eq!(snapshot.url, "https://example.com/after");
        assert_eq!(*latest_url.borrow(), "https://example.com/after");
        assert_eq!(known_refs.borrow().get("ref-1"), Some(&2));
        assert!(latest_refs.borrow().contains_key("ref-1"));
    }

    #[test]
    fn install_snapshot_should_keep_original_generation_when_ref_already_known() {
        use stapler_mcp_core::ports::AxNode;

        let latest_refs = Rc::new(RefCell::new(HashMap::new()));
        let known_refs = Rc::new(RefCell::new(HashMap::new()));
        known_refs.borrow_mut().insert("ref-1".to_string(), 0);
        let latest_url = Rc::new(RefCell::new(String::new()));

        let mut refs = HashMap::new();
        refs.insert("ref-1".to_string(), fake_resolved_ref(1, "button"));
        let capture = ax::AxCapture {
            snapshot: AxSnapshot {
                root: AxNode {
                    node_ref: String::new(),
                    role: "WebArea".to_string(),
                    name: String::new(),
                    value: None,
                    children: vec![],
                },
                url: "https://example.com/".to_string(),
                truncated: false,
                navigated_from: None,
            },
            refs,
        };

        install_snapshot(&latest_refs, &known_refs, &latest_url, 5, capture);

        // Still tagged with the generation it was first issued under (0), not
        // the current one (5) — this is exactly the bookkeeping
        // `resolve_locator_should_return_stale_ref_message_...` depends on.
        assert_eq!(known_refs.borrow().get("ref-1"), Some(&0));
    }

    // ---- Epic 3: session-registry checks `navigate`/`click`/`type_text`/
    // `snapshot` all perform on entry ----
    //
    // `NativeBrowser::navigate`/`click`/`snapshot` themselves need a real
    // `chromiumoxide::Browser`/`Page` (a live Chromium process), which this
    // offline unit-test environment doesn't have — chromiumoxide only ever
    // constructs those via a live CDP handshake. Each method's very first
    // action, before touching `self.browser`/any `Page`, is the identical
    // `touch_or_evict` call already covered by Story 2.3's tests above; these
    // tests exercise that same entry check under names that map onto
    // validation.md's per-method rows, documenting that `navigate`/`snapshot`
    // inherit it unchanged. Full request/response round trips against a real
    // page are covered by the `#[ignore]`d Story 6.2 integration test.

    #[test]
    fn navigate_should_return_not_found_when_session_id_given_but_absent() {
        let sessions: RefCell<HashMap<String, FakeSession>> = RefCell::new(HashMap::new());

        let err = touch_or_evict(&sessions, "sess-missing", now_millis()).unwrap_err();

        match err {
            PortError::NotFound(msg) => {
                assert!(msg.contains("sess-missing"));
                assert!(msg.contains("stapler_browser_navigate"));
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_should_return_session_crashed_then_not_found_when_crash_flag_set() {
        let sessions: RefCell<HashMap<String, FakeSession>> = RefCell::new(HashMap::new());
        let session = FakeSession::new(0);
        session.set_crashed();
        sessions.borrow_mut().insert("sess-1".to_string(), session);

        let first = touch_or_evict(&sessions, "sess-1", now_millis()).unwrap_err();
        match first {
            PortError::SessionCrashed(msg) => assert!(msg.contains("sess-1")),
            other => panic!("expected SessionCrashed, got {other:?}"),
        }

        let second = touch_or_evict(&sessions, "sess-1", now_millis()).unwrap_err();
        match second {
            PortError::NotFound(_) => {}
            other => panic!("expected NotFound after eviction, got {other:?}"),
        }
    }

    // ---- Epic 3, Story 3.4: in-page-navigation SSRF re-check ----

    #[test]
    fn next_call_should_return_blocked_error_when_inpage_navigation_hit_link_local_address() {
        // Simulates what `spawn_session_listeners`'s `Page.frameNavigated`
        // closure does when an in-page navigation (link click, JS redirect,
        // ...) lands on a link-local address the SSRF guard would have
        // rejected at `navigate` time.
        let msg = frame_navigated_blocked_message(
            "sess-1",
            "http://169.254.169.254/latest/meta-data/",
            NetworkPolicy::Enforce,
        )
        .expect("link-local address must be blocked under NetworkPolicy::Enforce");

        assert!(msg.contains("sess-1"));
        assert!(msg.contains("169.254.169.254"));
        assert!(msg.contains("stapler_browser_navigate"));

        // The "next call" fallback: `blocked` is a plain `Option<String>`
        // behind an `Rc<RefCell<_>>` in production; a subsequent `snapshot`
        // (or any other `BrowserDriver` method) checks it on entry and
        // returns it verbatim as `PortError::NotFound`, exactly like this.
        let blocked: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(Some(msg.clone())));
        let err = match blocked.borrow().clone() {
            Some(reason) => PortError::NotFound(reason),
            None => panic!("expected blocked to be set"),
        };
        match err {
            PortError::NotFound(reported) => assert_eq!(reported, msg),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn frame_navigated_blocked_message_should_return_none_when_host_is_public() {
        let msg = frame_navigated_blocked_message(
            "sess-1",
            "https://example.com/",
            NetworkPolicy::Enforce,
        );
        assert!(msg.is_none());
    }

    /// `poll_blocked_grace_period` is a pure function over `Rc<RefCell<Option<String>>>`
    /// with no `Page`/CDP dependency, so — unlike the rest of this same-call-redirect
    /// race — it's exercisable fast and offline, without a real Chromium binary.
    /// This is the native-side counterpart of wasm's
    /// `npm/test/browser_glue.test.js` fake-`page.goto`/`setTimeout(...,5)`
    /// tests: it proves the *polling* (not in-band ordering with `goto`/
    /// `wait_for_navigation`) is what catches a `Page.frameNavigated` listener
    /// setting `blocked` a few milliseconds after `navigate`'s driver calls
    /// would otherwise have already returned.
    #[tokio::test]
    async fn poll_blocked_grace_period_should_observe_flag_set_shortly_after_polling_starts() {
        let blocked: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let setter_blocked = blocked.clone();

        // Races the poll against a delayed mutation — analogous to the
        // wasm test's `setTimeout(..., 5)` firing after the fake driver
        // call's promise has already resolved.
        let delayed_set = async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            *setter_blocked.borrow_mut() = Some("blocked-host".to_string());
        };

        let (reason, ()) = tokio::join!(poll_blocked_grace_period(&blocked), delayed_set);

        assert_eq!(
            reason,
            Some("blocked-host".to_string()),
            "the 20ms-step poll (up to 200ms) must observe a flag set 5ms after polling started"
        );
    }

    // ---- Epic 3, Story 3.3: click/type_text dispatch outcome shape ----
    //
    // The real dispatch (`DOM.resolveNode` -> `Runtime.callFunctionOn`) needs
    // a live `Page`/CDP connection and is exercised only by the `#[ignore]`d
    // Story 6.2 integration test. What's unit-testable offline is the data
    // flow around dispatch: `install_snapshot` replacing `latest_refs` with
    // the post-action capture, and `navigated_from` being set exactly when
    // the action caused the url to change — both exercised directly here
    // under names mapping onto validation.md's `click`/`snapshot` rows.

    #[test]
    fn click_should_return_updated_snapshot_when_button_ref_resolved() {
        use stapler_mcp_core::ports::AxNode;

        let latest_refs = Rc::new(RefCell::new(HashMap::new()));
        latest_refs
            .borrow_mut()
            .insert("ref-1".to_string(), fake_resolved_ref(1, "button"));
        let known_refs = Rc::new(RefCell::new(HashMap::new()));
        let latest_url = Rc::new(RefCell::new("https://example.com/".to_string()));

        // Resolve the locator exactly as `dispatch_action` does before
        // dispatch.
        let (backend_node_id, role) = resolve_locator_impl(
            &latest_refs.borrow(),
            &known_refs.borrow(),
            0,
            &Locator("ref-1".to_string()),
            &latest_url.borrow(),
        )
        .expect("ref-1 must resolve");
        assert_eq!(*backend_node_id.inner(), 1);
        assert_eq!(role, "button");

        // Simulate the post-dispatch capture: a new snapshot with a fresh
        // ref set and a changed url (the button navigated the page), then
        // `install_snapshot` + `navigated_from` assignment exactly as
        // `dispatch_action` performs it.
        let url_before = latest_url.borrow().clone();
        let mut refs = HashMap::new();
        refs.insert("ref-2".to_string(), fake_resolved_ref(2, "heading"));
        let capture = ax::AxCapture {
            snapshot: AxSnapshot {
                root: AxNode {
                    node_ref: String::new(),
                    role: "WebArea".to_string(),
                    name: String::new(),
                    value: None,
                    children: vec![],
                },
                url: "https://example.com/after-click".to_string(),
                truncated: false,
                navigated_from: None,
            },
            refs,
        };
        let mut snapshot = install_snapshot(&latest_refs, &known_refs, &latest_url, 0, capture);
        if snapshot.url != url_before {
            snapshot.navigated_from = Some(url_before.clone());
        }

        assert_eq!(snapshot.url, "https://example.com/after-click");
        assert_eq!(snapshot.navigated_from, Some(url_before));
        // Old ref must be gone (fully replaced, not merged) and the new
        // snapshot's ref must be present.
        assert!(!latest_refs.borrow().contains_key("ref-1"));
        assert!(latest_refs.borrow().contains_key("ref-2"));
    }

    #[test]
    fn snapshot_should_return_ok_without_mutating_page_when_session_live() {
        use stapler_mcp_core::ports::AxNode;

        // `snapshot`'s body performs no dispatch call at all — this test
        // documents that property by exercising exactly the capture+install
        // path `NativeBrowser::snapshot` runs (real `ax::capture_snapshot`
        // needs a live `Page`, so a hand-built `AxCapture` stands in for its
        // output), and confirming nothing beyond `latest_refs`/`known_refs`/
        // `latest_url` bookkeeping happens: no `blocked`/`crashed` flag is
        // touched, and no `navigated_from` is set.
        let latest_refs = Rc::new(RefCell::new(HashMap::new()));
        latest_refs
            .borrow_mut()
            .insert("ref-1".to_string(), fake_resolved_ref(1, "button"));
        let known_refs = Rc::new(RefCell::new(HashMap::new()));
        let latest_url = Rc::new(RefCell::new("https://example.com/".to_string()));
        let blocked: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let crashed = Rc::new(Cell::new(false));

        assert!(blocked.borrow().is_none(), "snapshot must check this on entry, unmodified here");

        let mut refs = HashMap::new();
        refs.insert("ref-1".to_string(), fake_resolved_ref(1, "button"));
        let capture = ax::AxCapture {
            snapshot: AxSnapshot {
                root: AxNode {
                    node_ref: String::new(),
                    role: "WebArea".to_string(),
                    name: String::new(),
                    value: None,
                    children: vec![],
                },
                url: "https://example.com/".to_string(),
                truncated: false,
                navigated_from: None,
            },
            refs,
        };
        let snapshot = install_snapshot(&latest_refs, &known_refs, &latest_url, 0, capture);

        assert_eq!(snapshot.url, "https://example.com/");
        assert_eq!(snapshot.navigated_from, None);
        assert!(!blocked.borrow().is_some());
        assert!(!crashed.get());
    }

    // ---- Epic 6 / Story 6.1: reaper eviction through the public tool API
    // (pre-mortem P2 item #4) ----
    //
    // `touch_or_evict_should_return_session_crashed_then_not_found_when_crash_flag_set`
    // above already proves the crash path end-to-end at the `touch_or_evict`
    // level; nothing previously proved the *reaper* eviction path all the way
    // through `stapler_mcp_core::tools::browser`'s public
    // `browser_navigate`/`browser_click`/`browser_snapshot` functions — only
    // the reaper's own map-scan (`reap_expired_should_remove_...` above) and
    // the tool layer's error-mapping (`crates/core/src/tools/browser.rs`,
    // against a canned `FakeBrowserDriver`) were tested in isolation. This
    // `BrowserDriver` impl wires the two real functions
    // (`reap_expired`/`touch_or_evict`) together behind a `FakeSession` map
    // (no real `chromiumoxide::Page`), so a bug in *how the lookup path
    // consults the session map* — not just in the reaper or the tool-layer
    // error mapping alone — would be caught here.
    struct ReapAwareFakeDriver {
        sessions: Rc<RefCell<HashMap<String, FakeSession>>>,
    }

    fn fake_ax_snapshot() -> AxSnapshot {
        AxSnapshot {
            root: stapler_mcp_core::ports::AxNode {
                node_ref: "e1".to_string(),
                role: "generic".to_string(),
                name: String::new(),
                value: None,
                children: Vec::new(),
            },
            url: "https://example.com/".to_string(),
            truncated: false,
            navigated_from: None,
        }
    }

    impl BrowserDriver for ReapAwareFakeDriver {
        async fn navigate_and_extract(
            &self,
            _url: &str,
            _timeout: Duration,
        ) -> Result<PageExtract, PortError> {
            panic!("not exercised by this test");
        }

        async fn navigate(
            &self,
            _url: &str,
            session_id: Option<&SessionId>,
            _timeout: Duration,
        ) -> Result<NavigateResult, PortError> {
            let id = session_id
                .expect("this test always reuses an existing session id")
                .0
                .clone();
            touch_or_evict(&self.sessions, &id, now_millis())?;
            Ok(NavigateResult {
                session_id: SessionId(id),
                final_url: fake_ax_snapshot().url,
                snapshot: fake_ax_snapshot(),
            })
        }

        async fn click(
            &self,
            session_id: &SessionId,
            _locator: &Locator,
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            touch_or_evict(&self.sessions, &session_id.0, now_millis())?;
            Ok(fake_ax_snapshot())
        }

        async fn type_text(
            &self,
            session_id: &SessionId,
            _locator: &Locator,
            _text: &str,
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            touch_or_evict(&self.sessions, &session_id.0, now_millis())?;
            Ok(fake_ax_snapshot())
        }

        async fn snapshot(
            &self,
            session_id: &SessionId,
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            touch_or_evict(&self.sessions, &session_id.0, now_millis())?;
            Ok(fake_ax_snapshot())
        }
    }

    #[tokio::test]
    async fn reaped_session_should_surface_not_found_through_browser_navigate_click_and_snapshot()
    {
        use stapler_mcp_core::schema::{
            BrowserClickInput, BrowserNavigateInput, BrowserSnapshotInput,
        };
        use stapler_mcp_core::tools::browser::{browser_click, browser_navigate, browser_snapshot};
        use stapler_mcp_core::tools::webcrawl::NetworkPolicy;

        let now = now_millis();
        let idle_since = now - (SESSION_IDLE_TIMEOUT.as_millis() as u64 + 1_000);
        let sessions: Rc<RefCell<HashMap<String, FakeSession>>> = Rc::new(RefCell::new(HashMap::new()));
        sessions
            .borrow_mut()
            .insert("sess-1".to_string(), FakeSession::new(idle_since));

        // Simulate the reaper's background scan evicting the idle session —
        // the exact same function the real `spawn_reaper` loop calls.
        reap_expired(&sessions, now).await;
        assert_eq!(sessions.borrow().len(), 0, "reaper should have evicted the idle session");

        let driver = ReapAwareFakeDriver { sessions };

        let navigate_err = browser_navigate(
            &driver,
            BrowserNavigateInput {
                url: "https://example.com/next".to_string(),
                session_id: Some("sess-1".to_string()),
                timeout_seconds: None,
            },
            NetworkPolicy::Enforce,
        )
        .await
        .expect_err("navigate against a reaped session id should error");
        assert_eq!(
            navigate_err,
            "no active browser session named 'sess-1'; call stapler_browser_navigate to start a new session"
        );

        let click_err = browser_click(
            &driver,
            BrowserClickInput {
                session_id: "sess-1".to_string(),
                ref_id: "e1".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("click against a reaped session id should error");
        assert_eq!(
            click_err,
            "no active browser session named 'sess-1'; call stapler_browser_navigate to start a new session"
        );

        let snapshot_err = browser_snapshot(
            &driver,
            BrowserSnapshotInput {
                session_id: "sess-1".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("snapshot against a reaped session id should error");
        assert_eq!(
            snapshot_err,
            "no active browser session named 'sess-1'; call stapler_browser_navigate to start a new session"
        );
    }
}
