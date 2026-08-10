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
}

impl std::fmt::Display for PortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PortError::Io(e) => write!(f, "io error: {e}"),
            PortError::Timeout => write!(f, "timed out"),
            PortError::Other(e) => write!(f, "{e}"),
            PortError::NotFound(e) => write!(f, "not found: {e}"),
            PortError::SessionCrashed(e) => write!(f, "session crashed: {e}"),
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
    /// Captures a fresh accessibility-tree snapshot of `session_id`'s current
    /// page without mutating it.
    async fn snapshot(
        &self,
        session_id: &SessionId,
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

    /// Task 1.1.2 AC: a test double implementing only the 5 `BrowserDriver`
    /// methods with `todo!()` bodies must compile — confirms the trait
    /// signatures are well-formed and don't conflict with
    /// `navigate_and_extract`. The bodies are never invoked.
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

        async fn snapshot(
            &self,
            _session_id: &SessionId,
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            todo!()
        }
    }

    #[test]
    fn stub_browser_driver_should_compile_when_five_methods_have_todo_bodies() {
        // Merely constructing it is the assertion: if the trait signatures
        // were malformed or clashed with `navigate_and_extract`, this file
        // wouldn't compile at all.
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
}
