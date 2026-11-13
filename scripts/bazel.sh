#!/usr/bin/env bash
# Local entry point for Bazel: bazelisk with a dedicated, size-capped output base and a TMPDIR outside $HOME.
#   scripts/bazel.sh test //...
# The disk cache is NOT set here: ~/.bazelrc owns it (shared across workspaces, size-capped by Bazel's own GC).
# Environment (all optional):
#   SM_BAZEL_OUTPUT_ROOT  output user root   (default ~/.cache/stapler-mcp-bazel; holds the output base, the
#                                            repository cache and the downloaded toolchains; ~10-15 GB when built)
#   SM_BAZEL_MAX_GB       when the output root is larger than this before a build/test, `bazel clean` runs first,
#                         then `bazel clean --expunge` if that was not enough (default 20; 0 disables the cap)
#   SM_BAZEL_PLAIN=1      pass arguments through unchanged (CI: setup-bazel owns the caches and output base)
# Why TMPDIR: crate_universe splices Cargo workspaces under $TMPDIR and refuses to run when a ~/.cargo/config.toml is
# found in a parent directory, which is the case for any TMPDIR below $HOME.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"
case "${TMPDIR:-}" in "" | "$HOME"/*) export TMPDIR=/tmp ;; esac
out=${SM_BAZEL_OUTPUT_ROOT:-$HOME/.cache/stapler-mcp-bazel}
max_gb=${SM_BAZEL_MAX_GB:-20}
bz=${SM_BAZELISK:-$(command -v bazelisk || command -v bazel || true)}
[ -n "$bz" ] || { echo "scripts/bazel.sh: neither bazelisk nor bazel is on PATH (brew install bazelisk)" >&2; exit 127; }
if [ "${SM_BAZEL_PLAIN:-0}" = 1 ]; then exec "$bz" "$@"; fi
if [ $# -eq 0 ]; then exec "$bz" --output_user_root="$out"; fi

size_gb() { du -sk "$out" 2>/dev/null | awk '{print int($1 / 1048576)}'; }

cmd=$1
case $cmd in
  build | test | run | coverage)
    if [ "$max_gb" -gt 0 ] && [ -d "$out" ] && [ "$(size_gb)" -gt "$max_gb" ]; then
      echo "scripts/bazel.sh: $out is $(size_gb) GB (> $max_gb GB): bazel clean" >&2
      "$bz" --output_user_root="$out" clean
      if [ "$(size_gb)" -gt "$max_gb" ]; then "$bz" --output_user_root="$out" clean --expunge; fi
    fi ;;
esac
exec "$bz" --output_user_root="$out" "$@"
