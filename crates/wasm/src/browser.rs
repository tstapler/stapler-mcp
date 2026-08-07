use std::time::Duration;

use serde::Deserialize;
use stapler_mcp_core::ports::{
    AxNode, AxSnapshot, BrowserDriver, Locator, NavigateResult, PageExtract, PortError, SessionId,
};
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
    fn js_browser_navigate(url: &str, session_id: Option<String>, timeout_ms: f64) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsBrowserClick)]
    fn js_browser_click(session_id: &str, ref_id: &str, timeout_ms: f64) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsBrowserType)]
    fn js_browser_type(session_id: &str, ref_id: &str, text: &str, timeout_ms: f64) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsBrowserSnapshot)]
    fn js_browser_snapshot(session_id: &str, timeout_ms: f64) -> js_sys::Promise;
}

pub struct WasmBrowser;

impl WasmBrowser {
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
    } else if lower.contains("no session") || lower.contains("blocked") || lower.contains("not found") {
        PortError::NotFound(message)
    } else {
        PortError::Other(message)
    }
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

    async fn snapshot(
        &self,
        session_id: &SessionId,
        timeout: Duration,
    ) -> Result<AxSnapshot, PortError> {
        let result = JsFuture::from(js_browser_snapshot(&session_id.0, timeout.as_millis() as f64))
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
        let message = "no session 'sess-9' found; call stapler_browser_navigate to start one".to_string();

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
        let browser = WasmBrowser;

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
        let browser = WasmBrowser;

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
