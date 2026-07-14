// Command stapler-mcp is a thin-client/daemon pair that reimplements
// several third-party MCP servers previously run per-session via
// npx/uvx. See README.md and NOTES.md for the architecture rationale.
//
// Usage:
//
//	stapler-mcp           # thin stdio MCP server (what Claude Code launches)
//	stapler-mcp --daemon  # long-running background daemon (auto-started)
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"os/signal"
	"syscall"

	"github.com/tstapler/stapler-mcp/internal/daemon"
	"github.com/tstapler/stapler-mcp/internal/mcpserver"
	"github.com/tstapler/stapler-mcp/internal/tools/fetch"
	"github.com/tstapler/stapler-mcp/internal/tools/search"
)

func main() {
	daemonFlag := flag.Bool("daemon", false, "run as the long-running background daemon instead of the thin MCP stdio server")
	flag.Parse()

	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer cancel()

	if *daemonFlag {
		if err := runDaemon(ctx); err != nil {
			log.Fatalf("daemon: %v", err)
		}
		return
	}

	if err := mcpserver.Run(ctx); err != nil {
		log.Fatalf("mcp server: %v", err)
	}
}

// runDaemon wires every daemon-side tool implementation into a Daemon
// and serves it. Returning daemon.ErrAlreadyRunning is treated as
// success: another instance already won the single-instance lock.
func runDaemon(ctx context.Context) error {
	d := daemon.New()

	fetcher := fetch.NewFetcher()
	defer fetcher.Close()
	d.Register("fetch_page", jsonHandler(fetcher.Fetch))

	searchClient := search.NewClient()
	d.Register("brave_web_search", jsonHandler(searchClient.Search))

	err := d.Run(ctx)
	if err == daemon.ErrAlreadyRunning {
		log.Println("daemon: another instance is already running, exiting")
		return nil
	}
	return err
}

// jsonHandler adapts a typed tool function (In -> (Out, error)) into a
// daemon.Handler operating on raw JSON, so each tool package can work in
// terms of its own tooltypes structs without knowing about the wire
// format.
func jsonHandler[In, Out any](fn func(context.Context, In) (Out, error)) daemon.Handler {
	return func(ctx context.Context, params json.RawMessage) (json.RawMessage, error) {
		var in In
		if len(params) > 0 {
			if err := json.Unmarshal(params, &in); err != nil {
				return nil, fmt.Errorf("unmarshal params: %w", err)
			}
		}
		out, err := fn(ctx, in)
		if err != nil {
			return nil, err
		}
		return json.Marshal(out)
	}
}
