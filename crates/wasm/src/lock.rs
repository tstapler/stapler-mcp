use stapler_mcp_core::ports::{LockError, LockGuard, ProcessLock};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/src/glue/lock.js")]
extern "C" {
    #[wasm_bindgen(js_name = jsAcquireLock)]
    fn js_acquire_lock(path: &str) -> bool;
    #[wasm_bindgen(js_name = jsReleaseLock)]
    fn js_release_lock(path: &str);
}

pub struct WasmLock;

pub struct WasmLockGuard {
    path: String,
}

impl LockGuard for WasmLockGuard {
    fn write_pid(&mut self, _pid: u32) {
        // no-op: `jsAcquireLock` already wrote `process.pid` — the real Node
        // daemon process's own pid — into the lock dir at acquire time.
    }
}

impl Drop for WasmLockGuard {
    fn drop(&mut self) {
        js_release_lock(&self.path);
    }
}

impl ProcessLock for WasmLock {
    type Guard = WasmLockGuard;

    async fn acquire_exclusive(&self, path: &str) -> Result<Self::Guard, LockError> {
        if js_acquire_lock(path) {
            Ok(WasmLockGuard { path: path.to_string() })
        } else {
            Err(LockError::AlreadyRunning)
        }
    }
}
