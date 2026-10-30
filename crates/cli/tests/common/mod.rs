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

impl Drop for Harness {
    /// Graceful `shutdown` first, SIGKILL as the fallback, so a failed test
    /// does not leave a daemon (and its Chrome) running.
    fn drop(&mut self) {
        let pid = std::fs::read_to_string(&self.lock_path)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok());
        let sock = self.sock_path.clone();
        let _ = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(client::call(
                    &NativeSocketFactory,
                    &sock,
                    "shutdown",
                    None,
                    Duration::from_secs(5),
                ))
        })
        .join();
        if let Some(pid) = pid {
            std::thread::sleep(Duration::from_millis(500));
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}
