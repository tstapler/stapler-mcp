use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use stapler_mcp_core::ports::{PortError, ProcessOutput, ProcessSpawner};

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

    async fn spawn_and_capture(&self, argv: &[&str]) -> Result<ProcessOutput, PortError> {
        let [program, args @ ..] = argv else {
            return Err(PortError::Io("spawn_and_capture: empty argv".to_string()));
        };

        // Hardening per pitfalls.md §1c: the eventual `op` invocation needs
        // its own env cleared of everything except what it explicitly
        // requires, rather than inheriting the daemon's full environment
        // (which `spawn_daemon` above intentionally does).
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args).env_clear();
        for key in ["OP_SERVICE_ACCOUNT_TOKEN", "PATH"] {
            if let Ok(value) = std::env::var(key) {
                cmd.env(key, value);
            }
        }

        let output = cmd
            .output()
            .await
            .map_err(|e| PortError::Io(e.to_string()))?;

        Ok(ProcessOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            // `.code()` is `None` only when the process was killed by a
            // signal rather than exiting normally; -1 is not a real exit
            // code so it can't be mistaken for a genuine success/failure.
            exit_code: output.status.code().unwrap_or(-1),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_and_capture_should_return_stdout_and_zero_exit_code_when_command_succeeds() {
        let spawner = NativeSpawner;

        let result = spawner
            .spawn_and_capture(&["/bin/echo", "hello"])
            .await
            .unwrap();

        assert_eq!(result.stdout, b"hello\n");
        assert_eq!(result.exit_code, 0);
    }

    #[tokio::test]
    async fn spawn_and_capture_should_separate_stdout_and_stderr_when_command_fails() {
        let spawner = NativeSpawner;

        // `sh -c` here is only this test's fixture for constructing a failing
        // process with distinct stdout/stderr — `spawn_and_capture` itself
        // never shells out (see the impl above).
        let result = spawner
            .spawn_and_capture(&["/bin/sh", "-c", "echo out; echo err >&2; exit 1"])
            .await
            .unwrap();

        assert_eq!(result.stdout, b"out\n");
        assert_eq!(result.stderr, b"err\n");
        assert_eq!(result.exit_code, 1);
    }
}
