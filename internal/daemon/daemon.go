// Package daemon implements the long-running background process that
// owns the actual heavyweight state (browser instances, HTTP clients,
// caches) for stapler-mcp's tools — exactly one instance shared across
// the whole machine. Thin MCP-server processes (internal/daemonclient)
// connect to it over a Unix socket instead of duplicating that state
// per-session.
package daemon

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"net"
	"os"
	"time"

	"github.com/tstapler/stapler-mcp/internal/ipc"
)

// ErrAlreadyRunning is returned by Run when another daemon process
// already holds the single-instance lock. This is the expected outcome
// when multiple thin clients race to auto-start the daemon: all but one
// spawned process gets this and exits quietly.
var ErrAlreadyRunning = errors.New("daemon: another instance is already running")

// Handler processes one tool call's raw JSON params and returns raw JSON
// result (or an error, surfaced to the client as Response.Error).
type Handler func(ctx context.Context, params json.RawMessage) (json.RawMessage, error)

// Daemon dispatches incoming IPC requests to registered tool handlers.
type Daemon struct {
	handlers map[string]Handler
	shutdown chan struct{}
}

// New creates a Daemon with the built-in ping/shutdown handlers
// registered. Callers register their real tools (fetch_page,
// brave_web_search, ...) via Register before calling Run.
func New() *Daemon {
	d := &Daemon{
		handlers: make(map[string]Handler),
		shutdown: make(chan struct{}),
	}
	d.handlers[ipc.PingTool] = func(context.Context, json.RawMessage) (json.RawMessage, error) {
		return json.RawMessage(`{"pong":true}`), nil
	}
	d.handlers[ipc.ShutdownTool] = func(context.Context, json.RawMessage) (json.RawMessage, error) {
		close(d.shutdown)
		return json.RawMessage(`{"ok":true}`), nil
	}
	return d
}

// Register adds a tool handler under name. Registering the same name
// twice panics — that's a programming error (main.go wiring), not a
// runtime condition to handle gracefully.
func (d *Daemon) Register(name string, h Handler) {
	if _, exists := d.handlers[name]; exists {
		panic(fmt.Sprintf("daemon: handler %q already registered", name))
	}
	d.handlers[name] = h
}

// Run acquires the single-instance lock, binds the Unix socket, and
// serves requests until ctx is canceled, the shutdown tool is invoked, or
// an unrecoverable error occurs. If another daemon already holds the
// lock, Run returns ErrAlreadyRunning immediately — callers should treat
// that as a successful no-op (someone else is already serving).
func (d *Daemon) Run(ctx context.Context) error {
	baseDir, err := ipc.EnsureBaseDir()
	if err != nil {
		return fmt.Errorf("ensure base dir: %w", err)
	}

	lockPath, err := ipc.LockPath()
	if err != nil {
		return err
	}
	lockFile, err := acquireExclusiveLock(lockPath)
	if err != nil {
		return err
	}
	defer lockFile.Close()

	sockPath, err := ipc.SocketPath()
	if err != nil {
		return err
	}
	// We hold the exclusive lock, so any stale socket file left behind by
	// a previous crashed daemon is safe to remove.
	_ = os.Remove(sockPath)

	listener, err := net.Listen("unix", sockPath)
	if err != nil {
		return fmt.Errorf("listen on %s: %w", sockPath, err)
	}
	defer listener.Close()
	defer os.Remove(sockPath)

	log.Printf("daemon: listening on %s (base dir %s)", sockPath, baseDir)

	// Close the listener when the caller cancels ctx or a client invokes
	// the shutdown tool, whichever comes first — both unblock Accept.
	go func() {
		select {
		case <-ctx.Done():
		case <-d.shutdown:
		}
		listener.Close()
	}()

	for {
		conn, err := listener.Accept()
		if err != nil {
			select {
			case <-ctx.Done():
				return nil
			case <-d.shutdown:
				return nil
			default:
				return fmt.Errorf("accept: %w", err)
			}
		}
		go d.handleConn(ctx, conn)
	}
}

func (d *Daemon) handleConn(ctx context.Context, conn net.Conn) {
	defer conn.Close()
	_ = conn.SetDeadline(time.Now().Add(2 * time.Minute))

	var req ipc.Request
	if err := json.NewDecoder(bufio.NewReader(conn)).Decode(&req); err != nil {
		d.writeResponse(conn, ipc.Response{Error: fmt.Sprintf("decode request: %v", err)})
		return
	}

	handler, ok := d.handlers[req.Tool]
	if !ok {
		d.writeResponse(conn, ipc.Response{Error: fmt.Sprintf("unknown tool %q", req.Tool)})
		return
	}

	result, err := handler(ctx, req.Params)
	if err != nil {
		d.writeResponse(conn, ipc.Response{Error: err.Error()})
		return
	}
	d.writeResponse(conn, ipc.Response{Result: result})
}

func (d *Daemon) writeResponse(conn net.Conn, resp ipc.Response) {
	if err := json.NewEncoder(conn).Encode(resp); err != nil {
		log.Printf("daemon: write response: %v", err)
	}
}
