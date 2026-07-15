use std::time::Duration;

use stapler_mcp_core::ports::{Conn, Listener, PortError, SocketFactory};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::js_util::js_err_to_string;

#[wasm_bindgen(module = "/src/glue/socket.js")]
extern "C" {
    #[wasm_bindgen(js_name = jsRemoveStale)]
    fn js_remove_stale(path: &str);
    #[wasm_bindgen(js_name = jsBind)]
    fn js_bind(path: &str) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsAccept)]
    fn js_accept(listener_id: u32) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsCloseListener)]
    fn js_close_listener(listener_id: u32);
    #[wasm_bindgen(js_name = jsConnect)]
    fn js_connect(path: &str, timeout_ms: f64) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsReadLine)]
    fn js_read_line(conn_id: u32, timeout_ms: f64) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsWriteLine)]
    fn js_write_line(conn_id: u32, line: &str) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsCloseConn)]
    fn js_close_conn(conn_id: u32);
}

fn err_to_port_error(v: JsValue) -> PortError {
    let msg = js_err_to_string(&v);
    if msg == "timeout" || msg == "connect timeout" {
        PortError::Timeout
    } else {
        PortError::Io(msg)
    }
}

async fn await_id(promise: js_sys::Promise) -> Result<u32, PortError> {
    let v = JsFuture::from(promise).await.map_err(err_to_port_error)?;
    Ok(v.as_f64().unwrap_or(0.0) as u32)
}

pub struct WasmConn {
    id: u32,
    timeout_ms: f64,
}

impl Drop for WasmConn {
    fn drop(&mut self) {
        js_close_conn(self.id);
    }
}

impl Conn for WasmConn {
    async fn read_frame(&mut self) -> Result<Option<Vec<u8>>, PortError> {
        let v = JsFuture::from(js_read_line(self.id, self.timeout_ms))
            .await
            .map_err(err_to_port_error)?;
        if v.is_null() || v.is_undefined() {
            Ok(None)
        } else {
            Ok(v.as_string().map(String::into_bytes))
        }
    }

    async fn write_frame(&mut self, bytes: &[u8]) -> Result<(), PortError> {
        let line = String::from_utf8_lossy(bytes).into_owned();
        JsFuture::from(js_write_line(self.id, &line))
            .await
            .map_err(err_to_port_error)?;
        Ok(())
    }

    fn set_timeout(&mut self, dur: Duration) {
        self.timeout_ms = dur.as_millis() as f64;
    }
}

pub struct WasmListener {
    id: u32,
}

impl Drop for WasmListener {
    fn drop(&mut self) {
        js_close_listener(self.id);
    }
}

impl Listener for WasmListener {
    type C = WasmConn;

    async fn accept(&mut self) -> Result<Self::C, PortError> {
        let id = await_id(js_accept(self.id)).await?;
        Ok(WasmConn {
            id,
            timeout_ms: 120_000.0,
        })
    }
}

pub struct WasmSocketFactory;

impl SocketFactory for WasmSocketFactory {
    type L = WasmListener;
    type C = WasmConn;

    async fn bind(&self, path: &str) -> Result<Self::L, PortError> {
        let id = await_id(js_bind(path)).await?;
        Ok(WasmListener { id })
    }

    async fn connect(&self, path: &str, timeout: Duration) -> Result<Self::C, PortError> {
        let timeout_ms = timeout.as_millis() as f64;
        let id = await_id(js_connect(path, timeout_ms)).await?;
        Ok(WasmConn { id, timeout_ms })
    }

    async fn remove_stale(&self, path: &str) -> Result<(), PortError> {
        js_remove_stale(path);
        Ok(())
    }
}
