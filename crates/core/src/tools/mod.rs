#[cfg(not(target_arch = "wasm32"))]
pub mod docs;
pub mod fetch;
pub mod search;
pub mod webcrawl;
