use std::collections::HashMap;

use stapler_mcp_core::ports::{HttpClient, HttpResponse, PortError};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::js_util::js_err_to_string;

#[wasm_bindgen(module = "/src/glue/http.js")]
extern "C" {
    #[wasm_bindgen(js_name = jsHttpGet)]
    fn js_http_get(url: &str, headers_json: &str) -> js_sys::Promise;
}

pub struct WasmHttp;

impl HttpClient for WasmHttp {
    async fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<HttpResponse, PortError> {
        let headers_map: HashMap<&str, &str> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let headers_json =
            serde_json::to_string(&headers_map).map_err(|e| PortError::Other(e.to_string()))?;

        let result = JsFuture::from(js_http_get(url, &headers_json))
            .await
            .map_err(|e| PortError::Io(js_err_to_string(&e)))?;

        let status = js_sys::Reflect::get(&result, &JsValue::from_str("status"))
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as u16;
        let body_val = js_sys::Reflect::get(&result, &JsValue::from_str("body"))
            .map_err(|_| PortError::Other("missing body in HTTP response".to_string()))?;
        let body = js_sys::Uint8Array::new(&body_val).to_vec();

        // TODO(docs-index-wasm): capture the real post-redirect URL via response.url
        // on the JS side if/when a wasm Embedder exists.
        Ok(HttpResponse {
            status,
            body,
            final_url: url.to_string(),
        })
    }
}
