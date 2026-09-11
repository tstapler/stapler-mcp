# ADR-001: Daemon-side credential resolution via a new `CredentialStore` port

**Status**: Accepted
**Date**: 2026-09-08

## Context

`stapler-mcp`'s browser tools need a way to type a credential into a page
without the plaintext value ever appearing in the MCP tool-call transcript.
Two places could hold the standing 1Password access needed to resolve a
`CredentialRef` into a real value: the thin client (spawned fresh per Claude
Code session, stateless, no OS/network access today) or the daemon (already
the sole OS/network-touching process, owns the browser pool for its whole
lifetime).

`architecture.md` §1 traced the wire protocol precisely: `crates/cli/src/thin_client.rs`'s
`call_daemon` forwards a tool's typed input struct as `params` verbatim over
the Unix socket to the daemon. The socket is not a separate, more-private
channel — it carries the identical JSON-RPC payload one hop later. So
"resolve in the thin client instead" does not reduce what crosses the wire;
it only relocates which process holds the 1Password access.

## Decision

The daemon resolves credentials. A new `CredentialStore` port
(`crates/core/src/ports.rs`) is added alongside the existing 8 ports
(`SocketFactory`, `ProcessLock`, `ProcessSpawner`, `EnvPort`, `ClockPort`,
`HttpClient`, `FileStore`, `BrowserDriver`):

```rust
pub trait CredentialStore {
    async fn resolve(&self, credential_ref: &CredentialRef) -> Result<SecretValue, PortError>;
}
```

Two adapters implement it: `NativeCredentialStore` (`crates/native/src/vault.rs`,
shells `op`) and `WasmCredentialStore` (`crates/wasm/src/vault.rs` + `glue/vault.js`,
wraps `@1password/sdk`) — the same Adapter/ports-and-adapters split every
other port already uses. The 1Password service-account token is read once at
daemon startup via the existing `EnvPort`, never stored in thin-client-readable
config.

## Consequences

- No new IPC hop is invented. Composition happens by dependency injection,
  not by a second call site: the daemon constructs each
  `CredentialStore` adapter (`NativeCredentialStore`/`WasmCredentialStore`)
  at startup and injects it into the matching `BrowserDriver` adapter
  (`NativeBrowser`/`WasmBrowser`) before registering any tool. The
  `BrowserDriver::type_secret` implementation then calls its own injected
  `CredentialStore::resolve` internally, as the first step of its own body —
  the tool-layer handler built on top makes exactly one call
  (`browser.type_secret(...)`) and never calls `CredentialStore::resolve`
  itself. This is a single call path by construction, not an incidental
  optimization: it's also what makes ADR-003's in-flight dedup effective,
  since a tool-layer resolve call would bypass the adapter's dedup map
  entirely (see `implementation/plan.md` Phase 1 Task 1.3.1a and Phase 3
  Story 3.4.0/Phase 4 Story 4.3.0).
- The thin client gains no new capability and needs no code changes to
  support this feature beyond the new tool's schema types.
- `crates/core` stays `#![no OS calls]` — the trait is a pure interface; `op`
  process-spawning and the 1Password Node SDK both live in the adapter
  crates.
- This does **not**, by itself, close the leak this feature exists to fix —
  see `research/architecture.md` §0/§3: resolving server-side still requires
  structural `AxSnapshot` redaction, tracked separately in this plan's Phase
  2. This ADR covers only where resolution happens, not the response-side
  leak.

## Alternatives Considered

- **Thin-client resolves.** Rejected: the wire-protocol analysis above shows
  it buys no privacy the daemon-side approach lacks, and it would require
  giving a stateless, per-session process standing 1Password access — a
  wider blast radius (every Claude Code session process, not just the one
  daemon) for no benefit.
- **Reuse `BrowserDriver::type_text` with an `is_secret: bool` flag** instead
  of a new port + new method. Rejected: `type_text`'s signature already takes
  a plaintext `text: &str`, so a flag doesn't stop a caller from passing the
  secret through the unflagged path, and it does nothing about the leaky
  return type (`AxNode.value`), which is the actual problem.
