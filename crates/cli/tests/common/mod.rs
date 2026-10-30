//! Shared harness for the daemon-recovery e2e tests. Each test file is its own
//! process, so setting `STAPLER_MCP_HOME` here cannot race other files.
#![allow(dead_code)]

use std::time::Duration;

use stapler_mcp_core::client::{self, EnsureOptions};
use stapler_mcp_core::paths;
use stapler_mcp_core::ports::EnvPort;
use stapler_mcp_native::{NativeClock, NativeSleeper, NativeSocketFactory, NativeSpawner};

pub struct TestEnv {
    pub home: String,
}

impl EnvPort for TestEnv {
    fn var(&self, key: &str) -> Option<String> {
        (key == "STAPLER_MCP_HOME").then(|| self.home.clone())
    }
    fn home_dir(&self) -> Option<String> {
        Some(self.home.clone())
    }
}

pub struct Harness {
    pub env: TestEnv,
    pub sock_path: String,
    pub lock_path: String,
    pub log_path: String,
    _tmp: tempfile::TempDir,
}

impl Harness {
    /// Isolated state dir; must run before any daemon is spawned, because the
    /// spawned daemon snapshots the real process environment.
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().to_string_lossy().to_string();
        std::env::set_var("STAPLER_MCP_HOME", &home);
        let env = TestEnv { home };
        std::fs::create_dir_all(paths::base_dir(&env)).unwrap();
        Self {
            sock_path: paths::socket_path(&env),
            lock_path: paths::lock_path(&env),
            log_path: paths::log_path(&env),
            env,
            _tmp: tmp,
        }
    }

    pub async fn ensure(&self, budget: Duration) -> Result<(), stapler_mcp_core::ports::PortError> {
        ensure(&self.sock_path, &self.log_path, budget).await
    }

    pub async fn ping(&self, timeout: Duration) -> Result<(), stapler_mcp_core::ports::PortError> {
        client::ping(&NativeSocketFactory, &self.sock_path, timeout).await
    }

    pub fn daemon_pid(&self) -> i32 {
        std::fs::read_to_string(&self.lock_path)
            .expect("lock file")
            .trim()
            .parse()
            .expect("pid in lock file")
    }
}

pub async fn ensure(
    sock_path: &str,
    log_path: &str,
    budget: Duration,
) -> Result<(), stapler_mcp_core::ports::PortError> {
    client::ensure_daemon(
        &NativeSocketFactory,
        &NativeSpawner,
        &NativeSleeper,
        &NativeClock,
        sock_path,
        log_path,
        EnsureOptions {
            startup_timeout: Some(budget),
            exe_hint: Some(env!("CARGO_BIN_EXE_stapler-mcp-daemon").to_string()),
        },
    )
    .await
}

/// Sends `sig` to `pid`; refuses pids <= 1 so a corrupt lock file can never
/// signal a process group or every process.
pub fn signal(pid: i32, sig: i32) -> bool {
    if pid <= 1 {
        return false;
    }
    // SAFETY: kill(2) takes plain integers and touches no Rust memory; `pid` is
    // our own daemon's, read from its lock file and guarded above.
    unsafe { libc::kill(pid, sig) == 0 }
}

/// Polls until `pid` is gone (or a zombie) or `within` elapses.
pub fn wait_for_exit(pid: i32, within: Duration) -> bool {
    let deadline = std::time::Instant::now() + within;
    while std::time::Instant::now() < deadline {
        let state = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok();
        // Field 3 of /proc/<pid>/stat is the state; 'Z' = exited, awaiting reap.
        let alive = state
            .as_deref()
            .and_then(|s| s.rsplit(')').next())
            .and_then(|rest| rest.trim_start().chars().next())
            .is_some_and(|c| c != 'Z');
        if !alive {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

impl Drop for Harness {
    /// Graceful `shutdown` first, SIGKILL if the daemon is still alive after,
    /// so a failed test does not leave a daemon (and its Chrome) running.
    fn drop(&mut self) {
        let pid = std::fs::read_to_string(&self.lock_path)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok());
        let sock = self.sock_path.clone();
        // A separate thread with its own runtime: `block_on` directly inside a
        // `#[tokio::test]` runtime would panic.
        let shutdown = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            rt.block_on(client::call(
                &NativeSocketFactory,
                &sock,
                "shutdown",
                None,
                Duration::from_secs(5),
            ))
            .ok()
        })
        .join();
        if !matches!(shutdown, Ok(Some(_))) {
            eprintln!("harness: graceful daemon shutdown failed");
        }
        if let Some(pid) = pid {
            if !wait_for_exit(pid, Duration::from_secs(5)) {
                signal(pid, libc::SIGKILL);
            }
        }
    }
}
