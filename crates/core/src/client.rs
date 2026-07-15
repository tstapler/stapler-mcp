//! Thin-client side: `ensure_daemon` (ping, else spawn + backoff-poll) and a
//! generic `call`. Holds no persistent connection — one dial per call, matching
//! the original Go client's stateless-proxy design.

use std::time::Duration;

use crate::ports::{ClockPort, Conn, PortError, ProcessSpawner, SleepPort, SocketFactory};
use crate::protocol::{Request, Response, PING_TOOL};

const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const DIAL_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_START: Duration = Duration::from_millis(50);
const POLL_CAP: Duration = Duration::from_millis(500);

#[derive(Clone, Default)]
pub struct EnsureOptions {
    pub startup_timeout: Option<Duration>,
    pub exe_hint: Option<String>,
}

/// Sends `tool`/`params`, returns the raw JSON result (or the daemon's error
/// string, wrapped).
pub async fn call<S: SocketFactory>(
    socket: &S,
    sock_path: &str,
    tool: &str,
    params: Option<serde_json::Value>,
    timeout: Duration,
) -> Result<serde_json::Value, PortError> {
    let mut conn = socket.connect(sock_path, timeout).await?;
    conn.set_timeout(timeout);
    let req = Request {
        tool: tool.to_string(),
        params,
    };
    let req_bytes = serde_json::to_vec(&req).map_err(|e| PortError::Other(e.to_string()))?;
    conn.write_frame(&req_bytes).await?;
    let resp_bytes = conn
        .read_frame()
        .await?
        .ok_or_else(|| PortError::Other("daemon closed connection with no response".into()))?;
    let resp: Response =
        serde_json::from_slice(&resp_bytes).map_err(|e| PortError::Other(e.to_string()))?;
    if let Some(err) = resp.error {
        return Err(PortError::Other(format!("daemon: {err}")));
    }
    Ok(resp.result.unwrap_or(serde_json::Value::Null))
}

pub async fn ping<S: SocketFactory>(
    socket: &S,
    sock_path: &str,
    timeout: Duration,
) -> Result<(), PortError> {
    call(socket, sock_path, PING_TOOL, None, timeout).await?;
    Ok(())
}

/// Ping; if that fails, spawn a daemon and poll with exponential backoff
/// (50ms doubling, capped at 500ms) until a ping succeeds, `opts.startup_timeout`
/// elapses (default 10s), or `clock`/`sleeper` report otherwise.
#[allow(clippy::too_many_arguments)]
pub async fn ensure_daemon<S, Sp, Sl, C>(
    socket: &S,
    spawner: &Sp,
    sleeper: &Sl,
    clock: &C,
    sock_path: &str,
    log_path: &str,
    opts: EnsureOptions,
) -> Result<(), PortError>
where
    S: SocketFactory,
    Sp: ProcessSpawner,
    Sl: SleepPort,
    C: ClockPort,
{
    if ping(socket, sock_path, DIAL_TIMEOUT).await.is_ok() {
        return Ok(());
    }

    spawner
        .spawn_daemon(opts.exe_hint.as_deref(), log_path)
        .await?;

    let deadline =
        clock.now_millis() + opts.startup_timeout.unwrap_or(DEFAULT_STARTUP_TIMEOUT).as_millis() as u64;
    let mut backoff = POLL_START;
    let mut last_err = PortError::Other("daemon never became reachable".into());
    loop {
        if clock.now_millis() >= deadline {
            return Err(last_err);
        }
        match ping(socket, sock_path, POLL_CAP).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = e;
                sleeper.sleep(backoff).await;
                backoff = std::cmp::min(backoff * 2, POLL_CAP);
            }
        }
    }
}
