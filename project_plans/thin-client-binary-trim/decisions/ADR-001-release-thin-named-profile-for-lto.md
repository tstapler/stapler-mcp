# ADR-001: Deliver AC3's `lto = "fat"` through a named `release-thin` profile

**Status**: Accepted (deviation from the literal wording of AC3; owner to confirm in PR review)
**Date**: 2026-10-18

## Context

AC3 asks for `strip = true`, `lto = "fat"`, `opt-level = "z"`, `codegen-units = 1`, "scoped via `[profile.release.package."stapler-mcp"]`".
Cargo rejects `lto` (and `panic`) in a per-package profile: `lto may not be specified in a package profile`
(VERIFIED in a scratch crate, `research/stack.md` section (a)). `strip` is parsed per package but is a link-time action on the final binary, so a per-package value does nothing useful.

## Decision

- Keep `[profile.release] strip = true` and `[profile.release.package."stapler-mcp"] opt-level = "z", codegen-units = 1` (the parts of AC3 that Cargo supports per package).
- Put the full recipe (`lto = "fat"`, `opt-level = "z"`, `codegen-units = 1`, `strip = true`) in `[profile.release-thin]`, built with `cargo build --profile release-thin -p stapler-mcp`.
- The thin-client artifact that ships is the `release-thin` one (`target/release-thin/stapler-mcp`). The daemon keeps plain `--release`, so daemon build time and behaviour are unchanged.

## Consequences

- Measured stripped sizes: pre-split 48,517,136 B; post-split `--release` 3,657,192 B; `release-thin` 2,733,624 B (-25.3% vs the plain release of the same crate). Both clear AC4 (>= 20% smaller) by a wide margin.
- The per-package override is nearly a no-op for size (dependencies stay at the profile's opt-level); it is retained so AC3 is met as literally as Cargo allows.
- Anything that builds the shipped thin client (README, CI size gate, any future release script) must pass `--profile release-thin`; output lands in `target/release-thin/`, not `target/release/`.

## Alternatives rejected

- Workspace-wide `[profile.release] lto = "fat"`: slows and changes the daemon build, which the requirements say not to modify.
- `-Zbuild-std` / UPX: need nightly or break macOS signing and raise RSS (`research/build-vs-buy.md` section 1).
