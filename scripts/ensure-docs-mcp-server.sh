#!/usr/bin/env bash
# Idempotent "make sure the shared docs-mcp-server is up" check.
#
# docs-mcp-server (github.com/arabold/docs-mcp-server, see NOTES.md) is a
# separate, out-of-scope-for-Rust-port third-party MCP server this user runs
# — but it was being spawned fresh per Claude Code session via stdio instead
# of connecting to one shared instance, which is exactly the duplication
# problem this whole project exists to fix. The fix here is a config change,
# not new stapler-mcp code: run it once as a systemd --user service and
# point ~/.claude.json's "docs" entry at its SSE endpoint instead.
#
# This script does not enable the service for auto-start on login/boot —
# deliberately just an on-demand "start it if it's not already running"
# check, not a persistence mechanism.

set -euo pipefail

SERVICE="docs-mcp-server.service"
HEALTH_URL="http://127.0.0.1:6280/"

if curl -sf -o /dev/null --max-time 3 "$HEALTH_URL"; then
    echo "docs-mcp-server already up at $HEALTH_URL"
    exit 0
fi

echo "docs-mcp-server not responding at $HEALTH_URL — starting $SERVICE"
systemctl --user start "$SERVICE"

for _ in $(seq 1 15); do
    sleep 1
    if curl -sf -o /dev/null --max-time 3 "$HEALTH_URL"; then
        echo "docs-mcp-server is up"
        exit 0
    fi
done

echo "docs-mcp-server did not become healthy within 15s — check: systemctl --user status $SERVICE" >&2
exit 1
