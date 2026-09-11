use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use stapler_mcp_core::ports::{
    AxNode, AxSnapshot, BrowserDriver, CredentialRef, CredentialStore, HistoryAction, Locator,
    NavigateResult, PageExtract, PortError, SecretValue, SessionId, SessionSummary, TabAction,
    TabInfo, TabsResult, WaitCondition,
};
use stapler_mcp_core::tools::webcrawl::same_host;
use url::Url;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::js_util::js_err_to_string;

#[wasm_bindgen(module = "/src/glue/browser.js")]
extern "C" {
    #[wasm_bindgen(js_name = jsNavigateAndExtract)]
    fn js_navigate_and_extract(url: &str, timeout_ms: f64) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsCloseBrowser)]
    fn js_close_browser() -> js_sys::Promise;

    #[wasm_bindgen(js_name = jsBrowserNavigate)]
    fn js_browser_navigate(
        url: &str,
        session_id: Option<String>,
        timeout_ms: f64,
    ) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsBrowserClick)]
    fn js_browser_click(session_id: &str, ref_id: &str, timeout_ms: f64) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsBrowserType)]
    fn js_browser_type(
        session_id: &str,
        ref_id: &str,
        text: &str,
        timeout_ms: f64,
    ) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsBrowserSnapshot)]
    fn js_browser_snapshot(session_id: &str, timeout_ms: f64) -> js_sys::Promise;

    #[wasm_bindgen(js_name = jsCloseSession)]
    fn js_close_session(session_id: &str) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsListSessions)]
    fn js_list_sessions() -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsBrowserTabs)]
    fn js_browser_tabs(session_id: &str, action_json: &str, timeout_ms: f64) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsBrowserHover)]
    fn js_browser_hover(session_id: &str, ref_id: &str, timeout_ms: f64) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsBrowserSelectOption)]
    fn js_browser_select_option(
        session_id: &str,
        ref_id: &str,
        values_json: &str,
        timeout_ms: f64,
    ) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsBrowserPressKey)]
    fn js_browser_press_key(
        session_id: &str,
        key: &str,
        ref_id: Option<String>,
        timeout_ms: f64,
    ) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsBrowserWaitFor)]
    fn js_browser_wait_for(
        session_id: &str,
        condition_json: &str,
        timeout_ms: f64,
    ) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsBrowserScreenshot)]
    fn js_browser_screenshot(session_id: &str, full_page: bool, timeout_ms: f64)
        -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsBrowserEvaluate)]
    fn js_browser_evaluate(
        session_id: &str,
        function: &str,
        ref_id: Option<String>,
        timeout_ms: f64,
    ) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsBrowserHistory)]
    fn js_browser_history(session_id: &str, action: &str, timeout_ms: f64) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsBrowserResize)]
    fn js_browser_resize(
        session_id: &str,
        width: u32,
        height: u32,
        timeout_ms: f64,
    ) -> js_sys::Promise;

    // Epic 4.3: `type_secret` dispatch support (see `crates/wasm/src/glue/
    // browser.js`'s matching doc comments).
    #[wasm_bindgen(js_name = jsBrowserCurrentUrl)]
    fn js_browser_current_url(session_id: &str) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsBrowserTypeSecret)]
    fn js_browser_type_secret(
        session_id: &str,
        ref_id: &str,
        secret_value: &str,
        timeout_ms: f64,
    ) -> js_sys::Promise;
}

/// Object-safe erasure of `CredentialStore` for storage behind `dyn`, mirroring
/// `crates/native/src/browser.rs`'s identically-named trait/impl/helper.
/// `CredentialStore::resolve` is a native `async fn` in a trait (ports.rs's
/// deliberate choice for every *other* port — generic callers, never `Box<dyn
/// Port>`), which has no dyn-compatible vtable representation, so `dyn
/// CredentialStore` cannot be named directly. Boxing the future once, at this
/// boundary, is the standard shim for that mismatch — `WasmBrowser` needs a
/// `Rc<dyn ...>` field so `set_credential_store` can inject a concrete store
/// post-construction without making `WasmBrowser` generic over it.
trait DynCredentialStore {
    fn resolve<'a>(
        &'a self,
        credential_ref: &'a CredentialRef,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<SecretValue, PortError>> + 'a>>;
}

impl<T: CredentialStore> DynCredentialStore for T {
    fn resolve<'a>(
        &'a self,
        credential_ref: &'a CredentialRef,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<SecretValue, PortError>> + 'a>>
    {
        Box::pin(CredentialStore::resolve(self, credential_ref))
    }
}

/// Story 4.3.0's fail-closed lookup: `None` (no `set_credential_store` call
/// ever made) becomes `PortError::CredentialUnauthenticated`, not a panic —
/// pulled out of `type_secret`'s body so it's unit-testable against a bare
/// `RefCell` slot, mirroring native's `resolve_via_injected_store`.
fn resolve_via_injected_store(
    slot: &RefCell<Option<Rc<dyn DynCredentialStore>>>,
) -> Result<Rc<dyn DynCredentialStore>, PortError> {
    slot.borrow().clone().ok_or_else(|| {
        PortError::CredentialUnauthenticated(
            "no credential store is configured for this browser session — not typed".to_string(),
        )
    })
}

/// Story 3.4.3's `check_credential_domain`, reproduced verbatim for wasm
/// (`crates/native/src/browser.rs` has the native copy): rejects
/// `type_secret` when the session's freshly-queried live URL doesn't match
/// `credential_ref`'s requested domain — checked *before*
/// `CredentialStore::resolve` is ever called, closing the same
/// same-call-redirect staleness class `6b6b56a` already fixed for the SSRF
/// guard (a cached URL would let a same-call redirect slip a credential past
/// this check).
fn check_credential_domain(live_url: &str, requested_domain: &str) -> Result<(), PortError> {
    let live = Url::parse(live_url).map_err(|_| {
        PortError::Other(format!(
            "current page url \"{live_url}\" could not be parsed"
        ))
    })?;
    // A bare host has no scheme; `same_host` only compares `Url::host_str()`
    // (mirrors native's identical construction in `crates/native/src/vault.rs`
    // and `crates/wasm/src/glue/vault.js`'s `sameHost`).
    let requested = Url::parse(&format!("https://{requested_domain}"))
        .map_err(|_| PortError::Other(format!("invalid domain \"{requested_domain}\"")))?;
    if same_host(&live, &requested) {
        return Ok(());
    }
    let current_host = live.host_str().unwrap_or(live_url);
    Err(PortError::CredentialDomainMismatch(format!(
        "current page is \"{current_host}\", but this credential is scoped to \"{requested_domain}\" — not typed; call stapler_browser_snapshot to confirm the current page, or use the correct domain"
    )))
}

pub struct WasmBrowser {
    /// Injected post-construction via `set_credential_store` (Story 4.3.0) —
    /// `None` until `crates/wasm/src/lib.rs`'s daemon wiring sets one, so
    /// `type_secret` must fail closed rather than panic when it's unset.
    credential_store: RefCell<Option<Rc<dyn DynCredentialStore>>>,
}

impl Default for WasmBrowser {
    fn default() -> Self {
        Self::new()
    }
}

impl WasmBrowser {
    pub fn new() -> Self {
        WasmBrowser {
            credential_store: RefCell::new(None),
        }
    }

    /// Injects the `CredentialStore` `type_secret` resolves against — see
    /// `DynCredentialStore`'s doc comment for why this takes `Rc<S>` (generic
    /// over the concrete store type) rather than `Rc<dyn CredentialStore>`
    /// directly. Callers write this the same either way
    /// (`browser.set_credential_store(Rc::new(store))`).
    pub fn set_credential_store<S: CredentialStore + 'static>(&self, store: Rc<S>) {
        *self.credential_store.borrow_mut() = Some(store as Rc<dyn DynCredentialStore>);
    }

    /// Must be called once, explicitly, at daemon shutdown — there is no
    /// synchronous `Drop` equivalent that can await a promise, so this can't
    /// just be a destructor.
    pub async fn close(&self) {
        let _ = JsFuture::from(js_close_browser()).await;
    }
}

/// Mirrors the plain-object shape `browser.js`'s `parseAriaSnapshot`/
/// `captureSnapshot` produce. Kept private to this module — `AxNode`/
/// `AxSnapshot` (the core port types) deliberately don't derive
/// `Deserialize`, since every other adapter builds them by hand too; this DTO
/// exists only to let `serde_wasm_bindgen` do the JS-object walk once, then
/// gets converted into the real port types below.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsAxNode {
    #[serde(rename = "ref")]
    node_ref: String,
    role: String,
    name: String,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    children: Vec<JsAxNode>,
}

impl From<JsAxNode> for AxNode {
    fn from(n: JsAxNode) -> Self {
        AxNode {
            node_ref: n.node_ref,
            role: n.role,
            name: n.name,
            value: n.value,
            children: n.children.into_iter().map(AxNode::from).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsAxSnapshot {
    root: JsAxNode,
    url: String,
    truncated: bool,
    #[serde(default)]
    navigated_from: Option<String>,
}

impl From<JsAxSnapshot> for AxSnapshot {
    fn from(s: JsAxSnapshot) -> Self {
        AxSnapshot {
            root: s.root.into(),
            url: s.url,
            truncated: s.truncated,
            navigated_from: s.navigated_from,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsNavigateResult {
    session_id: String,
    final_url: String,
    snapshot: JsAxSnapshot,
}

/// Mirrors `TabAction` for the one-way Rust->JS crossing: serialized to JSON
/// (via `serde_json`, same convention as `WasmHttp::get`'s `headers_json`)
/// and parsed with `JSON.parse` on `browser.js`'s side, since wasm-bindgen's
/// `extern "C"` functions can't take a Rust enum directly.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum JsTabAction {
    List,
    New { url: Option<String> },
    Select { index: usize },
    Close { index: Option<usize> },
}

impl From<&TabAction> for JsTabAction {
    fn from(a: &TabAction) -> Self {
        match a {
            TabAction::List => JsTabAction::List,
            TabAction::New { url } => JsTabAction::New { url: url.clone() },
            TabAction::Select { index } => JsTabAction::Select { index: *index },
            TabAction::Close { index } => JsTabAction::Close { index: *index },
        }
    }
}

/// Mirrors `WaitCondition` for the same one-way JSON crossing as `JsTabAction`.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum JsWaitCondition {
    TextAppears { text: String },
    TextDisappears { text: String },
    TimeMs { ms: u64 },
}

impl From<&WaitCondition> for JsWaitCondition {
    fn from(c: &WaitCondition) -> Self {
        match c {
            WaitCondition::TextAppears(text) => JsWaitCondition::TextAppears { text: text.clone() },
            WaitCondition::TextDisappears(text) => {
                JsWaitCondition::TextDisappears { text: text.clone() }
            }
            WaitCondition::TimeMs(ms) => JsWaitCondition::TimeMs { ms: *ms },
        }
    }
}

/// Mirrors `browser.js`'s `buildTabsResult` plain-object shape.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsTabInfo {
    index: usize,
    url: String,
    title: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsTabsResult {
    tabs: Vec<JsTabInfo>,
    active_index: usize,
    #[serde(default)]
    snapshot: Option<JsAxSnapshot>,
}

/// Maps a JS-thrown error's message to a `PortError`, per Task 4.3.1: a
/// recognizable "no session"/"blocked" marker (both of which `browser.js`'s
/// `requireSession`/`checkBlocked`/ref-resolution failures always include)
/// becomes `PortError::NotFound`, so the tool layer's session/ref recovery
/// messaging applies uniformly regardless of which adapter is behind it. A
/// "crashed" marker (from `browser.js`'s `crashedMessage`/`evictIfCrashed`,
/// wired up to Playwright's `page.on('crash', ...)`) becomes
/// `PortError::SessionCrashed`, mirroring native's `Target.targetCrashed`
/// handling — checked before the broader "not found" markers since native's
/// analogous `crashed_message` text also happens to contain "session".
/// Anything else is `PortError::Other`.
fn map_js_error(message: String) -> PortError {
    let lower = message.to_ascii_lowercase();
    if lower.contains("crashed") {
        PortError::SessionCrashed(message)
    } else if lower.contains("timed out") {
        // `browser.js`'s `jsBrowserWaitFor` wraps a text-condition timeout
        // with this exact phrase (distinct from Playwright's own "Timeout
        // 5000ms exceeded" wording, which never contains "timed out") so it
        // maps here rather than falling through to `PortError::Other` — lets
        // `wait_for`'s timeout look the same as native's regardless of
        // adapter. Checked before the "not found" markers below since a
        // stale-ref click/type timeout is rewritten by
        // `describeActionError` into "... not found or no longer attached:
        // Timeout ...ms exceeded", which must still classify as `NotFound`,
        // not `Timeout` — that message never contains "timed out" either, so
        // there's no overlap between the two branches.
        PortError::Timeout
    } else if lower.contains("no session")
        || lower.contains("blocked")
        || lower.contains("not found")
    {
        PortError::NotFound(message)
    } else if lower.contains("type_secret refused") {
        // `browser.js`'s `jsBrowserTypeSecret` throws this exact marker
        // (`secretFieldShapeRefusal`, Task 4.3.2b) when its dispatch-time
        // type/autocomplete re-check refuses the write — mirrors native's
        // `secret_field_shape_refusal` variant choice
        // (`PortError::NotActionable`, `crates/native/src/browser.rs`).
        PortError::NotActionable(message)
    } else {
        PortError::Other(message)
    }
}

/// Mirrors `browser.js`'s `jsListSessions` plain-object shape.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsSessionSummary {
    session_id: String,
    tab_count: usize,
    idle_ms: u64,
    blocked: bool,
    crashed: bool,
}

fn js_reject_to_port_error(e: JsValue) -> PortError {
    map_js_error(js_err_to_string(&e))
}

impl BrowserDriver for WasmBrowser {
    async fn navigate_and_extract(
        &self,
        url: &str,
        timeout: Duration,
    ) -> Result<PageExtract, PortError> {
        let result = JsFuture::from(js_navigate_and_extract(url, timeout.as_millis() as f64))
            .await
            .map_err(|e| PortError::Other(js_err_to_string(&e)))?;

        let get = |k: &str| {
            js_sys::Reflect::get(&result, &JsValue::from_str(k))
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_default()
        };

        Ok(PageExtract {
            title: get("title"),
            html: get("html"),
            text: get("text"),
            final_url: get("finalUrl"),
        })
    }

    async fn navigate(
        &self,
        url: &str,
        session_id: Option<&SessionId>,
        timeout: Duration,
    ) -> Result<NavigateResult, PortError> {
        let session_id = session_id.map(|s| s.0.clone());
        let result = JsFuture::from(js_browser_navigate(
            url,
            session_id,
            timeout.as_millis() as f64,
        ))
        .await
        .map_err(js_reject_to_port_error)?;

        let parsed: JsNavigateResult =
            serde_wasm_bindgen::from_value(result).map_err(|e| PortError::Other(e.to_string()))?;

        Ok(NavigateResult {
            session_id: SessionId(parsed.session_id),
            final_url: parsed.final_url,
            snapshot: parsed.snapshot.into(),
        })
    }

    async fn click(
        &self,
        session_id: &SessionId,
        locator: &Locator,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError> {
        let result = JsFuture::from(js_browser_click(
            &session_id.0,
            &locator.0,
            timeout.as_millis() as f64,
        ))
        .await
        .map_err(js_reject_to_port_error)?;

        let parsed: JsAxSnapshot =
            serde_wasm_bindgen::from_value(result).map_err(|e| PortError::Other(e.to_string()))?;
        Ok(parsed.into())
    }

    async fn type_text(
        &self,
        session_id: &SessionId,
        locator: &Locator,
        text: &str,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError> {
        let result = JsFuture::from(js_browser_type(
            &session_id.0,
            &locator.0,
            text,
            timeout.as_millis() as f64,
        ))
        .await
        .map_err(js_reject_to_port_error)?;

        let parsed: JsAxSnapshot =
            serde_wasm_bindgen::from_value(result).map_err(|e| PortError::Other(e.to_string()))?;
        Ok(parsed.into())
    }

    /// Epic 4.3: resolves `credential_ref` internally (never accepted as a
    /// resolved value from a caller — see the trait doc comment) and types it
    /// into `locator`. Two `wasm_bindgen` calls, in order, so the domain
    /// check (Story 4.3.3) can gate resolution instead of following it: (1)
    /// `jsBrowserCurrentUrl` fetches the session's fresh live URL — no
    /// resolve has happened yet — and `check_credential_domain` rejects a
    /// mismatch immediately, before `self.credential_store` is ever touched;
    /// (2) only on a match, `self.credential_store.resolve(...)` runs, and
    /// `jsBrowserTypeSecret` performs the actual DOM write (its own
    /// dispatch-time type re-check and unconditional own-node redaction live
    /// entirely in `crates/wasm/src/glue/browser.js`). This is the wasm-side
    /// equivalent of native's domain-check -> resolve -> dispatch-time
    /// re-check -> write ordering, reached via an extra JS round trip because
    /// (unlike native) this side has no direct handle to the page object.
    async fn type_secret(
        &self,
        session_id: &SessionId,
        locator: &Locator,
        credential_ref: &CredentialRef,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError> {
        let live_url_value = JsFuture::from(js_browser_current_url(&session_id.0))
            .await
            .map_err(js_reject_to_port_error)?;
        let live_url = live_url_value.as_string().ok_or_else(|| {
            PortError::Other("jsBrowserCurrentUrl resolved with a non-string value".to_string())
        })?;
        check_credential_domain(&live_url, &credential_ref.domain)?;

        let store = resolve_via_injected_store(&self.credential_store)?;
        let secret = store.resolve(credential_ref).await?;

        let result = JsFuture::from(js_browser_type_secret(
            &session_id.0,
            &locator.0,
            secret.expose(),
            timeout.as_millis() as f64,
        ))
        .await
        .map_err(js_reject_to_port_error)?;

        let parsed: JsAxSnapshot =
            serde_wasm_bindgen::from_value(result).map_err(|e| PortError::Other(e.to_string()))?;
        Ok(parsed.into())
    }

    async fn snapshot(
        &self,
        session_id: &SessionId,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError> {
        let result = JsFuture::from(js_browser_snapshot(
            &session_id.0,
            timeout.as_millis() as f64,
        ))
        .await
        .map_err(js_reject_to_port_error)?;

        let parsed: JsAxSnapshot =
            serde_wasm_bindgen::from_value(result).map_err(|e| PortError::Other(e.to_string()))?;
        Ok(parsed.into())
    }

    async fn close_session(&self, session_id: &SessionId) -> Result<(), PortError> {
        JsFuture::from(js_close_session(&session_id.0))
            .await
            .map_err(js_reject_to_port_error)?;
        Ok(())
    }

    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, PortError> {
        let result = JsFuture::from(js_list_sessions())
            .await
            .map_err(js_reject_to_port_error)?;

        let parsed: Vec<JsSessionSummary> =
            serde_wasm_bindgen::from_value(result).map_err(|e| PortError::Other(e.to_string()))?;
        Ok(parsed
            .into_iter()
            .map(|s| SessionSummary {
                session_id: s.session_id,
                tab_count: s.tab_count,
                idle_ms: s.idle_ms,
                blocked: s.blocked,
                crashed: s.crashed,
            })
            .collect())
    }

    async fn tabs(
        &self,
        session_id: &SessionId,
        action: TabAction,
        timeout: Duration,
    ) -> Result<TabsResult, PortError> {
        let action_json = serde_json::to_string(&JsTabAction::from(&action))
            .map_err(|e| PortError::Other(e.to_string()))?;
        let result = JsFuture::from(js_browser_tabs(
            &session_id.0,
            &action_json,
            timeout.as_millis() as f64,
        ))
        .await
        .map_err(js_reject_to_port_error)?;

        let parsed: JsTabsResult =
            serde_wasm_bindgen::from_value(result).map_err(|e| PortError::Other(e.to_string()))?;
        Ok(TabsResult {
            tabs: parsed
                .tabs
                .into_iter()
                .map(|t| TabInfo {
                    index: t.index,
                    url: t.url,
                    title: t.title,
                })
                .collect(),
            active_index: parsed.active_index,
            snapshot: parsed.snapshot.map(AxSnapshot::from),
        })
    }

    async fn hover(
        &self,
        session_id: &SessionId,
        locator: &Locator,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError> {
        let result = JsFuture::from(js_browser_hover(
            &session_id.0,
            &locator.0,
            timeout.as_millis() as f64,
        ))
        .await
        .map_err(js_reject_to_port_error)?;

        let parsed: JsAxSnapshot =
            serde_wasm_bindgen::from_value(result).map_err(|e| PortError::Other(e.to_string()))?;
        Ok(parsed.into())
    }

    async fn select_option(
        &self,
        session_id: &SessionId,
        locator: &Locator,
        values: &[String],
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError> {
        let values_json =
            serde_json::to_string(values).map_err(|e| PortError::Other(e.to_string()))?;
        let result = JsFuture::from(js_browser_select_option(
            &session_id.0,
            &locator.0,
            &values_json,
            timeout.as_millis() as f64,
        ))
        .await
        .map_err(js_reject_to_port_error)?;

        let parsed: JsAxSnapshot =
            serde_wasm_bindgen::from_value(result).map_err(|e| PortError::Other(e.to_string()))?;
        Ok(parsed.into())
    }

    async fn press_key(
        &self,
        session_id: &SessionId,
        key: &str,
        locator: Option<&Locator>,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError> {
        let ref_id = locator.map(|l| l.0.clone());
        let result = JsFuture::from(js_browser_press_key(
            &session_id.0,
            key,
            ref_id,
            timeout.as_millis() as f64,
        ))
        .await
        .map_err(js_reject_to_port_error)?;

        let parsed: JsAxSnapshot =
            serde_wasm_bindgen::from_value(result).map_err(|e| PortError::Other(e.to_string()))?;
        Ok(parsed.into())
    }

    async fn wait_for(
        &self,
        session_id: &SessionId,
        condition: WaitCondition,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError> {
        let condition_json = serde_json::to_string(&JsWaitCondition::from(&condition))
            .map_err(|e| PortError::Other(e.to_string()))?;
        let result = JsFuture::from(js_browser_wait_for(
            &session_id.0,
            &condition_json,
            timeout.as_millis() as f64,
        ))
        .await
        .map_err(js_reject_to_port_error)?;

        let parsed: JsAxSnapshot =
            serde_wasm_bindgen::from_value(result).map_err(|e| PortError::Other(e.to_string()))?;
        Ok(parsed.into())
    }

    /// Unlike every other method here, the resolved value is read via
    /// `js_sys::Uint8Array` rather than `serde_wasm_bindgen` — `browser.js`'s
    /// `jsBrowserScreenshot` resolves with the raw `Buffer` Playwright's
    /// `page.screenshot()` returns, and a `Buffer` (a `Uint8Array` subclass)
    /// has no JSON shape for `serde_wasm_bindgen` to walk.
    async fn screenshot(
        &self,
        session_id: &SessionId,
        full_page: bool,
        timeout: Duration,
    ) -> Result<Vec<u8>, PortError> {
        let result = JsFuture::from(js_browser_screenshot(
            &session_id.0,
            full_page,
            timeout.as_millis() as f64,
        ))
        .await
        .map_err(js_reject_to_port_error)?;

        Ok(js_sys::Uint8Array::new(&result).to_vec())
    }

    async fn evaluate(
        &self,
        session_id: &SessionId,
        function: &str,
        locator: Option<&Locator>,
        timeout: Duration,
    ) -> Result<serde_json::Value, PortError> {
        let ref_id = locator.map(|l| l.0.clone());
        let result = JsFuture::from(js_browser_evaluate(
            &session_id.0,
            function,
            ref_id,
            timeout.as_millis() as f64,
        ))
        .await
        .map_err(js_reject_to_port_error)?;

        serde_wasm_bindgen::from_value(result).map_err(|e| PortError::Other(e.to_string()))
    }

    async fn history(
        &self,
        session_id: &SessionId,
        action: HistoryAction,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError> {
        let action_str = match action {
            HistoryAction::Back => "back",
            HistoryAction::Forward => "forward",
            HistoryAction::Reload => "reload",
        };
        let result = JsFuture::from(js_browser_history(
            &session_id.0,
            action_str,
            timeout.as_millis() as f64,
        ))
        .await
        .map_err(js_reject_to_port_error)?;

        let parsed: JsAxSnapshot =
            serde_wasm_bindgen::from_value(result).map_err(|e| PortError::Other(e.to_string()))?;
        Ok(parsed.into())
    }

    async fn resize(
        &self,
        session_id: &SessionId,
        width: u32,
        height: u32,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError> {
        let result = JsFuture::from(js_browser_resize(
            &session_id.0,
            width,
            height,
            timeout.as_millis() as f64,
        ))
        .await
        .map_err(js_reject_to_port_error)?;

        let parsed: JsAxSnapshot =
            serde_wasm_bindgen::from_value(result).map_err(|e| PortError::Other(e.to_string()))?;
        Ok(parsed.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Task 4.3.1 AC / REQ-4 wasm error mapping: a JS-thrown error carrying
    /// a "no session" marker (as `browser.js`'s `requireSession` always
    /// includes) must map to `PortError::NotFound`, not `PortError::Other`,
    /// so the tool layer's session-recovery messaging (name the id, point at
    /// `stapler_browser_navigate`) applies to the wasm adapter too.
    #[test]
    fn wasm_browser_navigate_should_map_no_session_marker_to_port_error_not_found() {
        let message =
            "no session 'sess-9' found; call stapler_browser_navigate to start one".to_string();

        let err = map_js_error(message.clone());

        match err {
            PortError::NotFound(m) => assert_eq!(m, message),
            other => panic!("expected PortError::NotFound, got {other:?}"),
        }
    }

    /// BLOCKER 1 fix: a JS-thrown error carrying a "crashed" marker (as
    /// `browser.js`'s `crashedMessage`/`evictIfCrashed` always includes once
    /// the `page.on('crash', ...)` listener fires) must map to
    /// `PortError::SessionCrashed`, not `PortError::NotFound` — the tool
    /// layer needs to tell "start a new session" apart from "this exact id
    /// is dead" (see `PortError::SessionCrashed`'s doc comment).
    #[test]
    fn wasm_browser_should_map_crashed_marker_to_port_error_session_crashed() {
        let message = "browser session \"sess-1\" crashed — call stapler_browser_navigate (without sessionId) to start a fresh session".to_string();

        let err = map_js_error(message.clone());

        match err {
            PortError::SessionCrashed(m) => assert_eq!(m, message),
            other => panic!("expected PortError::SessionCrashed, got {other:?}"),
        }
    }

    #[test]
    fn map_js_error_should_return_other_when_message_has_no_recognizable_marker() {
        let err = map_js_error("boom: something unrelated broke".to_string());

        match err {
            PortError::Other(_) => {}
            other => panic!("expected PortError::Other, got {other:?}"),
        }
    }

    /// Issue #12: `browser.js`'s `jsBrowserWaitFor` wraps a text-condition
    /// timeout with the exact phrase "timed out" (see the doc comment on the
    /// `"timed out"` branch above) so `wait_for`'s timeout classifies as
    /// `PortError::Timeout` like native's, not `PortError::Other`.
    #[test]
    fn map_js_error_should_map_wait_for_timed_out_marker_to_port_error_timeout() {
        let message =
            "wait_for timed out waiting for text to appear: \"Loaded\": Timeout 5000ms exceeded"
                .to_string();

        let err = map_js_error(message);

        match err {
            PortError::Timeout => {}
            other => panic!("expected PortError::Timeout, got {other:?}"),
        }
    }

    /// Companion to the test above: a stale-ref click/type timeout rewritten
    /// by `describeActionError` into "... not found or no longer attached:
    /// Timeout ...ms exceeded" must still classify as `PortError::NotFound`
    /// — it never contains the two-word phrase "timed out", so it must not
    /// be caught by the new branch.
    #[test]
    fn map_js_error_should_still_map_stale_ref_timeout_to_not_found() {
        let message =
            "element with ref 'e5' not found or no longer attached: Timeout 5000ms exceeded"
                .to_string();

        let err = map_js_error(message.clone());

        match err {
            PortError::NotFound(m) => assert_eq!(m, message),
            other => panic!("expected PortError::NotFound, got {other:?}"),
        }
    }

    /// Task 4.3.2b: `browser.js`'s `jsBrowserTypeSecret` throws its
    /// dispatch-time refusal with the `"type_secret refused"` marker (see
    /// `secretFieldShapeRefusal`) — this must map to `PortError::NotActionable`,
    /// matching native's variant choice for the identical situation.
    #[test]
    fn map_js_error_should_map_type_secret_refused_marker_to_not_actionable() {
        let message = "type_secret refused: ref \"e14\" resolves to a plain text field (role=textbox, no protected/password state), not a password or TOTP input — use stapler_browser_type for non-secret fields, or re-snapshot if this field should be a password field.".to_string();

        let err = map_js_error(message.clone());

        match err {
            PortError::NotActionable(m) => assert_eq!(m, message),
            other => panic!("expected PortError::NotActionable, got {other:?}"),
        }
    }

    // -- Story 3.4.3/4.3.3 parity: domain check against the live navigation URL --

    /// Required test (validation.md): the domain-mismatch gate rejects
    /// *before* `self.credential_store` is ever touched — `type_secret`
    /// (see its doc comment above) calls `check_credential_domain` on the
    /// freshly-fetched `jsBrowserCurrentUrl` result strictly before calling
    /// `resolve_via_injected_store`/`store.resolve`, so proving the pure gate
    /// itself rejects a mismatch is exactly what proves resolution is never
    /// attempted on this path — mirrors native's identically-shaped test for
    /// the same AC (`crates/native/src/browser.rs`).
    #[test]
    fn wasm_browser_type_secret_should_return_credential_domain_mismatch_when_live_url_mismatches_before_resolve_attempted(
    ) {
        let result = check_credential_domain("https://evil-example.com/", "example.com");

        match result {
            Err(PortError::CredentialDomainMismatch(msg)) => {
                assert!(msg.contains("evil-example.com"), "msg = {msg}");
                assert!(msg.contains("example.com"), "msg = {msg}");
            }
            other => panic!("expected PortError::CredentialDomainMismatch, got {other:?}"),
        }
    }

    #[test]
    fn check_credential_domain_should_succeed_when_live_host_matches_requested_domain() {
        assert!(check_credential_domain("https://example.com/login", "example.com").is_ok());
    }

    // -- Story 4.3.0: store injection, fail-closed with no store --

    /// Hand-rolled `CredentialStore` test double, mirroring native's
    /// `FakeCredentialStore` (`crates/native/src/browser.rs`).
    struct FakeCredentialStore {
        calls: RefCell<Vec<CredentialRef>>,
        response: Result<String, PortError>,
    }

    impl FakeCredentialStore {
        fn with_response(response: Result<String, PortError>) -> Self {
            FakeCredentialStore {
                calls: RefCell::new(Vec::new()),
                response,
            }
        }
    }

    impl CredentialStore for FakeCredentialStore {
        async fn resolve(&self, credential_ref: &CredentialRef) -> Result<SecretValue, PortError> {
            self.calls.borrow_mut().push(credential_ref.clone());
            match &self.response {
                Ok(value) => Ok(SecretValue::new(value.clone())),
                Err(_) => Err(PortError::Other(
                    "FakeCredentialStore configured error".to_string(),
                )),
            }
        }
    }

    fn password_ref(domain: &str) -> CredentialRef {
        CredentialRef {
            domain: domain.to_string(),
            field: stapler_mcp_core::ports::CredentialField::Password,
        }
    }

    #[tokio::test]
    async fn wasm_browser_type_secret_should_reach_injected_store_when_credential_store_set() {
        let browser = WasmBrowser::new();
        let fake = Rc::new(FakeCredentialStore::with_response(
            Ok("hunter2".to_string()),
        ));
        browser.set_credential_store(Rc::clone(&fake));

        let store =
            resolve_via_injected_store(&browser.credential_store).expect("store was just injected");
        let secret = store
            .resolve(&password_ref("example.com"))
            .await
            .expect("fake store is configured to succeed");

        assert_eq!(secret.expose(), "hunter2");
        assert_eq!(fake.calls.borrow().len(), 1);
        assert_eq!(fake.calls.borrow()[0], password_ref("example.com"));
    }

    #[test]
    fn wasm_browser_type_secret_should_return_error_not_panic_when_no_credential_store_injected() {
        // AC (Task 4.3.0, mirroring native's Task 3.4.0b): a freshly
        // `WasmBrowser::new()`-constructed browser with no
        // `set_credential_store` call made returns
        // `Err(PortError::CredentialUnauthenticated(...))` rather than
        // panicking.
        let browser = WasmBrowser::new();

        let result = resolve_via_injected_store(&browser.credential_store);

        match result {
            Err(PortError::CredentialUnauthenticated(_)) => {}
            Err(other) => panic!("expected PortError::CredentialUnauthenticated, got {other:?}"),
            Ok(_) => panic!("expected an error with no store injected, got Ok"),
        }
    }
}

/// Task 4.3.1's AC-mandated `wasm-pack test` harness: exercises
/// `WasmBrowser`'s bindings against the *real* `crates/wasm/src/glue/browser.js`
/// (not a mock), the part of the stack `npm/test/browser_glue.test.js`
/// deliberately doesn't cover — that suite drives the JS glue directly with a
/// hand-built mock `page`, so it never touches the `#[wasm_bindgen] extern
/// "C"` bindings, `JsFuture` awaiting, or `serde_wasm_bindgen::from_value`
/// deserialization declared in this file. Everything below runs the
/// Rust-to-JS boundary end to end instead.
///
/// Left at `wasm-bindgen-test`'s default execution target, node.js — no
/// `run_in_browser` opt-in — because `browser.js` launches a real Chromium
/// via `playwright-core`'s `chromium.launch(...)`, which needs Node's
/// `require("playwright-core")` and a child-process launcher, neither of
/// which exist in a browser's wasm sandbox. `wasm-pack test --headless
/// --chrome` would run the *test* in a headless Chrome tab that then tries
/// (from inside that tab) to launch a second, separate Chromium via
/// Playwright — not meaningfully different from the node.js default, but far
/// heavier and without CommonJS `require`. Node is therefore the only mode
/// that matches what the compiled `.wasm` actually does at daemon runtime
/// (the daemon itself is a Node process launching Playwright's Chromium).
///
/// Requires: a system Chrome (`channel: "chrome"`, per `browser.js`) and
/// `playwright-core` resolvable by Node's `require()`. `wasm-bindgen-test`
/// copies `browser.js` into a scratch temp directory before running it, so
/// normal upward `node_modules` resolution from `crates/wasm` (which has no
/// `package.json`/`node_modules` of its own) never finds the copy already
/// installed at `npm/node_modules/playwright-core` — Node's CJS loader also
/// consults the `NODE_PATH` env var, though, so point it there:
///   `NODE_PATH="$(pwd)/npm/node_modules" wasm-pack test --node crates/wasm`
/// (run from the repo root). Launches a real headless browser process, so
/// it is slower and more environment-sensitive than the rest of the suite —
/// expected for the one test this AC exists to add.
#[cfg(test)]
mod wasm_pack_tests {
    use super::*;
    use std::time::Duration;
    use wasm_bindgen_test::*;

    // No `wasm_bindgen_test_configure!` call: node.js is `wasm-bindgen-test`'s
    // default execution target (see the doc comment above this module for
    // why that default, not `run_in_browser`, is the right one here) — only
    // `run_in_browser`/`run_in_*_worker` need an explicit opt-in.

    /// Task 4.3.1 AC: `WasmBrowser::navigate("https://example.com", None,
    /// timeout)` against the real glue returns `Ok(NavigateResult { .. })`
    /// with a non-empty `session_id`.
    #[wasm_bindgen_test]
    async fn wasm_browser_navigate_returns_session_and_final_url_against_real_glue() {
        let browser = WasmBrowser::new();

        let result = browser
            .navigate("https://example.com", None, Duration::from_secs(30))
            .await
            .expect("navigate should succeed against a real, reachable page");

        assert!(
            !result.session_id.0.is_empty(),
            "navigate must return a non-empty session_id"
        );
        assert!(
            result.final_url.contains("example.com"),
            "final_url should reflect the navigated page, got {}",
            result.final_url
        );

        browser.close().await;
    }

    /// Exercises the second leg of the Rust<->JS boundary this AC is about:
    /// a snapshot taken against a live session round-trips through
    /// `serde_wasm_bindgen::from_value` into a populated `AxSnapshot` with at
    /// least a root node — not just `navigate`'s `NavigateResult` path.
    #[wasm_bindgen_test]
    async fn wasm_browser_snapshot_round_trips_after_navigate_against_real_glue() {
        let browser = WasmBrowser::new();

        let nav = browser
            .navigate("https://example.com", None, Duration::from_secs(30))
            .await
            .expect("navigate should succeed against a real, reachable page");

        let snapshot = browser
            .snapshot(&nav.session_id, Duration::from_secs(30))
            .await
            .expect("snapshot should succeed for a session just created by navigate");

        assert!(
            snapshot.url.contains("example.com"),
            "snapshot url should reflect the session's current page, got {}",
            snapshot.url
        );

        browser.close().await;
    }
}
