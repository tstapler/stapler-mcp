// Package ipc defines the wire protocol and filesystem layout shared by
// the stapler-mcp daemon and its thin clients: the Unix socket request/
// response envelope, and the socket/lockfile/log paths both sides agree on.
package ipc

import (
	"os"
	"path/filepath"
)

// envHomeOverride lets tests (and advanced users) point every socket/lock/log
// path at an isolated directory instead of the real ~/.stapler-mcp.
const envHomeOverride = "STAPLER_MCP_HOME"

// BaseDir returns the directory that holds the daemon's socket, lockfile,
// and log — normally ~/.stapler-mcp, overridable via STAPLER_MCP_HOME.
func BaseDir() (string, error) {
	if dir := os.Getenv(envHomeOverride); dir != "" {
		return dir, nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".stapler-mcp"), nil
}

// SocketPath returns the path to the daemon's Unix domain socket.
func SocketPath() (string, error) {
	dir, err := BaseDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "daemon.sock"), nil
}

// LockPath returns the path to the daemon's liveness/single-instance lockfile.
func LockPath() (string, error) {
	dir, err := BaseDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "daemon.lock"), nil
}

// LogPath returns the path the daemon should append its own logs to when
// spawned detached by a client (no controlling terminal to log to).
func LogPath() (string, error) {
	dir, err := BaseDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "daemon.log"), nil
}

// EnsureBaseDir creates BaseDir (and parents) if it does not exist yet.
func EnsureBaseDir() (string, error) {
	dir, err := BaseDir()
	if err != nil {
		return "", err
	}
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return "", err
	}
	return dir, nil
}
