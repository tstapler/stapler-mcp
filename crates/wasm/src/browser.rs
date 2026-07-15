use std::time::Duration;

use stapler_mcp_core::ports::{BrowserDriver, PageExtract, PortError};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::js_util::js_err_to_string;

#[wasm_bindgen(module = "/src/glue/browser.js")]
extern "C" {
    #[wasm_bindgen(js_name = jsNavigateAndExtract)]
    fn js_navigate_and_extract(url: &str, timeout_ms: f64) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsCloseBrowser)]
    fn js_close_browser() -> js_sys::Promise;
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

impl BrowserDriver for WasmBrowser {
    async fn navigate_and_extract(&self, url: &str, timeout: Duration) -> Result<PageExtract, PortError> {
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
}
