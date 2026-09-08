# Implementation Plan: credential-vault

**Feature**: A daemon-side `CredentialStore` port (1Password-backed) and a new
`BrowserDriver::type_secret` method let the calling LLM log into a site —
including TOTP/2FA — via an opaque `CredentialRef` that never carries a
plaintext value in the MCP request, combined with structural `AxSnapshot`
redaction (on both the native and wasm adapters) that closes the independent,
pre-existing leak where any password/OTP-shaped field's value is returned
unredacted by `type_text`, `browser_fill_form`, and `browser_snapshot`.
**Date**: 2026-09-08
**Status**: Ready for implementation
**ADRs**:
- [ADR-001](../decisions/ADR-001-daemon-side-credentialstore-port.md) — daemon-side resolution, `CredentialStore` port shape
- [ADR-002](../decisions/ADR-002-exact-host-domain-matching.md) — exact-host-string domain matching, not eTLD+1
- [ADR-003](../decisions/ADR-003-no-persistent-cache-in-flight-dedup-only.md) — no persistent secret cache; in-flight dedup only

---

## Step 0.5 — Alternatives explored before committing

Three shapes were considered for the whole feature, not just its port
signature (`type_secret`'s own alternatives are covered by ADR-001 and the
Pattern Decisions table below):

1. **Daemon resolves + structural per-adapter redaction (chosen).** Strength:
   closes both the request-side leak (opaque `CredentialRef` over the wire)
   and the response-side leak (`AxSnapshot.value`) that `architecture.md` §0
   proved exists independently of where resolution happens. Weakness: real
   work on two independent adapters (native `op` CLI, wasm `@1password/sdk`),
   roughly doubling the surface area of every story in Phases 3–4.
2. **Thin-client resolves, daemon just types whatever it's handed.**
   Strength: keeps the already-large daemon surface smaller. Weakness:
   `architecture.md` §1 proves this doesn't reduce exposure at all — the
   thin-client↔daemon socket carries the same JSON-RPC bytes one hop later —
   and it requires inventing OS/network access for a process that has none
   today. Rejected (see ADR-001).
3. **Text-matching redaction (Playwright MCP's `--secrets` model): scan
   snapshot text for known secret substrings and replace matches.**
   Strength: no DOM/AX-tree type-attribute work needed, works today. Weakness:
   `research/ux.md` §1 documents Playwright's own maintainers calling this
   "a convenience, not a guaranteed security boundary" — it misses
   base64/concatenated/split secrets and cannot protect a *browser-autofilled*
   password before `type_secret` is ever called (`features.md` §2.2), which
   is exactly the pre-existing leak this project also needs to close.
   Rejected in favor of structural (AX-tree-attribute-keyed) redaction.

Approach 1 was chosen. Approaches 2 and 3 are recorded in the Pattern
Decisions table below with their rejection reasons.

---

## Domain Glossary

| Term | Definition | Notes |
|------|-----------|-------|
| `CredentialRef` | `{domain: String, field: CredentialField}` — opaque locator for a vault entry; never carries a secret value. | `crates/core/src/ports.rs`. Matched against the session's live URL by exact host-string equality (ADR-002). |
| `CredentialField` | Closed **port-level** enum, `Username \| Password \| Totp`, no `serde`/`schemars` derives — mirrors `HistoryAction`'s plain-enum shape exactly (same file, same convention). | `crates/core/src/ports.rs`. Converted from the wire-level `CredentialFieldInput` (`schema.rs`) via an exhaustive match in `tools/credential.rs`, mirroring the existing `BrowserHistoryAction → HistoryAction` conversion (`crates/core/src/tools/browser.rs:764-766`) — never a stringly-typed round-trip through the port boundary. |
| `CredentialFieldInput` | Closed **wire-level** enum, `#[serde(rename_all = "snake_case")]`: renders as a 3-value JSON Schema string enum (`"username"\|"password"\|"totp"`). | `crates/core/src/schema.rs`. Prevents an LLM inventing plausible-but-wrong field names (`"login"`, `"otp"`); exists specifically because `CredentialField` (the port enum) carries no `serde`/`schemars` derives, matching `BrowserHistoryAction`/`HistoryAction`'s wire/port split. |
| `SecretValue` | Newtype wrapping `zeroize::Zeroizing<String>`; hand-rolled `Debug` that always prints `"SecretValue(\"[REDACTED]\")"`, never the contents. | `crates/core/src/ports.rs`. Only `CredentialStore::resolve` produces one; only `BrowserDriver::type_secret` consumes one. |
| `CredentialStore` | New core port trait: `async fn resolve(&self, credential_ref: &CredentialRef) -> Result<SecretValue, PortError>`. | Ninth port, alongside the existing 8 in `ports.rs`. See ADR-001. Called ONLY by the `BrowserDriver` adapter's own `type_secret` implementation, via a `CredentialStore` handle injected at daemon-startup construction time (Story 3.4.0/4.3.0) — never by the tool layer (`tools/credential.rs`) directly. |
| `type_secret` | New `BrowserDriver` method: resolves `credential_ref` internally (via the adapter's own injected `CredentialStore`) and types the result into a locator, returns a redacted `AxSnapshot`, never accepts or logs plaintext. | Same call shape as `click`/`type_text` for ergonomic parity (per requirements' Alternatives Considered). The tool layer calls this exactly once per request and never calls `CredentialStore::resolve` separately — see Task 1.3.1a. |
| `REDACTED_PLACEHOLDER` | `pub const REDACTED_PLACEHOLDER: &str = "[REDACTED]";` — the one fixed, non-length-preserving sentinel substituted for every redacted `AxNode.value`. | `crates/core/src/ports.rs`. Per `research/ux.md`'s Open Questions recommendation: fixed string, never length/shape-preserving. |
| Redaction key | The structural trigger for substituting `REDACTED_PLACEHOLDER`: DOM `type == "password"` **OR** `autocomplete` in `{one-time-code, current-password, new-password}`. | `architecture.md` §6 — the single most consequential research finding: a `type=password`-only key leaks every TOTP code. |
| Fail-safe redaction invariant | When the type/autocomplete probe cannot resolve (e.g. a closed shadow root `DOM.describeNode` can't pierce), the node **is redacted**, never left in the clear. | `pitfalls.md` §3c — redact-when-uncertain, never redact-only-when-confirmed. |
| Own-node redaction | `type_secret`'s own acted-on node is *always* redacted in that call's own returned snapshot, independent of whatever the structural redaction key decides. | Belt-and-suspenders invariant, `architecture.md` §6 point 1. |
| `same_host` | Shared exact-host-string-equality helper, `crates/core/src/tools/webcrawl.rs`. Reused (not reimplemented) by the credential domain-match guard. | See ADR-002. |
| `spawn_and_capture` | New `ProcessSpawner` method returning captured stdout/stderr/exit status — distinct from `spawn_daemon`'s fire-and-forget, no-capture shape. | `crates/core/src/ports.rs`. Corrects the research's original (wrong) assumption that `spawn_daemon` could be reused as-is. |
| `NativeCredentialStore` | Native `CredentialStore` adapter; shells `op` via `spawn_and_capture`. | `crates/native/src/vault.rs` (new file). |
| `WasmCredentialStore` | Wasm `CredentialStore` adapter; wraps `@1password/sdk` via wasm-bindgen glue. | `crates/wasm/src/vault.rs` (new file) + `crates/wasm/src/glue/vault.js` (new file). |
| Vault/item ID cache | In-memory `HashMap<domain, (vault_id, item_id)>`, per-daemon-lifetime, populated on first successful resolve per domain. Identifiers only — never a `SecretValue`. | `NativeCredentialStore`. See ADR-003. Avoids `op`'s 3x-request cost for name-based lookups (`stack.md`). |
| In-flight dedup | Concurrent `resolve()` calls for the identical `CredentialRef` share one underlying `op`/SDK call; nothing persists once every waiter is served. | Both adapters — native: Story 3.2.5 (`futures::future::Shared`, `crates/native/src/vault.rs`); wasm: Story 4.2.2 (a `Map` of in-flight `Promise`s, `crates/wasm/src/glue/vault.js`). See ADR-003 — the chosen rate-limit mitigation. |
| `PortError::CredentialDomainMismatch` | The requested domain doesn't match the session's live page host. Renders `"credential rejected: ..."`. | Not retryable by the LLM as-is. |
| `PortError::CredentialAmbiguous` | Multiple vault items matched the domain; candidate titles are named, nothing is typed. Renders `"credential rejected: ..."`. | Not retryable with the same `CredentialRef` shape — requires human disambiguation (`architecture.md` §7). |
| `PortError::CredentialUnauthenticated` | The vault backend itself isn't authenticated/reachable. Renders `"vault unauthenticated: ..."`. | Operator-fixable, not LLM-retryable. |
| `PortError::CredentialRateLimited` | 1Password's account-wide rate limit was hit. Renders `"vault rate-limited: retry after Ns — ..."` (or an unknown-delay variant). | The only *environmental* retryable case — retry after a delay, not immediately. |
| `PortError::CredentialExpired` | The resolved TOTP code's ~30s window elapsed before it could be typed. Renders `"credential expired: ..."`. | The one immediately-LLM-retryable case — distinct prefix family so an LLM can pattern-match retryability from the string alone (`ux.md` §4). |
| Dispatch-time type re-check | `type_secret` refuses to type (returns an error, types nothing) if the resolved node's *live* DOM type/autocomplete isn't password/TOTP-shaped at the moment of dispatch — independent of what the caller's locator claimed. | `features.md` §2.5 policy (a), chosen over permit-but-redact. See Pattern Decisions. |
| `CredentialRefInput` / `BrowserTypeSecretInput` | Wire-facing `schema.rs` structs for the new tool's JSON-RPC input. | `crates/core/src/schema.rs`. |
| `stapler_browser_type_secret` | The new MCP-exposed tool name. | Registered in `crates/cli/src/main.rs`, described in `crates/cli/src/thin_client.rs`. |

---

## Pattern Decisions

| Component | Pattern Chosen | Source | Alternative Rejected | Reason |
|-----------|---------------|--------|---------------------|--------|
| Secret resolution locus | Daemon-side resolution | `architecture.md` §1 | Thin-client-side resolution | Socket carries identical bytes one hop later — no privacy gained; thin client has no OS/network access today. See ADR-001. |
| `CredentialStore` adapter split | Adapter (GoF) / Ports-and-Adapters, mirrors the existing `BrowserDriver` native/wasm split | `architecture.md` §4, `ports.rs`'s existing 8-port pattern | Single `cfg(target_arch)`-guarded implementation | Native (process spawn) and wasm (Node SDK) touch fundamentally different OS surfaces — no shared implementation is possible, matching every other port in this codebase. |
| `type_secret` port shape | New distinct method, `Result<AxSnapshot, PortError>`, adapter-enforced redaction on the returned node | `architecture.md` §2 option 1; `requirements.md` Alternatives Considered | (a) bool/enum flag on `type_text`; (b) `Result<(), PortError>`, no snapshot | (a) doesn't change the leaky return type and still forces a plaintext `text: &str` param on the same method; (b) breaks `click`/`type_text` ergonomic parity and doesn't remove the need for structural redaction on every *other* snapshot-producing call anyway. |
| Snapshot redaction locus | Structural interceptor at the single `AxSnapshot`-building choke point per adapter (`build_tree` / `captureSnapshot`) | `architecture.md` §3/§6; `features.md` §2.2 | Call-scoped redaction only inside `type_secret`'s own return; text-matching redaction (Playwright MCP model) | Call-scoped: the very next `browser_snapshot` (or an autofilled field) re-exposes the value — confirmed pre-existing leak, independent of credential-vault. Text-matching: `ux.md` §1 documents its own maintainers calling it "a convenience, not a guaranteed security boundary" — misses transformed secrets and can't catch a field `type_secret` was never called on. |
| Redaction key | `type == "password"` OR `autocomplete` in `{one-time-code, current-password, new-password}`, fail-safe on unresolvable | `architecture.md` §6; `pitfalls.md` §3c | `type == "password"` only | TOTP fields are conventionally `type="text"` + `autocomplete="one-time-code"` — a type-only key leaks every TOTP code typed via `type_secret`, the single most consequential finding of the whole research phase. |
| Domain-matching algorithm | Exact host-string equality, sourced from the session's live navigation URL, via the shared `same_host` helper | `architecture.md` §7 | eTLD+1/public-suffix-aware matching (`psl`/`publicsuffix`, per `build-vs-buy.md`'s blanket recommendation) | `build-vs-buy.md`'s recommendation is generic, not grounded in this codebase's specific `same_host` precedent or its confidential/regulated NFR classification. See ADR-002 for the full resolution of this tension. |
| Credential value objects | `CredentialRef`, `SecretValue` as newtypes / Value Objects (DDD) | Existing `SessionId`/`Locator` newtype convention already in `ports.rs` | Plain `String` parameters for domain/field/secret | Primitive obsession — a bare `String` secret parameter is indistinguishable from any other string at a call site and invites an accidental `format!()`/log. |
| Vault/secret caching stance | No `SecretValue` cache (always fresh); vault/item **ID** cache + in-flight dedup only | `pitfalls.md` §2a | (i) fully stateless, zero dedup; (ii) short-TTL value cache | (i) leaves the documented account-wide rate-limit crash-loop risk unaddressed for concurrent multi-tab resolution of the same credential; (ii) increases TOTP-staleness risk and deviates further from requirements' "resolved fresh every call." See ADR-003. |
| Dispatch-time field-type policy | Refuse (type nothing) if the live DOM type/autocomplete isn't password/TOTP-shaped at dispatch time | `features.md` §2.5 policy (a) | Permit-but-redact regardless of live DOM type | Matches the "never silent, no plaintext fallback" ethos already used elsewhere in Risk Control; refusing at dispatch is the safer default when a caller's locator targets something unexpected, and doesn't weaken the structural redaction safety net for every other path. |
| Domain discovery | Attempt-and-reject only (no `list_domains()` tool) for this pass | `features.md` §3.1 | Add a `list_domains()`-style discovery tool | Appetite (Large, 3–6 weeks) is already stretched across two adapters + TOTP + structural redaction; the ambiguous-match error's candidate list partially covers the "what does the vault have" need without new tool surface. |

---

## Tech Debt Disposition

| Area | Existing Issue | Disposition | Justification |
|------|----------------|--------------|----------------|
| `ProcessSpawner` capture capability | None identified — the ProcessSpawner gap is new-code scope (adding a `spawn_and_capture` method), not an existing violation to formalize a disposition for. | N/A | `research/architecture.md`'s re-verification note and `research/pitfalls.md` §1a independently confirm `spawn_daemon` (`crates/core/src/ports.rs:98-103`) was never capable of returning captured output — it's a fire-and-forget detach launcher. The original research's "reuse `ProcessSpawner`" framing was simply incorrect, not a pre-existing SOLID/DDD violation this plan needs to formalize a disposition for. Phase 3, Epic 3.1 adds the missing method as new-code scope. |

---

## Migration Plan

N/A — confirmed explicitly, not omitted silently. This feature adds a new
port, a new `BrowserDriver` method, two new adapter modules, and new schema
types; it touches no on-disk schema, no database, and no existing tool's
wire shape (`AxSnapshot`'s JSON shape is unchanged — redaction only changes
the *value* of an already-optional `value` field, never its presence/type).
`requirements.md`'s Risk Control section states "no schema migrations" — this
plan does not deviate from that.

## Observability Plan

- **Logs**: One `eprintln!` line per `CredentialStore::resolve` attempt,
  mirroring `webcrawl.rs`'s existing SSRF-deny-log convention
  (`crates/core/src/tools/webcrawl.rs:266`) — same stderr channel (stdout
  carries the MCP JSON-RPC stream and can't be used). The line always
  contains, in plain text (never behind an opaque ref requiring a lookup):
  the `domain`, the `field`, and one of `resolved` / `rejected-domain-mismatch`
  / `rejected-ambiguous` / `vault-lookup-failed` / `rate-limited` /
  `totp-expired`. Never the resolved value — `SecretValue`'s hand-rolled
  `Debug` makes this a type-level guarantee, not just a convention (Task
  1.1.1c's test locks this down). A rejection is logged identically to a
  success — per `ux.md` §3, both are equally audit-worthy.
- **Metrics**: None — per `requirements.md`'s NFR section, standard request
  logging is sufficient for a single-user local daemon; no new
  metrics/alerting infrastructure is in scope.
- **Alerts**: None — same reasoning; this is a `journalctl`/log-tail-scale
  audit surface, not a monitored service.

## Risk Control

- **Feature flag**: Infrastructure-level opt-in, not a config toggle.
  `CredentialStore` is only constructed at daemon startup if
  `OP_SERVICE_ACCOUNT_TOKEN` is present via `EnvPort`
  (`crates/cli/src/main.rs`, Phase 6). Absent the token,
  `stapler_browser_type_secret` is registered with a handler that always
  returns `"vault not configured: set OP_SERVICE_ACCOUNT_TOKEN in the
  daemon's environment and restart"` — it never constructs a
  `CredentialStore` or touches `BrowserDriver` at all.
- **Rollback procedure**: Revert the port/adapter/tool changes. No
  persistent state to unwind — the daemon holds no credentials at rest,
  and the only in-memory state added (the vault/item ID cache, ADR-003) dies
  with the daemon process. Structural redaction (Phase 2) is a strict safety
  improvement to existing tools' output and is safe to leave in place even
  if the rest of the feature is rolled back — no caller should be relying on
  the pre-existing plaintext-password-in-snapshot leak.
- **Staged rollout**: Native adapter (Phase 3) ships and is verified before
  wasm adapter work (Phase 4) begins, per `pitfalls.md` §4c's explicit
  sequencing recommendation — the wasm adapter is the higher-uncertainty
  stretch goal (`@1password/sdk` nested inside this project's own
  wasm-bindgen glue is unproven). Phase 4 opens with an explicit 30-minute
  spike (Task 4.1.1) with a go/no-go checkpoint before further wasm work
  proceeds.

## Unresolved Questions

- [ ] Whether `op item list --categories Login` (native) reliably exposes
      enough per-item URL data to filter by domain client-side at the scale
      of a real personal vault, or whether pagination/rate-cost makes the
      list-then-filter approach (Task 3.3.1a) impractical — `architecture.md`
      §7 flags this as UNVERIFIED. Blocks Story 3.3.1 — owner: implementer,
      spike against a real (or sufessufficiently large test) vault before
      writing the final filtering logic.
- [ ] Whether `@1password/sdk` actually instantiates and resolves inside this
      project's real wasm/Node host without wasm-in-wasm-glue nesting issues
      — `pitfalls.md` §4a/4b, UNVERIFIED. Blocks Phase 4 entirely — owner:
      implementer, resolved by Task 4.1.1's spike before any further Phase 4
      work proceeds.
- [ ] Whether `op service-account ratelimit` (or an equivalent diagnostic)
      reliably yields a retry-after duration in practice, or whether
      `CredentialRateLimited`'s message must fall back to an unknown-delay
      phrasing most of the time — `pitfalls.md` §2a. Blocks Task 3.2.4b —
      owner: implementer, verify against the real CLI during that task.

## Dependency Visualization

```
Phase 1: Core port & domain types (ports.rs, schema.rs, webcrawl.rs helper)
  |
  |-------------------------------------------------------------.
  v                                                              v
Phase 2: Structural redaction                          Phase 3: Native CredentialStore
  (native ax.rs + wasm browser.js)                        adapter (op CLI) + native
  -- independent of CredentialStore existing --           type_secret dispatch
  |                                                              |
  |                                                              v
  |                                                  Phase 4: Wasm CredentialStore
  |                                                    adapter (@1password/sdk)
  |                                                    -- sequenced AFTER Phase 3 --
  |                                                              |
  '------------------------.        .----------------------------'
                            v        v
                    Phase 5: Tool-layer wiring + schema + observability
                            |
                            v
                    Phase 6: Daemon wiring / opt-in feature flag
                            |
                            v
                    Phase 7: Cross-cutting acceptance & verification
                    (requires Phase 2 AND Phase 6 both complete)
```

Phase 2 (redaction) has no dependency on `CredentialStore` existing at all —
it fixes a leak that's present today regardless of this feature — so it can
proceed in parallel with Phase 3. Phase 4 is strictly sequenced after Phase 3
per `pitfalls.md` §4c. Phase 7's acceptance tests need both the redaction
guarantee (Phase 2) and the wired-up tool (Phase 6) to exist.

---

## Phase 1: Core Port & Domain Types

### Epic 1.1: `CredentialRef` / `SecretValue` / `CredentialStore` port

**Goal**: Establish the vault-facing types and trait in `crates/core`,
`#![no OS calls]`, with a type-enforced non-leaking `Debug`.

#### Story 1.1.1: Define `CredentialRef` and `SecretValue` newtypes
**As a** daemon developer, **I want** `CredentialRef` and `SecretValue` as
distinct types rather than bare `String`s, **so that** a secret value can
never be accidentally formatted into a log line or error message.
**Acceptance Criteria**:
- `SecretValue`'s `Debug` impl never prints the wrapped value, regardless of
  content.
  - *Given* `SecretValue::new("hunter2".to_string())`, *When* the value is
    formatted with `format!("{:?}", secret)`, *Then* the result is exactly
    `"SecretValue(\"[REDACTED]\")"` and does not contain the substring
    `"hunter2"`.
- `SecretValue`'s backing memory is zeroed on drop.
  - *Given* a `SecretValue` wrapping `zeroize::Zeroizing<String>`, *When* the
    value is dropped, *Then* `Zeroizing`'s own `Drop` impl (already exercised
    by `zeroize`'s own test suite) overwrites the backing buffer — verified
    here only by confirming the field type is `Zeroizing<String>`, not by
    re-testing `zeroize` itself.
**Files**: `crates/core/src/ports.rs`, `crates/core/Cargo.toml`

##### Task 1.1.1a: Add `zeroize` as a direct dependency (~2 min)
- Add `zeroize = "1"` to `crates/core/Cargo.toml`'s `[dependencies]` — it's
  already a transitive dependency (`Cargo.lock` pins `zeroize 1.9.0`), so
  this only promotes it to direct, no new supply-chain surface.
- Files: `crates/core/Cargo.toml`

##### Task 1.1.1b: Define `CredentialField` enum and `CredentialRef` struct (~4 min)
- Add to `crates/core/src/ports.rs`, near `HistoryAction` (same file, same
  convention — a closed, compile-time-exhaustive vocabulary with no
  `serde`/`schemars` derives; wire-schema concerns stay at the schema/wire
  layer, Task 5.1.1a):
  ```rust
  /// The specific vault field a `CredentialRef` names. Closed by
  /// construction — exactly 3 legal values, exactly like `HistoryAction` —
  /// so a typo or unexpected value fails to compile rather than silently
  /// misresolving (e.g. falling through to the password-shaped path).
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub enum CredentialField {
      Username,
      Password,
      Totp,
  }
  ```
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq, Hash)]
  pub struct CredentialRef {
      pub domain: String,
      pub field: CredentialField,
  }
  ```
  Both derive `Hash` (in addition to `Eq`) specifically so `CredentialRef`
  can key a `HashMap` for in-flight dedup (Story 3.2.5/4.2.2, ADR-003 item
  3) — not needed by any other consumer, but cheap and harmless to add here
  rather than as a later breaking change.
  (`domain` stays a plain `String` — it's a genuinely open-ended opaque
  identifier, matching `Locator`/`SessionId`'s shape; `field` is NOT
  open-ended — it has exactly 3 legal values known at compile time, so it
  gets the same closed-enum treatment `HistoryAction`/`TabAction`/
  `WaitCondition` already get at this exact port boundary, not the
  `Locator`/`SessionId` precedent.)
- Doc comment on `CredentialRef`: opaque, non-secret domain/field
  identifier; `domain` matched by exact host-string equality against the
  session's live URL (cross-ref `same_host`); never carries a credential
  value.
- Files: `crates/core/src/ports.rs`

##### Task 1.1.1c: Define `SecretValue` + `REDACTED_PLACEHOLDER` + Debug test (~5 min)
- Add `SecretValue(zeroize::Zeroizing<String>)` with `pub fn new(value:
  String) -> Self` and `pub fn expose(&self) -> &str`.
- Hand-roll `impl std::fmt::Debug for SecretValue` per the AC above.
- Add `pub const REDACTED_PLACEHOLDER: &str = "[REDACTED]";` near `AxNode`.
- Add the unit test from this story's first AC to `ports.rs`'s existing
  `#[cfg(test)] mod tests`.
- Files: `crates/core/src/ports.rs`

### Epic 1.2: `CredentialStore` trait + vault `PortError` variants

**Goal**: The trait itself, plus the 5 distinct error variants `ux.md`/`pitfalls.md`
require for LLM-pattern-matchable retryability.

#### Story 1.2.1: Add `CredentialStore` trait and vault error variants
**As a** daemon developer, **I want** `resolve()` to return one of 5 distinct,
named failure shapes instead of a generic error, **so that** the calling LLM
(and the tool layer built on top) can tell a retryable TOTP-expiry from a
non-retryable domain mismatch without parsing prose.
**Acceptance Criteria**:
- Each of the 5 new `PortError` variants renders with its documented prefix.
  - *Given* `PortError::CredentialExpired("TOTP code for example.com expired
    before it could be typed".to_string())`, *When* formatted with `{err}`,
    *Then* the result starts with `"credential expired: "`.
  - *Given* `PortError::CredentialDomainMismatch("no vault entry for domain
    \"example.com\"".to_string())`, *When* formatted with `{err}`, *Then* the
    result starts with `"credential rejected: "`.
**Files**: `crates/core/src/ports.rs`

##### Task 1.2.1a: Add `CredentialStore` trait (~2 min)
- ```rust
  pub trait CredentialStore {
      async fn resolve(&self, credential_ref: &CredentialRef) -> Result<SecretValue, PortError>;
  }
  ```
- Doc comment cross-references `SecretValue`'s non-leaking `Debug` and states
  every implementation must log its own domain/field/outcome (Observability
  Plan) but must never log the resolved value.
- Files: `crates/core/src/ports.rs`

##### Task 1.2.1b: Add 5 new `PortError` variants + `Display` arms (~5 min)
- Add to the `PortError` enum, each `(String)`-payload like `NotFound`, with
  a doc comment stating the caller's fix (matching the existing
  `NotFound`/`SessionCrashed`/`NotActionable` convention):
  ```rust
  /// The requested domain doesn't match the session's current live page
  /// host (see `same_host`) — nothing was typed. Not fixable by retrying
  /// the same call; the caller must re-check the actual current-page
  /// domain or accept there's no credential for this site.
  CredentialDomainMismatch(String),
  /// Multiple vault items matched the domain; nothing was typed. Not
  /// fixable with `CredentialRef`'s shape alone — requires human
  /// disambiguation (see the candidate list in the message).
  CredentialAmbiguous(String),
  /// The vault backend itself isn't authenticated/reachable. An operator
  /// problem, not something the calling LLM can fix by retrying.
  CredentialUnauthenticated(String),
  /// 1Password's account-wide rate limit was hit. Retryable, but only
  /// after the message's stated delay — not immediately.
  CredentialRateLimited(String),
  /// The resolved TOTP code's ~30s validity window elapsed before it
  /// could be typed. The one immediately-retryable case in this family.
  CredentialExpired(String),
  ```
- Add matching `Display` arms: `CredentialDomainMismatch`/`CredentialAmbiguous`
  render `"credential rejected: {e}"`; `CredentialUnauthenticated` renders
  `"vault unauthenticated: {e}"`; `CredentialRateLimited` renders `"vault
  rate-limited: {e}"`; `CredentialExpired` renders `"credential expired: {e}"`.
- Note in a doc comment (mirroring `browser.rs`'s existing map_error
  convention comment): callers that already know the specific variant should
  pass the inner `String` through verbatim rather than via `Display`, exactly
  like `NotFound`/`SessionCrashed` today — the adapter constructing the
  variant is expected to have already built the full user-facing sentence
  (matching `ux.md`'s exact drafted strings) into the inner `String`.
- Files: `crates/core/src/ports.rs`

##### Task 1.2.1c: Unit tests for the 5 `Display` prefixes (~3 min)
- One test per variant, asserting `format!("{err}")` starts with the
  documented prefix (mirrors the existing
  `port_error_not_found_display_should_include_message_when_formatted`
  test's shape).
- Files: `crates/core/src/ports.rs`

### Epic 1.3: `BrowserDriver::type_secret` port method

**Goal**: The trait method itself, plus the compile-check/fake-driver
scaffolding every other `BrowserDriver` method already has.

#### Story 1.3.1: Add `type_secret` to `BrowserDriver`
**As a** daemon developer, **I want** `type_secret` to be a distinct trait
method with the same call shape as `click`/`type_text`, **so that** the tool
layer built on top gets `AxSnapshot` ergonomic parity with every other
mutating action.
**Acceptance Criteria**:
- The trait compiles with a `todo!()`-bodied stub implementing every method
  including `type_secret`.
  - *Given* `StubBrowser` (the existing test double in `ports.rs`'s test
    module) extended with a `type_secret` arm, *When* the crate is compiled,
    *Then* compilation succeeds — this is the existing
    `stub_browser_driver_should_compile_when_all_methods_have_todo_bodies`
    test, re-run with the new method present.
**Files**: `crates/core/src/ports.rs`

##### Task 1.3.1a: Add the trait method (~3 min)
- ```rust
  /// Resolves `credential_ref` and types the result into `locator`, exactly
  /// like `type_text` except: (1) no plaintext ever crosses this *trait*
  /// call boundary in either direction — the implementor resolves
  /// internally, it is never handed a resolved `SecretValue` as an
  /// argument — and (2) the acted-on node's `value` in the returned
  /// `AxSnapshot` is always `REDACTED_PLACEHOLDER`, independent of the
  /// structural redaction every snapshot-producing call already applies
  /// (see `ax.rs`/`browser.js`'s redaction pass) — belt-and-suspenders per
  /// `architecture.md` §6.
  ///
  /// **Resolution locus, explicit**: the implementing adapter (native/wasm)
  /// resolves `credential_ref` by calling its OWN adapter-owned
  /// `CredentialStore` handle internally, as the first step of this method's
  /// own body — it is not resolved by the tool-layer caller beforehand and
  /// handed in. The daemon is responsible for constructing/injecting that
  /// `CredentialStore` dependency into the `BrowserDriver` adapter at
  /// startup (Phase 3/4 wiring, Phase 6 daemon wiring) — see ADR-001. The
  /// tool-layer handler built on top of this trait (Phase 5) calls this
  /// method exactly once per `type_secret` request and never calls
  /// `CredentialStore::resolve` itself; this is also what makes ADR-003's
  /// in-flight dedup (Story 3.2.5/4.2.2) actually effective — every resolve
  /// request passes through the one adapter-owned `CredentialStore`, so its
  /// dedup map sees all of them, not just some.
  async fn type_secret(
      &self,
      session_id: &SessionId,
      locator: &Locator,
      credential_ref: &CredentialRef,
      timeout: Duration,
  ) -> Result<AxSnapshot, PortError>;
  ```
- Files: `crates/core/src/ports.rs`

##### Task 1.3.1b: Extend `StubBrowser` with a `todo!()` arm (~2 min)
- Add the matching `async fn type_secret(...) -> Result<AxSnapshot, PortError>
  { todo!() }` arm to the existing `StubBrowser` impl in `ports.rs`'s test
  module.
- Files: `crates/core/src/ports.rs`

##### Task 1.3.1c: Extend `FakeBrowserDriver` in `tools/browser.rs`'s tests (~4 min)
- Add a `type_secret_result: RefCell<Option<Result<AxSnapshot, PortError>>>`
  field, `with_type_secret(...)` builder, and the trait impl arm to
  `FakeBrowserDriver` (mirrors every other `with_*`/impl arm already there),
  so Phase 5's tool-layer tests have a working fake to call against.
- Files: `crates/core/src/tools/browser.rs`

### Epic 1.4: Shared domain-matching helper

**Goal**: One `same_host`-shaped comparison, called by both the SSRF guard
and the new credential domain guard — per ADR-002, never two independent
implementations.

#### Story 1.4.1: Make `same_host` reusable outside `webcrawl.rs`
**As a** daemon developer, **I want** the credential-domain guard to call the
exact same host-comparison function the SSRF guard already uses, **so that**
a future "helpful" loosening of one can't silently diverge from the other.
**Acceptance Criteria**:
- The SSRF guard's existing behavior is unchanged after the visibility
  change.
  - *Given* the existing `should_block_private_ipv4_ranges_when_enforcing`
    test suite in `webcrawl.rs`, *When* run after this story's change,
    *Then* every existing test in that module still passes unmodified.
- The helper is callable from `crates/core/src/tools/credential.rs` (Phase 5).
  - *Given* two `url::Url` values `https://example.com/login` and
    `https://example.com/`, *When* `webcrawl::same_host(&a, &b)` is called
    from outside the `webcrawl` module, *Then* it compiles and returns `true`.
**Files**: `crates/core/src/tools/webcrawl.rs`

##### Task 1.4.1a: Change `same_host`'s visibility to `pub(crate)` (~2 min)
- Change `fn same_host` to `pub(crate) fn same_host`, and extend its doc
  comment to note it's now a dual-purpose helper: the SSRF guard's own-host
  crawl-boundary check, and (per ADR-002) the credential-domain guard's exact
  host-equality check — both intentionally use the identical function.
- Files: `crates/core/src/tools/webcrawl.rs`

##### Task 1.4.1b: Add a cross-module reuse test (~3 min)
- Add a test in `webcrawl.rs`'s existing test module asserting `same_host`
  handles the IPv6-bracket case correctly (`Url::host_str()` brackets IPv6
  literals — the exact bug class `webcrawl.rs:248-250`'s comment already
  documents), so the credential-domain guard inherits that fix for free
  rather than needing its own equivalent bug found later.
- Files: `crates/core/src/tools/webcrawl.rs`

---

## Phase 2: Structural Snapshot Redaction

This phase has no dependency on `CredentialStore` existing — it fixes a
pre-existing, credential-vault-independent leak (`architecture.md` §3) — and
can proceed in parallel with Phase 3.

### Epic 2.1: Redaction key documentation (core)

**Goal**: One documented, shared definition of the redaction key both
adapters implement independently (each adapter still needs its own
mechanism — CDP vs. Playwright's `ariaSnapshot()` — but the *rule* must be
identical, matching the "same wire shape on both adapters" success metric).

#### Story 2.1.1: Document the redaction key and fail-safe invariant on `AxNode`
**As a** future contributor implementing or reviewing either adapter's
redaction pass, **I want** the exact key and its fail-safe rule written once,
in the platform-agnostic type both adapters produce, **so that** the native
and wasm implementations can't silently drift.
**Acceptance Criteria**:
- `AxNode.value`'s doc comment states the redaction key and the fail-safe
  invariant.
  - *Given* `crates/core/src/ports.rs`'s `AxNode` struct after this task,
    *When* a reviewer reads its doc comment, *Then* it states: "a value is
    replaced with `REDACTED_PLACEHOLDER` when the underlying DOM node's
    `type` is `password`, or its `autocomplete` is one of `one-time-code`,
    `current-password`, `new-password` — and, when that determination can't
    be made with confidence (e.g. inside a closed shadow root), the node is
    redacted anyway, never left in the clear."
**Files**: `crates/core/src/ports.rs`

##### Task 2.1.1a: Extend `AxNode.value`'s doc comment (~3 min)
- Files: `crates/core/src/ports.rs`

### Epic 2.2: Native redaction (`crates/native/src/ax.rs`)

**Goal**: `build_tree`/`walk_children` substitute `REDACTED_PLACEHOLDER` for
any node matching the redaction key, sourced from one combined CDP probe per
node (not two separate calls, per `pitfalls.md` §3a's atomicity concern),
with `pierce: true` so shadow-DOM content is covered, and fail-safe on any
resolution failure.

#### Story 2.2.1: Fetch DOM type/autocomplete alongside AX capture, redact matches
**As** the native adapter, **I want** to know each surviving node's DOM
`type`/`autocomplete` at the same time I already have its AX value, **so
that** I can redact before the value ever leaves `build_tree`.
**Acceptance Criteria**:
- A node with DOM `type="password"` has its `value` replaced.
  - *Given* a `RawAxNode` whose backend node resolves to `<input
    type="password" value="hunter2">`, *When* `build_tree` runs, *Then* the
    corresponding `AxNode.value` is `Some("[REDACTED]".to_string())`, not
    `Some("hunter2".to_string())`.
- A node with `autocomplete="one-time-code"` (not `type="password"`) has its
  value replaced too.
  - *Given* a `RawAxNode` whose backend node resolves to `<input type="text"
    autocomplete="one-time-code" value="482913">`, *When* `build_tree` runs,
    *Then* the corresponding `AxNode.value` is `Some("[REDACTED]".to_string())`.
- A node matching neither key is unaffected.
  - *Given* a `RawAxNode` resolving to `<input type="text" value="Jane">`
    with no `autocomplete`, *When* `build_tree` runs, *Then*
    `AxNode.value` is `Some("Jane".to_string())`, unchanged.
**Files**: `crates/native/src/ax.rs`

##### Task 2.2.1a: Add a combined type/autocomplete probe function (~5 min)
- Add `async fn probe_redaction(page: &Page, backend_node_id: BackendNodeId)
  -> Result<bool, ()>` using a single `Runtime.callFunctionOn` eval (mirroring
  `browser.rs`'s existing `invoke_on_node`/`ACTIONABILITY_CHECK_JS` pattern,
  not a new `DescribeNodeParams` round-trip) returning `{redact: bool}`
  computed as `this.type === 'password' || ['one-time-code',
  'current-password', 'new-password'].includes(this.autocomplete)` in one
  JS expression — this is the "single combined eval" `pitfalls.md` §3a
  recommends over two independent CDP calls that can straddle a DOM mutation.
- `Err(())` (any CDP failure — node gone, exception, etc.) is the fail-safe
  signal, handled by the caller (Task 2.2.1b) as "redact."
- Files: `crates/native/src/ax.rs`

##### Task 2.2.1b: Thread the redaction decision through `build_tree`/`walk_children` (~5 min)
- `capture_snapshot`'s caller loop (or `walk_children` itself, taking a
  `&Page` — check which keeps the pure-tree-walk unit tests working; if
  `walk_children` must stay `Page`-free for testability, add a resolution
  pass over `refs` immediately after `walk_children` returns, keyed by the
  same `BackendNodeId` map already built) calls `probe_redaction` for every
  surviving node whose `role` suggests a form control (`textbox`/`searchbox`/
  similar — avoids probing every generic/button node needlessly) and
  substitutes `REDACTED_PLACEHOLDER` for `value` when `probe_redaction`
  returns `Ok(true)` **or** `Err(())` (fail-safe).
- Files: `crates/native/src/ax.rs`

##### Task 2.2.1c: Unit tests for the three ACs above (~5 min)
- Extend `ax.rs`'s existing `#[cfg(test)] mod tests` with fakes/mocks at
  whatever seam Task 2.2.1b introduced (a trait-object or closure standing in
  for `probe_redaction` so these stay pure unit tests, matching this file's
  existing "no live `Page` needed" testing convention).
- Files: `crates/native/src/ax.rs`

#### Story 2.2.2: Shadow-DOM fail-safe coverage
**As** the native adapter, **I want** the type/autocomplete probe to attempt
piercing shadow roots, and to redact when it can't, **so that** a
web-component login form's password field can never leak through the
open/closed shadow-root asymmetry `pitfalls.md` §3c identified.
**Acceptance Criteria**:
- An open-shadow-root password field is redacted via successful piercing.
  - *Given* a test page with `<my-login>` whose open `shadowRoot` contains
    `<input type="password">`, *When* a snapshot is captured, *Then* that
    field's `AxNode.value` is `"[REDACTED]"`.
- A closed-shadow-root password field is redacted via the fail-safe path
  (not via successful piercing, since closed roots are unobservable).
  - *Given* a test page with `<my-login>` whose **closed** `shadowRoot`
    contains `<input type="password">`, *When* a snapshot is captured,
    *Then* that field's `AxNode.value` is `"[REDACTED]"` — verified as the
    fail-safe path specifically by asserting the probe's own result was
    `Err(())`/unresolved for that node, not a false-positive type match.
**Files**: `crates/native/src/ax.rs`

##### Task 2.2.2a: Use `Runtime.callFunctionOn`'s natural piercing, not `DescribeNodeParams` (~3 min)
- Confirm/ensure Task 2.2.1a's eval-based probe (not a `DOM.describeNode`
  call) is what's used — a JS eval bound to `this` naturally sees into an
  *open* shadow root's own DOM the same way any page script would, sidestepping
  `DescribeNodeParams`'s `pierce: false` default entirely (`pitfalls.md` §3c's
  root cause). Add a comment on `probe_redaction` stating this explicitly, so
  a future refactor back to `DescribeNodeParams` doesn't silently reintroduce
  the asymmetry.
- Files: `crates/native/src/ax.rs`

##### Task 2.2.2b: Integration test — open shadow root (~5 min)
- Add a `chromiumoxide`-driven integration test (alongside this crate's
  existing browser integration tests, if any exist under `tests/`, else a
  `#[ignore]`d test in `ax.rs` requiring a real Chrome — check
  `crates/native/tests/` for the existing convention before choosing) loading
  a data URL with an open-shadow-root password field, asserting redaction.
- Files: `crates/native/src/ax.rs` (or `crates/native/tests/`, matching
  whatever convention Task 2.2.2b's investigation finds)

##### Task 2.2.2c: Integration test — closed shadow root, fail-safe path (~5 min)
- Same shape as 2.2.2b but with `{mode: 'closed'}`, asserting redaction still
  occurs (via fail-safe, since closed roots can't be pierced by any means —
  document this explicitly in the test's own comment, since it's the load-
  bearing assertion of this whole story).
- Files: same as 2.2.2b

#### Story 2.2.3: Empirical headless-vs-headed AX masking test
**As** a future maintainer, **I want** a documented, run-once empirical check
of whether headless vs. headed Chrome differ in what `getFullAXTree` reports
for a password field, **so that** `pitfalls.md` §3b's UNVERIFIED flag becomes
a recorded fact rather than an assumption.
**Acceptance Criteria**:
- The comparison is recorded, not just performed transiently.
  - *Given* the same password-field test page loaded once headless and once
    headed, *When* `getFullAXTree`'s raw response is compared for that node,
    *Then* the finding (identical or different) is written as a doc comment
    on `probe_redaction` or in this file's module doc comment, citing the
    Chrome version tested.
**Files**: `crates/native/src/ax.rs`

##### Task 2.2.3a: Run and document the comparison (~5 min)
- A `#[ignore]`d manual test (not part of the default `cargo test` run, since
  it requires launching Chrome twice in two modes) that launches both and
  diffs the relevant AX node's fields, printing the result — plus the
  doc-comment write-up per the AC.
- Files: `crates/native/src/ax.rs`

### Epic 2.3: Wasm redaction (`crates/wasm/src/glue/browser.js`)

**Goal**: `captureSnapshot`'s `page.ariaSnapshot()`-based parse has no `type`
attribute in it at all (`stack.md`) — a second live-DOM pass must be merged
in by `ref`, with the identical redaction key and fail-safe rule as native.

#### Story 2.3.1: Second-pass live DOM query merged into `captureSnapshot`
**As** the wasm adapter, **I want** a `page.evaluate()` pass over every
`input`/`textarea` collecting `{ref, type, autocomplete}`, merged into the
tree `parseAriaSnapshot` already built, **so that** redaction has parity with
native despite Playwright's aria-snapshot text carrying no type information.
**Acceptance Criteria**:
- A `type="password"` field is redacted.
  - *Given* a page with `<input type="password" aria-ref="e3">` (post-`fill`),
    *When* `captureSnapshot` runs, *Then* the resulting tree's node for `e3`
    has no plaintext value — `"[REDACTED]"` in its place.
- An `autocomplete="one-time-code"` field is redacted even though
  `type="text"`.
  - *Given* a page with `<input type="text" autocomplete="one-time-code"
    aria-ref="e5">`, *When* `captureSnapshot` runs, *Then* node `e5`'s value
    is `"[REDACTED]"`.
- A `ref` present in the aria-snapshot tree but not resolvable in the live
  DOM pass (fail-safe) is redacted if it looked like a form control.
  - *Given* a node the aria-snapshot parse assigned `role: "textbox"` but
    which the live-DOM `evaluate()` pass could not find (e.g. removed
    between the two calls), *When* the merge runs, *Then* that node's value
    is redacted rather than left as whatever `ariaSnapshot()` reported.
**Files**: `crates/wasm/src/glue/browser.js`

##### Task 2.3.1a: Add the live-DOM collection pass (~5 min)
- New function `collectRedactionInfo(page)` — `page.evaluate(() =>
  Array.from(document.querySelectorAll('input, textarea')).map(el => ({
  ref: el.getAttribute('aria-ref') || null, redact: el.type === 'password' ||
  ['one-time-code','current-password','new-password'].includes(el.autocomplete)
  })))` — mirrors native's single-combined-check shape (one predicate per
  element, no separate type/autocomplete round trips).
- Files: `crates/wasm/src/glue/browser.js`

##### Task 2.3.1b: Merge into `parseAriaSnapshot`'s tree, fail-safe on unmatched textbox-like nodes (~5 min)
- Update `captureSnapshot` to call `collectRedactionInfo` after
  `parseAriaSnapshot`, walk the tree, and substitute `"[REDACTED]"` for any
  node whose `ref` matches a `redact: true` entry, **and** for any
  `role`-suggests-form-control node (`textbox`, or whatever role
  `ariaSnapshot()` emits for `<input>`) with no matching entry at all in the
  collected list (fail-safe: unresolvable — treat as "can't confirm it's
  safe").
- Files: `crates/wasm/src/glue/browser.js`

##### Task 2.3.1c: Node `--test` unit test for redaction key parity (~5 min)
- Following this file's existing test conventions (exported pure functions
  tested via a Node test harness with mock `page` objects, per the module doc
  comment's "so a Node test harness can drive them directly with mock
  page/browser objects" note) — a test constructing a fake `page.evaluate`
  response and asserting the merge substitutes `"[REDACTED]"` per the key.
- Files: `crates/wasm/src/glue/browser.js` (or a sibling test file, matching
  whatever `crates/wasm`'s existing `.test.js`/Node-test convention is — check
  for an existing `*.test.js` file before creating a new pattern)

#### Story 2.3.2: Shadow-DOM equivalent coverage for wasm
**As** the wasm adapter, **I want** the redaction-info collection pass to
walk open shadow roots, and to fail-safe-redact when it can't (closed roots),
**so that** wasm has the same fail-safe guarantee native does.
**Acceptance Criteria**:
- Open shadow root: password field found and redacted via a recursive walk.
  - *Given* a page with an open-shadow-root `<input type="password">`,
    *When* `collectRedactionInfo` runs, *Then* the recursive
    `element.shadowRoot.querySelectorAll(...)` walk finds it and it's
    redacted.
- Closed shadow root: redacted via fail-safe (documented as an accepted
  parity constraint, not a gap — Playwright/browser JS cannot observe a
  closed shadow root's contents by any means, matching native's identical
  limitation).
  - *Given* a page with a closed-shadow-root `<input type="password">`,
    *When* `collectRedactionInfo` runs (and cannot see inside the closed
    root), *Then* if that node surfaces in `ariaSnapshot()`'s own output at
    all (accessibility trees generally do reflect closed-shadow content even
    when JS can't query it directly — the same asymmetry `pitfalls.md` §3c
    describes for CDP), the merge's fail-safe rule (Task 2.3.1b) redacts it
    for lack of a matching `collectRedactionInfo` entry.
**Files**: `crates/wasm/src/glue/browser.js`

##### Task 2.3.2a: Recursive shadow-root walker in the evaluate pass (~4 min)
- Extend Task 2.3.1a's `page.evaluate()` body with a recursive function that
  also descends into `el.shadowRoot` when present and not `null` (open roots
  only — `el.shadowRoot` is `null` for closed roots by spec, which is exactly
  the fail-safe boundary).
- Files: `crates/wasm/src/glue/browser.js`

##### Task 2.3.2b: Test — closed shadow root triggers fail-safe redaction (~4 min)
- Mirrors 2.3.1c's fake-`page`-object test shape, with a fixture where
  `ariaSnapshot()`'s mocked output includes a textbox-role node with no
  corresponding `collectRedactionInfo` entry; assert it's redacted.
- Files: same as 2.3.1c

---

## Phase 3: Native `CredentialStore` Adapter (`op` CLI)

Sequenced before Phase 4 per `pitfalls.md` §4c (lower uncertainty; no nested
wasm-in-wasm concerns).

### Epic 3.1: `ProcessSpawner` capture capability

**Goal**: `spawn_daemon` cannot capture output (confirmed by two independent
research passes) — add the missing capability as new-code scope, not a
`ProcessSpawner`-reuse claim.

#### Story 3.1.1: `spawn_and_capture` on `ProcessSpawner`
**As** the native `CredentialStore` adapter, **I want** a port method that
runs an arbitrary argv and hands back its stdout/stderr/exit status, **so
that** I can invoke `op` without inventing a one-off `std::process::Command`
call outside the port abstraction.
**Acceptance Criteria**:
- A successful subprocess's stdout is captured.
  - *Given* `spawn_and_capture(&["/bin/echo", "hello"])`, *When* awaited,
    *Then* the result's `stdout` is `b"hello\n"` and `exit_code` is `0`.
- A failing subprocess's stderr is captured, stdout is not conflated with it.
  - *Given* `spawn_and_capture(&["/bin/sh", "-c", "echo out; echo err >&2;
    exit 1"])` — note: this test's own argv is the *test's* invocation of
    `sh -c` to construct a fixture process, not a pattern `NativeCredentialStore`
    itself uses (which never shells out via `sh -c`, per Task 3.2.1b) —
    *When* awaited, *Then* `stdout` is `b"out\n"`, `stderr` is `b"err\n"`,
    and `exit_code` is `1`.
**Files**: `crates/core/src/ports.rs`, `crates/native/src/spawn.rs`

##### Task 3.1.1a: Add `ProcessOutput` + `spawn_and_capture` to the port trait (~4 min)
- ```rust
  pub struct ProcessOutput {
      pub stdout: Vec<u8>,
      pub stderr: Vec<u8>,
      pub exit_code: i32,
  }

  pub trait ProcessSpawner {
      async fn spawn_daemon(&self, exe_hint: Option<&str>, log_path: &str) -> Result<(), PortError>;
      /// Runs `argv[0]` with `argv[1..]` as literal arguments (never a shell
      /// string — no `sh -c`), waits for it to exit, and returns its
      /// captured stdout/stderr/exit code. Distinct from `spawn_daemon`,
      /// which is fire-and-forget with no capture and a hardcoded
      /// `--daemon` arg — this method exists specifically because
      /// `spawn_daemon` cannot serve `op` invocations (confirmed:
      /// `architecture.md`'s re-verification note, `pitfalls.md` §1a).
      async fn spawn_and_capture(&self, argv: &[&str]) -> Result<ProcessOutput, PortError>;
  }
  ```
- Files: `crates/core/src/ports.rs`

##### Task 3.1.1b: Implement `NativeSpawner::spawn_and_capture` (~5 min)
- `Command::new(argv[0]).args(&argv[1..])` — argv only, never a shell string
  (per `pitfalls.md` §1a). Per `pitfalls.md` §1c's hardening flag: call
  `.env_clear()` then explicitly re-add only the specific env vars the
  eventual `op` call needs (`OP_SERVICE_ACCOUNT_TOKEN`, `PATH`) rather than
  inheriting the daemon's full environment by default — this is a hardening
  item distinct from the `op`-invocation itself (which correctly *wants* the
  token in its env).
- Use `tokio::process::Command`'s async `.output()` (this crate already
  depends on `tokio` with the `process` feature per `Cargo.toml`), not
  blocking `std::process::Command::output()`, to stay consistent with every
  other adapter method being `async`.
- Files: `crates/native/src/spawn.rs`

##### Task 3.1.1c: Unit tests for the two ACs above (~4 min)
- Files: `crates/native/src/spawn.rs`

### Epic 3.2: `op` CLI invocation logic

**Goal**: `NativeCredentialStore::resolve` for password/username and TOTP,
with the ID cache (ADR-003 item 1), in-flight dedup of concurrent identical
`resolve()` calls (ADR-003 item 3 — the chosen rate-limit mitigation for the
account-wide `op` rate limit, `pitfalls.md` §2a), and error mapping to the 5
`PortError` variants.

#### Story 3.2.1: `NativeCredentialStore` skeleton + password/username resolve
**As** the native adapter, **I want** `resolve()` to invoke `op` with only a
secret *reference* on argv and read the value from stdout once, **so that**
the resolved value takes the shortest possible path from `op`'s pipe into a
`SecretValue`.
**Acceptance Criteria**:
- The constructed argv never contains a shell string.
  - *Given* a `CredentialRef{domain: "example.com", field:
    CredentialField::Password}` and a resolved `(vault_id, item_id)`, *When*
    the argv for the `op` call is
    built, *Then* it is a `Vec<&str>`/`Vec<String>` of literal arguments
    (e.g. `["op", "read", "--vault", "<id>", "op://<vault_id>/<item_id>/password"]`),
    never a single string passed through a shell.
- The resolved value reaches `SecretValue` via one buffer.
  - *Given* `spawn_and_capture` returns `stdout: b"hunter2\n"`, *When*
    `resolve()` builds its `SecretValue`, *Then* it is constructed directly
    from that `Vec<u8>` (trimmed of the trailing newline) converted once to
    `String` — never routed through `serde_json::Value` (per `pitfalls.md`
    §1d, which has no `zeroize` impl and would add an untracked intermediate
    copy).
**Files**: `crates/native/src/vault.rs` (new)

##### Task 3.2.1a: Create `crates/native/src/vault.rs` with the struct skeleton (~4 min)
- ```rust
  pub struct NativeCredentialStore<S: ProcessSpawner> {
      spawner: S,
      service_account_token: String, // read once at construction via EnvPort
      id_cache: RefCell<HashMap<String, (String, String)>>, // domain -> (vault_id, item_id)
  }
  ```
- `pub fn new(spawner: S, service_account_token: String) -> Self` — token is
  read by the caller (Phase 6, `main.rs`) via `EnvPort` and passed in, keeping
  this struct itself free of a direct `EnvPort` dependency (matching how
  other native adapters are constructed with already-resolved config, not
  live port handles, where practical).
- Register the new module in `crates/native/src/lib.rs` (`mod vault;` +
  `pub use vault::NativeCredentialStore;`).
- Files: `crates/native/src/vault.rs`, `crates/native/src/lib.rs`

##### Task 3.2.1b: Implement the password/username `op read` invocation (~5 min)
- Convert `credential_ref.field: CredentialField` to its lowercase wire
  segment via a small exhaustive match (`CredentialField::Username =>
  "username"`, `CredentialField::Password => "password"` — `Totp` never
  reaches this function, see Story 3.2.2), then build argv as `["op", "read",
  "--vault", &vault_id, &format!("op://{vault_id}/{item_id}/{field_str}")]`
  — vault/item IDs, not names (per `stack.md`'s 3x-request-cost note), sourced
  from `id_cache` (populated by Story 3.3.1's lookup on a cache miss).
  `--vault` is passed explicitly per `architecture.md` §6's note that service
  accounts have no default-vault inference.
- Call `spawn_and_capture`, trim a single trailing `\n` from `stdout`, build
  `SecretValue::new(String::from_utf8_lossy(...).into_owned())` directly.
- Build any error from `stderr` only, never `stdout` (per `pitfalls.md` §1b)
  — even `stderr` is treated as untrusted text, so map it to one of the fixed
  `PortError` variants (Story 3.2.4) rather than `format!()`-ing it wholesale
  into an error message.
- Files: `crates/native/src/vault.rs`

##### Task 3.2.1c: Unit tests for the two ACs above (~4 min)
- Use a fake `ProcessSpawner` test double (mirroring `FakeBrowserDriver`'s
  shape) to assert the exact argv built and the `SecretValue` construction
  path, without a real `op` binary.
- Files: `crates/native/src/vault.rs`

#### Story 3.2.2: TOTP resolve path
**As** the native adapter, **I want** `field == CredentialField::Totp` to
invoke `op item get --otp` instead of `op read`, **so that** 1Password's
server-side TOTP generation is used (never a local `totp-rs`/seed-based
library, per `build-vs-buy.md`'s explicit constraint).
**Acceptance Criteria**:
- `field: CredentialField::Totp` builds the `--otp` argv shape.
  - *Given* `CredentialRef{domain: "example.com", field:
    CredentialField::Totp}`, *When* the argv is built, *Then* it is `["op",
    "item", "get", "--vault", &vault_id, &item_id, "--otp"]` — not the `op
    read` shape used for password/username.
- `field: CredentialField::Password`/`Username` are unaffected by this
  branch.
  - *Given* `CredentialRef{domain: "example.com", field:
    CredentialField::Password}`, *When* the argv is built, *Then* it is
    unchanged from Task 3.2.1b's `op read` shape.
**Files**: `crates/native/src/vault.rs`

##### Task 3.2.2a: Exhaustive match on `credential_ref.field` (~3 min)
- `match credential_ref.field { CredentialField::Totp => ..., CredentialField::Username
  | CredentialField::Password => ... }` — an exhaustive match over the closed
  `CredentialField` enum (Task 1.1.1b), not a stringly-typed branch. Because
  both the wire enum (`CredentialFieldInput`) and this port enum have exactly
  3 legal values and the tool-layer conversion (Task 5.2.1b) is itself an
  exhaustive 1:1 match, there is no "unrecognized field" runtime state to
  handle here — the compiler rejects any future third-branch gap at build
  time, closing the fallback-arm concern a raw-`String` `field` would have
  left open.
- Files: `crates/native/src/vault.rs`

##### Task 3.2.2b: Unit test asserting both argv shapes (~3 min)
- An argv-shape assertion test (not a real `op` invocation), per the ACs.
- Files: `crates/native/src/vault.rs`

#### Story 3.2.3: Vault/item ID cache (ADR-003)
**As** the native adapter, **I want** to resolve a domain's `(vault_id,
item_id)` once per daemon lifetime and reuse it, **so that** repeated
`type_secret` calls against the same site don't each pay `op`'s 3x-request
name-lookup cost.
**Acceptance Criteria**:
- A second `resolve()` call for a previously-seen domain skips the
  list/filter step.
  - *Given* `id_cache` already contains `"example.com" -> ("v1", "i1")`
    (from a prior successful resolve), *When* `resolve()` is called again
    with `CredentialRef{domain: "example.com", field:
    CredentialField::Password}`, *Then* the domain-lookup/filter logic from
    Story 3.3.1 is not invoked a second time — only the `op read`/`op item
    get` call itself runs.
- The cache holds only identifiers, never a `SecretValue`.
  - *Given* the `id_cache` field's type `RefCell<HashMap<String, (String,
    String)>>`, *When* inspected, *Then* it structurally cannot hold a
    `SecretValue` — there is no code path that could insert one, since the
    value type is `(String, String)`.
**Files**: `crates/native/src/vault.rs`

##### Task 3.2.3a: Check-then-populate the cache around the domain lookup (~4 min)
- Files: `crates/native/src/vault.rs`

##### Task 3.2.3b: Doc comment explicitly reconciling this with requirements.md (~2 min)
- A doc comment on `id_cache` stating explicitly: this cache holds only
  vault/item identifiers, never resolves a value from cache — cross-
  references ADR-003 by filename so a future reader doesn't need to
  re-derive the reasoning.
- Files: `crates/native/src/vault.rs`

#### Story 3.2.4: Error mapping — `op` failures to the 5 `PortError` variants
**As** the native adapter, **I want** `op`'s exit code/stderr mapped to the
correct one of the 5 vault `PortError` variants, **so that** the tool layer
(and ultimately the LLM) gets the right retryability signal.
**Acceptance Criteria**:
- An unauthenticated `op` maps to `CredentialUnauthenticated`.
  - *Given* `stderr: b"[ERROR] 2026/09/08 You are not currently signed in.
    Please run `op signin --help` for instructions.\n"` and a non-zero exit
    code, *When* mapped, *Then* the result is `PortError::CredentialUnauthenticated("vault
    unauthenticated: 1Password CLI reports not signed in — not typed. This
    requires human action (run `op signin` or unlock the desktop app); the
    agent cannot resolve this itself.".to_string())` (verbatim string per
    `ux.md` §4 example 3, with the `Display` prefix already baked in per
    Task 1.2.1b's convention).
- A rate-limited `op` maps to `CredentialRateLimited` with a retry-after
  detail when available.
  - *Given* `stderr` matching 1Password's rate-limit error text, *When*
    mapped, *Then* the result is `PortError::CredentialRateLimited(...)`
    whose rendered message contains the substring `"retry after"` — either a
    concrete second count (if `op service-account ratelimit`'s diagnostic
    succeeded) or an explicit "retry after an unspecified delay" fallback
    phrase (never a bare generic I/O error, per `pitfalls.md` §2a).
**Files**: `crates/native/src/vault.rs`

##### Task 3.2.4a: `stderr` pattern match for `CredentialUnauthenticated` (~4 min)
- A small, fixed set of substring checks against `stderr` (never `format!()`-ing
  the raw `stderr` buffer into the error per `pitfalls.md` §1b — only fixed,
  hand-written strings per matched pattern), using the exact `ux.md` §4
  example-3 wording verbatim.
- Files: `crates/native/src/vault.rs`

##### Task 3.2.4b: `stderr` pattern match + `op service-account ratelimit` for `CredentialRateLimited` (~5 min)
- On detecting a rate-limit pattern in `stderr`, best-effort shell `op
  service-account ratelimit` (via the same `spawn_and_capture`) to extract a
  retry-after duration; on any failure of that secondary call, fall back to
  the unspecified-delay phrasing rather than failing the whole error-mapping
  path.
- Files: `crates/native/src/vault.rs`

##### Task 3.2.4c: Unit tests for both mappings using canned `stderr` fixtures (~4 min)
- No real `op` binary needed — fixed `stderr`/exit-code fixtures per case.
- Files: `crates/native/src/vault.rs`

#### Story 3.2.5: In-flight dedup of concurrent identical `resolve()` calls (ADR-003 item 3)
**As** the native adapter, **I want** two or more concurrent `resolve()`
calls for the identical `CredentialRef` to share one underlying `op`
invocation rather than each issuing its own, **so that** the documented
account-wide 1Password service-account rate limit (`pitfalls.md` §2a,
`openclaw#56217`) isn't tripped by ordinary concurrent multi-tab use — the
mitigation ADR-003 explicitly chose and named, distinct from Story 3.2.3's
identifier cache (which only avoids repeat *name→ID* lookups, not repeat
concurrent *value* resolves).
**Acceptance Criteria**:
- Two concurrent `resolve()` calls for the same `CredentialRef` produce
  exactly one `op` invocation.
  - *Given* two concurrent `type_secret` calls for the same domain+field on
    different tabs, both dispatching within the same in-flight window (i.e.
    the second call's `resolve()` starts before the first's has returned),
    *When* both complete, *Then* the fake `ProcessSpawner`'s call count for
    `op read`/`op item get` is exactly 1 (not 2), and both callers receive
    the correct resolved value.
- Two concurrent `resolve()` calls for *different* `CredentialRef`s are
  unaffected (each gets its own `op` call, no cross-talk).
  - *Given* two concurrent `resolve()` calls, one for `{domain:
    "a.example", field: CredentialField::Password}` and one for `{domain:
    "b.example", field: CredentialField::Password}`, *When* both complete,
    *Then* the fake `ProcessSpawner`'s call count is exactly 2, and each
    caller receives the value for its own domain (never the other's).
- Nothing persists once every waiter has been served (this is dedup, not a
  value cache — ADR-003's explicit distinction).
  - *Given* the in-flight map after both waiters above have been served,
    *When* inspected, *Then* it contains no entry for either `CredentialRef`
    — a subsequent `resolve()` call for the same ref triggers a fresh `op`
    invocation, not a cached value.
**Files**: `crates/native/src/vault.rs`

##### Task 3.2.5a: Add an in-flight-requests map keyed by `CredentialRef` (~5 min)
- Add `pending: RefCell<HashMap<CredentialRef, futures::future::Shared<...>>>`
  (or an equivalent single-flight primitive — e.g. a
  `HashMap<CredentialRef, Vec<oneshot::Sender<Result<SecretValue, PortError>>>>`
  of waiters) to `NativeCredentialStore`. This crate's `Cargo.toml` already
  depends on `futures = "0.3"` (`crates/native/Cargo.toml`), so
  `futures::future::Shared` (wrapping a `LocalBoxFuture`, matching this
  daemon's single-threaded `current_thread` + `LocalSet` runtime — `Rc`,
  not `Arc`, per the rest of this crate's convention) is directly usable
  without a new dependency. `SecretValue` itself is not `Clone` (per Task
  1.1.1c's zeroize-on-drop design) — the `Shared` future's `Output` must
  therefore be `Result<Rc<SecretValue>, PortError>` (or the map stores
  `Result<SecretValue, PortError>` behind a wrapper cloneable without
  duplicating the underlying buffer), not a bare `Result<SecretValue,
  PortError>` — resolve this constraint explicitly here rather than
  discovering it mid-implementation.
- Files: `crates/native/src/vault.rs`

##### Task 3.2.5b: Wire `resolve()` to check-then-join-or-spawn (~5 min)
- On entry: if `pending` already has an entry for this `CredentialRef`,
  await (clone of) its `Shared` future/join its waiter list instead of
  starting a new `op` call. Otherwise, spawn the real resolve work (via
  `tokio::task::spawn_local`, matching this daemon's `LocalSet` runtime),
  register it in `pending` before awaiting it, and remove the entry once it
  completes (success or error — both cases clear the entry, so a failed
  resolve doesn't permanently poison future calls for that ref).
- Files: `crates/native/src/vault.rs`

##### Task 3.2.5c: Unit tests for all three ACs above (~5 min)
- Uses a fake `ProcessSpawner` whose `spawn_and_capture` blocks on a
  `tokio::sync::Notify` (or similar) until both concurrent callers have
  started, so the test can deterministically force the in-flight-overlap
  window rather than relying on timing — asserting exact call counts per the
  ACs.
- Files: `crates/native/src/vault.rs`

### Epic 3.3: Domain-scoped lookup + disambiguation

**Goal**: `op item list` filtered by domain, using the shared `same_host`
helper (Epic 1.4) on both the page-URL side and the vault-item-URL side.

#### Story 3.3.1: List-and-filter vault items by domain
**As** the native adapter, **I want** to find the vault item(s) matching a
requested domain by listing Login-category items and filtering their `urls[]`
by exact host equality, **so that** a `resolve()` call for an unseen domain
can populate the ID cache (Story 3.2.3) correctly.
**Acceptance Criteria**:
- Exactly one matching item populates the cache and proceeds.
  - *Given* `op item list --categories Login --format json`'s output
    contains exactly one item whose `urls[].href` has host `example.com`,
    *When* `resolve()` is called with `CredentialRef{domain: "example.com",
    ...}`, *Then* `id_cache["example.com"]` is populated with that item's
    vault/item IDs and the `op read`/`op item get` call proceeds.
- Zero matching items reject as a domain mismatch.
  - *Given* no item's `urls[].href` host equals `example.com`, *When*
    `resolve()` is called, *Then* the result is
    `PortError::CredentialDomainMismatch(...)` and no `op read`/`op item get`
    call is ever made.
**Files**: `crates/native/src/vault.rs`

##### Task 3.3.1a: Implement the list-and-filter call (~5 min)
- `op item list --categories Login --format json`, parse each item's
  `urls[].href`, filter using `same_host` (Epic 1.4's now-`pub(crate)`
  helper — imported from `stapler_mcp_core::tools::webcrawl`, or wherever it
  ends up being re-exported from for cross-crate use — check whether it
  needs a `pub` re-export from `crates/core`'s lib root for `crates/native`
  to reach it, and add one if so).
- Files: `crates/native/src/vault.rs`, possibly `crates/core/src/lib.rs` (if a
  re-export is needed for cross-crate visibility)

##### Task 3.3.1b: Zero-match and multi-match error construction (~4 min)
- Zero matches → `CredentialDomainMismatch` using `ux.md` §4 example-1's
  verbatim template (substituting the actual requested/current domains).
  Multi-match → `CredentialAmbiguous` using example-2's verbatim template,
  including a comma-separated candidate title list.
- Files: `crates/native/src/vault.rs`

##### Task 3.3.1c: Unit test — two canned items, same domain, ambiguous (~4 min)
- Fixture JSON with 2 items sharing a host; assert the error names both
  titles.
- Files: `crates/native/src/vault.rs`

### Epic 3.4: Native `type_secret` dispatch

**Goal**: Wire `CredentialStore::resolve` + `BrowserDriver::type_secret`
together in `NativeBrowser`, with the dispatch-time DOM re-check, own-node
redaction, and live-URL domain check.

#### Story 3.4.0: Inject `NativeCredentialStore` into `NativeBrowser`
**As** the native `BrowserDriver` adapter, **I want** to hold my own
`CredentialStore` handle rather than have one passed into `type_secret`'s
arguments, **so that** `type_secret` can resolve internally in a single
adapter-owned call path — the design Task 1.3.1a's trait doc commits to, and
the only path through which ADR-003's in-flight dedup (Story 3.2.5) sees
every resolve request.
**Acceptance Criteria**:
- `NativeBrowser` compiles with a `credential_store` field and a setter.
  - *Given* `NativeBrowser::launch()` followed by
    `browser.set_credential_store(Rc::new(fake_store))`, *When*
    `type_secret` is subsequently called, *Then* it reaches the injected
    store's `resolve()` — asserted via the fake store's own call-count/arg
    capture, not by inspecting private state.
- With no store injected, `type_secret` fails closed, not by panicking.
  - *Given* a freshly-`launch()`ed `NativeBrowser` with no
    `set_credential_store` call made, *When* `type_secret` is called, *Then*
    it returns `Err(PortError::CredentialUnauthenticated(...))` (or an
    equivalent named vault variant — implementer's choice, documented in the
    method's doc comment) rather than panicking — this path is expected to
    be unreachable in practice, since Phase 6 only registers the
    `stapler_browser_type_secret` tool at all when a store was successfully
    constructed and injected, but the adapter itself must not assume that
    invariant.
**Files**: `crates/native/src/browser.rs`

##### Task 3.4.0a: Add `credential_store: RefCell<Option<Rc<dyn CredentialStore>>>` field + setter (~3 min)
- Add the field to `NativeBrowser`'s struct definition (alongside `sessions`/
  `pending_new_sessions`), plus `pub fn set_credential_store(&self, store:
  Rc<dyn CredentialStore>)` (interior mutability, matching this struct's
  existing `RefCell`/`Cell` convention — `launch()`'s own signature stays
  unchanged, avoiding a breaking change to every existing `launch()` call
  site).
- Files: `crates/native/src/browser.rs`

##### Task 3.4.0b: Unit test for both ACs above (~3 min)
- Files: `crates/native/src/browser.rs`

#### Story 3.4.1: `Action::TypeSecret` dispatch arm with dispatch-time type refusal
**As** the native `BrowserDriver`, **I want** `type_secret` to refuse typing
if the resolved node's live DOM type/autocomplete isn't password/TOTP-shaped
at dispatch time, **so that** the tool can't be misused as a plaintext-typer
with extra steps (per the chosen refuse-not-permit policy).
**Acceptance Criteria**:
- A locator resolving to a genuine password field succeeds.
  - *Given* a resolved node with live `type="password"`, *When*
    `type_secret` dispatches with a resolved `SecretValue`, *Then* the DOM
    write proceeds via the same atomic `this.value = text` path
    `dispatch_type` already uses.
- A locator resolving to a plain text field is refused before any write.
  - *Given* a resolved node with live `type="text"` and no
    TOTP-shaped `autocomplete`, and `credential_ref.field !=
    CredentialField::Totp`, *When*
    `type_secret` dispatches, *Then* the call returns an error whose message
    is `ux.md` §4 example-5's verbatim template (substituting the real
    `ref`/role), and no DOM write occurs.
**Files**: `crates/native/src/browser.rs`

##### Task 3.4.1a: Add `Action::TypeSecret(SecretValue)` to the `Action` enum (~2 min)
- Files: `crates/native/src/browser.rs`

##### Task 3.4.1b: Dispatch-time re-check + refusal (~5 min)
- Reuse the combined probe from Task 2.2.1a (`probe_redaction`, or a small
  variant returning the specific type/autocomplete values rather than just a
  bool, since the refusal message needs the actual `role` for its `ux.md`
  template) immediately before dispatch, after `verify_node_live` — this
  ordering (domain check → `self.credential_store.resolve(credential_ref)`
  (Story 3.4.0's injected handle, called internally — never a value passed
  in from the caller) → `verify_node_live` → this re-check → write)
  deliberately keeps the type re-check as close to the actual write as
  `verify_node_live`'s own TOCTOU check already is
  (`features.md` §2.3's SPA-re-render concern), accepting the minor cost of
  an occasional already-completed `resolve()` call going unused when the
  field turns out to be the wrong shape — cheaper than reordering and
  reopening the TOCTOU gap this check exists to close. For `field ==
  CredentialField::Totp`, accept `autocomplete == "one-time-code"` OR `type
  == "password"`; for `field` in `{CredentialField::Password,
  CredentialField::Username}`, require `type == "password"` —
  refuse otherwise by returning `PortError::NotActionable` (the existing
  variant, whose doc comment already covers "a resolved element failed a
  click/type actionability check... the ref itself is still valid" — broaden
  its doc comment to note a type-mismatch refusal is the same shape of
  problem, not a 6th vault-specific variant) carrying the example-5 message.
- Files: `crates/native/src/browser.rs`, `crates/core/src/ports.rs` (doc
  comment broadening only)

##### Task 3.4.1c: Wire the atomic DOM write using `SecretValue::expose()` (~4 min)
- Reuse `dispatch_type`'s exact JS (`this.value = text; ...dispatchEvent...`)
  with `secret.expose()` as the value argument — the exposed `&str` is used
  only for the immediate `invoke_on_node` call, never stored, formatted, or
  logged anywhere in this path.
- Files: `crates/native/src/browser.rs`

#### Story 3.4.2: Unconditional own-node redaction
**As** the native `BrowserDriver`, **I want** `type_secret`'s own returned
snapshot to redact its acted-on node regardless of the structural redaction
pass's own verdict, **so that** a field the structural key happens to miss
(e.g. an unconventional `autocomplete` value) is still never exposed for the
one call that's guaranteed to know it just held a secret.
**Acceptance Criteria**:
- The acted-on node is redacted even if structural redaction wouldn't have
  caught it.
  - *Given* a (hypothetical, test-only) field with `type="text"` and no
    matching `autocomplete` value — one the structural key from Phase 2
    would NOT redact on its own — that `type_secret` just wrote a secret
    into, *When* `type_secret` returns its snapshot, *Then* that specific
    node's `value` is `"[REDACTED]"` anyway, because `type_secret`
    unconditionally overwrites its own acted-on node's value after the
    structural pass runs.
**Files**: `crates/native/src/browser.rs`

##### Task 3.4.2a: Force-redact the acted-on node after `capture_snapshot` (~4 min)
- After the post-dispatch `capture_snapshot` call (same pattern
  `dispatch_action` already uses for `click`/`type_text`), locate the acted-on
  node by its `node_ref` in the returned tree and set its `value` to
  `REDACTED_PLACEHOLDER` unconditionally, whether or not the structural pass
  already redacted it.
- Files: `crates/native/src/browser.rs`

##### Task 3.4.2b: Unit test for the AC above (~4 min)
- Files: `crates/native/src/browser.rs`

#### Story 3.4.3: Domain check against the live navigation URL
**As** the native `BrowserDriver`, **I want** the domain check to read the
page's URL fresh, in the same call as dispatch — not a cached
`latest_url` — **so that** a same-call redirect can't slip a credential past
the check (the exact staleness-bug class `6b6b56a` already fixed once for
SSRF).
**Acceptance Criteria**:
- A session whose live URL doesn't match the requested domain is rejected
  before `CredentialStore::resolve` is ever called.
  - *Given* a session whose `page.url()` (queried fresh, not `latest_url`)
    is `https://evil-example.com/`, and `CredentialRef{domain:
    "example.com", ...}`, *When* `type_secret` is called, *Then* the result
    is `PortError::CredentialDomainMismatch(...)` and `CredentialStore::resolve`
    is never invoked (asserted via the fake `CredentialStore`'s call-count).
**Files**: `crates/native/src/browser.rs`

##### Task 3.4.3a: Query `page.url()` fresh, same pattern as `dispatch_action`'s existing `url_before` (~3 min)
- Mirror the existing `let url_before = page.url().await...` line already in
  `dispatch_action` (`browser.rs:2512-2516`) — `type_secret`'s dispatch path
  reads the URL the identical way, then calls `same_host` against it before
  doing anything else.
- Files: `crates/native/src/browser.rs`

##### Task 3.4.3b: Unit/integration test for the AC above (~4 min)
- Files: `crates/native/src/browser.rs`

---

## Phase 4: Wasm `CredentialStore` Adapter (`@1password/sdk`)

**Sequenced strictly after Phase 3** per `pitfalls.md` §4c. Do not begin
Epic 4.2+ until Epic 4.1's spike has a recorded go/no-go result.

### Epic 4.1: Feasibility spike (explicit, blocking)

#### Story 4.1.1: 30-minute spike — `@1password/sdk` inside this project's Node host
**As** the implementer, **I want** to confirm `@1password/sdk` actually
constructs a client and resolves one secret inside this project's real
wasm-bindgen/Node glue setup before committing to full wasm-adapter work,
**so that** the wasm-in-wasm-glue nesting risk (`pitfalls.md` §4a/4b,
UNVERIFIED) is resolved empirically, not assumed.
**Acceptance Criteria**:
- The spike produces a recorded pass/fail verdict before any Epic 4.2 task
  starts.
  - *Given* `@1password/sdk@0.5.0` installed in the same Node host
    `crates/wasm/src/glue/browser.js` runs in, *When* `createClient({auth:
    process.env.OP_SERVICE_ACCOUNT_TOKEN})` followed by one
    `client.items.get(...)` call is attempted, *Then* the outcome (success,
    or the specific failure mode) is written into this plan's Unresolved
    Questions section (already stubbed above) or, if it succeeds cleanly,
    the corresponding Unresolved Questions bullet is checked off with a
    one-line note of what was verified.
**Files**: none (a throwaway spike script), `project_plans/credential-vault/implementation/plan.md` (recording the result)

##### Task 4.1.1a: Install `@1password/sdk` in the Node host, pin exact version (~2 min)
- `npm install @1password/sdk@0.5.0` (pre-1.0, pin exact per `stack.md`) in
  `npm/package.json`'s `dependencies`.
- Files: `npm/package.json`

##### Task 4.1.1b: Run the spike, record the result (~15 min)
- A throwaway Node script (not committed, or committed under a `scratch/`
  path excluded from the crate build) exercising `createClient` +
  `items.get()` against a real or disposable test 1Password vault item.
  Record: did it construct without a wasm-in-wasm-glue conflict, did the
  resolve call return a value, any error text encountered.
- Files: none tracked; updates this plan's Unresolved Questions section

##### Task 4.1.1c: Go/no-go checkpoint (~2 min)
- If the spike fails outright (client construction throws, or the module
  can't be `require()`d from within the existing wasm-bindgen glue process),
  stop and escalate to the user before proceeding to Epic 4.2 — this is an
  explicit blocking decision point, not a soft recommendation. If it
  succeeds, proceed to Epic 4.2 as planned.
- Files: none

### Epic 4.2: `vault.js` glue

**Goal**: Mirrors `browser.js`'s lazy-singleton pattern; field-conditional
branch for TOTP (`architecture.md` §6 — the Node SDK's `items.get()` doesn't
return OTP values; `secrets.resolve()` with `?attribute=otp` is required
instead); in-flight dedup of concurrent identical resolves (ADR-003 item 3),
mirroring native's Story 3.2.5.

#### Story 4.2.1: `getVaultClient()` lazy singleton + `jsResolveCredential`
**As** the wasm adapter, **I want** one shared authenticated `@1password/sdk`
client for the daemon's lifetime, **so that** re-authentication doesn't
happen per call (mirrors `getBrowser()`'s existing pattern).
**Acceptance Criteria**:
- The client is constructed at most once across multiple `resolve()` calls.
  - *Given* two sequential `jsResolveCredential(...)` calls, *When* both
    complete, *Then* `createClient` was invoked exactly once (asserted via a
    call-count spy in the Node test, mirroring how `browser.js`'s existing
    tests would verify `getBrowser()`'s singleton behavior).
- `field: "totp"` uses `secrets.resolve()` with the `?attribute=otp` query,
  not `items.get()`.
  - *Given* `jsResolveCredential("example.com", "totp")`, *When* called,
    *Then* the underlying SDK call is `client.secrets.resolve(` a reference
    string ending in `"?attribute=otp"` `)`, not `client.items.get(...)`.
**Files**: `crates/wasm/src/glue/vault.js` (new)

##### Task 4.2.1a: Create `vault.js` with the lazy singleton (~4 min)
- ```js
  const { createClient } = require("@1password/sdk");
  let clientPromise = null;
  function getVaultClient() {
      if (!clientPromise) {
          clientPromise = createClient({
              auth: process.env.OP_SERVICE_ACCOUNT_TOKEN,
              integrationName: "stapler-mcp",
              integrationVersion: "0.1.0",
          });
      }
      return clientPromise;
  }
  ```
- Files: `crates/wasm/src/glue/vault.js`

##### Task 4.2.1b: `jsResolveCredential(domain, field)` field-conditional branch (~5 min)
- `field === "totp"` → `client.secrets.resolve(\`op://${vaultId}/${itemId}/${field}?attribute=otp\`)`;
  else → the item-field path (`client.items.get(vaultId, itemId)`, reading
  the named field's value). Mirrors native's Story 3.2.2 branch, one level
  up in the wasm/JS stack.
- Files: `crates/wasm/src/glue/vault.js`

##### Task 4.2.1c: Domain-scoped item listing + ambiguous/mismatch errors (~5 min)
- Mirrors native's Story 3.3.1: list Login-category items, filter by URL
  host. Since `same_host`'s Rust implementation isn't reachable from JS, this
  needs its own JS-side exact-host comparison — reuse the *shape* of
  `isBlockedHost`'s existing bracket/lowercase-normalization handling in
  `browser.js` as the model for correctness (not the private-IP logic
  itself, just the "normalize before comparing" discipline), rather than a
  naive `===` on raw hostnames. Emits the same `ux.md` §4 example-1/2
  verbatim strings as native.
- Files: `crates/wasm/src/glue/vault.js`

#### Story 4.2.2: In-flight dedup of concurrent identical resolves (ADR-003 item 3)
**As** the wasm adapter, **I want** two or more concurrent
`jsResolveCredential` calls for the identical domain+field to share one
underlying `client.secrets.resolve()`/`client.items.get()` SDK call, **so
that** wasm has the same rate-limit mitigation native gets from Story 3.2.5
— required by both reviews' blockers, since ADR-003 makes no adapter-specific
carve-out.
**Acceptance Criteria**:
- Two concurrent `jsResolveCredential` calls for the same domain+field
  produce exactly one underlying SDK call.
  - *Given* two concurrent `type_secret` calls for the same domain+field on
    different tabs, both dispatching within the same in-flight window,
    *When* both complete, *Then* the mocked SDK client's call-count spy
    shows exactly 1 call (not 2), and both callers receive the correct
    resolved value.
- Two concurrent calls for *different* domain+field pairs are unaffected.
  - *Given* two concurrent calls for different domain+field pairs, *When*
    both complete, *Then* the call-count spy shows exactly 2 calls, one per
    pair.
- Nothing persists once every waiter has been served.
  - *Given* the in-flight map after both waiters above have been served,
    *When* inspected, *Then* it has no entry for either key.
**Files**: `crates/wasm/src/glue/vault.js`

##### Task 4.2.2a: Add an in-flight `Map` keyed by `` `${domain}::${field}` `` holding in-progress `Promise`s (~4 min)
- A module-level `const pending = new Map();` (alongside `clientPromise`,
  Task 4.2.1a) — `jsResolveCredential` checks `pending` first; if an entry
  exists, `return pending.get(key)` (returning the *same* `Promise` to the
  new caller, which is how concurrent `await`s naturally share one
  in-flight resolution in JS — no separate waiter-list plumbing needed,
  unlike native's `Shared`-future shape, since a `Promise` is inherently
  awaitable by multiple callers with no extra cloning). Otherwise, start the
  real resolve, store its `Promise` in `pending` under `key`, and — critically
  — `.finally(() => pending.delete(key))` so the entry is removed once
  settled (success or rejection), keeping this dedup-only, never a value
  cache.
- Files: `crates/wasm/src/glue/vault.js`

##### Task 4.2.2b: Node test for all three ACs above (~5 min)
- Following this crate's existing Node-test-harness convention (mock SDK
  client with a call-count spy whose resolution is controllable — e.g. a
  manually-resolved `Promise` — so the test can force the in-flight-overlap
  window deterministically, mirroring Task 3.2.5c's approach).
- Files: `crates/wasm/src/glue/vault.js` (or its sibling test file, matching
  whatever convention Task 2.3.1c's investigation already established)

### Epic 4.3: Wasm-side Rust wrapper + `type_secret`

#### Story 4.3.0: Inject `WasmCredentialStore` into `WasmBrowser`
**As** the wasm `BrowserDriver` adapter, **I want** to hold my own
`CredentialStore` handle exactly like native's `NativeBrowser` does (Story
3.4.0), **so that** `WasmBrowser::type_secret` resolves internally too —
wasm parity with native's single-adapter-owned-resolve design, per Task
1.3.1a's trait doc.
**Acceptance Criteria**: same shape as Story 3.4.0's two ACs, exercised
against `WasmBrowser` instead of `NativeBrowser`.
**Files**: `crates/wasm/src/browser.rs`

##### Task 4.3.0a: Give `WasmBrowser` a `credential_store` field + setter (~3 min)
- `WasmBrowser` (`crates/wasm/src/browser.rs:89`) is currently a zero-field
  unit struct (`pub struct WasmBrowser;`); this task adds
  `credential_store: RefCell<Option<Rc<dyn CredentialStore>>>` and a
  `pub fn set_credential_store(&self, store: Rc<dyn CredentialStore>)`
  setter, plus a `WasmBrowser::new()` constructor (replacing bare unit-struct
  construction at call sites — check `crates/wasm/src/lib.rs` for existing
  `WasmBrowser` construction sites and update them).
- Files: `crates/wasm/src/browser.rs`, `crates/wasm/src/lib.rs`

##### Task 4.3.0b: Unit test mirroring Task 3.4.0b (~3 min)
- Files: `crates/wasm/src/browser.rs`

#### Story 4.3.1: `WasmCredentialStore`
**As** the wasm adapter, **I want** a Rust `CredentialStore` impl wrapping
`vault.js` via wasm-bindgen, **so that** `crates/core`'s trait boundary is
satisfied identically to the native side.
**Acceptance Criteria**:
- A JS-thrown vault error maps to the correct `PortError` variant.
  - *Given* `jsResolveCredential` rejects with an error whose message
    matches the ambiguous-match pattern, *When* `WasmCredentialStore::resolve`
    catches it, *Then* the result is `PortError::CredentialAmbiguous(...)`
    with the `ux.md` example-2 verbatim message — mirroring
    `crates/wasm/src/browser.rs`'s existing `map_js_error` pattern for
    `NotFound`/`SessionCrashed`.
**Files**: `crates/wasm/src/vault.rs` (new)

##### Task 4.3.1a: `WasmCredentialStore` wrapping `vault.js` via `wasm-bindgen` extern (~5 min)
- Mirrors `crates/wasm/src/browser.rs`'s existing `#[wasm_bindgen(module =
  "/src/glue/browser.js")] extern "C" { ... }` pattern for the new
  `jsResolveCredential` binding. Since JS has no enum type, convert
  `credential_ref.field: CredentialField` to its lowercase wire string via
  the same small exhaustive match Task 3.2.1b/3.2.2a use natively, *before*
  crossing the `wasm_bindgen` extern boundary — `jsResolveCredential`
  receives a plain string, never the Rust enum.
- Register in `crates/wasm/src/lib.rs`.
- Files: `crates/wasm/src/vault.rs`, `crates/wasm/src/lib.rs`

##### Task 4.3.1b: Map JS error strings to the 5 `PortError` vault variants (~5 min)
- Mirrors `crates/wasm/src/browser.rs`'s existing `map_js_error` regex-style
  dispatch (e.g. `MISSING_REF_ERROR_RE` in `browser.js`, and its Rust-side
  consumer) — one regex/substring match per vault error family.
- Files: `crates/wasm/src/vault.rs`

#### Story 4.3.2: `jsBrowserTypeSecret` + `WasmBrowser::type_secret`
**As** the wasm `BrowserDriver`, **I want** `type_secret` to reuse
`locator.fill()`'s atomic write and apply the same dispatch-time refusal and
own-node redaction as native, **so that** wasm has parity with the native
adapter's guarantees.
**Acceptance Criteria**:
- A non-password/non-TOTP-shaped locator is refused before any write, same
  message family as native.
  - *Given* a locator resolving to `<input type="text">` with no matching
    `autocomplete`, and `field: "password"`, *When* `jsBrowserTypeSecret` is
    called, *Then* it rejects before calling `locator.fill()`, with the
    `ux.md` example-5 verbatim message.
- The acted-on node is always redacted in the returned snapshot.
  - *Given* a successful `type_secret` call, *When* the returned snapshot is
    inspected, *Then* the acted-on node's value is `"[REDACTED]"`
    unconditionally, mirroring native's Task 3.4.2a.
**Files**: `crates/wasm/src/glue/browser.js`, `crates/wasm/src/browser.rs`

##### Task 4.3.2a: `jsBrowserTypeSecret(sessionId, refId, secretValue, timeoutMs)` in `browser.js` (~5 min)
- Reuses `refLocator`/`locator.fill()` (atomic write, same as `jsBrowserType`)
  and `waitForBlockedGracePeriod`/`checkBlocked` exactly as `jsBrowserType`
  does. `secretValue` crosses the wasm boundary as a plain JS string
  immediately before this call and is never stored on `session` or logged.
- Files: `crates/wasm/src/glue/browser.js`

##### Task 4.3.2b: Dispatch-time type re-check before `locator.fill()` (~4 min)
- `elementHandle.evaluate(el => ({type: el.type, autocomplete:
  el.autocomplete}))` immediately before the fill, same refusal policy and
  same ordering rationale as native's Task 3.4.1b (re-check as close to the
  write as possible, after resolve, not before) — the JS-side rejection
  carries the example-5 message, mapped by `WasmBrowser`'s error-mapping
  layer to `PortError::NotActionable`, matching native's variant choice.
- Files: `crates/wasm/src/glue/browser.js`

##### Task 4.3.2c: Force-redact the acted-on node in the returned snapshot (~3 min)
- Mirrors native's Task 3.4.2a — after `captureSnapshot`, locate the node by
  `refId` and overwrite its value unconditionally.
- Files: `crates/wasm/src/glue/browser.js`

##### Task 4.3.2d: `WasmBrowser::type_secret` Rust-side wiring (~6 min)
- Two wasm-bindgen calls, not one, so the domain check (Story 4.3.3) can gate
  resolution instead of following it:
  1. Cross the wasm-bindgen boundary calling the new `jsBrowserCurrentUrl`
     binding (Task 4.3.3a) to get the session's fresh `page.url()` — no
     resolve has happened yet.
  2. Compare it to `credential_ref.domain` using the same domain-match
     mechanism Story 3.4.3a's native check uses. On mismatch, return
     `Err(PortError::CredentialDomainMismatch(...))` immediately —
     `self.credential_store` is never touched.
  3. Only on a match, call `self.credential_store` (Story 4.3.0's injected
     handle — `None` fails closed per Story 4.3.0's second AC) to resolve
     `credential_ref`.
  4. Cross the wasm-bindgen boundary a second time, calling
     `jsBrowserTypeSecret` with the resolved plaintext value and
     deserializing the snapshot result — mirrors `WasmBrowser::type_text`'s
     existing shape for the JS-call/deserialize part, but adds the internal
     resolve step `type_text` doesn't have.
  This is the wasm-side equivalent of native's Task 3.4.1b/c ordering
  (domain check → resolve → dispatch-time re-check → write), reached via an
  extra JS round-trip because (unlike native) wasm's Rust side has no direct
  handle to the page object — steps 1-2 replace native's synchronous
  in-process check. The tool layer (Story 5.2.1) never calls
  `WasmCredentialStore::resolve` itself.
- Files: `crates/wasm/src/browser.rs`

#### Story 4.3.3: Domain check against the live navigation URL (wasm parity with Story 3.4.3)
**As** the wasm `BrowserDriver`, **I want** `type_secret`'s domain check to
read the page's URL fresh, queried via a dedicated JS glue call made *before*
`WasmBrowser::type_secret` (Task 4.3.2d) calls `self.credential_store.resolve`
— never a value passed in from an earlier call or snapshot, and never
computed only after resolution has already happened — **so that** wasm
closes the exact staleness-bug class already found and fixed once for the
SSRF guard in commit `6b6b56a` ("race SSRF redirect detection against
goto"), matching native's Story 3.4.3 and `requirements.md`'s 4th Success
Metric ("same wire shape on both adapters"). Without this story, wasm's
`type_secret` could use a cached/stale URL for its domain-match check while
native's cannot — an asymmetry the adversarial review flagged as
reintroducing a bug class this codebase already paid to fix once. Unlike
native, whose Rust code holds the page object directly and can check the
domain synchronously before calling `resolve` in the same function (Story
3.4.3), wasm's Rust side has no direct handle to the page — the domain check
necessarily costs a small JS round-trip, and this story's job is to make
sure that round-trip happens *first*, strictly before `resolve`, not folded
into the same JS call that ultimately performs the DOM write.
**Acceptance Criteria**:
- A session whose live URL doesn't match the requested domain is rejected
  before any vault resolution is attempted.
  - *Given* a session whose `page.url()` (queried fresh via
    `jsBrowserCurrentUrl`, immediately before `WasmBrowser::type_secret`
    calls `self.credential_store.resolve` — never Playwright's page object's
    own potentially-stale cached URL property, and never a URL threaded in
    from an earlier call) is `https://evil-example.com/`, and a request for
    `{domain: "example.com", ...}`, *When* `WasmBrowser::type_secret` is
    called, *Then* it rejects with the same `ux.md` §4 example-1
    domain-mismatch message family native uses
    (`PortError::CredentialDomainMismatch`), and the injected
    `credential_store`'s resolve path is never invoked (asserted via a
    call-count spy on the fake `CredentialStore`, mirroring native's Task
    3.4.3b's fake-`CredentialStore` call-count assertion) — `jsBrowserTypeSecret`
    (the DOM-write function) is not called at all on this path either.
**Files**: `crates/wasm/src/glue/browser.js`, `crates/wasm/src/browser.rs`

##### Task 4.3.3a: `jsBrowserCurrentUrl(sessionId)` — a minimal URL-only glue call, and the Rust-side domain match before resolve (~5 min)
- `browser.js` currently has no standalone "just give me the current URL"
  export — every existing function (`jsBrowserClick`/`jsBrowserType`/etc.)
  reads `session.page.url()` inline as one step of a larger action, and
  `captureSnapshot`'s `url` field (used by `jsBrowserSnapshot` and friends)
  only comes back bundled with a full accessibility-tree build. Add
  `jsBrowserCurrentUrl(sessionId)`, reusing the same session-lookup
  liveness check (`requireLiveSession`) every other `jsBrowser*` function
  already uses, but skipping tree construction entirely — it does nothing
  but return `session.page.url()`. `WasmBrowser::type_secret` (Task 4.3.2d)
  calls this binding first, then performs the domain match in Rust — the
  same exact-host comparison Story 3.4.3a's native check uses — and returns
  `PortError::CredentialDomainMismatch(...)` without ever calling
  `self.credential_store.resolve(...)` on a mismatch. Only on a match does
  `WasmBrowser::type_secret` proceed to resolve and then call
  `jsBrowserTypeSecret` (Task 4.3.2a) to perform the DOM write —
  `jsBrowserTypeSecret` itself no longer does any domain checking; it
  receives an already-resolved, already-domain-checked plaintext value, same
  as it does today.
- Files: `crates/wasm/src/glue/browser.js`, `crates/wasm/src/browser.rs`

##### Task 4.3.3b: Tests for the AC above (~5 min)
- Node test: mock `session.page.url()` and assert `jsBrowserCurrentUrl`
  returns it verbatim, with no tree-building side effect.
- Rust test: with a mismatched URL returned from the mocked
  `jsBrowserCurrentUrl` binding, assert `WasmBrowser::type_secret` returns
  `PortError::CredentialDomainMismatch` and the fake `CredentialStore`'s
  `resolve` call count is 0 — mirroring Task 3.4.3b's native assertion
  shape, but at the Rust/`WasmBrowser` layer rather than inside
  `jsBrowserTypeSecret`, since the check no longer lives there.
- Files: `crates/wasm/src/glue/browser.js` (or its sibling test file),
  `crates/wasm/src/browser.rs`

---

## Phase 5: Tool-Layer Wiring, MCP Schema, Observability

### Epic 5.1: Schema types

#### Story 5.1.1: `CredentialFieldInput` enum + `CredentialRefInput` + `BrowserTypeSecretInput`
**As** the MCP tool schema, **I want** `field` to be a closed enum and
`domain`'s doc comment to state exact matching semantics, **so that** the
calling LLM can't invent a plausible-but-wrong field name or misunderstand
what `domain` means.
**Acceptance Criteria**:
- `field`'s JSON Schema is a closed enum of exactly 3 values.
  - *Given* `CredentialFieldInput`'s generated `schemars` JSON Schema, *When*
    inspected, *Then* it is an enum containing exactly `["username",
    "password", "totp"]` (via `#[serde(rename_all = "snake_case")]`), no
    open string type.
**Files**: `crates/core/src/schema.rs`

##### Task 5.1.1a: `CredentialFieldInput` enum (~3 min)
- ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
  #[serde(rename_all = "snake_case")]
  pub enum CredentialFieldInput {
      Username,
      Password,
      Totp,
  }
  ```
- Named distinctly from `crates/core/src/ports.rs`'s port-level
  `CredentialField` (Task 1.1.1b) — mirrors the existing
  `BrowserHistoryAction` (wire, `schema.rs`) / `HistoryAction` (port,
  `ports.rs`) split exactly: same 3-way vocabulary, two types, one
  exhaustive conversion (Task 5.2.1b). Also mirrors `BrowserFormFieldType`'s
  existing shape/doc-comment convention.
- Files: `crates/core/src/schema.rs`

##### Task 5.1.1b: `CredentialRefInput` with `ux.md`-worded `domain` doc comment (~3 min)
- ```rust
  #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
  #[serde(rename_all = "camelCase")]
  pub struct CredentialRefInput {
      /// The site's domain (e.g. "github.com") — matched against the
      /// current page's origin. Wrong or ambiguous matches are rejected,
      /// never guessed.
      pub domain: String,
      /// Which credential to type. Field names must match 1Password's
      /// field labels for the item; `totp` requests the item's
      /// current TOTP/2FA code (generated fresh by 1Password, never
      /// cached).
      pub field: CredentialFieldInput,
  }
  ```
- Files: `crates/core/src/schema.rs`

##### Task 5.1.1c: `BrowserTypeSecretInput` (~3 min)
- ```rust
  #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
  #[serde(rename_all = "camelCase")]
  pub struct BrowserTypeSecretInput {
      pub session_id: String,
      /// A `ref` from a previous `AxSnapshotOutput`.
      pub ref_id: String,
      pub credential: CredentialRefInput,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub timeout_seconds: Option<u32>,
  }
  ```
- Files: `crates/core/src/schema.rs`

### Epic 5.2: `browser_type_secret` tool function

#### Story 5.2.1: `crates/core/src/tools/credential.rs`
**As** the daemon, **I want** a tool-layer function bridging
`BrowserDriver::type_secret` to the wire schema, **so that** it mirrors
`tools/browser.rs`'s existing bridging role for every other browser tool.
**This function calls `BrowserDriver::type_secret` exactly once and never
calls `CredentialStore::resolve` itself** — resolution happens inside the
adapter (Story 3.4.0/4.3.0's injected `CredentialStore` handle), per Task
1.3.1a's trait doc and ADR-001. A separate, tool-layer-owned resolve call
would (a) double the vault round-trips per `type_secret` invocation,
worsening the exact rate-limit exposure ADR-003 exists to mitigate, and (b)
bypass the adapter's in-flight dedup map entirely, since dedup only sees
requests that go through the adapter's own `CredentialStore` handle.
**Acceptance Criteria**:
- A successful call always sets `note` to the fixed success string.
  - *Given* a `BrowserDriver` fake whose `type_secret` succeeds, *When*
    `browser_type_secret` is called, *Then* the returned
    `BrowserActionOutput.note` is exactly `Some("value redacted; typed
    successfully".to_string())` — per `ux.md` §2 point 5, since every other
    `type`-family tool returns the real post-type value and an LLM
    pattern-matching off that convention needs the explicit signal.
- Each of the 5 vault error variants surfaces with its exact `ux.md`
  verbatim string, passed through (not re-wrapped), mirroring `map_error`'s
  existing `NotFound`/`SessionCrashed` convention.
  - *Given* a `BrowserDriver` fake whose `type_secret` returns
    `PortError::CredentialExpired("credential expired: TOTP code for
    example.com expired before it could be typed (generated {t}, window
    closed {t+30s}) — not typed. Retry the same call; a fresh code will be
    generated.".to_string())` (the adapter's own internal
    `self.credential_store.resolve(...)` call is what produced this variant
    — `browser_type_secret` only ever sees it as `type_secret`'s return
    value, never by calling `resolve` itself), *When* `browser_type_secret`
    maps the error, *Then* the returned `Err(String)` is exactly that
    string, unmodified.
- `browser_type_secret` never calls `CredentialStore::resolve`.
  - *Given* the function's full body, *When* read, *Then* it references
    `CredentialStore` nowhere — its only generic bound is `B: BrowserDriver`,
    and its only vault-touching call is the single `browser.type_secret(...)`
    invocation.
**Files**: `crates/core/src/tools/credential.rs` (new), `crates/core/src/tools/mod.rs`

##### Task 5.2.1a: Create the file + function signature (~4 min)
- ```rust
  pub async fn browser_type_secret<B: BrowserDriver>(
      browser: &B,
      input: BrowserTypeSecretInput,
  ) -> Result<BrowserActionOutput, String> { ... }
  ```
  Single generic bound (`B: BrowserDriver`) — no `CredentialStore` parameter,
  deliberately, per this story's header note.
- Input validation mirrors every other tool fn in `tools/browser.rs`
  (`sessionId`/`refId` non-empty checks).
- Add `pub mod credential;` to `crates/core/src/tools/mod.rs`.
- Files: `crates/core/src/tools/credential.rs`, `crates/core/src/tools/mod.rs`

##### Task 5.2.1b: Map `CredentialRefInput` → `CredentialRef`, call `browser.type_secret` once, map errors (~5 min)
- Convert `input.credential.field: CredentialFieldInput` (wire) to
  `CredentialField` (port) via an exhaustive match, mirroring
  `browser_history`'s existing `BrowserHistoryAction → HistoryAction`
  conversion (`crates/core/src/tools/browser.rs:764-766`) exactly:
  ```rust
  let field = match input.credential.field {
      CredentialFieldInput::Username => CredentialField::Username,
      CredentialFieldInput::Password => CredentialField::Password,
      CredentialFieldInput::Totp => CredentialField::Totp,
  };
  ```
  Both enums have exactly 3 variants and this match has no wildcard arm, so
  there is no "unrecognized field string" case to handle here — the wire
  enum's `schemars`/`serde` derives already reject any value outside the 3
  legal ones at JSON-RPC deserialization time, before this function is ever
  called, and this match's own exhaustiveness is compiler-enforced against
  drift between the two enums. (This resolves architecture-review's Concern
  C5 by construction, not by adding a dead runtime error branch for a case
  that cannot occur.)
- Build the `CredentialRef{domain, field}` and pass it straight into
  `browser.type_secret(&session_id, &locator, &credential_ref, timeout)` —
  this is the ONE vault-touching call this function makes; there is no
  separate `resolve()` call before or after it.
- Each of the 5 `PortError` vault variants' inner `String` (surfaced via
  `type_secret`'s own `Result`, since resolution happened inside it) is
  passed through verbatim (per the AC), mirroring `map_error`'s existing
  `NotFound`/`SessionCrashed` pass-through logic — every other `PortError`
  variant falls to a generic `format!("type secret for {domain}: {other}")`
  wrap.
- Files: `crates/core/src/tools/credential.rs`

##### Task 5.2.1c: Set `note` unconditionally on success (~3 min)
- Files: `crates/core/src/tools/credential.rs`

### Epic 5.3: Tool registration + description

#### Story 5.3.1: Register `stapler_browser_type_secret`
**As** the calling LLM, **I want** the tool description to lead with the
trigger condition, **so that** I reach for it instead of
`stapler_browser_type` on a login form.
**Acceptance Criteria**:
- The description's first sentence names the trigger condition, not the type
  shape.
  - *Given* the `#[tool(...)]` macro's `description` string for
    `stapler_browser_type_secret`, *When* read, *Then* it begins with "Use
    this instead of `stapler_browser_type` whenever a field is a password,
    TOTP/2FA code, or other secret you have a stored credential for" (per
    `ux.md` §2 point 1, mirroring `stapler_browser_fill_form`'s existing
    when-to-use-first convention).
**Files**: `crates/cli/src/thin_client.rs`

##### Task 5.3.1a: Add the `#[tool(...)]` registration (~4 min)
- ```rust
  #[tool(
      name = "stapler_browser_type_secret",
      description = "Use this instead of stapler_browser_type whenever a field is a password, TOTP/2FA code, or other secret you have a stored credential for. Types a credential resolved server-side from the daemon's configured vault into an element in an existing browser session, identified by a `ref` from a previous snapshot — the credential value never appears in this tool's request or in any returned accessibility-tree snapshot (a fixed [REDACTED] placeholder takes its place). Returns the accessibility-tree snapshot after typing, same as stapler_browser_type, with note set to confirm success since the visible value won't change to show it."
  )]
  async fn browser_type_secret(
      &self,
      params: Parameters<BrowserTypeSecretInput>,
  ) -> Result<Json<BrowserActionOutput>, String> {
      call_daemon("stapler_browser_type_secret", params.0).await.map(Json)
  }
  ```
- Files: `crates/cli/src/thin_client.rs`

### Epic 5.4: Observability

#### Story 5.4.1: Structured per-attempt log line
**As** the operator, **I want** one grep-able log line per resolve attempt
naming the domain/field/outcome in plain text, **so that** `grep <domain>`
against the daemon log answers "did the agent ever touch this credential."
**Acceptance Criteria**:
- A rejected attempt logs identically to a successful one — both are
  audit-worthy (per `ux.md` §3).
  - *Given* a `resolve()` call that fails with `CredentialDomainMismatch`,
    *When* the call completes, *Then* one `eprintln!` line is emitted
    containing the literal substrings `"example.com"` (the domain),
    `"password"` (the field), and `"rejected-domain-mismatch"` (the
    outcome) — never the resolved value (there is none to leak in this
    case, but the same line shape is used for a success too, where the
    value is still never included).
**Files**: `crates/native/src/vault.rs`, `crates/wasm/src/glue/vault.js`

##### Task 5.4.1a: `eprintln!` in `NativeCredentialStore::resolve` (~3 min)
- Mirrors `webcrawl.rs:266`'s exact style: `eprintln!("stapler-mcp: credential
  resolve domain='{domain}' field='{field}' outcome={outcome}");` at every
  return point (success and each error variant).
- Files: `crates/native/src/vault.rs`

##### Task 5.4.1b: Equivalent `console.error` in `vault.js` (~3 min)
- Same line shape, JS-side, for `WasmCredentialStore`.
- Files: `crates/wasm/src/glue/vault.js`

##### Task 5.4.1c: Unit test asserting the log line's required substrings (~3 min)
- Capture `eprintln!` output via a test-only writer seam (check whether
  `webcrawl.rs`'s own SSRF-deny-log test, if one exists, already has a
  pattern for this — reuse it rather than inventing a new capture mechanism).
- Files: `crates/native/src/vault.rs`

---

## Phase 6: Daemon Wiring / Opt-In Feature Flag

### Epic 6.1: `CredentialStore` construction at startup

#### Story 6.1.1: Native `main.rs` opt-in wiring
**As** the daemon, **I want** `CredentialStore` to exist only when
`OP_SERVICE_ACCOUNT_TOKEN` is present, **so that** the feature is
infrastructure-level opt-in per `requirements.md`'s Risk Control section.
**Acceptance Criteria**:
- No token present → the tool is registered but always returns the
  not-configured error, without touching `BrowserDriver` or constructing a
  `NativeCredentialStore` at all.
  - *Given* a daemon started with `OP_SERVICE_ACCOUNT_TOKEN` unset, *When*
    `stapler_browser_type_secret` is called, *Then* the response is
    `Err("vault not configured: set OP_SERVICE_ACCOUNT_TOKEN in the
    daemon's environment and restart")`, and no `NativeCredentialStore`
    instance exists in the running daemon (verified structurally:
    `browser.set_credential_store` is never called in this branch, so
    `NativeBrowser`'s own `credential_store` field stays `None`).
- Token present → the real path is wired.
  - *Given* a daemon started with `OP_SERVICE_ACCOUNT_TOKEN` set, *When*
    `stapler_browser_type_secret` is called with a valid `CredentialRef`,
    *Then* the call reaches `NativeBrowser::type_secret`, which internally
    calls its injected `NativeCredentialStore::resolve` (Story 3.4.0) — the
    tool layer itself never calls `resolve` directly.
**Files**: `crates/cli/src/main.rs`

##### Task 6.1.1a: Read the token via `EnvPort`, construct and inject `NativeCredentialStore` into `browser` (~5 min)
- Construct the store first, then inject it into the already-constructed
  `NativeBrowser` via Story 3.4.0's setter — the daemon never holds a
  separate long-lived handle to `credential_store` for calling `resolve()`
  itself; the handle exists only so the registration branch below can decide
  whether the tool is wired at all:
  ```rust
  let credential_store_present = stapler_mcp_core::ports::EnvPort::var(&env, "OP_SERVICE_ACCOUNT_TOKEN")
      .map(|token| {
          let store: Rc<dyn stapler_mcp_core::ports::CredentialStore> =
              Rc::new(stapler_mcp_native::NativeCredentialStore::new(
                  stapler_mcp_native::NativeSpawner, token,
              ));
          browser.set_credential_store(store);
      })
      .is_some();
  ```
  placed alongside the existing `let http = ...`/`let fs = ...` construction
  block, after `browser` itself is constructed.
- Files: `crates/cli/src/main.rs`

##### Task 6.1.1b: Register `stapler_browser_type_secret`, branching only on presence (~4 min)
- The registration branch now captures only `browser` (already holding the
  injected store, or not) — no separate `vault` capture, matching Story
  5.2.1's single-`BrowserDriver`-argument tool function:
  ```rust
  daemon.register(
      "stapler_browser_type_secret",
      if credential_store_present {
          json_handler({
              let browser = browser.clone();
              move |input: BrowserTypeSecretInput| {
                  let browser = browser.clone();
                  async move { credential::browser_type_secret(&*browser, input).await }
              }
          })
      } else {
          json_handler(|_input: BrowserTypeSecretInput| async {
              Err("vault not configured: set OP_SERVICE_ACCOUNT_TOKEN in the daemon's environment and restart".to_string())
          })
      },
  );
  ```
- Files: `crates/cli/src/main.rs`

#### Story 6.1.2: Wasm entry-point opt-in wiring
**As** the wasm daemon entry point, **I want** the identical opt-in behavior,
**so that** both distributions have the same Risk Control guarantee.
**Acceptance Criteria**:
- Same two ACs as Story 6.1.1, exercised against the wasm entry point.
**Files**: `crates/wasm/src/lib.rs`

##### Task 6.1.2a: Mirror Task 6.1.1a's construction + injection in `crates/wasm/src/lib.rs` (~4 min)
- `WasmBrowser` (`crates/wasm/src/browser.rs:89`) is today a zero-field unit
  struct (`pub struct WasmBrowser;`) — Story 4.3.0 gives it a
  `credential_store: RefCell<Option<Rc<dyn CredentialStore>>>` field and
  setter mirroring Story 3.4.0's native shape exactly. This task wires
  `let browser = Rc::new(browser::WasmBrowser::new())` (or `::default()`,
  implementer's choice) followed by the same
  `EnvPort`-token-present-then-`set_credential_store` pattern as Task
  6.1.1a, before `browser` is passed to `daemon.register`'s other closures.
- Files: `crates/wasm/src/lib.rs`, `crates/wasm/src/browser.rs`

##### Task 6.1.2b: Mirror Task 6.1.1b's registration branch (~4 min)
- Same single-`browser`-handle registration shape, no separate `vault`
  capture.
- Files: `crates/wasm/src/lib.rs`

##### Task 6.1.2c: Integration test — no token, tool call, not-configured error, zero vault/browser touch (~5 min)
- Files: `crates/cli/tests/` (whichever existing integration test file covers
  browser-session tool registration, e.g. alongside
  `browser_session.rs`'s existing conventions — extend rather than create a
  new file if one fits)

---

## Phase 7: Cross-Cutting Acceptance & Verification

Requires Phase 2 (redaction) and Phase 6 (wiring) both complete.

### Epic 7.1: Success-metrics acceptance tests

Each story below maps directly to one bullet of `requirements.md`'s Success
Metrics section.

#### Story 7.1.1: Request payload never contains a value
**As** a reviewer verifying the feature's core guarantee, **I want** an
automated check that the JSON-RPC request the daemon receives for
`stapler_browser_type_secret` structurally cannot carry a secret, **so that**
this isn't just asserted by code inspection.
**Acceptance Criteria**:
- `BrowserTypeSecretInput` has no field capable of holding a secret value.
  - *Given* `BrowserTypeSecretInput`'s field list (`session_id`, `ref_id`,
    `credential: CredentialRefInput{domain, field}`, `timeout_seconds`),
    *When* enumerated, *Then* none is a free-text value field — `credential`
    is the only credential-shaped field, and it's `{domain: String, field:
    CredentialFieldInput}`, neither of which is documented or usable as a
    secret carrier.
**Files**: `crates/core/src/schema.rs` (test module)

##### Task 7.1.1a: Structural test enumerating `BrowserTypeSecretInput`'s fields (~4 min)
- Files: `crates/core/src/schema.rs`

#### Story 7.1.2: Autofill-out-of-band leak test (native + wasm)
**As** a reviewer verifying the redaction guarantee closes the pre-existing
leak too, **I want** the literal test `requirements.md`'s Success Metrics
section names, **so that** the autofill case (not just the `type_secret`
case) is proven closed.
**Acceptance Criteria**:
- An out-of-band-autofilled password field never appears in a snapshot.
  - *Given* a test page where a password field's value is set directly via
    `page.evaluate()`/CDP (simulating browser autofill, never via
    `type_secret` or `type_text`), *When* `stapler_browser_snapshot` is
    called against that session, *Then* the field's `value` in the response
    is `"[REDACTED]"`, not the autofilled value.
**Files**: `crates/native/tests/` (or wherever browser integration tests
live — confirm the exact directory before creating a new file),
`crates/wasm/src/glue/browser.js` (Node test)

##### Task 7.1.2a: Native integration test (~5 min)
- Files: `crates/native/tests/`

##### Task 7.1.2b: Wasm Node test (~5 min)
- Files: `crates/wasm/src/glue/browser.js` test file

#### Story 7.1.3: TOTP end-to-end (gated, requires a real/test 1Password vault)
**As** a reviewer verifying the TOTP flow works with a real 1Password
service account, **I want** a gated integration test that only runs when
`OP_SERVICE_ACCOUNT_TOKEN` (and a designated test item) is available, **so
that** CI without vault access doesn't fail, while a developer/CI-with-secrets
run does exercise the real path.
**Acceptance Criteria**:
- Skips cleanly without the token; exercises the real flow with it.
  - *Given* `OP_SERVICE_ACCOUNT_TOKEN` is unset in the test environment,
    *When* the test runs, *Then* it reports skipped, not failed.
  - *Given* the token is set and points at a designated test vault item with
    a TOTP field, *When* `type_secret` is called with `field: "totp"`
    against a test OTP input, *Then* the field's live DOM value (read via a
    separate, test-only `evaluate` call — not through the redacted snapshot)
    matches a currently-valid TOTP code for that item.
**Files**: `crates/native/tests/`, `crates/wasm/src/glue/browser.js` test file

##### Task 7.1.3a: Native gated test (~5 min)
- Files: `crates/native/tests/`

##### Task 7.1.3b: Wasm gated test (~5 min)
- Files: `crates/wasm/src/glue/browser.js` test file

#### Story 7.1.4: Native/wasm parity table
**As** a reviewer verifying "same wire shape on both adapters," **I want** a
single shared fixture table (domain/field/expected-outcome triples) run
against both adapters' `resolve()`/`type_secret`, **so that** parity is
proven by one shared test definition, not two independently-written suites
that could silently diverge.
**Acceptance Criteria**:
- Every fixture row produces the identical `PortError` variant (by discriminant,
  not exact string, since the two adapters' underlying tools produce
  different raw text) on both adapters.
  - *Given* the fixture row `{domain: "nomatch.example", field: "password",
    expect: CredentialDomainMismatch}`, *When* run against both
    `NativeCredentialStore` (with a canned `op` fixture) and
    `WasmCredentialStore` (with a canned JS-mock fixture), *Then* both
    return `PortError::CredentialDomainMismatch(_)`.
**Files**: a shared fixture definition (format TBD by implementer — plain
Rust data usable from `crates/native`'s test module, with a hand-maintained
JS mirror for `crates/wasm`'s Node tests, since the two test harnesses don't
share a runtime)

##### Task 7.1.4a: Define the fixture table (~4 min)
- Files: `crates/native/src/vault.rs` (test module) or a new shared test-data
  file, implementer's choice given the cross-language constraint above

##### Task 7.1.4b: Run it against both adapters (~5 min)
- Files: `crates/native/src/vault.rs`, `crates/wasm/src/glue/browser.js` (or
  `vault.js`) test files
