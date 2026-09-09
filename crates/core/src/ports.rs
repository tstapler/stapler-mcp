//! OS-touching behavior lives entirely behind these traits so `stapler-mcp-core`
//! itself never calls `std::net`/`fs`/`process`/`env`/`time::Instant` directly.
//! A native adapter (tokio + fs4 + reqwest + chromiumoxide) and a wasm-bindgen
//! adapter (delegating to a Node.js host) each implement the same surface.
//!
//! Every port trait below uses native `async fn` in traits deliberately, not
//! `#[async_trait]`: every caller is generic over the concrete port type
//! (`fn index_source<H: HttpClient, ...>`, never `Box<dyn HttpClient>`), so
//! the lack of `dyn`-compatibility this lint warns about doesn't apply here.

#![allow(async_fn_in_trait)]

use std::time::Duration;

#[derive(Debug)]
pub enum PortError {
    Io(String),
    Timeout,
    Other(String),
    /// No entry exists for the given id — either it was never issued, or it
    /// was evicted (e.g. by the session idle reaper). The caller's fix is
    /// always "start a new session."
    NotFound(String),
    /// The entry is still present but its underlying resource has died (e.g.
    /// a browser tab crashed). Deliberately distinct from `NotFound`: the
    /// caller's fix is *not* "start a new session with a fresh id" — silently
    /// reusing the same crashed session id would just crash again, so this
    /// tells the caller the specific id it holds is now dead.
    SessionCrashed(String),
    /// A resolved element failed a `click`/`type` actionability check
    /// (hidden, disabled, still animating, or covered by another element)
    /// after exhausting the retry/backoff window. Distinct from `NotFound`:
    /// the ref itself is still valid, so the caller's fix is to wait/inspect
    /// the page, not to re-snapshot for a fresh ref.
    NotActionable(String),
    /// The requested domain doesn't match the session's current live page
    /// host (see `same_host`) — nothing was typed. Not fixable by retrying
    /// the same call; the caller must re-check the actual current-page
    /// domain or accept there's no credential for this site.
    CredentialDomainMismatch(String),
    /// Multiple vault items matched the domain; nothing was typed. Not
    /// fixable with `CredentialRef`'s shape alone — requires human
    /// disambiguation (see the candidate list in the message).
    CredentialAmbiguous(String),
    /// The vault backend itself isn't authenticated/reachable. An operator
    /// problem, not something the calling LLM can fix by retrying.
    CredentialUnauthenticated(String),
    /// 1Password's account-wide rate limit was hit. Retryable, but only
    /// after the message's stated delay — not immediately.
    CredentialRateLimited(String),
    /// The resolved TOTP code's ~30s validity window elapsed before it
    /// could be typed. The one immediately-retryable case in this family.
    CredentialExpired(String),
}

impl std::fmt::Display for PortError {
    // Like `NotFound`/`SessionCrashed` above, the adapter constructing a
    // `Credential*` variant is expected to have already built the full
    // user-facing sentence (matching `ux.md`'s exact drafted strings) into
    // the inner `String` — callers that already know the specific variant
    // should pass it through verbatim rather than via `Display`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PortError::Io(e) => write!(f, "io error: {e}"),
            PortError::Timeout => write!(f, "timed out"),
            PortError::Other(e) => write!(f, "{e}"),
            PortError::NotFound(e) => write!(f, "not found: {e}"),
            PortError::SessionCrashed(e) => write!(f, "session crashed: {e}"),
            PortError::NotActionable(e) => write!(f, "not actionable: {e}"),
            PortError::CredentialDomainMismatch(e) => write!(f, "credential rejected: {e}"),
            PortError::CredentialAmbiguous(e) => write!(f, "credential rejected: {e}"),
            PortError::CredentialUnauthenticated(e) => write!(f, "vault unauthenticated: {e}"),
            PortError::CredentialRateLimited(e) => write!(f, "vault rate-limited: {e}"),
            PortError::CredentialExpired(e) => write!(f, "credential expired: {e}"),
        }
    }
}

impl std::error::Error for PortError {}

#[derive(Debug)]
pub enum LockError {
    /// Another instance already holds the exclusive lock. Not an error condition
    /// for the caller — it's the losing side of the flock race, expected to exit cleanly.
    AlreadyRunning,
    Other(String),
}

pub trait Conn {
    /// Reads up to (and stripping) the next newline. `Ok(None)` means clean EOF
    /// before any data arrived.
    async fn read_frame(&mut self) -> Result<Option<Vec<u8>>, PortError>;
    /// Writes `bytes` followed by a single newline.
    async fn write_frame(&mut self, bytes: &[u8]) -> Result<(), PortError>;
    fn set_timeout(&mut self, dur: Duration);
}

pub trait Listener {
    type C: Conn;
    async fn accept(&mut self) -> Result<Self::C, PortError>;
}

pub trait SocketFactory {
    type L: Listener<C = Self::C>;
    type C: Conn;
    async fn bind(&self, path: &str) -> Result<Self::L, PortError>;
    async fn connect(&self, path: &str, timeout: Duration) -> Result<Self::C, PortError>;
    /// Best-effort removal of a socket file left behind by a crashed daemon.
    /// Only ever called after the caller has won the exclusive lock, which is
    /// what proves no other daemon owns that socket.
    async fn remove_stale(&self, path: &str) -> Result<(), PortError>;
}

pub trait LockGuard {
    /// Best-effort write of the current PID into the lockfile, purely for
    /// operator debugging of a stuck daemon.
    fn write_pid(&mut self, pid: u32);
}

pub trait ProcessLock {
    type Guard: LockGuard;
    /// Held for the daemon's entire lifetime — dropping the guard releases the lock.
    async fn acquire_exclusive(&self, path: &str) -> Result<Self::Guard, LockError>;
}

pub trait ProcessSpawner {
    /// Spawns a detached `--daemon` process, redirecting its stdout/stderr to
    /// `log_path` (it has no controlling terminal once detached). Does not wait
    /// for the child; the daemon must outlive the spawning process.
    async fn spawn_daemon(&self, exe_hint: Option<&str>, log_path: &str) -> Result<(), PortError>;
}

pub trait EnvPort {
    fn var(&self, key: &str) -> Option<String>;
    fn home_dir(&self) -> Option<String>;
}

pub trait ClockPort {
    fn now_millis(&self) -> u64;
}

pub trait SleepPort {
    async fn sleep(&self, dur: Duration);
}

pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    /// The URL actually served, after following any redirects.
    pub final_url: String,
}

pub trait HttpClient {
    async fn get(&self, url: &str, headers: &[(String, String)])
        -> Result<HttpResponse, PortError>;
}

pub struct PageExtract {
    pub title: String,
    pub html: String,
    pub text: String,
    pub final_url: String,
}

/// Opaque handle to a persistent browser session (a live tab), returned by
/// `navigate` and threaded through every subsequent `click`/`type_text`/
/// `snapshot` call against the same tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionId(pub String);

/// A `ref` string from a previously-returned `AxSnapshot`, identifying the
/// element to act on. Opaque to callers outside this crate — never a CSS
/// selector or role/name pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locator(pub String);

/// Placeholder substituted for a resolved secret's value everywhere it would
/// otherwise appear in output — `SecretValue`'s `Debug` impl, and the
/// acted-on node's `value` in `type_secret`'s returned `AxSnapshot`.
pub const REDACTED_PLACEHOLDER: &str = "[REDACTED]";

/// A credential value resolved from a `CredentialStore`. Backed by
/// `zeroize::Zeroizing<String>` so the backing memory is overwritten on
/// drop, and its `Debug` impl never prints the wrapped value — use
/// `expose()` only at the point the value must actually be used (e.g.
/// dispatching a keystroke), never in a log line or error message.
pub struct SecretValue(zeroize::Zeroizing<String>);

impl SecretValue {
    pub fn new(value: String) -> Self {
        Self(zeroize::Zeroizing::new(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SecretValue")
            .field(&REDACTED_PLACEHOLDER)
            .finish()
    }
}

/// One node in an accessibility-tree snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct AxNode {
    pub node_ref: String,
    pub role: String,
    pub name: String,
    /// `None` for non-form-control nodes (buttons, generic containers).
    /// `Some(current_text)` for textbox/combobox-like nodes, populated from
    /// the CDP AX node's own `value` property where present — lets a caller
    /// confirm typed text landed from `type_text`'s own returned snapshot
    /// without a follow-up `snapshot` call.
    pub value: Option<String>,
    pub children: Vec<AxNode>,
}

/// A full accessibility-tree snapshot of a session's current page.
#[derive(Debug, Clone, PartialEq)]
pub struct AxSnapshot {
    pub root: AxNode,
    pub url: String,
    pub truncated: bool,
    /// `None` for every snapshot except one returned by `click`/`type_text`
    /// when that specific call's dispatch caused the page's URL to change —
    /// in which case it holds the pre-dispatch URL. Never conflated with the
    /// unrelated SSRF-`blocked` signal.
    pub navigated_from: Option<String>,
}

pub struct NavigateResult {
    pub session_id: SessionId,
    pub final_url: String,
    pub snapshot: AxSnapshot,
}

/// What `BrowserDriver::tabs` should do to `session_id`'s tab set.
#[derive(Debug, Clone, PartialEq)]
pub enum TabAction {
    List,
    New {
        url: Option<String>,
    },
    Select {
        index: usize,
    },
    /// `None` closes whichever tab is currently active.
    Close {
        index: Option<usize>,
    },
}

/// What `BrowserDriver::history` should do to `session_id`'s current tab.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HistoryAction {
    Back,
    Forward,
    Reload,
}

/// The specific vault field a `CredentialRef` names. Closed by
/// construction — exactly 3 legal values, exactly like `HistoryAction` —
/// so a typo or unexpected value fails to compile rather than silently
/// misresolving (e.g. falling through to the password-shaped path).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CredentialField {
    Username,
    Password,
    Totp,
}

/// An opaque, non-secret domain/field identifier used to look up a
/// credential in a `CredentialStore` — never carries a credential value
/// itself. `domain` is matched by exact host-string equality against the
/// session's live URL (see `same_host`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CredentialRef {
    pub domain: String,
    pub field: CredentialField,
}

/// One entry in a `TabsResult` listing.
#[derive(Debug, Clone, PartialEq)]
pub struct TabInfo {
    pub index: usize,
    pub url: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TabsResult {
    pub tabs: Vec<TabInfo>,
    pub active_index: usize,
    /// Populated only for `TabAction::New`/`Select` where a fresh snapshot of
    /// the (now-active) tab is useful; `None` for `List`/`Close`.
    pub snapshot: Option<AxSnapshot>,
}

/// One entry in a `list_sessions` listing.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub session_id: String,
    pub tab_count: usize,
    /// Milliseconds since this session's last activity, mirroring
    /// `reap_expired`'s `now.saturating_sub(s.last_used())`.
    pub idle_ms: u64,
    /// `true` if an in-page navigation landed this session on a host the
    /// SSRF guard would have blocked (see `BrowserSession::blocked`) — the
    /// session is unusable until re-navigated.
    pub blocked: bool,
    /// `true` if the session's page target has crashed (see
    /// `BrowserSession::crashed`) — the session is unusable and will be
    /// evicted on next touch.
    pub crashed: bool,
}

/// What `BrowserDriver::wait_for` polls for before returning.
#[derive(Debug, Clone, PartialEq)]
pub enum WaitCondition {
    TextAppears(String),
    TextDisappears(String),
    /// A fixed delay, not a poll — used when the caller just needs to give
    /// the page time (e.g. an animation) rather than watch for text.
    TimeMs(u64),
}

/// Resolves an opaque `CredentialRef` to the vault-held `SecretValue` it
/// names. Cross-references `SecretValue`'s non-leaking `Debug`: every
/// implementation must log its own domain/field/outcome (see the
/// Observability Plan) but must never log the resolved value itself.
pub trait CredentialStore {
    async fn resolve(&self, credential_ref: &CredentialRef) -> Result<SecretValue, PortError>;
}

pub trait BrowserDriver {
    /// Coarse, call-level operation (navigate + read title/HTML/text/final-URL
    /// in one hop) rather than exposing CDP-message-level primitives — this is
    /// what keeps the wasm↔JS boundary to one crossing per tool call once a
    /// wasm-bindgen adapter exists.
    async fn navigate_and_extract(
        &self,
        url: &str,
        timeout: Duration,
    ) -> Result<PageExtract, PortError>;

    /// Navigates to `url`, either in a fresh tab (`session_id: None`) or an
    /// existing session's tab (`session_id: Some(id)`, erroring with
    /// `PortError::NotFound` if `id` has no live session).
    async fn navigate(
        &self,
        url: &str,
        session_id: Option<&SessionId>,
        timeout: Duration,
    ) -> Result<NavigateResult, PortError>;
    /// Resolves `locator` against `session_id`'s current snapshot and clicks it.
    async fn click(
        &self,
        session_id: &SessionId,
        locator: &Locator,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError>;
    /// Resolves `locator` against `session_id`'s current snapshot and types
    /// `text` into it.
    async fn type_text(
        &self,
        session_id: &SessionId,
        locator: &Locator,
        text: &str,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError>;
    /// Resolves `credential_ref` and types the result into `locator`, exactly
    /// like `type_text` except: (1) no plaintext ever crosses this *trait*
    /// call boundary in either direction — the implementor resolves
    /// internally, it is never handed a resolved `SecretValue` as an
    /// argument — and (2) the acted-on node's `value` in the returned
    /// `AxSnapshot` is always `REDACTED_PLACEHOLDER`, independent of the
    /// structural redaction every snapshot-producing call already applies
    /// (see `ax.rs`/`browser.js`'s redaction pass) — belt-and-suspenders per
    /// `architecture.md` §6.
    ///
    /// **Resolution locus, explicit**: the implementing adapter (native/wasm)
    /// resolves `credential_ref` by calling its OWN adapter-owned
    /// `CredentialStore` handle internally, as the first step of this method's
    /// own body — it is not resolved by the tool-layer caller beforehand and
    /// handed in. The daemon is responsible for constructing/injecting that
    /// `CredentialStore` dependency into the `BrowserDriver` adapter at
    /// startup (Phase 3/4 wiring, Phase 6 daemon wiring) — see ADR-001. The
    /// tool-layer handler built on top of this trait (Phase 5) calls this
    /// method exactly once per `type_secret` request and never calls
    /// `CredentialStore::resolve` itself; this is also what makes ADR-003's
    /// in-flight dedup (Story 3.2.5/4.2.2) actually effective — every resolve
    /// request passes through the one adapter-owned `CredentialStore`, so its
    /// dedup map sees all of them, not just some.
    async fn type_secret(
        &self,
        session_id: &SessionId,
        locator: &Locator,
        credential_ref: &CredentialRef,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError>;

    /// Captures a fresh accessibility-tree snapshot of `session_id`'s current
    /// page without mutating it.
    async fn snapshot(
        &self,
        session_id: &SessionId,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError>;

    /// Tears down `session_id`'s browser context/tab entirely. Distinct from
    /// `tabs`' `Close` action: this ends the whole session (all its tabs),
    /// not just one tab within it.
    async fn close_session(&self, session_id: &SessionId) -> Result<(), PortError>;

    /// Lists every currently live session, across all callers — not scoped to
    /// one session id, unlike every other method on this trait.
    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, PortError>;

    /// Lists, opens, switches, or closes tabs within `session_id`'s browser
    /// context.
    async fn tabs(
        &self,
        session_id: &SessionId,
        action: TabAction,
        timeout: Duration,
    ) -> Result<TabsResult, PortError>;

    /// Resolves `locator` against `session_id`'s current snapshot and moves
    /// the pointer over it, without clicking — needed for hover-triggered
    /// menus/tooltips that `click` can't reach.
    async fn hover(
        &self,
        session_id: &SessionId,
        locator: &Locator,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError>;

    /// Resolves `locator` against `session_id`'s current snapshot (must be a
    /// `<select>`-like control) and sets its selected option(s) to `values`.
    async fn select_option(
        &self,
        session_id: &SessionId,
        locator: &Locator,
        values: &[String],
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError>;

    /// Sends `key` (e.g. `"Enter"`, `"ArrowDown"`) to `locator` if given, or
    /// to the page's currently focused element otherwise.
    async fn press_key(
        &self,
        session_id: &SessionId,
        key: &str,
        locator: Option<&Locator>,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError>;

    /// Blocks until `condition` is satisfied or `timeout` elapses, then
    /// returns a fresh snapshot. `timeout` bounds the whole wait, not a
    /// single poll attempt.
    async fn wait_for(
        &self,
        session_id: &SessionId,
        condition: WaitCondition,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError>;

    /// Captures `session_id`'s current page as PNG bytes. `full_page: true`
    /// captures the full scrollable page rather than just the current
    /// viewport.
    async fn screenshot(
        &self,
        session_id: &SessionId,
        full_page: bool,
        timeout: Duration,
    ) -> Result<Vec<u8>, PortError>;

    /// Runs `function` (a JS function-expression string, e.g. `"() =>
    /// document.title"`) in `session_id`'s current page and returns its
    /// result as JSON. If `locator` is given, `function` is invoked with the
    /// resolved element as its argument (`function(element) { ... }`)
    /// instead of running at page scope.
    async fn evaluate(
        &self,
        session_id: &SessionId,
        function: &str,
        locator: Option<&Locator>,
        timeout: Duration,
    ) -> Result<serde_json::Value, PortError>;

    /// Navigates `session_id`'s current tab back/forward through its history,
    /// or reloads it in place. Subject to the same blocked-host guard as
    /// every other navigation-causing call — `PortError::NotFound` if the
    /// resulting page is on a blocked host.
    async fn history(
        &self,
        session_id: &SessionId,
        action: HistoryAction,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError>;

    /// Resizes `session_id`'s current tab's viewport to `width`x`height`
    /// (CSS pixels) and returns a fresh snapshot, since a resize can change
    /// what's visible/laid out.
    async fn resize(
        &self,
        session_id: &SessionId,
        width: u32,
        height: u32,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError>;
}

pub trait FileStore {
    /// Creates parent directories as needed.
    async fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), PortError>;
    /// `Ok(None)` means the file doesn't exist (a cache miss), not an error.
    async fn read_file(&self, path: &str) -> Result<Option<Vec<u8>>, PortError>;
    /// Idempotent — deleting a path that doesn't exist is `Ok(())`, not an error.
    async fn delete_file(&self, path: &str) -> Result<(), PortError>;
}

/// Runs local embedding inference (loading an ONNX model, tokenizing, tensor
/// math). Intentionally native-only for v1: no wasm implementation exists,
/// because `fastembed`'s `ort` dependency has no path to
/// `wasm32-unknown-unknown` (maintainer-abandoned wasm support) — see
/// docs-index's ADR-0002. `tools::docs`, the only caller, is compiled out of
/// the wasm32 target entirely, so this asymmetry with every other port trait
/// (which all have at least a partial wasm adapter) is deliberate, not a gap.
pub trait Embedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axnode_debug_format_should_contain_node_ref_role_name_when_button_node_given() {
        let node = AxNode {
            node_ref: "e3".into(),
            role: "button".into(),
            name: "Submit".into(),
            value: None,
            children: vec![],
        };

        let formatted = format!("{node:?}");

        assert!(formatted.contains("node_ref: \"e3\""));
        assert!(formatted.contains("role: \"button\""));
        assert!(formatted.contains("name: \"Submit\""));
    }

    #[test]
    fn axnode_value_should_carry_typed_text_independently_of_name_when_textbox_node_given() {
        let node = AxNode {
            node_ref: "e2".into(),
            role: "textbox".into(),
            name: "Email".into(),
            value: Some("user@example.com".into()),
            children: vec![],
        };

        assert_eq!(node.name, "Email");
        assert_eq!(node.value, Some("user@example.com".into()));
    }

    #[test]
    fn axsnapshot_navigated_from_should_default_to_none_when_returned_by_navigate_or_snapshot() {
        let snapshot = AxSnapshot {
            root: AxNode {
                node_ref: "e1".into(),
                role: "generic".into(),
                name: String::new(),
                value: None,
                children: vec![],
            },
            url: "https://example.com/a".into(),
            truncated: false,
            navigated_from: None,
        };

        assert_eq!(snapshot.navigated_from, None);
    }

    /// Task 1.1.2 AC: a test double implementing all `BrowserDriver`
    /// methods with `todo!()` bodies must compile — confirms the trait
    /// signatures are well-formed and don't conflict with each other. The
    /// bodies are never invoked.
    struct StubBrowser;

    impl BrowserDriver for StubBrowser {
        async fn navigate_and_extract(
            &self,
            _url: &str,
            _timeout: Duration,
        ) -> Result<PageExtract, PortError> {
            todo!()
        }

        async fn navigate(
            &self,
            _url: &str,
            _session_id: Option<&SessionId>,
            _timeout: Duration,
        ) -> Result<NavigateResult, PortError> {
            todo!()
        }

        async fn click(
            &self,
            _session_id: &SessionId,
            _locator: &Locator,
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            todo!()
        }

        async fn type_text(
            &self,
            _session_id: &SessionId,
            _locator: &Locator,
            _text: &str,
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            todo!()
        }

        async fn type_secret(
            &self,
            _session_id: &SessionId,
            _locator: &Locator,
            _credential_ref: &CredentialRef,
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            todo!()
        }

        async fn snapshot(
            &self,
            _session_id: &SessionId,
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            todo!()
        }

        async fn close_session(&self, _session_id: &SessionId) -> Result<(), PortError> {
            todo!()
        }

        async fn list_sessions(&self) -> Result<Vec<SessionSummary>, PortError> {
            todo!()
        }

        async fn tabs(
            &self,
            _session_id: &SessionId,
            _action: TabAction,
            _timeout: Duration,
        ) -> Result<TabsResult, PortError> {
            todo!()
        }

        async fn hover(
            &self,
            _session_id: &SessionId,
            _locator: &Locator,
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            todo!()
        }

        async fn select_option(
            &self,
            _session_id: &SessionId,
            _locator: &Locator,
            _values: &[String],
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            todo!()
        }

        async fn press_key(
            &self,
            _session_id: &SessionId,
            _key: &str,
            _locator: Option<&Locator>,
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            todo!()
        }

        async fn wait_for(
            &self,
            _session_id: &SessionId,
            _condition: WaitCondition,
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            todo!()
        }

        async fn screenshot(
            &self,
            _session_id: &SessionId,
            _full_page: bool,
            _timeout: Duration,
        ) -> Result<Vec<u8>, PortError> {
            todo!()
        }

        async fn evaluate(
            &self,
            _session_id: &SessionId,
            _function: &str,
            _locator: Option<&Locator>,
            _timeout: Duration,
        ) -> Result<serde_json::Value, PortError> {
            todo!()
        }

        async fn history(
            &self,
            _session_id: &SessionId,
            _action: HistoryAction,
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            todo!()
        }

        async fn resize(
            &self,
            _session_id: &SessionId,
            _width: u32,
            _height: u32,
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            todo!()
        }
    }

    #[test]
    fn stub_browser_driver_should_compile_when_type_secret_arm_has_todo_body() {
        // Merely constructing it is the assertion: if the trait signatures
        // were malformed or clashed with each other (including the new
        // `type_secret` method), this file wouldn't compile at all.
        let _stub = StubBrowser;
    }

    #[test]
    fn port_error_not_found_display_should_include_message_when_formatted() {
        let err = PortError::NotFound("session sess-1".into());
        assert_eq!(format!("{err}"), "not found: session sess-1");
    }

    #[test]
    fn port_error_session_crashed_display_should_include_message_when_formatted() {
        let err = PortError::SessionCrashed("sess-2".into());
        assert_eq!(format!("{err}"), "session crashed: sess-2");
    }

    #[test]
    fn secret_value_debug_should_redact_when_formatted() {
        let secret = SecretValue::new("hunter2".to_string());

        let formatted = format!("{secret:?}");

        assert_eq!(formatted, "SecretValue(\"[REDACTED]\")");
        assert!(!formatted.contains("hunter2"));
    }

    #[test]
    fn credential_ref_should_hash_and_eq_when_used_as_map_key() {
        use std::collections::HashSet;

        let username_ref = CredentialRef {
            domain: "example.com".into(),
            field: CredentialField::Username,
        };
        let password_ref = CredentialRef {
            domain: "example.com".into(),
            field: CredentialField::Password,
        };

        assert_ne!(username_ref, password_ref);

        let mut set = HashSet::new();
        set.insert(username_ref.clone());
        set.insert(password_ref.clone());
        assert_eq!(set.len(), 2);
        assert!(set.contains(&username_ref));
        assert!(set.contains(&password_ref));
    }

    #[test]
    fn secret_value_should_wrap_zeroizing_string_when_constructed() {
        let secret = SecretValue::new("hunter2".to_string());

        // Structural check per the AC: confirms the backing field type is
        // `Zeroizing<String>` (whose own `Drop` impl, already exercised by
        // `zeroize`'s own test suite, zeroes the buffer) rather than a bare
        // `String`.
        let backing: &zeroize::Zeroizing<String> = &secret.0;
        assert_eq!(backing.as_str(), "hunter2");
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn port_error_credential_domain_mismatch_display_should_start_with_credential_rejected_prefix()
    {
        let err = PortError::CredentialDomainMismatch(
            "no vault entry for domain \"example.com\"".into(),
        );
        assert!(format!("{err}").starts_with("credential rejected: "));
    }

    #[test]
    fn port_error_credential_ambiguous_display_should_start_with_credential_rejected_prefix() {
        let err = PortError::CredentialAmbiguous("2 vault items matched \"example.com\"".into());
        assert!(format!("{err}").starts_with("credential rejected: "));
    }

    #[test]
    fn port_error_credential_unauthenticated_display_should_start_with_vault_unauthenticated_prefix()
     {
        let err = PortError::CredentialUnauthenticated("1Password CLI not signed in".into());
        assert!(format!("{err}").starts_with("vault unauthenticated: "));
    }

    #[test]
    fn port_error_credential_rate_limited_display_should_start_with_vault_rate_limited_prefix() {
        let err = PortError::CredentialRateLimited("retry after 30s".into());
        assert!(format!("{err}").starts_with("vault rate-limited: "));
    }

    #[test]
    fn port_error_credential_expired_display_should_start_with_credential_expired_prefix() {
        let err = PortError::CredentialExpired(
            "TOTP code for example.com expired before it could be typed".into(),
        );
        assert!(format!("{err}").starts_with("credential expired: "));
    }
}
