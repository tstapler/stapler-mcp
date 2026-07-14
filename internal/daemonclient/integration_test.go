package daemonclient_test

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
	"time"

	"github.com/tstapler/stapler-mcp/internal/daemonclient"
)

// TestEnsureDaemon_AutoStartsAndRoundTrips is the "at least one real
// check" for the daemon architecture: it builds the actual stapler-mcp
// binary, points a completely isolated STAPLER_MCP_HOME at a temp dir (so
// this never touches a real daemon on the machine running the test),
// confirms no daemon is reachable yet, calls EnsureDaemon (which must
// spawn the binary with --daemon and wait for it to come up), performs a
// real ping round-trip over the Unix socket, and then shuts the daemon
// down cleanly via the shutdown tool.
func TestEnsureDaemon_AutoStartsAndRoundTrips(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping subprocess-spawning integration test in -short mode")
	}

	homeDir := t.TempDir()
	t.Setenv("STAPLER_MCP_HOME", homeDir)

	binPath := buildStaplerMCP(t)

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	// Sanity check: nothing should be listening yet.
	preClient, err := daemonclient.New()
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	if err := preClient.Ping(ctx); err == nil {
		t.Fatal("expected Ping to fail before any daemon has started")
	}

	client, err := daemonclient.EnsureDaemon(ctx, daemonclient.EnsureOptions{
		ExecutablePath: binPath,
		StartupTimeout: 15 * time.Second,
	})
	if err != nil {
		t.Fatalf("EnsureDaemon: %v", err)
	}

	var pong struct {
		Pong bool `json:"pong"`
	}
	if err := client.Call(ctx, "ping", nil, &pong); err != nil {
		t.Fatalf("ping round trip: %v", err)
	}
	if !pong.Pong {
		t.Fatalf("ping response = %+v, want pong:true", pong)
	}

	// A second EnsureDaemon call must reuse the already-running instance
	// rather than spawning another one (that's the whole point of the
	// architecture: one daemon serves every thin client).
	client2, err := daemonclient.EnsureDaemon(ctx, daemonclient.EnsureOptions{
		ExecutablePath: binPath,
		StartupTimeout: 5 * time.Second,
	})
	if err != nil {
		t.Fatalf("second EnsureDaemon: %v", err)
	}
	if err := client2.Call(ctx, "ping", nil, nil); err != nil {
		t.Fatalf("second client ping: %v", err)
	}

	// Clean teardown.
	if err := client.Call(ctx, "shutdown", nil, nil); err != nil {
		t.Fatalf("shutdown: %v", err)
	}
}

// buildStaplerMCP compiles cmd/stapler-mcp to a temp binary and returns
// its path, so the test exercises the real daemon/client wiring rather
// than a mock.
func buildStaplerMCP(t *testing.T) string {
	t.Helper()

	binPath := filepath.Join(t.TempDir(), "stapler-mcp")
	cmd := exec.Command("go", "build", "-o", binPath, "github.com/tstapler/stapler-mcp/cmd/stapler-mcp")
	cmd.Env = os.Environ()
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("go build cmd/stapler-mcp: %v\n%s", err, out)
	}
	return binPath
}
