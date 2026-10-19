//! Native adapter: implements every `stapler-mcp-core::ports` trait for real
//! OS primitives (tokio Unix sockets, `fs4` flock, detached process spawn).

#[cfg(feature = "heavy")]
mod ax;
#[cfg(feature = "heavy")]
mod browser;
#[cfg(feature = "heavy")]
mod embed;
mod env;
mod fs;
#[cfg(feature = "heavy")]
mod http;
pub mod http_token;
mod lock;
#[cfg(feature = "heavy")]
mod network;
mod sleep;
mod socket;
mod spawn;
#[cfg(feature = "heavy")]
mod vault;

#[cfg(feature = "heavy")]
pub use browser::NativeBrowser;
#[cfg(feature = "heavy")]
pub use embed::NativeEmbedder;
pub use env::NativeEnv;
pub use fs::NativeFs;
#[cfg(feature = "heavy")]
pub use http::NativeHttp;
pub use lock::{NativeLock, NativeLockGuard};
pub use sleep::{NativeClock, NativeSleeper};
pub use socket::{NativeConn, NativeListener, NativeSocketFactory};
pub use spawn::NativeSpawner;
#[cfg(feature = "heavy")]
pub use vault::NativeCredentialStore;
