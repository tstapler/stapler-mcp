//! Native adapter: implements every `stapler-mcp-core::ports` trait for real
//! OS primitives (tokio Unix sockets, `fs4` flock, detached process spawn).

mod browser;
mod embed;
mod env;
mod fs;
mod http;
mod lock;
mod sleep;
mod socket;
mod spawn;

pub use browser::NativeBrowser;
pub use embed::NativeEmbedder;
pub use env::NativeEnv;
pub use fs::NativeFs;
pub use http::NativeHttp;
pub use lock::{NativeLock, NativeLockGuard};
pub use sleep::{NativeClock, NativeSleeper};
pub use socket::{NativeConn, NativeListener, NativeSocketFactory};
pub use spawn::NativeSpawner;
