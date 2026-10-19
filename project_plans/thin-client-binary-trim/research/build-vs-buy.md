# Build vs Buy: Thin-Client Binary Size

Evidence labels: VERIFIED = command run in this worktree; PRIOR-KNOWLEDGE = general Rust ecosystem knowledge, not re-checked here.

## Measured baseline (VERIFIED)

- `crates/thin-client/src/thin_client.rs`: 505 lines, 27 `#[tool(` handlers on `#[tool_router]` / `#[tool_handler]` (grep). Tool schemas come from rmcp + schemars derive.
- `cargo tree -p stapler-mcp --offline` (normal edges): 207 unique entries (`sort -u | wc -l`). No `chromiumoxide`/`fastembed`/`reqwest` visible at depth 1; deps are rmcp 2.2.0, tokio 1.52.3, serde, serde_json, core, native(default-features=false). Transitive extras include chrono, regex, futures-*, schemars, mio.
- Existing binaries: `target/release/stapler-mcp` 3,657,192 B (release profile, per-package opt-level=z, no LTO); `target/release-thin/stapler-mcp` 2,733,624 B (`release-thin`: fat LTO, z, cgu=1, strip). That is -25.3% vs the plain release of the same split crate. The pre-split combined binary (AC4 baseline) was not measured here; it must be built from `main` for the >=20% check.
- `cargo-bloat` and `upx` are not installed on this machine.

## 1. OSS size-reduction techniques

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| Crate split / `heavy` feature gate (already in progress) | Removes chromiumoxide/fastembed/reqwest from the graph; biggest single win; also the attack-surface goal | Churn in native crate | Recommended |
| min-sized-rust recipe: `opt-level="z"`, `lto="fat"`, `codegen-units=1`, `strip` | Already applied via `release-thin`; measured 25% off | `lto` is profile-wide, so needs a named profile and build/CI/npm must pass `--profile release-thin` (the `[profile.release.package]` override alone cannot set lto) | Recommended |
| `panic = "abort"` | Typically drops unwinding tables and landing pads (PRIOR-KNOWLEDGE: 5-10% in small binaries); no downside for a stateless relay | Profile-wide; tests (`cargo test`) ignore it, but `catch_unwind`/`JoinError` panic handling in tokio tasks stops working, so a panicking task kills the process; and the rmcp service must not depend on catching panics (verify) | Viable: try it in `release-thin`, measure |
| `cargo bloat --release-thin --crates` / `cargo tree -d` | Read-only diagnostic, finds what rmcp/chrono/regex drag in; installable via `cargo install cargo-bloat` (or brew) | Diagnostic only | Recommended as a one-off measurement step |
| Trim features (`regex`, `chrono` via rmcp/schemars; `default-features=false` on rmcp) | Cheap; may cut deps without changing code | rmcp feature set may be fixed by macros; needs `cargo tree -i chrono` to confirm who pulls it | Viable |
| `-Zbuild-std` with `panic_immediate_abort` | Smallest possible std (PRIOR-KNOWLEDGE: ~20-30% extra on tiny binaries) | Needs nightly + rust-src, breaks reproducible stable CI and the npm release pipeline | Not recommended |
| UPX | Large on-disk shrink (often 50%+) | Not installed; breaks macOS code signing/notarization (the binary is modified post-sign; Apple Silicon requires valid signature); AV false positives; decompression at every exec of ~30 per-session processes, and RSS goes UP because compressed pages are fully unpacked into anonymous memory, defeating the shared-page-cache advantage the requirements note | Not recommended |

## 2. Replace rmcp+tokio with a blocking `std::os::unix::net::UnixStream` relay (backlog #3)

What the thin client must still do: answer MCP `initialize`, answer `tools/list` with 27 schemas, and forward `tools/call` to the daemon. The schema surface is the hard part, not the transport.

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| A. Keep rmcp + tokio, apply profile tuning and panic=abort | Zero behavior risk; `tests/tool_schema.rs` keeps passing unchanged; schemas stay generated from the same types as the daemon | Floor stays at rmcp+tokio+schemars+chrono size (~2.7 MB measured, not cut further) | Recommended for this change |
| B. Blocking std relay, schemas still via rmcp/schemars | Removes tokio runtime only | rmcp's server stack is async-only (tokio-bound), so you cannot keep rmcp's service loop and drop tokio; only schema types survive, which is mostly the dependency we wanted gone | Not recommended |
| C. Blocking std relay with no rmcp: hand-written JSON-RPC line framing (read stdin line, forward to daemon socket, write reply), `tools/list` served from a build-time-generated static JSON file produced by the daemon crate | Likely removes rmcp, tokio, schemars, chrono, futures, mio: probably the majority of the remaining 2.7 MB (UNVERIFIED estimate; needs a spike with cargo-bloat); fewer threads, lower per-process overhead, tiny attack surface | Reimplements a protocol subset: must handle `initialize` negotiation, `notifications/*` (no reply), `ping`, cancellation, id echo, progress, protocol-version drift as rmcp upgrades; schema JSON must be kept in sync (needs a generated-artifact test or the `tool_schema.rs` test pointed at it); thin client currently also handles daemon connect/auto-start via `NativeSocketFactory/Spawner/Sleeper/Clock` ports, which are async traits, so the ports would need a sync twin | Viable, as a follow-up spike behind the optional goal 4 |
| D. Transparent byte pipe: thin client does only `ensure_daemon` then splices stdin/stdout to the Unix socket, daemon speaks MCP over it | Smallest and simplest (about 100 lines, std only, two `io::copy` threads); no protocol parsing so no drift | Daemon must serve MCP per-connection (a change to the daemon, which Non-Goals exclude) and per-session identity/handshake would move into the daemon; closely related to the issue #38 direction | Viable later, out of scope now |

## 3. LLM-written bespoke relay vs library correctness risk

- Library (rmcp): spec conformance is maintained upstream; the cost is binary size. Correctness risk is low, size risk is the thing being tuned.
- Bespoke relay: the happy path is easy to generate; the risk is the long tail (notification vs request distinction, batch arrays, large messages split across reads, partial lines, EOF/daemon-death propagation, stdout flush ordering, stderr hygiene, unknown-method error codes). Those are exactly the cases an LLM-authored first draft misses and that surface only under a real Claude Code client across ~30 sessions.
- Mitigation if pursued: a differential test replaying recorded rmcp-thin-client sessions against the bespoke relay (byte-compare responses), plus a golden `tools/list` snapshot from the daemon's registered tools. With that harness the correctness risk is acceptable; without it, not.
- Verdict: do not take on the bespoke relay in this change (Not recommended now); revisit as option C/D once the cheap wins are measured and if the residual size still matters.

## 4. Fork / adapt options

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| Fork rmcp and strip to server-only minimal core | Keeps typed schema macros, drops chrono/other deps | Permanent maintenance burden on a fast-moving v2 crate for a few hundred KB | Not recommended |
| Adopt a lighter MCP crate (e.g. other Rust MCP SDKs) | Might be smaller | Most are also tokio-based; migration touches all 27 tool definitions and `tool_schema.rs` expectations; unverified maturity (not evaluated here) | Not recommended |
| Vendor only the JSON-RPC types from rmcp::model into core, hand-roll the loop | Reuses vetted types | Types come from the same crate, so no dependency win | Not recommended |

## Recommendation

1. Ship the crate split + `heavy` gate + `release-thin` profile (done/in progress); add `panic="abort"` to `release-thin` only if `tool_schema.rs` and the daemon-spawn integration test still pass and the size drops measurably.
2. Install `cargo-bloat`, run `cargo bloat --profile release-thin -p stapler-mcp --crates` to decide whether rmcp/tokio dominate before any rewrite.
3. Defer blocking relay (options C/D) to a separate spike with the differential-test harness; skip build-std and UPX.
4. Measure the pre-split baseline from `main` to check AC4 (>=20%).
