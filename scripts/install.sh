#!/usr/bin/env bash
# Installs the thin client and daemon into the same directory: the thin client
# finds `stapler-mcp-daemon` beside itself. `cargo install` takes one package
# per call, so both run here. Extra args (e.g. `--root DIR`) go to both.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo install --path crates/thin-client --locked --profile release-thin "$@"
cargo install --path crates/cli --locked "$@"
