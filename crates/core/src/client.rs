//! Thin-client side: `ensure_daemon` (ping, else spawn + backoff-poll) and a
//! generic `call`. Holds no persistent connection — one dial per call, matching
//! the original Go client's stateless-proxy design.

use std::time::Duration;

use crate::ports::{ClockPort, Conn, PortError, ProcessSpawner, SleepPort, SocketFactory};
use crate::protocol::{Request, Response, PING_TOOL};

// Chrome launches (main.rs: after the lock, before the socket binds), so a cold
// start can take well over 10s.
const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
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

/// Ping; if nothing is listening, spawn a daemon once and poll with exponential
/// backoff (50ms doubling, capped at 500ms) until a ping succeeds or
/// `opts.startup_timeout` elapses (default 30s). A ping that *times out* means
/// a listener exists but is busy (the daemon serves one connection at a time,
/// and the ping queues behind in-flight requests), so it counts as alive:
/// spawning or waiting here would burn the budget without sending the real call.
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
    let mut spawned = false;
    let deadline = clock.now_millis()
        + opts
            .startup_timeout
            .unwrap_or(DEFAULT_STARTUP_TIMEOUT)
            .as_millis() as u64;
    let mut backoff = POLL_START;
    let mut ping_timeout = DIAL_TIMEOUT;
    loop {
        let err = match ping(socket, sock_path, ping_timeout).await {
            Ok(()) | Err(PortError::Timeout) => return Ok(()),
            Err(e) => e,
        };
        if !spawned {
            spawner
                .spawn_daemon(opts.exe_hint.as_deref(), log_path)
                .await?;
            spawned = true;
            ping_timeout = POLL_CAP;
        }
        if clock.now_millis() >= deadline {
            return Err(err);
        }
        sleeper.sleep(backoff).await;
        backoff = std::cmp::min(backoff * 2, POLL_CAP);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    struct PongConn;

    impl Conn for PongConn {
        async fn read_frame(&mut self) -> Result<Option<Vec<u8>>, PortError> {
            Ok(Some(br#"{"result":{"pong":true}}"#.to_vec()))
        }
        async fn write_frame(&mut self, _bytes: &[u8]) -> Result<(), PortError> {
            Ok(())
        }
        fn set_timeout(&mut self, _dur: Duration) {}
    }

    struct NoListener;

    impl crate::ports::Listener for NoListener {
        type C = PongConn;
        async fn accept(&mut self) -> Result<PongConn, PortError> {
            unreachable!("clients never accept")
        }
    }

    #[derive(Clone, Copy)]
    enum Outcome {
        Pong,
        Refused,
        TimedOut,
    }

    impl Outcome {
        fn into_result(self) -> Result<(), PortError> {
            match self {
                Outcome::Pong => Ok(()),
                Outcome::Refused => Err(PortError::Io("Connection refused (os error 111)".into())),
                Outcome::TimedOut => Err(PortError::Timeout),
            }
        }
    }

    /// Scripted `connect` outcomes; once exhausted, repeats the last one.
    struct FakeSocket {
        script: RefCell<VecDeque<Outcome>>,
        last: Outcome,
    }

    impl FakeSocket {
        fn new(script: Vec<Outcome>) -> Self {
            Self {
                last: *script.last().expect("non-empty script"),
                script: RefCell::new(script.into()),
            }
        }
    }

    impl SocketFactory for FakeSocket {
        type L = NoListener;
        type C = PongConn;
        async fn bind(&self, _path: &str) -> Result<NoListener, PortError> {
            unreachable!()
        }
        async fn connect(&self, _path: &str, _timeout: Duration) -> Result<PongConn, PortError> {
            let outcome = self.script.borrow_mut().pop_front().unwrap_or(self.last);
            outcome.into_result().map(|()| PongConn)
        }
        async fn remove_stale(&self, _path: &str) -> Result<(), PortError> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct CountingSpawner(Cell<u32>);

    impl ProcessSpawner for CountingSpawner {
        async fn spawn_daemon(&self, _exe: Option<&str>, _log: &str) -> Result<(), PortError> {
            self.0.set(self.0.get() + 1);
            Ok(())
        }
        async fn spawn_and_capture(
            &self,
            _argv: &[&str],
        ) -> Result<crate::ports::ProcessOutput, PortError> {
            unreachable!()
        }
    }

    struct FakeTime(Cell<u64>);

    impl ClockPort for FakeTime {
        fn now_millis(&self) -> u64 {
            self.0.get()
        }
    }

    impl SleepPort for FakeTime {
        async fn sleep(&self, dur: Duration) {
            self.0.set(self.0.get() + dur.as_millis() as u64);
        }
    }

    async fn run(script: Vec<Outcome>, opts: EnsureOptions) -> (Result<(), PortError>, u32, u64) {
        let socket = FakeSocket::new(script);
        let spawner = CountingSpawner::default();
        let time = FakeTime(Cell::new(0));
        let result = ensure_daemon(&socket, &spawner, &time, &time, "s", "l", opts).await;
        (result, spawner.0.get(), time.0.get())
    }

    #[tokio::test]
    async fn ensure_daemon_should_not_spawn_when_first_ping_succeeds() {
        let (result, spawns, _) = run(vec![Outcome::Pong], EnsureOptions::default()).await;
        assert!(result.is_ok());
        assert_eq!(spawns, 0);
    }

    #[tokio::test]
    async fn ensure_daemon_should_spawn_once_when_not_listening() {
        let (result, spawns, _) = run(
            vec![Outcome::Refused, Outcome::Refused, Outcome::Pong],
            EnsureOptions::default(),
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(spawns, 1);
    }

    #[tokio::test]
    async fn ensure_daemon_should_return_ok_without_spawning_when_ping_times_out() {
        let (result, spawns, _) = run(vec![Outcome::TimedOut], EnsureOptions::default()).await;
        assert!(result.is_ok());
        assert_eq!(spawns, 0);
    }

    #[tokio::test]
    async fn ensure_daemon_should_return_ok_when_poll_ping_times_out_after_spawn() {
        let (result, spawns, _) = run(
            vec![Outcome::Refused, Outcome::Refused, Outcome::TimedOut],
            EnsureOptions::default(),
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(spawns, 1);
    }

    #[tokio::test]
    async fn ensure_daemon_should_spawn_at_most_once_and_return_last_error_at_deadline() {
        let opts = EnsureOptions {
            startup_timeout: Some(Duration::from_secs(5)),
            ..Default::default()
        };
        let (result, spawns, _) = run(vec![Outcome::Refused], opts).await;
        assert!(matches!(result, Err(PortError::Io(ref m)) if m.contains("Connection refused")));
        assert_eq!(spawns, 1);
    }

    #[tokio::test]
    async fn ensure_daemon_default_timeout_should_be_30s() {
        let (result, _, elapsed_ms) = run(vec![Outcome::Refused], EnsureOptions::default()).await;
        assert!(result.is_err());
        assert!(
            (30_000..31_000).contains(&elapsed_ms),
            "gave up after {elapsed_ms}ms"
        );
    }
}
