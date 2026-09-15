use std::fs::File;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use stapler_mcp_core::ports::{PortError, ProcessSpawner};

pub struct NativeSpawner;

/// Opens (creating if needed) the daemon's stdout/stderr redirect target,
/// returning two independent handles to the same file. Split out from
/// `spawn_daemon` so the mode-0600 behavior can be unit-tested without
/// actually spawning a process.
fn open_log_file(log_path: &str) -> Result<(File, File), PortError> {
    if let Some(parent) = std::path::Path::new(log_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // `daemon.log` may carry request/response detail from the daemon's
    // stdout/stderr, so it gets the same owner-only mode as `daemon.lock`
    // (see `lock.rs`) rather than relying on the umask default.
    let log_out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(log_path)
        .map_err(|e| PortError::Io(e.to_string()))?;
    let log_err = log_out
        .try_clone()
        .map_err(|e| PortError::Io(e.to_string()))?;
    Ok((log_out, log_err))
}

impl ProcessSpawner for NativeSpawner {
    async fn spawn_daemon(&self, exe_hint: Option<&str>, log_path: &str) -> Result<(), PortError> {
        let exe = match exe_hint {
            Some(e) => e.to_string(),
            None => std::env::current_exe()
                .map_err(|e| PortError::Io(e.to_string()))?
                .to_string_lossy()
                .to_string(),
        };

        let (log_out, log_err) = open_log_file(log_path)?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_log_file_should_create_file_with_owner_only_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("create tempdir");
        let log_path = dir.path().join("daemon.log");
        let log_path_str = log_path.to_str().expect("utf8 path").to_string();

        open_log_file(&log_path_str).expect("open log file");

        let mode = std::fs::metadata(&log_path)
            .expect("stat log file")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
