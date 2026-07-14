// Package mcpserver is the thin stdio MCP server that Claude Code
// actually launches per session. It holds no heavyweight state itself —
// every tool call is proxied to the shared daemon (internal/daemon) via
// internal/daemonclient, auto-starting the daemon on first use.
package mcpserver

import (
	"context"
	"fmt"

	"github.com/modelcontextprotocol/go-sdk/mcp"
	"github.com/tstapler/stapler-mcp/internal/daemonclient"
	"github.com/tstapler/stapler-mcp/internal/tooltypes"
)

// Version is the stapler-mcp release identifier reported to MCP clients.
const Version = "0.1.0"

// Run builds the MCP server, registers every tool, and serves it over
// stdio until ctx is canceled or the transport closes.
func Run(ctx context.Context) error {
	server := mcp.NewServer(&mcp.Implementation{Name: "stapler-mcp", Version: Version}, nil)

	mcp.AddTool(server, &mcp.Tool{
		Name:        "fetch_page",
		Description: "Render a URL in a headless browser and return its title and extracted text (optionally saving the rendered HTML to a local file). Backed by the shared stapler-mcp daemon's browser pool.",
	}, fetchPageHandler)

	mcp.AddTool(server, &mcp.Tool{
		Name:        "brave_web_search",
		Description: "Search the web via the Brave Search API. Requires BRAVE_API_KEY in the daemon's environment.",
	}, braveWebSearchHandler)

	return server.Run(ctx, &mcp.StdioTransport{})
}

func fetchPageHandler(ctx context.Context, _ *mcp.CallToolRequest, in tooltypes.FetchPageInput) (*mcp.CallToolResult, tooltypes.FetchPageOutput, error) {
	var out tooltypes.FetchPageOutput
	c, err := daemonclient.EnsureDaemon(ctx, daemonclient.EnsureOptions{})
	if err != nil {
		return nil, out, fmt.Errorf("ensure daemon: %w", err)
	}
	if err := c.Call(ctx, "fetch_page", in, &out); err != nil {
		return nil, out, err
	}
	return nil, out, nil
}

func braveWebSearchHandler(ctx context.Context, _ *mcp.CallToolRequest, in tooltypes.BraveSearchInput) (*mcp.CallToolResult, tooltypes.BraveSearchOutput, error) {
	var out tooltypes.BraveSearchOutput
	c, err := daemonclient.EnsureDaemon(ctx, daemonclient.EnsureOptions{})
	if err != nil {
		return nil, out, fmt.Errorf("ensure daemon: %w", err)
	}
	if err := c.Call(ctx, "brave_web_search", in, &out); err != nil {
		return nil, out, err
	}
	return nil, out, nil
}
