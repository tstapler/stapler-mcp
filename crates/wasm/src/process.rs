use stapler_mcp_core::ports::{PortError, ProcessOutput, ProcessSpawner};
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

    /// Unused on wasm: `crates/wasm/src/glue/vault.js`'s `CredentialStore`
    /// adapter talks to `@1password/sdk` directly rather than shelling out to
    /// the `op` CLI, so nothing on this target ever calls this method — it
    /// exists solely to satisfy `ProcessSpawner`'s trait surface (added for
    /// native's `op` invocation, `crates/native/src/spawn.rs`).
    async fn spawn_and_capture(&self, _argv: &[&str]) -> Result<ProcessOutput, PortError> {
        Err(PortError::Other(
            "spawn_and_capture is not supported on the wasm target".into(),
        ))
    }
}
