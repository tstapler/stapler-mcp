use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use stapler_mcp_core::ports::{Conn, Listener, PortError, SocketFactory};

const DEFAULT_CONN_TIMEOUT: Duration = Duration::from_secs(120);

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
        let (stream, _addr) = self
            .inner
            .accept()
            .await
            .map_err(|e| PortError::Io(e.to_string()))?;
        Ok(NativeConn {
            stream,
            timeout: DEFAULT_CONN_TIMEOUT,
        })
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
