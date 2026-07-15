use std::time::Duration;

use stapler_mcp_core::ports::{ClockPort, SleepPort};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

#[wasm_bindgen(module = "/src/glue/clock.js")]
extern "C" {
    #[wasm_bindgen(js_name = jsNowMillis)]
    fn js_now_millis() -> f64;
    #[wasm_bindgen(js_name = jsSleep)]
    fn js_sleep(ms: f64) -> js_sys::Promise;
}

pub struct WasmClock;

impl ClockPort for WasmClock {
    fn now_millis(&self) -> u64 {
        js_now_millis() as u64
    }
}

pub struct WasmSleeper;

impl SleepPort for WasmSleeper {
    async fn sleep(&self, dur: Duration) {
        let _ = JsFuture::from(js_sleep(dur.as_millis() as f64)).await;
    }
}
