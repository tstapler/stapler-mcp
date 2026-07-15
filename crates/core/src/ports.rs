//! OS-touching behavior lives entirely behind these traits so `stapler-mcp-core`
//! itself never calls `std::net`/`fs`/`process`/`env`/`time::Instant` directly.
//! A native adapter (tokio + fs4 + reqwest + chromiumoxide) and a wasm-bindgen
//! adapter (delegating to a Node.js host) each implement the same surface.

use std::time::Duration;

#[derive(Debug)]
pub enum PortError {
    Io(String),
    Timeout,
    Other(String),
}

impl std::fmt::Display for PortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PortError::Io(e) => write!(f, "io error: {e}"),
            PortError::Timeout => write!(f, "timed out"),
            PortError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PortError {}

#[derive(Debug)]
pub enum LockError {
    /// Another instance already holds the exclusive lock. Not an error condition
    /// for the caller — it's the losing side of the flock race, expected to exit cleanly.
    AlreadyRunning,
    Other(String),
}

pub trait Conn {
    /// Reads up to (and stripping) the next newline. `Ok(None)` means clean EOF
    /// before any data arrived.
    async fn read_frame(&mut self) -> Result<Option<Vec<u8>>, PortError>;
    /// Writes `bytes` followed by a single newline.
    async fn write_frame(&mut self, bytes: &[u8]) -> Result<(), PortError>;
    fn set_timeout(&mut self, dur: Duration);
}

pub trait Listener {
    type C: Conn;
    async fn accept(&mut self) -> Result<Self::C, PortError>;
}

pub trait SocketFactory {
    type L: Listener<C = Self::C>;
    type C: Conn;
    async fn bind(&self, path: &str) -> Result<Self::L, PortError>;
    async fn connect(&self, path: &str, timeout: Duration) -> Result<Self::C, PortError>;
    /// Best-effort removal of a socket file left behind by a crashed daemon.
    /// Only ever called after the caller has won the exclusive lock, which is
    /// what proves no other daemon owns that socket.
    async fn remove_stale(&self, path: &str) -> Result<(), PortError>;
}

pub trait LockGuard {
    /// Best-effort write of the current PID into the lockfile, purely for
    /// operator debugging of a stuck daemon.
    fn write_pid(&mut self, pid: u32);
}

pub trait ProcessLock {
    type Guard: LockGuard;
    /// Held for the daemon's entire lifetime — dropping the guard releases the lock.
    async fn acquire_exclusive(&self, path: &str) -> Result<Self::Guard, LockError>;
}

pub trait ProcessSpawner {
    /// Spawns a detached `--daemon` process, redirecting its stdout/stderr to
    /// `log_path` (it has no controlling terminal once detached). Does not wait
    /// for the child; the daemon must outlive the spawning process.
    async fn spawn_daemon(&self, exe_hint: Option<&str>, log_path: &str) -> Result<(), PortError>;
}

pub trait EnvPort {
    fn var(&self, key: &str) -> Option<String>;
    fn home_dir(&self) -> Option<String>;
}

pub trait ClockPort {
    fn now_millis(&self) -> u64;
}

pub trait SleepPort {
    async fn sleep(&self, dur: Duration);
}

pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    /// The URL actually served, after following any redirects.
    pub final_url: String,
}

pub trait HttpClient {
    async fn get(&self, url: &str, headers: &[(String, String)]) -> Result<HttpResponse, PortError>;
}

pub struct PageExtract {
    pub title: String,
    pub html: String,
    pub text: String,
    pub final_url: String,
}

pub trait BrowserDriver {
    /// Coarse, call-level operation (navigate + read title/HTML/text/final-URL
    /// in one hop) rather than exposing CDP-message-level primitives — this is
    /// what keeps the wasm↔JS boundary to one crossing per tool call once a
    /// wasm-bindgen adapter exists.
    async fn navigate_and_extract(&self, url: &str, timeout: Duration) -> Result<PageExtract, PortError>;
}

pub trait FileStore {
    /// Creates parent directories as needed.
    async fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), PortError>;
    /// `Ok(None)` means the file doesn't exist (a cache miss), not an error.
    async fn read_file(&self, path: &str) -> Result<Option<Vec<u8>>, PortError>;
    /// Idempotent — deleting a path that doesn't exist is `Ok(())`, not an error.
    async fn delete_file(&self, path: &str) -> Result<(), PortError>;
}

/// Runs local embedding inference (loading an ONNX model, tokenizing, tensor
/// math). Intentionally native-only for v1: no wasm implementation exists,
/// because `fastembed`'s `ort` dependency has no path to
/// `wasm32-unknown-unknown` (maintainer-abandoned wasm support) — see
/// docs-index's ADR-0002. `tools::docs`, the only caller, is compiled out of
/// the wasm32 target entirely, so this asymmetry with every other port trait
/// (which all have at least a partial wasm adapter) is deliberate, not a gap.
pub trait Embedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError>;
}
