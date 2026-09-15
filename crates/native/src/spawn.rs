use std::fs::File;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use stapler_mcp_core::ports::{PortError, ProcessOutput, ProcessSpawner};

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

    /// C1 code review fix: the `.env_clear()` hardening (pitfalls.md §1c —
    /// stops daemon secrets leaking into the `op` subprocess environment) had
    /// zero test coverage. Sets a canary env var the daemon process would
    /// otherwise inherit, spawns `/usr/bin/env` (which just dumps its own
    /// environment to stdout) through `spawn_and_capture`, and asserts the
    /// canary never reaches the child — while `PATH` (one of the two
    /// explicitly allow-listed vars) still does, proving the child process
    /// actually ran with a real, non-empty environment rather than merely
    /// failing to launch.
    #[tokio::test]
    async fn spawn_and_capture_should_clear_env_except_allowlisted_vars_when_spawning_child() {
        std::env::set_var("STAPLER_MCP_TEST_CANARY", "should-not-leak-into-child-env");
        let spawner = NativeSpawner;

        let result = spawner.spawn_and_capture(&["/usr/bin/env"]).await;

        std::env::remove_var("STAPLER_MCP_TEST_CANARY");
        let result = result.unwrap();

        let stdout = String::from_utf8_lossy(&result.stdout);
        assert!(
            !stdout.contains("STAPLER_MCP_TEST_CANARY"),
            "child environment must not inherit arbitrary daemon env vars, got: {stdout}"
        );
        assert!(
            stdout.contains("PATH="),
            "the explicitly allow-listed PATH var should still reach the child, got: {stdout}"
        );
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
