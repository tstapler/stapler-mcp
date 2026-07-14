package daemonclient

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"os/exec"
	"syscall"
	"time"

	"github.com/tstapler/stapler-mcp/internal/ipc"
)

// Client is a thin Unix-socket IPC client for the stapler-mcp daemon. It
// holds no persistent connection — each Call dials, sends one request,
// reads one response, and closes, so it's safe for concurrent use.
type Client struct {
	socketPath string
	dialTO     time.Duration
}

// New returns a Client pointed at the daemon socket under ipc.BaseDir().
func New() (*Client, error) {
	sock, err := ipc.SocketPath()
	if err != nil {
		return nil, err
	}
	return &Client{socketPath: sock, dialTO: 2 * time.Second}, nil
}

// Call sends a single tool invocation to the daemon and decodes its
// result into out (pass a pointer, or nil to discard the result).
func (c *Client) Call(ctx context.Context, tool string, params, out any) error {
	rawParams, err := json.Marshal(params)
	if err != nil {
		return fmt.Errorf("marshal params: %w", err)
	}

	dialer := net.Dialer{Timeout: c.dialTO}
	conn, err := dialer.DialContext(ctx, "unix", c.socketPath)
	if err != nil {
		return fmt.Errorf("dial daemon: %w", err)
	}
	defer conn.Close()

	if deadline, ok := ctx.Deadline(); ok {
		_ = conn.SetDeadline(deadline)
	} else {
		_ = conn.SetDeadline(time.Now().Add(2 * time.Minute))
	}

	req := ipc.Request{Tool: tool, Params: rawParams}
	if err := json.NewEncoder(conn).Encode(req); err != nil {
		return fmt.Errorf("send request: %w", err)
	}

	var resp ipc.Response
	if err := json.NewDecoder(bufio.NewReader(conn)).Decode(&resp); err != nil {
		return fmt.Errorf("read response: %w", err)
	}
	if resp.Error != "" {
		return fmt.Errorf("daemon: %s", resp.Error)
	}
	if out != nil && len(resp.Result) > 0 {
		if err := json.Unmarshal(resp.Result, out); err != nil {
			return fmt.Errorf("unmarshal result: %w", err)
		}
	}
	return nil
}

// Ping checks whether a daemon is already listening and responsive.
func (c *Client) Ping(ctx context.Context) error {
	return c.Call(ctx, ipc.PingTool, nil, nil)
}

// EnsureOptions customizes EnsureDaemon's auto-start behavior.
type EnsureOptions struct {
	// ExecutablePath overrides the binary spawned as the daemon. Defaults
	// to os.Executable() (i.e. "re-run myself with --daemon"). Tests use
	// this to point at a binary built from this exact source tree.
	ExecutablePath string
	// StartupTimeout bounds how long to wait for a freshly spawned daemon
	// to become reachable. Defaults to 10s.
	StartupTimeout time.Duration
}

// EnsureDaemon returns a Client ready to talk to a running daemon,
// spawning one (detached, background) if none is reachable yet. If
// several callers race to do this concurrently, every spawned process
// independently attempts the daemon's single-instance lock (see
// internal/daemon.acquireExclusiveLock) — only one wins and binds the
// socket, so it is safe for many EnsureDaemon calls to spawn in parallel.
func EnsureDaemon(ctx context.Context, opts EnsureOptions) (*Client, error) {
	c, err := New()
	if err != nil {
		return nil, err
	}

	if err := c.Ping(ctx); err == nil {
		return c, nil // already running
	}

	if err := spawnDaemon(opts.ExecutablePath); err != nil {
		return nil, fmt.Errorf("spawn daemon: %w", err)
	}

	timeout := opts.StartupTimeout
	if timeout == 0 {
		timeout = 10 * time.Second
	}
	deadline := time.Now().Add(timeout)
	backoff := 50 * time.Millisecond
	for {
		pingCtx, cancel := context.WithTimeout(ctx, 500*time.Millisecond)
		err := c.Ping(pingCtx)
		cancel()
		if err == nil {
			return c, nil
		}
		if time.Now().After(deadline) {
			return nil, fmt.Errorf("daemon did not become ready within %s: %w", timeout, err)
		}
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-time.After(backoff):
		}
		if backoff < 500*time.Millisecond {
			backoff *= 2
		}
	}
}

// spawnDaemon launches execPath (or, if empty, the current binary)
// with --daemon, detached from this process's session so it survives
// after the thin client (and its parent Claude Code session) exits.
func spawnDaemon(execPath string) error {
	if execPath == "" {
		self, err := os.Executable()
		if err != nil {
			return fmt.Errorf("determine own executable: %w", err)
		}
		execPath = self
	}

	if _, err := ipc.EnsureBaseDir(); err != nil {
		return err
	}
	logPath, err := ipc.LogPath()
	if err != nil {
		return err
	}
	logFile, err := os.OpenFile(logPath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o600)
	if err != nil {
		return fmt.Errorf("open daemon log: %w", err)
	}
	defer logFile.Close()

	cmd := exec.Command(execPath, "--daemon")
	cmd.Stdout = logFile
	cmd.Stderr = logFile
	cmd.Stdin = nil
	// Detach into its own session so it isn't killed when the spawning
	// thin client's process group receives a signal (e.g. the parent
	// Claude Code session/tmux pane closing).
	cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true}
	// Propagate STAPLER_MCP_HOME (if set) so a spawned daemon in tests
	// uses the same isolated paths as the spawning client.
	cmd.Env = os.Environ()

	if err := cmd.Start(); err != nil {
		return err
	}
	// We deliberately do not Wait() — the daemon is meant to outlive us.
	// Release the OS process handle so it doesn't linger as a zombie
	// dependency of this process's lifetime.
	return cmd.Process.Release()
}
