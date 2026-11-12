#!/usr/bin/env bash
# Bazel leg of the thin client's attack-surface and size contract (scripts/check-thin-client.sh is the Cargo leg):
# the thin client's Bazel dependency graph must hold none of the heavy crates (the daemon's must: positive control),
# and the --config=dist binary must be stripped and under SIZE_CEILING bytes.
set -euo pipefail
cd "$(dirname "$0")/.."

bz=${SM_BAZEL:-scripts/bazel.sh}
DENYLIST='chromiumoxide|fastembed|reqwest|hyper|ort|tokio-rustls|rustls'

# Third-party crate names of a target's graph: @crates//:<name> labels, minus the version suffix.
crates_of() {
  "$bz" query "kind(rust_library, deps($1))" --output=label 2>/dev/null \
    | sed -n 's|^@@\{0,2\}[^/]*crates__\(.*\)-[0-9][0-9a-zA-Z.+-]*//:.*|\1|p' | sort -u
}

thin=$(crates_of //crates/thin-client:stapler-mcp)
daemon=$(crates_of //crates/cli:stapler-mcp-daemon)
[ -n "$thin" ] || { echo "FAIL: control: no third-party crates found in the thin client graph (label parsing broke)" >&2; exit 1; }
printf '%s\n' "$daemon" | grep -xqE "$DENYLIST" \
  || { echo "FAIL: control: the daemon graph holds none of the denylisted crates, so the check proves nothing" >&2; exit 1; }
hits=$(printf '%s\n' "$thin" | grep -xE "$DENYLIST" || true)
if [ -n "$hits" ]; then
  echo "FAIL: heavy crates in the thin client Bazel graph:" >&2
  echo "$hits" >&2
  exit 1
fi
SIZE_CEILING="${SIZE_CEILING:-8000000}"
"$bz" build --config=dist //crates/thin-client:stapler-mcp >/dev/null 2>&1 \
  || { echo "FAIL: bazel build --config=dist //crates/thin-client:stapler-mcp failed" >&2; exit 1; }
bin=$("$bz" cquery --config=dist --output=files //crates/thin-client:stapler-mcp 2>/dev/null | tail -1)
[ -f "$bin" ] || { echo "FAIL: dist binary $bin not found" >&2; exit 1; }
size=$(stat -c %s "$bin" 2>/dev/null || stat -f %z "$bin")
[ "$size" -le "$SIZE_CEILING" ] || { echo "FAIL: $bin is $size bytes, ceiling $SIZE_CEILING" >&2; exit 1; }
if command -v readelf >/dev/null; then
  [ "$(readelf -S "$bin" | grep -c '\.symtab' || true)" -eq 0 ] || { echo "FAIL: $bin still has a symbol table" >&2; exit 1; }
fi
echo "OK: thin client dist binary is $size bytes (ceiling $SIZE_CEILING)"
echo "OK: thin client Bazel graph has $(printf '%s\n' "$thin" | wc -l) crates, none heavy (daemon: $(printf '%s\n' "$daemon" | wc -l))"
