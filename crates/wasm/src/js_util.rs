use wasm_bindgen::JsValue;

/// Best-effort extraction of a human-readable message from a JS rejection
/// value (an `Error` object's `.message`, or the value itself if it's already
/// a string).
pub fn js_err_to_string(v: &JsValue) -> String {
    if let Some(s) = v.as_string() {
        return s;
    }
    js_sys::Reflect::get(v, &JsValue::from_str("message"))
        .ok()
        .and_then(|m| m.as_string())
        .unwrap_or_else(|| "unknown JS error".to_string())
}
