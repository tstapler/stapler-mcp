use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use stapler_mcp_core::ports::{Conn, Listener, PortError, SocketFactory};

const DEFAULT_CONN_TIMEOUT: Duration = Duration::from_secs(120);
const ACCEPT_RETRY_START: Duration = Duration::from_millis(100);
const ACCEPT_RETRY_CAP: Duration = Duration::from_secs(2);

fn is_transient_accept_error(e: &std::io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(
            libc::EMFILE
                | libc::ENFILE
                | libc::ENOMEM
                | libc::ENOBUFS
                | libc::ECONNABORTED
                | libc::EINTR
                | libc::EPROTO
        )
    )
}

pub struct NativeConn {
    stream: UnixStream,
    timeout: Duration,
}

impl Conn for NativeConn {
    // ponytail: byte-at-a-time read is the simplest correct implementation for
    // small JSON frames; switch to a buffered reader if profiling ever shows
    // this mattering.
    async fn read_frame(&mut self) -> Result<Option<Vec<u8>>, PortError> {
        let stream = &mut self.stream;
        let fut = async {
            let mut buf = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                let n = stream
                    .read(&mut byte)
                    .await
                    .map_err(|e| PortError::Io(e.to_string()))?;
                if n == 0 {
                    return Ok(if buf.is_empty() { None } else { Some(buf) });
                }
                if byte[0] == b'\n' {
                    return Ok(Some(buf));
                }
                buf.push(byte[0]);
            }
        };
        tokio::time::timeout(self.timeout, fut)
            .await
            .map_err(|_| PortError::Timeout)?
    }

    async fn write_frame(&mut self, bytes: &[u8]) -> Result<(), PortError> {
        let stream = &mut self.stream;
        let fut = async {
            stream
                .write_all(bytes)
                .await
                .map_err(|e| PortError::Io(e.to_string()))?;
            stream
                .write_all(b"\n")
                .await
                .map_err(|e| PortError::Io(e.to_string()))
        };
        tokio::time::timeout(self.timeout, fut)
            .await
            .map_err(|_| PortError::Timeout)?
    }

    fn set_timeout(&mut self, dur: Duration) {
        self.timeout = dur;
    }
}

pub struct NativeListener {
    inner: UnixListener,
}

impl Listener for NativeListener {
    type C = NativeConn;

    async fn accept(&mut self) -> Result<Self::C, PortError> {
        let mut backoff = ACCEPT_RETRY_START;
        loop {
            match self.inner.accept().await {
                Ok((stream, _addr)) => {
                    return Ok(NativeConn {
                        stream,
                        timeout: DEFAULT_CONN_TIMEOUT,
                    })
                }
                // Resource pressure or an aborted handshake says "try again", not
                // "the listener is broken"; exiting here would kill the daemon.
                Err(e) if is_transient_accept_error(&e) => {
                    eprintln!("stapler-mcp: transient accept error (retrying in {backoff:?}): {e}");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(ACCEPT_RETRY_CAP);
                }
                Err(e) => return Err(PortError::Io(e.to_string())),
            }
        }
    }
}

pub struct NativeSocketFactory;

impl SocketFactory for NativeSocketFactory {
    type L = NativeListener;
    type C = NativeConn;

    async fn bind(&self, path: &str) -> Result<Self::L, PortError> {
        let inner = UnixListener::bind(path).map_err(|e| PortError::Io(e.to_string()))?;
        Ok(NativeListener { inner })
    }

    async fn connect(&self, path: &str, timeout: Duration) -> Result<Self::C, PortError> {
        let stream = tokio::time::timeout(timeout, UnixStream::connect(path))
            .await
            .map_err(|_| PortError::Timeout)?
            .map_err(|e| PortError::Io(e.to_string()))?;
        Ok(NativeConn { stream, timeout })
    }

    async fn remove_stale(&self, path: &str) -> Result<(), PortError> {
        let _ = std::fs::remove_file(path);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_transient_accept_error_should_accept_fd_exhaustion_and_reject_permission_denied() {
        assert!(is_transient_accept_error(
            &std::io::Error::from_raw_os_error(libc::EMFILE)
        ));
        assert!(is_transient_accept_error(
            &std::io::Error::from_raw_os_error(libc::ECONNABORTED)
        ));
        assert!(!is_transient_accept_error(
            &std::io::Error::from_raw_os_error(libc::EACCES)
        ));
        assert!(!is_transient_accept_error(&std::io::Error::other(
            "no errno"
        )));
    }
}
