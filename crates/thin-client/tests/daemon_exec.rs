//! `stapler-mcp --daemon` must exec the sibling `stapler-mcp-daemon` with argv
//! forwarded verbatim, and fail loudly (exit 127) when there is none. Uses a
//! fake daemon script so no browser/embedder is involved.
#![cfg(unix)]

use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;
use std::process::{Command, Output, Stdio};

const THIN: &str = env!("CARGO_BIN_EXE_stapler-mcp");

fn install_thin(dir: &Path) -> std::path::PathBuf {
    let dst = dir.join("stapler-mcp");
    std::fs::copy(THIN, &dst).unwrap();
    dst
}

fn install_fake_daemon(dir: &Path, out: &Path) {
    let p = dir.join("stapler-mcp-daemon");
    std::fs::write(
        &p,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n", out.display()),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Retries ETXTBSY: a sibling test thread forking while our freshly written
/// binary is still open for writing makes exec fail transiently.
fn run(thin: &Path, args: &[&str]) -> Output {
    for _ in 0..20 {
        let res = Command::new(thin)
            .args(args)
            .env("PATH", "") // no PATH fallback
            .stdin(Stdio::null())
            .output();
        match res {
            Err(e) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(50))
            }
            other => return other.unwrap(),
        }
    }
    panic!("exec of {} kept failing with ETXTBSY", thin.display());
}

#[test]
fn daemon_flag_should_exec_sibling_with_argv_forwarded_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("argv.txt");
    let thin = install_thin(dir.path());
    install_fake_daemon(dir.path(), &out);

    let res = run(&thin, &["--daemon", "--extra", "x y"]);

    assert!(res.status.success(), "{res:?}");
    assert_eq!(
        std::fs::read_to_string(out).unwrap(),
        "--daemon\n--extra\nx y\n"
    );
}

#[test]
fn daemon_flag_should_exit_127_naming_lookup_when_no_sibling_and_empty_path() {
    let dir = tempfile::tempdir().unwrap();
    let thin = install_thin(dir.path());

    let res = run(&thin, &["--daemon"]);

    assert_eq!(res.status.code(), Some(127), "{res:?}");
    let stderr = String::from_utf8_lossy(&res.stderr);
    assert!(
        stderr.contains("cannot exec stapler-mcp-daemon"),
        "{stderr}"
    );
    assert!(stderr.contains("scripts/install.sh"), "{stderr}");
}

#[test]
fn daemon_flag_should_find_sibling_of_symlink_target() {
    let real = tempfile::tempdir().unwrap();
    let links = tempfile::tempdir().unwrap();
    let out = real.path().join("argv.txt");
    let thin = install_thin(real.path());
    install_fake_daemon(real.path(), &out);
    let link = links.path().join("stapler-mcp");
    symlink(&thin, &link).unwrap();

    let res = run(&link, &["--daemon"]);

    assert!(res.status.success(), "{res:?}");
    assert_eq!(std::fs::read_to_string(out).unwrap(), "--daemon\n");
}
