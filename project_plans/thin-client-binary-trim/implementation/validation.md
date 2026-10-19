# Validation Plan: thin-client-binary-trim

**Date**: 2026-10-18
**Complexity**: 2. Inputs: `implementation/plan.md`, `requirements.md` (7 acceptance criteria). No `design/ux.md` (no user-facing surface; the MCP tool surface is unchanged), so there is no UX section.

## Happy Path Scenario
Given the pre-split tree at `628ec11` (one combined 48,517,136 B `stapler-mcp` linking `chromiumoxide`, `fastembed` and `reqwest`), when a maintainer builds the split workspace and a Claude Code session starts `stapler-mcp` with no daemon running, then the thin client (no heavy crates in `cargo tree -p stapler-mcp`, 2,733,624 B under `release-thin`) auto-starts the sibling `stapler-mcp-daemon` through `ensure_daemon` with a sibling `exe_hint`, forwards a tool call over `daemon.sock`, and returns the daemon's answer.

## Requirement Identifiers

Requirements come from `requirements.md`: the 7 acceptance criteria (AC1-AC7), the 4 goals (G1-G4) and the 3 constraints (C1-C3). The Non-Goals are covered only where a test guards them (C3, tool surface).

| ID | Requirement |
|---|---|
| AC1 | `cargo tree -e normal --package stapler-mcp` shows no `chromiumoxide`, `fastembed`, `reqwest` |
| AC2 | `stapler-mcp-daemon` builds and `cargo test --workspace` passes |
| AC3 | Thin-client release profile `strip = true`, `lto = "fat"`, `opt-level = "z"`, `codegen-units = 1`, scoped via `[profile.release.package."stapler-mcp"]` |
| AC4 | Stripped release thin client >= 20% smaller than the pre-split combined binary |
| AC5 | `stapler-mcp-native` default-enabled `heavy` feature gates `NativeBrowser`, `NativeEmbedder`, `NativeHttp` and their deps; `crates/thin-client` uses `default-features = false` |
| AC6 | `ensure_daemon` called with an `exe_hint` resolving to the sibling `stapler-mcp-daemon`, verified by an integration test |
| AC7 | `npm/` packaging bundles both `stapler-mcp` and `stapler-mcp-daemon` |
| G1 | Smaller binary artifact: no linked-in `chromiumoxide`/`fastembed` symbols |
| G2 | Reduced attack surface: heavy symbols absent from the thin-client binary |
| G3 | Standard release-profile size tuning on the thin-client target |
| G4 | Evaluate (optional) blocking relay / tokio removal; keep only measured wins |
| C1 | Drop-in: same argv interface (no `--daemon` flag means thin-client mode); legacy `--daemon` keeps working |
| C2 | Existing `crates/cli` tests (`tool_schema.rs` etc.) keep passing |
| C3 | Daemon binary functions identically |

## Deviations From the Acceptance Criteria (explicit)

These are the places where the delivered system will not literally satisfy the AC text. Tests below assert the *deviation as designed*, not the literal AC, and the PR must say so.

### AC3: `lto = "fat"` is not scoped per package (ADR-001)
- **Literal AC**: all four settings scoped via `[profile.release.package."stapler-mcp"]`.
- **Why it cannot be literal**: Cargo accepts only per-package overrides for `opt-level`, `codegen-units` and a few others. `lto` and `strip` are profile-level and cannot appear under `[profile.*.package.*]` (Cargo reference, "Profile settings" / "Overrides").
- **Delivered**: `[profile.release.package."stapler-mcp"]` carries `opt-level = "z"` and `codegen-units = 1`. `lto = "fat"` and `strip = true` live in a named profile `[profile.release-thin]` (inherits `release`), built with `cargo build --profile release-thin -p stapler-mcp`. `[profile.release] strip = true` is also set and is **workspace-wide**, so it applies to the daemon too (strip does not change behavior; recorded in ADR-001).
- **Tests**: the three `profile_*` tests and `release_thin_build_*` in the AC3 table assert the delivered layout; `daemon_release_build_should_not_inherit_opt_z_When_built_verbose` proves the daemon is otherwise unaffected. Owner confirmation of the reading is an open item in plan.md ("Unresolved Questions").

### AC7: `npm/` does not bundle the native binaries (ADR-003, Proposed, owner decision pending)
- **Literal AC**: `npm/` packaging bundles both `stapler-mcp` and `stapler-mcp-daemon`.
- **Fact**: `npm/package.json` has `"files": ["bin", "pkg"]` and is a WASM distribution (the `wasm` CI job builds `stapler-mcp-wasm`); it ships no native executable today, so there is nothing to split.
- **Planned delivery (ADR-003 option 1)**: AC7 is reinterpreted as "no regression to the WASM npm package, and the native two-binary layout is documented in README". Bundling native binaries into npm (options 2/3) is **out of scope** and needs its own plan.
- **Consequence**: under option 1 no test can prove the literal AC7, and the validation says so. the first three AC7 tests verify the reinterpretation. The last AC7 test (`npm_pack_should_contain_both_native_binaries_*`) is a **tripwire**: if the owner picks option 2/3, it flips to the literal check (both binaries in `npm pack --dry-run`) and the AC7 tests must be redesigned. Until the owner decides, AC7 is "covered as reinterpreted, literal AC unmet by design".

### AC4: baseline is rebuilt, and the CI ceiling is not 20%
- AC4 compares to the pre-split combined binary (48,517,136 B, stripped, `main` at `628ec11`). That figure is rebuilt (Task 3.1.1a), not trusted.
- The CI size gate uses an 8,000,000 B regression ceiling (review resolution in plan.md), **not** `0.8 x 48,517,136`. The 20% claim is reported separately by the baseline, `release_thin_size_*` and `strings_scan_*` tests in the AC4 table. (Story 3.1.2 prose still says `0.8 *`; the review resolution supersedes it. The tests here follow the resolution; fix the story text during implementation.)

## Plan Gaps Found While Designing Tests

1. **AC6 is not proven by the planned tests.** Story 2.1.2 / Task 2.1.2b exercise the `--daemon` *exec* path (ADR-002), where the thin client runs `daemon_exe_path()` directly. AC6 is about the *auto-start* path: a tool call reaches `call_daemon` -> `client::ensure_daemon(.., EnsureOptions { exe_hint: Some(daemon_exe_path()) })` -> `NativeSpawner::spawn_daemon`. A regression that sets `exe_hint: None` (which spawns another thin client, per the plan Glossary) would pass every planned test. **Added test `thin_client_should_autostart_sibling_daemon_When_tool_call_arrives_with_no_daemon`** (`thin_exec.rs`: start the thin client over stdio with no daemon, send `initialize` + a `tools/call` for a daemon-routed zero-argument tool such as `stapler_browser_list_sessions`, assert `daemon.sock` answers `ping` within the 10 s `ensure_daemon` deadline and that the daemon process image is `stapler-mcp-daemon`). Add it to Task 2.1.2b.
2. **`call_daemon` builds `EnsureOptions` inline**, so `exe_hint` is only observable end to end. The `ensure_options_should_set_exe_hint_*` test needs a small pure seam, e.g. `fn ensure_options() -> EnsureOptions` in `thin_client.rs`, so the hint can be asserted without spawning. Add to Task 2.1.1a (XS).
3. **Gate script needs fixture seams.** To test the denylist and size gate without editing manifests or building 48 MB binaries, `scripts/check-thin-client.sh` should accept `TREE_FILE` (pre-captured `cargo tree` output) and `THIN_BIN` (binary path) environment overrides, and `scripts/test-check-thin-client.sh` holds the cases. Add to Task 3.1.2a.

## Requirement -> Test Mapping

Types: **Unit** = in-process or pure-function, no external build; **Integration** = builds binaries, runs `cargo`, spawns processes or touches sockets; **Manual** = a human-run measurement or decision. Test files in `crates/` are Rust (`cargo test`); `scripts/test-check-thin-client.sh` is shell run under `shellcheck` and bash.

### AC1: no heavy crates in `cargo tree -p stapler-mcp`

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| AC1 | `scripts/test-check-thin-client.sh` | `denylist_should_pass_When_tree_has_no_heavy_crates` | Unit | Happy path: fixture tree of clean crates -> exit 0 |
| AC1 | `scripts/test-check-thin-client.sh` | `denylist_should_fail_and_print_name_When_reqwest_in_tree` | Unit | Error path: fixture with `reqwest` -> non-zero exit, stderr names `reqwest` |
| AC1 | `scripts/test-check-thin-client.sh` | `denylist_should_match_whole_crate_names_When_name_contains_denylisted_substring` | Unit | Edge: `dtoa-short`/`support-ort`-style names do not trip `ort`; exact `ort` does |
| AC1 | `scripts/check-thin-client.sh` | `thin_client_graph_should_have_no_heavy_crates_When_cargo_tree_p_stapler_mcp` | Integration | Real `cargo tree -e normal -p stapler-mcp --prefix none` filtered by the exact-name denylist (`chromiumoxide`, `fastembed`, `reqwest`, `hyper`, `ort`, `tokio-rustls`, `rustls`) prints nothing |
| AC1 | `scripts/check-thin-client.sh` | `unification_control_should_show_heavy_crates_When_both_packages_selected` | Integration | Negative control: `cargo tree -e normal -p stapler-mcp -p stapler-mcp-daemon` DOES list heavy crates, proving the denylist check can fail and documenting why `-p stapler-mcp` must be alone |

### AC2: daemon builds, workspace tests pass

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| AC2 | `crates/cli/tests/daemon_ping.rs` | `daemon_architecture_and_tools_round_trip` | Integration | Happy path: spawns `CARGO_BIN_EXE_stapler-mcp-daemon`, pre-daemon ping fails, real ping succeeds, tool round trip (existing test, retargeted to the renamed bin) |
| AC2 | `crates/cli/tests/{browser_session,docs_index,webcrawl}.rs` | existing suites | Integration | Daemon-backed browser, docs-index and webcrawl suites pass with `CARGO_BIN_EXE_stapler-mcp-daemon` (these cover C3 too) |
| AC2 | `scripts/check-thin-client.sh` | `workspace_build_should_produce_both_binaries_When_cargo_build_workspace` | Integration | Happy path: `cargo build --workspace` leaves `target/debug/stapler-mcp` and `target/debug/stapler-mcp-daemon` |
| AC2 | `scripts/check-thin-client.sh` | `workspace_tests_should_pass_When_cargo_test_workspace` | Integration | `cargo test --workspace` exit 0; test count recorded per configuration |
| AC2 | `crates/cli/tests/thin_exec.rs` | `daemon_should_fail_with_message_When_thin_client_binary_not_built` | Unit | Error path: the sibling-lookup helper panics with the message "run `cargo build --workspace` first" when `stapler-mcp` is absent next to the daemon bin (guards the `cargo test -p stapler-mcp-daemon` alone trap) |

### AC3: release profile (deviation, see above)

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| AC3 | `scripts/test-check-thin-client.sh` | `profile_should_declare_lto_fat_opt_z_cgu1_strip_When_release_thin_in_root_manifest` | Unit | Happy path: parses root `Cargo.toml`; `[profile.release-thin]` has `lto = "fat"`, `opt-level = "z"`, `codegen-units = 1`, `strip = true`, `inherits = "release"` |
| AC3 | `scripts/test-check-thin-client.sh` | `profile_should_scope_opt_z_and_cgu1_to_stapler_mcp_When_release_package_override_present` | Unit | `[profile.release.package."stapler-mcp"]` has `opt-level = "z"`, `codegen-units = 1`; the literal AC's package scope for the two settings Cargo allows |
| AC3 | `scripts/test-check-thin-client.sh` | `profile_check_should_fail_When_lto_missing_from_release_thin_fixture` | Unit | Error path: fixture manifest without `lto` -> non-zero exit naming the setting |
| AC3 | `scripts/check-thin-client.sh` | `release_thin_build_should_pass_lto_fat_opt_z_cgu1_strip_flags_When_built_verbose` | Integration | `cargo build --profile release-thin -p stapler-mcp -v` rustc line for `stapler-mcp` contains `-C lto` fat, `opt-level=z`, `codegen-units=1`, `strip` (proves the declared profile is the effective one) |
| AC3 | `scripts/check-thin-client.sh` | `daemon_release_build_should_not_inherit_opt_z_When_built_verbose` | Integration | Daemon rustc line has no `opt-level=z` and no `codegen-units=1` from the thin-client override; only the workspace `strip` is shared (guards C3 and the "daemon release profile unchanged" claim in ADR-001) |

### AC4: >= 20% smaller than the pre-split binary

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| AC4 | `scripts/check-thin-client.sh` | `baseline_should_reproduce_48517136_bytes_When_rebuilt_from_628ec11` | Integration | Happy path: scratch worktree at `628ec11`, `CARGO_PROFILE_RELEASE_STRIP=true cargo build --release -p stapler-mcp`; `stat -c %s` equals 48,517,136 or the remeasured value is recorded with `rustc -V`, triple, `Cargo.lock` hash, SHA |
| AC4 | `scripts/check-thin-client.sh` | `release_thin_size_should_be_at_most_38813708_When_built_release_thin` | Integration | Like-for-like and shipped profiles both <= 38,813,708 B (80% of baseline): `--release` 3,657,192 B and `release-thin` 2,733,624 B (or remeasured); reduction percentage and gzip sizes reported |
| AC4 | `scripts/test-check-thin-client.sh` | `size_gate_should_pass_When_binary_is_2733624_bytes` | Unit | Fixture file of 2,733,624 B -> exit 0 |
| AC4 | `scripts/test-check-thin-client.sh` | `size_gate_should_fail_When_binary_exceeds_8000000_bytes` | Unit | Error path: 8,000,001 B fixture -> non-zero; also a 40,000,000 B fixture -> non-zero (regression to near the combined size) |
| AC4 | `scripts/test-check-thin-client.sh` | `size_gate_should_error_When_binary_missing` | Unit | Error path: `THIN_BIN` points at a nonexistent file -> non-zero with a "not built" message, never a silent pass |
| AC4 | `scripts/check-thin-client.sh` | `strings_scan_should_find_no_heavy_symbols_When_release_thin_binary` | Integration | Happy path (G1/G2 evidence): `strings -a target/release-thin/stapler-mcp \| grep -ciE 'chromiumoxide\|fastembed\|reqwest\|onnx'` = 0 |
| AC4 | `scripts/check-thin-client.sh` | `strings_scan_should_find_heavy_symbols_When_run_on_daemon_binary` | Integration | Positive control: same command on `target/release/stapler-mcp-daemon` > 0, so a 0 above is meaningful |

### AC5: `heavy` feature

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| AC5 | `scripts/check-thin-client.sh` | `native_graph_should_exclude_heavy_deps_When_no_default_features` | Integration | `cargo tree -e normal -p stapler-mcp-native --no-default-features --prefix none \| awk '{print $1}' \| grep -cxE 'chromiumoxide\|fastembed\|reqwest'` = 0 |
| AC5 | `scripts/check-thin-client.sh` | `native_graph_should_include_heavy_deps_When_default_features` | Integration | Same pipeline with default features >= 3 (default is `heavy`) |
| AC5 | `scripts/check-thin-client.sh` | `thin_client_graph_should_not_enable_heavy_When_cargo_tree_features` | Integration | `cargo tree -e features -p stapler-mcp -i stapler-mcp-native` shows no `heavy` (always `-p stapler-mcp` alone) |
| AC5 | `scripts/check-thin-client.sh` | `daemon_graph_should_enable_heavy_When_cargo_tree_features_p_daemon` | Integration | Positive control: daemon graph shows `heavy` on `stapler-mcp-native` |
| AC5 | `crates/native` (cargo) | `native_should_build_and_clippy_clean_When_no_default_features` | Integration | `cargo clippy -p stapler-mcp-native --no-default-features --all-targets -- -D warnings` exit 0 (no `unused_imports`/`dead_code` from gated helpers) |
| AC5 | `crates/native` (cargo) | `native_tests_should_pass_When_no_default_features` | Integration | `cargo test -p stapler-mcp-native --no-default-features` passes; its count (lower than with default features) is recorded beside the default-features count |
| AC5 | `crates/native/Cargo.toml` | `unexpected_cfgs_should_flag_misspelled_feature_When_cfg_feature_heavey` | Unit | Error path: a scratch change to `feature = "heavey"` makes `cargo check -p stapler-mcp-native` warn (`unexpected_cfgs`), proving the lint is not suppressed |
| AC5 | `crates/thin-client/Cargo.toml` | `thin_client_manifest_should_declare_native_with_default_features_false_When_parsed` | Unit | Manifest check: `stapler-mcp-native = { path = "../native", default-features = false }` |

### AC6: auto-start via sibling `exe_hint`

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| AC6 | `crates/thin-client/src/thin_client.rs` (tests mod) | `prefers_daemon_beside_current_exe` | Unit | Happy path: sibling file present -> that path (existing) |
| AC6 | `crates/thin-client/src/thin_client.rs` (tests mod) | `falls_back_to_bare_name_when_no_sibling` | Unit | Error path: no sibling -> `"stapler-mcp-daemon"` for `PATH` lookup (existing) |
| AC6 | `crates/thin-client/src/thin_client.rs` (tests mod) | `sibling_daemon_path_should_return_real_dir_When_current_exe_is_symlink` | Unit | `/x/bin/stapler-mcp` symlink to `/x/real/stapler-mcp`, daemon in `/x/real` -> `/x/real/stapler-mcp-daemon` (Task 2.1.1a canonicalization) |
| AC6 | `crates/thin-client/src/thin_client.rs` (tests mod) | `sibling_daemon_path_should_fall_back_When_sibling_is_a_directory` | Unit | A directory named `stapler-mcp-daemon` beside the exe is rejected (`is_file()`, not `exists()`) |
| AC6 | `crates/thin-client/src/thin_client.rs` (tests mod) | `sibling_daemon_path_should_return_bare_name_When_current_exe_is_none` | Unit | `current_exe()` failure -> bare name, never `None`/empty |
| AC6 | `crates/thin-client/src/thin_client.rs` (tests mod) | `ensure_options_should_set_exe_hint_to_daemon_path_When_built` | Unit | The `EnsureOptions` used by `call_daemon` has `exe_hint == Some(daemon_exe_path())`, never `None` (needs the seam in Plan Gap 2) |
| AC6 | `crates/cli/tests/thin_exec.rs` | `thin_client_should_autostart_sibling_daemon_When_tool_call_arrives_with_no_daemon` | Integration | **AC6 proof** (Plan Gap 1): thin client run over stdio from the real `target/` dir, isolated `STAPLER_MCP_HOME`, no daemon; `initialize` + `tools/call` of a zero-arg daemon-routed tool; `ping` on `<home>/daemon.sock` succeeds within 10 s; `/proc/<pid>/exe` of the socket owner is `stapler-mcp-daemon`; test fails (does not skip) with a message naming the missing binary if the thin client was not built |
| AC6 | `crates/thin-client/tests/daemon_exec.rs` | `thin_client_should_pass_through_to_sibling_When_fake_daemon_beside_copied_binary` | Integration | Happy path with a fake daemon: copy of `CARGO_BIN_EXE_stapler-mcp` + executable `#!/bin/sh` `stapler-mcp-daemon` writing argv to `argv.txt`; `--daemon` run; file contains `--daemon`; fails if the script is absent |
| AC6 | `crates/thin-client/tests/daemon_exec.rs` | `thin_client_should_exit_127_with_message_When_no_sibling_and_empty_path` | Integration | Error path: only `stapler-mcp` in the dir, `PATH` empty -> stderr contains `cannot exec stapler-mcp-daemon (looked beside`, exit 127 |
| AC6 | `crates/thin-client/tests/daemon_exec.rs` | `thin_client_should_resolve_sibling_in_real_dir_When_invoked_through_symlink` | Integration | Symlink layout end to end: invoked via symlink, fake daemon in the real dir runs |
| AC6 | `crates/cli/tests/thin_exec.rs` | `real_thin_client_should_become_real_daemon_When_run_with_daemon_flag` | Integration | Two real binaries: `--daemon` with isolated `STAPLER_MCP_HOME`; `ping` over `daemon.sock` within 10 s; child killed on drop |

### C1: drop-in invocation and legacy `--daemon`

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| C1 | `crates/thin-client/src/main.rs` (tests mod) | `launch_mode_should_return_thin_client_When_no_daemon_arg` | Unit | Happy path: `["stapler-mcp"]` -> `LaunchMode::ThinClient` |
| C1 | `crates/thin-client/src/main.rs` (tests mod) | `launch_mode_should_match_only_exact_daemon_flag_When_table_of_argvs` | Unit | Table: `[]`, `["x"]`, `["--daemons"]` -> `ThinClient`; `["--daemon"]`, `["x","--daemon"]` -> `ExecDaemon` |
| C1 | `crates/thin-client/tests/daemon_exec.rs` | `thin_client_should_forward_all_args_verbatim_When_daemon_flag_with_extra_args` | Integration | `stapler-mcp --daemon --flag-x` -> daemon receives argv `["--daemon", "--flag-x"]` |
| C1 | `crates/thin-client/tests/daemon_exec.rs` | `thin_client_should_serve_stdio_When_started_without_daemon_flag` | Integration | No flag -> stdio MCP server answers `initialize` (no exec, no daemon needed to list tools) |

### C2: existing tests keep passing

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| C2 | `crates/thin-client/tests/tool_schema.rs` | existing suite via `#[path = "../src/thin_client.rs"]` | Integration | Happy path: `cargo test -p stapler-mcp --test tool_schema` passes and asserts the same 27 registered tools as before the move |
| C2 | `crates/thin-client/tests/tool_schema.rs` | `thin_client_should_register_27_tools_When_tool_router_listed` | Unit | Tool-surface guard (Non-Goal: surface unchanged): count and names equal the pre-split list; fails if a handler is dropped in the move |
| C2 | `scripts/check-thin-client.sh` | `thin_client_only_tests_should_pass_When_cargo_test_p_stapler_mcp` | Integration | Error-path guard for feature isolation: `cargo test -p stapler-mcp` (thin features only, no daemon dev-dependency) passes alone |

### C3: daemon functions identically

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| C3 | `crates/cli/tests/{daemon_ping,browser_session,docs_index,webcrawl}.rs` | existing suites (see AC2) | Integration | Daemon behavior covered by the unchanged daemon suites run against `stapler-mcp-daemon` |
| C3 | `scripts/check-thin-client.sh` | `daemon_release_build_should_not_inherit_opt_z_When_built_verbose` | Integration | Same test as listed in AC3: daemon not size-tuned |
| C3 | `crates/cli/tests/thin_exec.rs` | `daemon_should_register_same_tool_names_as_thin_client_When_listed` | Integration | Every `call_daemon("<name>", ..)` in the thin client names a tool the daemon registers (renaming/moving did not drop a route); catches thin/daemon drift |

### AC7: npm (deviation, see above)

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| AC7 | `.github/workflows/ci.yml` `wasm` job / `npm/` | `npm_wasm_build_and_tests_should_pass_When_ci_wasm_job_steps_run_locally` | Integration | Happy path under ADR-003 option 1: `cargo build -p stapler-mcp-core -p stapler-mcp-wasm --target wasm32-unknown-unknown`, `wasm-bindgen`, `npm test` in `npm/` all pass (no regression from the split) |
| AC7 | `npm/package.json` | `npm_pack_should_list_only_bin_and_pkg_When_dry_run` | Integration | `npm pack --dry-run` lists only `bin/` and `pkg/` entries; no native executable (documents the reinterpretation) |
| AC7 | `README.md` | `readme_should_state_both_binaries_share_a_directory_When_native_install_documented` | Manual | README install section says `stapler-mcp` and `stapler-mcp-daemon` must sit in the same directory; `grep -n 'same directory' README.md` finds it |
| AC7 | `npm/package.json` | `npm_pack_should_contain_both_native_binaries_When_owner_chooses_option_2_or_3` | Manual | **Tripwire, not run under option 1**: literal AC7. If the owner selects ADR-003 option 2/3, this becomes the pass criterion and a new plan is required |

### G1, G2: smaller artifact, reduced attack surface

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| G1, G2 | `scripts/check-thin-client.sh` | `thin_client_graph_should_have_no_heavy_crates_When_cargo_tree_p_stapler_mcp` | Integration | Same test as AC1 (dependency-graph evidence) |
| G1, G2 | `scripts/check-thin-client.sh` | `strings_scan_should_find_no_heavy_symbols_When_release_thin_binary` | Integration | Same test as AC4 (binary evidence) with positive control `strings_scan_should_find_heavy_symbols_When_run_on_daemon_binary` |
| G1, G2 | `scripts/check-thin-client.sh` | `thin_client_size_and_graph_checks_should_run_in_ci_When_pr_touches_native_or_thin_client` | Integration | Error-path guard: `.github/workflows/ci.yml` runs `scripts/check-thin-client.sh` in its own job (fat-LTO build), so a re-link of heavy fails the PR; verified by a draft PR that adds `reqwest` to `crates/thin-client/Cargo.toml` and observes the job fail |

### G3: size tuning on the thin client

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| G3 | `scripts/test-check-thin-client.sh` | `profile_should_declare_lto_fat_opt_z_cgu1_strip_When_release_thin_in_root_manifest` | Unit | Same as AC3 |
| G3 | `scripts/check-thin-client.sh` | `release_thin_build_should_pass_lto_fat_opt_z_cgu1_strip_flags_When_built_verbose` | Integration | Same as AC3 |
| G3 | `scripts/check-thin-client.sh` | `release_thin_binary_should_be_smaller_than_release_binary_When_both_built` | Integration | `release-thin` (2,733,624 B) < post-split `--release` (3,657,192 B): the extra tuning earns its place |

### G4: evaluate further trims, keep only measured wins (optional goal)

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| G4 | PR description | `trim_candidate_should_be_kept_When_stripped_size_drops_and_all_tests_pass` | Manual | Per candidate (`rmcp` `default-features = false`; `panic = "abort"` in `release-thin` only; tokio `fs` gating): rebuild `release-thin`, `stat -c %s` < 2,733,624 B, then `cargo test --workspace` + `thin_exec.rs` + `daemon_exec.rs` pass; otherwise revert and note the measurement |
| G4 | PR description | `trim_candidate_should_be_reverted_When_size_does_not_drop_or_a_test_fails` | Manual | Error path: any regression or no gain -> candidate reverted, measurement recorded; `panic = "abort"` additionally requires confirming nothing in `thin_client.rs`/`main.rs` relies on `catch_unwind` or tokio `JoinError` isolation |
| G4 | PR description | `bloat_attribution_should_be_recorded_When_cargo_bloat_run_on_release_thin` | Manual | `cargo bloat --profile release-thin -p stapler-mcp --crates -n 15` output pasted in the PR; blocking-relay / byte-pipe option recorded as deferred with reason |

## UX Acceptance Tests
Not applicable: infrastructure and packaging change, no user-facing surface (`ux.md` intentionally skipped; MCP tool surface unchanged, guarded by C2 `thin_client_should_register_27_tools_When_tool_router_listed`).

## Migration Test
N/A: no schema or data changes (plan.md "Migration Plan"). The Unix socket path and wire protocol are unchanged.

## Test Stack
- **Unit**: Rust `#[test]` with `std::fs` + `tempfile` for path-layout cases (`sibling_daemon_path`, `launch_mode`, `ensure_options`); `bash` + `shellcheck` fixture tests for `scripts/test-check-thin-client.sh` (fixture `cargo tree` text and dummy binaries via `TREE_FILE` / `THIN_BIN` overrides).
- **Integration**: `cargo test` integration tests spawning real processes (`CARGO_BIN_EXE_*`, `tempfile` homes, `#!/bin/sh` fake daemon); `cargo tree`, `cargo build`, `strings`, `stat`, `npm pack --dry-run` driven by `scripts/check-thin-client.sh` and the CI `native`/`wasm`/thin-client jobs.
- **E2E / UX**: none. Manual: the AC7 README check, the G4 trim decisions and the draft-PR denylist demonstration.
- **Rules every cargo-driven test obeys**: thin-client checks use `-p stapler-mcp` alone and release artifacts come from separate invocations (feature unification); `thin_exec.rs` requires `cargo build --workspace` first because `cargo test -p stapler-mcp-daemon` does not build the thin client.

## Coverage Targets and How to Measure

| Stack | Coverage command | Target |
|---|---|---|
| Rust | `cargo tarpaulin -p stapler-mcp --out Stdout` | >= 80% line for the new/changed code (`daemon_exe_path`/`sibling_daemon_path`/`ensure_options`/`launch_mode`); the 27 `#[tool]` handlers moved verbatim are covered by `tool_schema.rs`, not line coverage |
| Shell | `shellcheck scripts/*.sh` (via `make ready`) | clean; every exit path of `check-thin-client.sh` (denylist hit, size over ceiling, missing binary, clean) exercised by `test-check-thin-client.sh` |

- All new public/`pub(crate)` functions: happy path + error path covered (`sibling_daemon_path`: 5 cases, `launch_mode`: table, exec failure: exit 127 test).
- Both feature configurations of `stapler-mcp-native` are tested and clippy-clean.
- Every AC (7), goal (4) and constraint (3) has at least one row above; deviations AC3 and AC7 are asserted as designed, with AC7 literal check recorded as a tripwire.

## Plan Amendments Required Before Implementation
1. Task 2.1.2b: add `thin_client_should_autostart_sibling_daemon_When_tool_call_arrives_with_no_daemon` (Plan Gap 1).
2. Task 2.1.1a: extract `ensure_options()` (Plan Gap 2).
3. Task 3.1.2a: add `TREE_FILE`/`THIN_BIN` overrides and `scripts/test-check-thin-client.sh`; add the AC3 profile/flags check; use the 8,000,000 B ceiling (Plan Gap 3).
4. Story 3.1.2 text: replace "`0.8 * 48_517_136`" with the 8,000,000 B ceiling to match the review resolution.
