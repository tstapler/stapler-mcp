use stapler_mcp_core::ports::{FileStore, PortError};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::js_util::js_err_to_string;

#[wasm_bindgen(module = "/src/glue/fs.js")]
extern "C" {
    #[wasm_bindgen(js_name = jsEnsureDir)]
    pub fn js_ensure_dir(path: &str);
    #[wasm_bindgen(js_name = jsWriteFile)]
    fn js_write_file(path: &str, bytes: &[u8]) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsReadFile)]
    fn js_read_file(path: &str) -> js_sys::Promise;
    #[wasm_bindgen(js_name = jsDeleteFile)]
    fn js_delete_file(path: &str) -> js_sys::Promise;
}

pub struct WasmFs;

impl FileStore for WasmFs {
    async fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), PortError> {
        JsFuture::from(js_write_file(path, bytes))
            .await
            .map_err(|e| PortError::Io(js_err_to_string(&e)))?;
        Ok(())
    }

    async fn read_file(&self, path: &str) -> Result<Option<Vec<u8>>, PortError> {
        let v = JsFuture::from(js_read_file(path))
            .await
            .map_err(|e| PortError::Io(js_err_to_string(&e)))?;
        if v.is_null() {
            Ok(None)
        } else {
            Ok(Some(js_sys::Uint8Array::new(&v).to_vec()))
        }
    }

    async fn delete_file(&self, path: &str) -> Result<(), PortError> {
        JsFuture::from(js_delete_file(path))
            .await
            .map_err(|e| PortError::Io(js_err_to_string(&e)))?;
        Ok(())
    }
}
