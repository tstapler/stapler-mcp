# Validation Plan: browser-automation

**Date**: 2026-08-06

## Happy Path Scenario

Given the Baseline state (no persistent browser session exists yet — the daemon only
exposes `fetch_page`'s one-shot `navigate_and_extract`), when an LLM caller invokes
`stapler_browser_navigate` with a URL and no `sessionId`, then invokes
`stapler_browser_click` using a `ref` taken from that response, then the second call
succeeds against the *same* session (proven by a fresh `AxSnapshot` reflecting the
post-click DOM state), demonstrating that session state persists across calls in a way
`fetch_page` never could.

## Requirement → Test Mapping

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-1a: `navigate` returns new/reused `SessionId` + `NavigateResult` | `crates/core/src/tools/browser.rs` | `browser_navigate_should_return_new_session_id_when_no_session_given` | Unit | `FakeBrowserDriver` returns `Ok(NavigateResult{..})`; assert output session id round-trips |
| REQ-1a: `navigate` error path | `crates/core/src/tools/browser.rs` | `browser_navigate_should_return_err_when_url_is_empty` | Unit | empty `url` short-circuits before calling the fake driver (Task 5.1.1 AC) |
| REQ-1a: `navigate` error path — unknown session | `crates/native/src/browser.rs` | `navigate_should_return_not_found_when_session_id_given_but_absent` | Unit | `SessionRegistry` has no entry for given `SessionId`; assert `PortError::NotFound` |
| REQ-1a: `navigate` integration | `crates/cli/tests/browser_session.rs` | `stapler_browser_navigate_should_return_nonempty_session_id_over_real_daemon` | Integration (`#[ignore]`) | real daemon + mock site, assert response JSON has non-empty `sessionId` (Task 5.2.1 AC) |
| REQ-1b: `click` resolves `Locator` and returns fresh `AxSnapshot` | `crates/native/src/browser.rs` | `click_should_return_updated_snapshot_when_button_ref_resolved` | Unit | fake `BrowserSession` with known `latest_refs`; click dispatched, `ax::capture_snapshot` fake returns new tree |
| REQ-1b: `click` error path | `crates/native/src/ax.rs` (or `browser.rs`) | `resolve_locator_should_return_not_found_when_ref_missing_from_latest_refs` | Unit | Task 3.1.2 AC: `latest_refs` contains only `"e2"`, lookup `"e9"` → `PortError::NotFound` message contains `"e9"` and `"stapler_browser_snapshot"` |
| REQ-1b: `click` integration | `crates/cli/tests/browser_session.rs` | `click_should_show_clicked_text_when_button_clicked_on_real_page` | Integration (`#[ignore]`) | Task 3.3.1 AC: real mock page with `<button id="go">`, click, assert `"clicked!"` node appears |
| REQ-1c: `type_text` reflects typed value | `crates/core/src/tools/browser.rs` | `browser_type_should_return_action_output_when_type_succeeds` | Unit | `FakeBrowserDriver::type_text` returns `Ok(AxSnapshot{..})`; assert `BrowserActionOutput.snapshot` populated |
| REQ-1c: `type_text` error path | `crates/core/src/tools/browser.rs` | `browser_type_should_return_err_when_ref_id_is_empty` | Unit | empty `ref_id` validation short-circuits (mirrors Task 5.1.2) |
| REQ-1c: `type_text` integration | `crates/cli/tests/browser_session.rs` | `type_text_should_reflect_typed_value_in_accessible_value_when_real_input_used` | Integration (`#[ignore]`) | Task 3.3.2 AC: type into `<input id="name">`, subsequent snapshot shows value `"hello"` |
| REQ-1d: `snapshot` is read-only | `crates/native/src/browser.rs` | `snapshot_should_return_ok_without_mutating_page_when_session_live` | Unit | Task 3.3.3 AC: call `snapshot`, assert no page-mutation side effect recorded on fake `Page` |
| REQ-1d: `snapshot` error path | `crates/core/src/tools/browser.rs` | `browser_snapshot_should_return_actionable_message_when_session_not_found` | Unit | Task 5.1.3 AC: `PortError::NotFound("sess-9")` → exact `research/ux.md` §Error-1 message |
| REQ-1d: `snapshot` integration | `crates/cli/tests/browser_session.rs` | `browser_navigate_click_type_snapshot_round_trip` | Integration (`#[ignore]`) | Story 6.2 full sequence; final `stapler_browser_snapshot` call confirms end state |
| REQ-2: native AX-tree walk (`ax::capture_snapshot`) prunes ignored nodes, assigns refs | `crates/native/src/ax.rs` | `capture_snapshot_should_prune_ignored_node_and_assign_refs_when_axtree_has_hidden_sibling` | Unit | Task 3.1.1 AC: 3 flat `AXNode`s (1 root, 1 button child, 1 `ignored:true` sibling) → root has exactly 1 child, `ref="e2"` |
| REQ-2: native AX-tree walk error path — stale ref, attributed | `crates/native/src/browser.rs` | `resolve_locator_should_return_stale_ref_message_when_ref_issued_before_navigation` | Unit | Task 3.1.3 AC 1: ref issued under an earlier `nav_generation`, session re-navigated (bumping `nav_generation`) → `PortError::NotFound` message contains `"navigated since this ref was issued"` |
| REQ-2: native AX-tree walk error path — unknown ref, generic | `crates/native/src/browser.rs` | `resolve_locator_should_return_generic_message_when_ref_never_issued` | Unit | Task 3.1.3 AC 2: ref never issued to this session at all → `PortError::NotFound` message does **not** contain `"navigated since"`, confirming the generic (non-attributed) message path is still exercised |
| REQ-2: native AX-tree walk integration | `crates/cli/tests/browser_session.rs` | `navigate_should_return_nonempty_snapshot_when_axtree_built_lazily_on_real_page` | Integration (`#[ignore]`) | Task 3.2.1's real-page AC: exercises the lazy-AX-tree race the synthetic unit test cannot |
| REQ-3: `SessionIdleReaper` evicts idle sessions | `crates/native/src/browser.rs` | `spawn_reaper_should_evict_session_when_idle_past_session_idle_timeout` | Unit | Task 2.2.1 AC: `FakeClock`/`FakeSleeper`, one entry idle 301s, scan body removes map entry before `page.close()`'s `.await` resolves |
| REQ-3: `SessionIdleReaper` — negative case (in-flight call not evicted) | `crates/native/src/browser.rs` | `bump_last_used_should_prevent_eviction_when_call_in_flight_during_scan` | Unit | Task 2.3.1 AC: `last_used` bumped synchronously at call entry; a mid-call reaper scan at T0+5s does not evict |
| REQ-3: reaper shutdown sequencing integration | `crates/cli/tests/browser_session.rs` | `shutdown_should_abort_reaper_before_browser_close_when_session_open` | Integration (`#[ignore]`) | Task 2.4.1 AC: `"shutdown"` call with one open session completes without hanging |
| REQ-3b: tab-crash detection sets `PortError::SessionCrashed` then evicts | `crates/native/src/browser.rs` | `snapshot_should_return_session_crashed_then_not_found_when_crash_flag_set` | Unit | Task 2.5.1/2.5.2 AC: synthetic `EventTargetCrashed` sets `crashed=true`; first call → `SessionCrashed`, second call (same id) → `NotFound` |
| REQ-4: wasm adapter — `ariaSnapshot` parsing | `crates/wasm/src/glue/browser.js` (Node test harness) | `jsBrowserSnapshot_should_parse_ref_annotated_node_when_aria_snapshot_string_given` | Unit | Task 4.2.1 AC: mock `page.ariaSnapshot()` returns `"- button \"Submit\" [ref=e1]"` → parsed node `{role:"button",name:"Submit",ref:"e1"}` |
| REQ-4: wasm adapter error path | `crates/wasm/src/browser.rs` | `wasm_browser_navigate_should_map_no_session_marker_to_port_error_not_found` | Unit | Task 4.3.1: JS-thrown `"no session"`-marker error maps to `PortError::NotFound`, not `Other` |
| REQ-4: wasm adapter integration | `crates/wasm/src/glue/browser.js` (Node test harness) | `session_interval_should_evict_idle_session_when_last_used_exceeds_timeout_ms` | Integration | Task 4.1.2 AC: session inserted with `lastUsed = Date.now()-301_000`; interval callback body invoked directly, `sessions.has(id)` false after |
| REQ-5: schema types serialize/deserialize per convention | `crates/core/src/schema.rs` | `browser_navigate_input_should_omit_session_id_when_none_and_use_camelcase` | Unit | Task 1.2.1 AC: serialize `BrowserNavigateInput{session_id:None,..}`, JSON contains `"url"`, omits `"sessionId"` |
| REQ-5: schema types — error/round-trip path | `crates/core/src/schema.rs` | `browser_type_input_should_round_trip_when_deserialized_from_camelcase_json` | Unit | Task 1.2.3 AC: deserialize `{"sessionId":..,"refId":..,"text":..}`, re-serialize, structurally equal |
| REQ-5: daemon registration + tool listing integration | `crates/wasm/src/lib.rs` (or native `list_tools_json` equivalent) | `list_tools_json_should_contain_four_browser_tool_descriptors_when_called` | Integration | Task 5.3.2 AC: exactly 4 new entries named `stapler_browser_navigate/click/type/snapshot` |
| REQ-6: SSRF guard blocks navigate to private host | `crates/core/src/tools/browser.rs` | `browser_navigate_should_return_blocked_err_when_url_resolves_to_private_host` | Unit | Task 3.4.3 AC: `url="http://127.0.0.1:1/"`, `NetworkPolicy::Enforce` → `Err` containing `"blocked"`, fake driver never called |
| REQ-6: `FrameNavigatedGuard` error path — poisoned session | `crates/native/src/browser.rs` | `next_call_should_return_blocked_error_when_inpage_navigation_hit_link_local_address` | Unit | Task 3.4.2 AC: synthetic `EventFrameNavigated` to `169.254.169.254` sets `blocked`; next `snapshot` call errors with `"blocked"` + host |
| REQ-6: `FrameNavigatedGuard` recovery integration | `crates/cli/tests/browser_session.rs` | `navigate_should_clear_blocked_flag_when_renavigating_previously_blocked_session_to_safe_url` | Integration (`#[ignore]`) | Task 3.4.2's second AC: re-navigate a `blocked`-flagged session to a safe URL succeeds and clears the flag |

## UX Acceptance Tests

| UX Criterion | Test File | Test Name | Tool | Steps |
|---|---|---|---|---|
| 1. Chainable via refs alone | `crates/cli/tests/browser_session.rs` | `ux_ac1_agent_should_complete_navigate_type_type_click_snapshot_using_only_response_refs` | Integration / Manual | Call `navigate`; take `ref`s from response only; call `type`, `type`, `click`, `snapshot` in sequence never issuing a role/name pair or CSS selector; confirm no step required a value not present in a prior response |
| 2. Self-correcting session errors | `crates/core/src/tools/browser.rs` | `ux_ac2_session_not_found_error_should_name_session_id_and_corrective_call` | Unit | Call `browser_snapshot` with a bogus `session_id`; assert error text contains the literal id and the string `"browser_navigate"` |
| 3. Self-correcting locator errors | `crates/native/src/browser.rs` | `ux_ac3_locator_not_found_error_should_name_ref_and_page_url_and_corrective_call` | Unit | Call `click`/`resolve_locator` with an unknown `ref`; assert error contains the ref value, the current page URL, and `"browser_snapshot"` |
| 4. Navigation announced, not silently discovered | `crates/core/src/tools/browser.rs` | `ux_ac4_click_should_set_note_naming_new_url_when_click_causes_navigation` | Unit | Fake driver's `click` returns a snapshot whose `url` differs from the session's pre-click URL; assert `BrowserActionOutput.note` is `Some` and names the new URL, in the *same* response |
| 5. No dead ends | `crates/core/src/tools/browser.rs` (table-driven) | `ux_ac5_every_error_variant_should_name_a_specific_recovery_call_except_ssrf_target_block` | Unit | Table test over session-not-found, locator-not-found, timeout, SSRF-blocked-session errors: each names `browser_navigate` or `browser_snapshot`; the sole "target URL blocked" case is asserted to omit a retry suggestion |
| 6. Truncation is legible | `crates/native/src/ax.rs` | `ux_ac6_snapshot_should_set_truncated_true_when_node_count_exceeds_cap` | Unit | Feed `capture_snapshot` a synthetic tree over the node cap; assert `AxSnapshot.truncated == true` |
| 7. Accessible role+name fidelity | `crates/native/src/ax.rs` | `ux_ac7_button_with_no_explicit_role_attribute_should_surface_as_role_button` | Unit | Task 3.1.1-style AC applied to plain `<button>` with no ARIA attrs; assert resolved `role: "button"` |
| 8. Hidden/non-interactive nodes pruned | `crates/native/src/ax.rs` | `ux_ac8_aria_hidden_and_ignored_nodes_should_be_absent_from_snapshot` | Unit | Synthetic AX response with an `ignored:true` node and a `display:none` node; assert neither appears in the resulting tree |
| 9. Error vs informational note distinguishable | `crates/core/src/tools/browser.rs` | `ux_ac9_successful_response_should_never_contain_top_level_error_key_alongside_note` | Unit | Serialize a successful `BrowserActionOutput` with `note: Some(..)`; assert no `error` key exists in the JSON; serialize a failure path and assert no `note`/`snapshot` key exists |
| 10. Idle-session failure attributed to timing, not bug | `crates/native/src/browser.rs` | `ux_ac10_reaped_session_error_should_be_textually_identical_to_never_existed_session_error` | Unit | Compare the `PortError::NotFound` message for a reaper-evicted `session_id` vs. one that was never issued; assert byte-identical error text |

## Test Stack

- **Unit**: Rust built-in `#[test]` + fakes (`FakeBrowserDriver`, `FakeClock`/`FakeSleeper`, synthetic CDP event payloads), following the `InMemoryFileStore`/`FakeHttpClient` convention already used in `crates/core/src/tools/docs.rs`'s test module.
- **Wasm-side unit/integration**: Node test harness invoking `browser.js` glue functions directly with mock `page`/`sessions` objects (no real Chromium/Playwright needed for these; mirrors the existing JS-glue testing gap noted in `research/pitfalls.md` — full behavioral parity with native still requires the real-daemon test below).
- **Integration**: `#[ignore]`d real-daemon tests mirroring `docs_index_round_trip` (`crates/cli/tests/docs_index.rs`), new file `crates/cli/tests/browser_session.rs`, spawning a mock HTTP site (`spawn_mock_site`) and driving the real Unix-socket protocol end to end. Require a real Chrome/Chromium binary; run via `cargo test -p stapler-mcp-cli --test browser_session -- --ignored`.
- **E2E / UX**: no browser-rendered UI to automate with Playwright — the "UI" is MCP tool call/response JSON consumed by an LLM caller. UX acceptance tests above are implemented as unit/integration tests wherever the assertion is mechanical (error text, JSON shape); `ux_ac1` additionally doubles as a manual checklist item for a human reviewing a real agent transcript.

## Coverage Targets and How to Measure

| Stack | Coverage command | Target |
|---|---|---|
| Rust | `cargo tarpaulin --out Stdout --workspace` | ≥80% line, for `crates/core/src/tools/browser.rs`, `crates/core/src/ports.rs` (new items), `crates/native/src/browser.rs`, `crates/native/src/ax.rs` |
| Wasm/JS glue | manual line-count review of `crates/wasm/src/glue/browser.js` against the Node test harness's covered functions (no JS coverage tool wired into this project's CI) | every exported `jsBrowser*` function has at least one Node-harness test |

- All public `BrowserDriver` methods (`navigate`, `click`, `type_text`, `snapshot`): happy path + error path (`NotFound`, and `SessionCrashed` where applicable) covered.
- All external integrations (chromiumoxide CDP `Accessibility`/`Target`/`Page` events; wasm `playwright-core` glue): unit-mocked (synthetic CDP payloads / mock JS `page`) plus the one real-daemon `#[ignore]`d integration test (`browser_navigate_click_type_snapshot_round_trip`) exercising the full stack against a real Chromium instance.
- All 10 UX acceptance criteria in `design/ux.md` have a corresponding test row above — none are "manual checklist only" except the human-transcript-review half of `ux_ac1`.
- `cargo clippy --workspace --all-targets -- -D warnings` passes with no new warnings (per requirements.md's Success Metrics), checked as part of CI, not a separate test case.
