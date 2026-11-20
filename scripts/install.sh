#!/usr/bin/env bash
# Installs the thin client and daemon into the same directory: the thin client
# finds `stapler-mcp-daemon` beside itself.
#
# Installs what this checkout contains: a source build by default. The prebuilt
# release (seconds, checksum-verified) is used only when HEAD sits exactly on a
# v* release tag, or when STAPLER_MCP_VERSION=vX.Y.Z names one; otherwise a
# "latest" download could silently install older code than the checkout.
# Falls back to a source build when the release/asset is missing or the checksum
# fails; STAPLER_MCP_FROM_SOURCE=1 skips the download attempt entirely.
#
# Source builds use the `release-fast` profile (thin LTO, parallel codegen) for
# the daemon; set STAPLER_MCP_PROFILE=release for the size/speed-optimized fat-LTO
# build that releases ship. Extra args (e.g. `--root DIR`) go to cargo; only a
# lone `--root DIR` is honoured by the prebuilt path.
#
set -euo pipefail
cd "$(dirname "$0")/.."

REPO="tstapler/stapler-mcp"

install_prebuilt() {
  local root="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}"
  case "$#" in
    0) ;;
    2) [[ "$1" == "--root" ]] || return 1; root="$2" ;;
    *) return 1 ;;
  esac

  local target
  case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) target=aarch64-apple-darwin ;;
    Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
    *) return 1 ;;
  esac

  local version="${STAPLER_MCP_VERSION:-$(git describe --exact-match --tags --match 'v*' HEAD 2>/dev/null || true)}"
  [[ -n "$version" ]] || return 1
  local base="https://github.com/$REPO/releases/download/$version"
  base="${STAPLER_MCP_RELEASE_BASE:-$base}" # test hook: any curl-readable dir/URL
  local asset="stapler-mcp-$target.tar.gz"

  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  curl -fsSL --retry 2 -o "$tmp/$asset" "$base/$asset" || return 1
  curl -fsSL --retry 2 -o "$tmp/$asset.sha256" "$base/$asset.sha256" || return 1
  (cd "$tmp" && shasum -a 256 -c "$asset.sha256" >/dev/null) || return 1

  tar -xzf "$tmp/$asset" -C "$tmp"
  mkdir -p "$root/bin"
  # Daemon first: a thin client without its daemon beside it is the broken state.
  install -m 0755 "$tmp/stapler-mcp-daemon" "$root/bin/stapler-mcp-daemon"
  install -m 0755 "$tmp/stapler-mcp" "$root/bin/stapler-mcp"
  echo "Installed prebuilt $asset into $root/bin"
}

if [[ "${STAPLER_MCP_FROM_SOURCE:-0}" != "1" ]] && install_prebuilt "$@"; then
  exit 0
fi
echo "Building from source (no prebuilt release for this checkout)" >&2

# Keep build artifacts in the repo so an interrupted build resumes instead of
# recompiling every dependency (`cargo install` otherwise uses a temp dir).
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"

# `cargo install` takes one package per call, so both run here.
cargo install --path crates/thin-client --locked --profile release-thin "$@"
cargo install --path crates/cli --locked --profile "${STAPLER_MCP_PROFILE:-release-fast}" "$@"
