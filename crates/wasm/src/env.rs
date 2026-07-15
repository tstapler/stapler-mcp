use stapler_mcp_core::ports::EnvPort;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/src/glue/env.js")]
extern "C" {
    #[wasm_bindgen(js_name = jsGetEnv)]
    fn js_get_env(key: &str) -> Option<String>;
    #[wasm_bindgen(js_name = jsHomeDir)]
    fn js_home_dir() -> Option<String>;
}

pub struct WasmEnv;

impl EnvPort for WasmEnv {
    fn var(&self, key: &str) -> Option<String> {
        js_get_env(key)
    }

    fn home_dir(&self) -> Option<String> {
        js_home_dir()
    }
}
