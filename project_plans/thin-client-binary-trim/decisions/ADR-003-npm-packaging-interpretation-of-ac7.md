# ADR-003: Interpretation of AC7 ("npm/ bundles both binaries")

**Status**: Accepted by the owner (option 1)
**Date**: 2026-10-18

## Context

AC7: "`npm/` packaging bundles both `stapler-mcp` and `stapler-mcp-daemon`."
The npm package is WASM-only by design: `npm/package.json` has `"files": ["bin", "pkg"]` and a description that says "no native binary required". `npm/bin/stapler-mcp.js` runs the WASM client and WASM daemon (`wasm.run_daemon()`), and never spawns a Rust-native daemon. README.md (lines 68-73) cites macOS Gatekeeper as the reason for shipping no native executable. `stapler-mcp-daemon` (chromiumoxide, fastembed/ONNX, Unix sockets) cannot be compiled to WASM. The split changes nothing the npm package uses (the CI `wasm` job builds only `stapler-mcp-core` and `stapler-mcp-wasm`).

## Options

1. **No regression (recommended default).** npm stays WASM-only. Verify it still builds and `npm test` passes, assert with `npm pack --dry-run` that the package contains no native binaries, and document in the README that native releases ship `stapler-mcp` and `stapler-mcp-daemon` side by side in one directory (required by sibling lookup).
2. Per-platform `optionalDependencies` carrying both native executables (esbuild/biome pattern). New scope: release CI matrix, macOS signing/notarization, file-mode checks. Contradicts the Gatekeeper rationale.
3. Reword AC7 to a native release artifact (tarball or two `cargo install --path` invocations that put both bins in `~/.cargo/bin`).

## Decision (proposed)

Option 1 now. Do not invent per-platform packaging and do not add `files` entries pointing at binaries that do not exist. If the owner picks option 2 or 3, it becomes a separate story with its own plan.

## Consequences

- AC7 is satisfied only under the "nothing regresses, native artifacts documented" reading. This must be stated in the PR description so the reviewer can overrule it.

## Decision record (2026-10-18, made by the implementing agent; no user present)

AC7 cannot be met as written: the npm package ships WASM only (`files: ["bin","pkg"]`) and a native `stapler-mcp-daemon` cannot be compiled to WASM, so there is nothing to bundle. Adding per-platform native packages (option 2) would be new scope and contradicts the Gatekeeper rationale in README.md. Resolution: AC7 is marked failed in the backlog with this reason; option 1 is implemented as a no-regression guard (npm build and tests pass, `npm pack --dry-run` shows no native binaries) and the README documents the side-by-side native layout. The owner decides at PR review whether to open a separate story for option 2 or 3.

## Owner decision required

The owner must choose one of: (1) accept npm as WASM-only (no regression; README documents the side-by-side native layout), (2) per-platform native packages (new scope, macOS signing), or (3) reword AC7 to cover native release artifacts. Until then AC7 stays failed (`fail-6`) and this ADR stays Proposed.

## Owner decision (reword of AC7)

The owner chose to reword criterion 7 on the backlog side, with the stored criterion text left unchanged. Criterion 7 is reported `pass` against this reworded text:

> npm package remains WASM-only; native stapler-mcp and stapler-mcp-daemon binaries are not bundled (ADR-003).

The original wording ("`npm/` packaging is updated to bundle both `stapler-mcp` and `stapler-mcp-daemon` binaries") is **not** met and is not claimed to be met. What was delivered for it: no regression to the WASM npm package, the glue now prefers a `stapler-mcp-daemon` on `PATH` (`crates/wasm/src/glue/process.js`, with a test), and the README documents the side-by-side native layout. The PR body repeats this reword.
