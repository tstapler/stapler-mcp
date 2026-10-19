//! Finds the sibling `stapler-mcp-daemon` and handles the legacy `--daemon` argv.
//!
//! Unix-only (exec). The thin client contains no daemon code, so it must always hand
//! `NativeSpawner` an explicit `exe_hint`: with `None` the spawner would re-exec
//! the current binary (this thin client) with `--daemon`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const DAEMON_BIN: &str = "stapler-mcp-daemon";
pub const INSTALL_HINT: &str =
    "scripts/install.sh (installs stapler-mcp and stapler-mcp-daemon into the same directory)";

/// `stapler-mcp-daemon` beside `current_exe` (as given, then symlink-resolved,
/// since macOS `current_exe` may not resolve symlinks), if it is a file.
pub fn sibling_daemon_path(current_exe: Option<&Path>) -> Option<PathBuf> {
    let exe = current_exe?;
    let candidates = [Some(exe.to_path_buf()), std::fs::canonicalize(exe).ok()];
    candidates
        .into_iter()
        .flatten()
        .map(|p| p.with_file_name(DAEMON_BIN))
        .find(|sibling| sibling.is_file())
}

/// Suffix for errors when the daemon cannot be started: says where we looked.
pub fn lookup_hint() -> String {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<unknown>".into());
    format!("looked for {DAEMON_BIN} beside {exe}, then PATH; install both binaries with {INSTALL_HINT}")
}

/// Sibling daemon path, else the bare name for a `PATH` lookup by the spawner.
pub fn daemon_exe_path() -> String {
    sibling_daemon_path(std::env::current_exe().ok().as_deref())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| DAEMON_BIN.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    Mcp,
    /// Legacy `stapler-mcp --daemon`: run the sibling daemon instead.
    Daemon,
}

/// `args` excludes argv[0].
pub fn launch_mode(args: &[OsString]) -> LaunchMode {
    if args.iter().any(|a| a == "--daemon") {
        LaunchMode::Daemon
    } else {
        LaunchMode::Mcp
    }
}

/// Replaces this process with the daemon; only returns (exit code 127) on failure.
#[cfg(unix)]
pub fn exec_daemon(args: Vec<OsString>) -> ! {
    use std::os::unix::process::CommandExt;

    let exe = daemon_exe_path();
    let err = std::process::Command::new(&exe).args(args).exec();
    eprintln!(
        "stapler-mcp: cannot exec {DAEMON_BIN} ({exe}): {err}; {}",
        lookup_hint()
    );
    std::process::exit(127);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_daemon_beside_current_exe() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = dir.path().join(DAEMON_BIN);
        std::fs::write(&daemon, b"").unwrap();
        let exe = dir.path().join("stapler-mcp");
        assert_eq!(sibling_daemon_path(Some(&exe)), Some(daemon));
    }

    #[test]
    fn no_sibling_when_missing_or_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("stapler-mcp");
        assert_eq!(sibling_daemon_path(Some(&exe)), None);
        assert_eq!(sibling_daemon_path(None), None);
        std::fs::create_dir(dir.path().join(DAEMON_BIN)).unwrap();
        assert_eq!(sibling_daemon_path(Some(&exe)), None);
    }

    #[cfg(unix)]
    #[test]
    fn finds_sibling_through_symlinked_exe() {
        let real = tempfile::tempdir().unwrap();
        let links = tempfile::tempdir().unwrap();
        let daemon = real.path().join(DAEMON_BIN);
        std::fs::write(&daemon, b"").unwrap();
        let target = real.path().join("stapler-mcp");
        std::fs::write(&target, b"").unwrap();
        let link = links.path().join("stapler-mcp");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let found = sibling_daemon_path(Some(&link)).unwrap();
        assert_eq!(
            found.canonicalize().unwrap(),
            daemon.canonicalize().unwrap()
        );
    }

    #[test]
    fn launch_mode_detects_daemon_flag() {
        assert_eq!(launch_mode(&[]), LaunchMode::Mcp);
        assert_eq!(launch_mode(&["--daemon".into()]), LaunchMode::Daemon);
        assert_eq!(
            launch_mode(&["--other".into(), "--daemon".into()]),
            LaunchMode::Daemon
        );
        assert_eq!(launch_mode(&["--daemonize".into()]), LaunchMode::Mcp);
    }
}
