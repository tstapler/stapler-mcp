use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use stapler_mcp_core::ports::{PortError, ProcessSpawner};

pub struct NativeSpawner;

impl ProcessSpawner for NativeSpawner {
    async fn spawn_daemon(&self, exe_hint: Option<&str>, log_path: &str) -> Result<(), PortError> {
        let exe = match exe_hint {
            Some(e) => e.to_string(),
            None => std::env::current_exe()
                .map_err(|e| PortError::Io(e.to_string()))?
                .to_string_lossy()
                .to_string(),
        };

        if let Some(parent) = std::path::Path::new(log_path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let log_out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .map_err(|e| PortError::Io(e.to_string()))?;
        let log_err = log_out
            .try_clone()
            .map_err(|e| PortError::Io(e.to_string()))?;

        // `Command` inherits the parent's environment by default — this is how
        // `STAPLER_MCP_HOME` propagates to the spawned daemon.
        let mut cmd = Command::new(exe);
        cmd.arg("--daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_out))
            .stderr(Stdio::from(log_err));

        // Detach into its own session so the daemon survives the parent's
        // process-group signals (e.g. a closed terminal/tmux pane), matching
        // the Go implementation's Setsid: true.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }

        // Deliberately not `.wait()`-ed: the daemon must outlive this process.
        // Dropping the returned `Child` does not kill it (unlike some async
        // process libraries) — it just releases our handle, exactly like Go's
        // `cmd.Process.Release()`.
        cmd.spawn().map_err(|e| PortError::Io(e.to_string()))?;
        Ok(())
    }
}
