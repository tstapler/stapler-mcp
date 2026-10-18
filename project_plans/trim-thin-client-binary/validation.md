# Validation: Trim Thin-Client Binary Size

## Adversarial Review

### Concern 1 — The `current_exe` → `--daemon` self-spawn coupling breaks the split

**Claim:** `NativeSpawner::spawn_daemon` uses `std::env::current_exe()` + `--daemon`
when `exe_hint` is `None`. If the thin-client binary no longer handles `--daemon`, the
daemon will never start.

**Evidence:** Verified in `crates/native/src/spawn.rs` lines 12–14 — `exe_hint` is an
existing escape hatch with `None` defaulting to `current_exe()`. `EnsureOptions` in
`client.rs` already accepts `exe_hint: Option<String>`.

**Resolution:** Task 3.2 in the plan addresses this directly by wiring `daemon_exe_path()`
into `EnsureOptions` in the thin-client's `call_daemon`. The approach is correct;
the implementation must be tested with an integration test that verifies daemon auto-start
still works after the split.

**Risk level:** High (blocking if missed). Mitigated by the plan; must be verified in CI.

---

### Concern 2 — `crates/cli` binary naming conflict

**Claim:** Both `crates/cli` and the new `crates/thin-client` target a binary named
`stapler-mcp`. Cargo will error at link time if both are built in the same workspace
build (`cargo build --workspace`).

**Evidence:** `crates/cli/Cargo.toml` already declares `[[bin]] name = "stapler-mcp"`.
The proposed `crates/thin-client/Cargo.toml` also uses that name.

**Resolution:** Rename `crates/cli`'s binary to `stapler-mcp-daemon`. The `crates/cli`
crate becomes the daemon-only crate. `crates/thin-client` retains the `stapler-mcp`
name. This must also update the `--daemon` flag handling: since the daemon binary is now
directly invoked as `stapler-mcp-daemon` (no flag needed), `main.rs` in `crates/cli`
can drop the `--daemon` branch entirely and simply run `run_daemon()` unconditionally.

**Impact:** Any docs, release scripts, or npm packaging that reference the daemon binary
by name must be updated to `stapler-mcp-daemon`.

---

### Concern 3 — `fastembed` conditional compilation: does it actually gate on `[features]`?

**Claim:** `fastembed` in `crates/native` is unconditional today. If the `optional = true`
change in `Cargo.toml` is correct but the code in `embed.rs` doesn't compile without
`fastembed` (e.g., it uses `use fastembed::...` at top level), the crate will fail to
compile under `default-features = false`.

**Evidence:** Not yet verified — `crates/native/src/embed.rs` was not read.

**Resolution:** Before merging, confirm that `#[cfg(feature = "heavy")]` on the module
declaration in `lib.rs` fully prevents `embed.rs` from being compiled when the feature
is absent. The `mod embed;` gating with `#[cfg(feature = "heavy")]` means Rust will not
even parse `embed.rs` in that configuration. This is the standard Rust pattern and is
correct, but the build must be verified with an explicit `--no-default-features` invocation:

```
cargo build -p stapler-mcp-native --no-default-features
cargo build -p stapler-mcp  # thin-client, via crates/thin-client
```

---

### Concern 4 — `tool_schema.rs` integration test broken by file move

**Claim:** `crates/cli/tests/tool_schema.rs` uses `#[path]` to pull in `thin_client.rs`
from `crates/cli/src/`. After the move to `crates/thin-client`, the path reference
breaks.

**Evidence:** `thin_client.rs` line 443–450 shows the `#[cfg(test)]` accessor
`registered_tools()` and notes it is used by `tests/tool_schema.rs`. The comment says
"Only called from that separate integration-test binary (which pulls this file in via
`#[path]`)".

**Resolution:** Move `tests/tool_schema.rs` to `crates/thin-client/tests/` with an
updated `#[path]` pointing to `src/thin_client.rs`. Or, better, add a `[lib]` target
to `crates/thin-client` and test via normal `use crate::` — but this may require
adjusting the `#[cfg(test)]` gating in `thin_client.rs`.

---

### Concern 5 — npm packaging ships binaries; needs updating

**Claim:** The `npm/` directory (visible at repo root) likely contains packaging scripts
that bundle and publish `stapler-mcp`. After the split, the package must ship two
binaries.

**Evidence:** `npm/` directory exists; not yet inspected.

**Resolution:** Before declaring the feature complete, verify `npm/` packaging and update
it to include both `stapler-mcp` and `stapler-mcp-daemon` in the same npm package
directory. This should be a task in Phase 5.

---

### Concern 6 — LTO + `opt-level = "z"` significantly increases daemon build time

**Claim:** `lto = "fat"` + `codegen-units = 1` on the workspace profile applies to ALL
crates including the daemon. The daemon links `chromiumoxide` and `fastembed` — LTO
over that dependency graph will be very slow.

**Evidence:** Legitimate concern based on known Rust compile-time behavior; not benchmarked.

**Resolution:** Scope the aggressive profile to the thin-client package only using
`[profile.release.package."stapler-mcp"]` (the thin-client crate), leaving the daemon
at standard release settings. Alternatively apply `strip = true` workspace-wide (it
only affects link time slightly) and scope only `opt-level = "z"` and `lto` to the
thin-client.

---

## Validated Plan

All six concerns have resolutions. The critical items (Concerns 1 and 2) are addressed
in the plan. Concerns 3, 4, 5, and 6 are implementation-time checks that must be
completed before the feature is considered done. The overall approach is sound.

## Acceptance Criteria Status

| AC   | Status    | Notes                                              |
|------|-----------|----------------------------------------------------|
| AC-1 | Pending   | Requires Phase 1+2 complete; verified via cargo tree |
| AC-2 | Pending   | Requires Phase 3 complete                          |
| AC-3 | Pending   | Scoped per Concern 6 to thin-client package only   |
| AC-4 | Pending   | Measurable after Phase 1–3 complete                |
| AC-5 | Pending   | Phase 1 task                                       |
| AC-6 | Pending   | CI gate on cargo test --workspace                  |
