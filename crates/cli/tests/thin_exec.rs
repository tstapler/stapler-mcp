//! Real two-binary tests: the thin client (`stapler-mcp`) next to the daemon
//! (`stapler-mcp-daemon`), as installed. `CARGO_BIN_EXE_*` only covers this
//! package's binary, so the thin client is located beside it; build both with
//! `cargo build --workspace` (or `scripts/install.sh`) before running.
//!
//! Tests run in parallel, so each child gets its isolated state dir through
//! `Command::env`, never the process-global environment.

#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use stapler_mcp_core::client;
use stapler_mcp_native::NativeSocketFactory;

fn daemon_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_stapler-mcp-daemon"))
}

/// The thin client sits beside the daemon binary; fail loudly (not skip) if absent.
fn thin_exe() -> PathBuf {
    let thin = daemon_exe().with_file_name("stapler-mcp");
    assert!(
        thin.is_file(),
        "{} not found: run `cargo build --workspace` first (a lone `cargo test -p stapler-mcp-daemon` does not build the thin client)",
        thin.display()
    );
    let mtime = |p: &std::path::Path| std::fs::metadata(p).and_then(|m| m.modified()).unwrap();
    assert!(
        mtime(&thin) + Duration::from_secs(1) >= mtime(&daemon_exe()),
        "{} is older than the daemon binary: rebuild with `cargo build --workspace`",
        thin.display()
    );
    thin
}

fn sock(home: &std::path::Path) -> String {
    format!("{}/daemon.sock", home.display())
}

async fn wait_for_ping(sock: &str, timeout: Duration) {
    let socket = NativeSocketFactory;
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if client::ping(&socket, sock, Duration::from_millis(500))
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("daemon never became reachable at {sock}");
}

async fn shutdown(sock: &str) {
    let socket = NativeSocketFactory;
    let _ = client::call(&socket, sock, "shutdown", None, Duration::from_secs(2)).await;
}

fn exe_name_of_pid(pid: u32) -> String {
    std::fs::read_link(format!("/proc/{pid}/exe"))
        .expect("read /proc/<pid>/exe")
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn reap(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// AC6: a tool call with no daemon running makes the thin client auto-start
/// the sibling `stapler-mcp-daemon` (not another copy of itself).
#[tokio::test]
async fn thin_client_should_autostart_sibling_daemon_when_tool_call_arrives_with_no_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let sock_path = sock(home);

    let mut child = Command::new(thin_exe())
        .env("STAPLER_MCP_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn thin client");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();

    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let send = |stdin: &mut std::process::ChildStdin, msg: &str| {
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    };
    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"thin_exec","version":"0"}}}"#,
    );
    rx.recv_timeout(Duration::from_secs(10))
        .expect("initialize response");
    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );
    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"stapler_browser_list_sessions","arguments":{}}}"#,
    );

    // The tool call itself may be slow (headless browser launch); the daemon
    // being reachable is what proves the spawn path.
    wait_for_ping(&sock_path, Duration::from_secs(40)).await;

    let pid: u32 = (0..50)
        .find_map(|_| {
            let pid = std::fs::read_to_string(home.join("daemon.lock"))
                .ok()
                .and_then(|s| s.trim().parse().ok());
            if pid.is_none() {
                std::thread::sleep(Duration::from_millis(100));
            }
            pid
        })
        .expect("pid in daemon.lock");
    assert_eq!(exe_name_of_pid(pid), "stapler-mcp-daemon");

    shutdown(&sock_path).await;
    reap(child);
}

/// Drop-in compatibility: `stapler-mcp --daemon` execs the sibling daemon.
#[tokio::test]
async fn thin_client_daemon_flag_should_exec_sibling_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let sock_path = sock(home);

    let child = Command::new(thin_exe())
        .arg("--daemon")
        .env("STAPLER_MCP_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn thin client --daemon");

    wait_for_ping(&sock_path, Duration::from_secs(40)).await;
    assert_eq!(exe_name_of_pid(child.id()), "stapler-mcp-daemon");

    shutdown(&sock_path).await;
    reap(child);
}
