#!/usr/bin/env bash
# Gate for the thin client's size/attack-surface contract (AC1, AC3, AC4, AC5).
# Always runs cargo with `-p stapler-mcp` alone: selecting the daemon too would
# unify its `heavy` native feature into the thin client and defeat the check.
set -euo pipefail
cd "$(dirname "$0")/.."

DENYLIST='chromiumoxide|fastembed|reqwest|hyper|ort|tokio-rustls|rustls'
SIZE_CEILING="${SIZE_CEILING:-8000000}"
BASELINE_BYTES=48517136 # pre-split stripped `stapler-mcp` at 628ec11
THIN_BIN="${THIN_BIN:-target/release-thin/stapler-mcp}"

crate_names() { awk '{print $1}' | grep -xE "$DENYLIST" || true; }

# Exact crate-name match so e.g. `dtoa-short` never trips `ort`.
check_tree() {
  local hits
  hits=$(cargo tree -e normal --prefix none "$@" | crate_names | sort -u)
  if [ -n "$hits" ]; then
    echo "FAIL: heavy crates in dependency tree ($*):" >&2
    echo "$hits" >&2
    exit 1
  fi
}

check_tree -p stapler-mcp
check_tree -p stapler-mcp-native --no-default-features
if cargo tree -e features --prefix none -p stapler-mcp | grep -q 'stapler-mcp-native feature "heavy"'; then
  echo 'FAIL: `heavy` feature enabled for the thin client' >&2
  exit 1
fi

# Bare repo-root default selection must also yield a light thin client.
cargo build --profile release-thin
[ -f "$THIN_BIN" ] || { echo "FAIL: $THIN_BIN not built" >&2; exit 1; }

size=$(stat -c %s "$THIN_BIN" 2>/dev/null || stat -f %z "$THIN_BIN")
if [ "$size" -gt "$SIZE_CEILING" ]; then
  echo "FAIL: $THIN_BIN is $size bytes, ceiling $SIZE_CEILING" >&2
  exit 1
fi
if strings -a "$THIN_BIN" | grep -qiE 'chromiumoxide|fastembed|reqwest|onnx'; then
  echo "FAIL: heavy symbols found in $THIN_BIN" >&2
  exit 1
fi
echo "OK: $THIN_BIN is $size bytes ($((100 - size * 100 / BASELINE_BYTES))% smaller than the $BASELINE_BYTES-byte pre-split binary)"
