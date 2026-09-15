#!/usr/bin/env bash
# Reproduces the process-count/RSS comparison behind issue #37/#38: N
# stdio thin-client sessions (one `stapler-mcp` process each, today's
# behavior) vs. N concurrent sessions over the Streamable HTTP transport
# (zero per-session processes, one shared `--daemon`).
#
# Real Claude Code sessions aren't available to script — this uses the
# same underlying mechanism instead: N literal `stapler-mcp` (no args)
# stdio thin-client processes, each with stdin held open on a FIFO this
# script itself keeps the write end of open (matching how a real session
# keeps its client's stdin open for the session's lifetime, without an
# extra `tail`/pipe process in between that would make `$!` point at the
# wrong PID) vs. N concurrent HTTP `tools/list` calls against the same
# daemon.
#
# Runs in a fully isolated STAPLER_MCP_HOME so it never touches a real
# daemon/config. Exits non-zero (rather than printing fabricated numbers)
# if the daemon won't start, a thin-client process doesn't show up in the
# BEFORE measurement, or an HTTP call fails in the AFTER measurement.

set -euo pipefail

N="${1:-5}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$REPO_ROOT/target/debug/stapler-mcp"

if [[ ! -x "$BIN" ]]; then
    echo "building $BIN (cargo build --bin stapler-mcp)..." >&2
    (cd "$REPO_ROOT" && cargo build --bin stapler-mcp)
fi

WORKDIR="$(mktemp -d)"
export STAPLER_MCP_HOME="$WORKDIR/home"
mkdir -p "$STAPLER_MCP_HOME"

CLIENT_PIDS=()
DAEMON_PID=""

cleanup() {
    for pid in "${CLIENT_PIDS[@]:-}"; do
        kill "$pid" 2>/dev/null || true
    done
    if [[ -n "$DAEMON_PID" ]]; then
        kill "$DAEMON_PID" 2>/dev/null || true
        for _ in $(seq 1 20); do
            [[ -d "/proc/$DAEMON_PID" ]] || break
            sleep 0.2
        done
        kill -9 "$DAEMON_PID" 2>/dev/null || true
    fi
    rm -rf "$WORKDIR"
}
trap cleanup EXIT

# Pick a free port ourselves (bind-then-release has a small TOCTOU race,
# same tradeoff every "grab a free port for a test subprocess" helper
# makes — acceptable here, nothing else on this machine is racing for it).
PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')"

echo "=== starting daemon (HTTP on 127.0.0.1:$PORT, STAPLER_MCP_HOME=$STAPLER_MCP_HOME) ===" >&2
STAPLER_MCP_HTTP_PORT="$PORT" setsid "$BIN" --daemon </dev/null >"$WORKDIR/daemon.log" 2>&1 &
DAEMON_PID=$!

for _ in $(seq 1 50); do
    [[ -f "$STAPLER_MCP_HOME/http-token" ]] && break
    sleep 0.1
done
if [[ ! -f "$STAPLER_MCP_HOME/http-token" ]]; then
    echo "FAIL: daemon never wrote http-token — did not start. Log:" >&2
    cat "$WORKDIR/daemon.log" >&2
    exit 1
fi
TOKEN="$(cat "$STAPLER_MCP_HOME/http-token")"

stapler_mcp_pids() {
    # Every `stapler-mcp` process (daemon + any thin clients), matched on
    # the exact binary path so this doesn't pick up unrelated processes.
    pgrep -f "^$BIN( --daemon)?\$" || true
}

echo "=== BEFORE: spawning $N stdio thin-client sessions (stdin held open) ===" >&2
for i in $(seq 1 "$N"); do
    fifo="$WORKDIR/stdin_$i"
    mkfifo "$fifo"
    # Open read-write on our own fd so the FIFO never sees EOF (i.e. stdin
    # stays open) until this script exits and the fd closes — mimics a
    # real session holding its client's stdin open.
    eval "exec {fd_$i}<>\"$fifo\""
    fd_var="fd_$i"
    "$BIN" <"$fifo" >"$WORKDIR/thin_$i.log" 2>&1 &
    CLIENT_PIDS+=($!)
done
sleep 1.5

ALL_PIDS_BEFORE="$(stapler_mcp_pids)"
BEFORE_CLIENT_COUNT=0
BEFORE_CLIENT_RSS=0
for pid in $ALL_PIDS_BEFORE; do
    [[ "$pid" == "$DAEMON_PID" ]] && continue
    rss="$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ')"
    [[ -n "$rss" ]] || continue
    BEFORE_CLIENT_COUNT=$((BEFORE_CLIENT_COUNT + 1))
    BEFORE_CLIENT_RSS=$((BEFORE_CLIENT_RSS + rss))
done

if [[ "$BEFORE_CLIENT_COUNT" -ne "$N" ]]; then
    echo "FAIL: expected $N stdio thin-client processes, found $BEFORE_CLIENT_COUNT — cannot report fabricated numbers." >&2
    exit 1
fi

echo "BEFORE: $BEFORE_CLIENT_COUNT stdio thin-client process(es), combined RSS ${BEFORE_CLIENT_RSS} KB"

# Kill the thin clients directly — `CLIENT_PIDS` holds `$BIN`'s own PID
# (no intermediate pipe process to confuse `$!`), so this reliably tears
# each one down before the AFTER measurement.
for pid in "${CLIENT_PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true
done
CLIENT_PIDS=()
for _ in $(seq 1 30); do
    remaining="$(stapler_mcp_pids | grep -v "^$DAEMON_PID\$" || true)"
    [[ -z "$remaining" ]] && break
    sleep 0.2
done

echo "=== AFTER: $N concurrent HTTP tools/list calls against the same daemon ===" >&2
AFTER_TMP="$WORKDIR/http_results"
mkdir -p "$AFTER_TMP"
HTTP_PIDS=()
for i in $(seq 1 "$N"); do
    curl -sS -X POST "http://127.0.0.1:$PORT/mcp" \
        -H "accept: application/json, text/event-stream" \
        -H "content-type: application/json" \
        -H "authorization: Bearer $TOKEN" \
        -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' \
        -o "$AFTER_TMP/$i.json" -w '%{http_code}' >"$AFTER_TMP/$i.status" 2>"$AFTER_TMP/$i.err" &
    HTTP_PIDS+=($!)
done
for pid in "${HTTP_PIDS[@]}"; do
    wait "$pid"
done

HTTP_OK=0
for i in $(seq 1 "$N"); do
    status="$(cat "$AFTER_TMP/$i.status" 2>/dev/null || echo "")"
    if [[ "$status" == "200" ]] && grep -q '"tools"' "$AFTER_TMP/$i.json" 2>/dev/null; then
        HTTP_OK=$((HTTP_OK + 1))
    fi
done

if [[ "$HTTP_OK" -ne "$N" ]]; then
    echo "FAIL: only $HTTP_OK/$N HTTP tools/list calls succeeded — cannot report fabricated numbers." >&2
    exit 1
fi

ALL_PIDS_AFTER="$(stapler_mcp_pids)"
AFTER_EXTRA_COUNT=0
for pid in $ALL_PIDS_AFTER; do
    [[ "$pid" == "$DAEMON_PID" ]] && continue
    AFTER_EXTRA_COUNT=$((AFTER_EXTRA_COUNT + 1))
done
DAEMON_RSS="$(ps -o rss= -p "$DAEMON_PID" 2>/dev/null | tr -d ' ')"
if [[ -z "$DAEMON_RSS" ]]; then
    echo "FAIL: daemon (pid $DAEMON_PID) not found when measuring AFTER RSS." >&2
    exit 1
fi

echo "AFTER: $N/$N HTTP tools/list calls succeeded; $AFTER_EXTRA_COUNT additional stapler-mcp process(es); daemon RSS ${DAEMON_RSS} KB"

echo ""
echo "=== summary ==="
echo "before (stdio, $N sessions): $BEFORE_CLIENT_COUNT process(es), ${BEFORE_CLIENT_RSS} KB combined"
echo "after  (HTTP,  $N sessions): $AFTER_EXTRA_COUNT additional process(es), ${DAEMON_RSS} KB (daemon only, serving all $N)"
