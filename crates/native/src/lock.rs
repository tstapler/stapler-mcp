use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;

use stapler_mcp_core::ports::{LockError, LockGuard, ProcessLock};

pub struct NativeLock;

pub struct NativeLockGuard {
    file: File,
}

impl LockGuard for NativeLockGuard {
    fn write_pid(&mut self, pid: u32) {
        // Best-effort, purely for operator debugging of a stuck daemon.
        let _ = self.file.set_len(0);
        let _ = self.file.seek(SeekFrom::Start(0));
        let _ = write!(self.file, "{pid}\n");
    }
}

impl ProcessLock for NativeLock {
    type Guard = NativeLockGuard;

    async fn acquire_exclusive(&self, path: &str) -> Result<Self::Guard, LockError> {
        let path = path.to_string();
        tokio::task::spawn_blocking(move || {
            use std::fs::TryLockError;

            let file = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(&path)
                .map_err(|e| LockError::Other(e.to_string()))?;

            match file.try_lock() {
                Ok(()) => Ok(NativeLockGuard { file }),
                Err(TryLockError::WouldBlock) => Err(LockError::AlreadyRunning),
                Err(TryLockError::Error(e)) => Err(LockError::Other(e.to_string())),
            }
        })
        .await
        .map_err(|e| LockError::Other(e.to_string()))?
    }
}
