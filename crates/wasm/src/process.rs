use stapler_mcp_core::ports::{PortError, ProcessSpawner};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/src/glue/process.js")]
extern "C" {
    #[wasm_bindgen(js_name = jsSpawnDaemon)]
    fn js_spawn_daemon(exe_hint: Option<String>, log_path: &str);
}

pub struct WasmSpawner;

impl ProcessSpawner for WasmSpawner {
    async fn spawn_daemon(&self, exe_hint: Option<&str>, log_path: &str) -> Result<(), PortError> {
        js_spawn_daemon(exe_hint.map(str::to_string), log_path);
        Ok(())
    }
}
