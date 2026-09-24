# Adversarial Review: credential-vault
**Date**: 2026-09-08
**Verdict**: CONCERNS — all 3 original blockers verified fixed (see
Resolved Blockers below); 5 concerns and 3 minors remain open but are not
blocking.

Reviewed: `implementation/plan.md` (full, all 1628 lines), `requirements.md`,
ADR-001/002/003, and all six research files (`architecture.md`,
`stack.md`, `features.md`, `pitfalls.md`, `ux.md`, `build-vs-buy.md`).

## Re-verification (2026-09-08, scoped re-review post repair-pass,
iteration 2)

Re-read the twice-repaired `implementation/plan.md` against the single
remaining blocker (wasm resolve-before-check ordering). It is now
genuinely fixed. Details below.

## Blockers

(none remaining)

## Resolved Blockers (verified 2026-09-08)

- [x] **Wasm's live-URL domain-freshness check was wired *after*
  credential resolution, not before.** Fixed. Task 4.3.2d
  (plan.md:1502-1526) now specifies an explicit, unambiguously ordered
  4-step sequence for `WasmBrowser::type_secret`: (1) call the new
  `jsBrowserCurrentUrl(sessionId)` glue binding to get the live URL —
  "no resolve has happened yet"; (2) domain-match in Rust against
  `credential_ref.domain`, and on mismatch return
  `Err(PortError::CredentialDomainMismatch(...))` immediately with
  `self.credential_store` "never touched"; (3) only on a match, call
  `self.credential_store.resolve(...)`; (4) cross the wasm-bindgen
  boundary a second time into `jsBrowserTypeSecret` with the resolved
  plaintext for the DOM write. This is a numbered, causally-ordered
  sequence (each step's precondition depends on the prior step), not a
  parallel/unordered list — no ambiguity about ordering remains. Task
  4.3.3a (plan.md:1565-1584) confirms `jsBrowserTypeSecret` itself "no
  longer does any domain checking," eliminating the prior
  contradiction where the check nominally lived inside the function
  that ran after resolve. Task 4.3.3b (plan.md:1586-1596) makes the
  guarantee concretely enforceable, not just descriptive: its Rust test
  asserts, against a mismatched URL from a mocked `jsBrowserCurrentUrl`,
  that `WasmBrowser::type_secret` returns
  `PortError::CredentialDomainMismatch` AND "the fake `CredentialStore`'s
  `resolve` call count is 0" — a call-count-spy assertion mirroring
  native's Task 3.4.3b, which would fail the test if implementation ever
  called resolve before/on a mismatch. Confirmed this genuinely mirrors
  native's Story 3.4.3 (plan.md:1226-1250) guarantee: native checks
  `page.url()` synchronously in-process before calling
  `CredentialStore::resolve` in the same function, because its Rust code
  holds the page object directly; wasm has no direct page handle, so it
  necessarily pays an extra JS round-trip (`jsBrowserCurrentUrl`) to get
  the same fresh-URL-before-resolve property — the *mechanism* differs
  (one JS round-trip vs. zero) but the *guarantee* (check-before-resolve,
  reject without ever touching the vault on mismatch) is identical, and
  Task 4.3.2d's own text says so explicitly ("wasm-side equivalent of
  native's Task 3.4.1b/c ordering... steps 1-2 replace native's
  synchronous in-process check"). Also re-scanned all of Phase 4
  (plan.md:1254-1929) and Phase 7 (plan.md:1930-2029) for any other
  reference to the old resolve-then-check ordering: no other Phase 4
  story references a conflicting order (Story 5.2.1, plan.md:1670-1708,
  independently reaffirms the tool layer never calls `resolve` at all),
  and spot-checking the repair agent's claim about Phase 7 — Stories
  7.1.1 (payload-shape test), 7.1.2 (autofill leak test), 7.1.3 (gated
  TOTP e2e test), and 7.1.4 (parity table asserting matching
  `PortError` discriminants) — confirmed none of the four says anything
  about call ordering between resolve and the domain check; they are
  genuinely ordering-agnostic as claimed.

- [x] **`CredentialStore::resolve` double-call / trait-signature
  contradiction.** Fixed. Task 1.3.1a's trait doc (plan.md:393-425) now
  states explicitly that the implementing adapter resolves
  `credential_ref` internally via its own injected `CredentialStore`, and
  that the tool layer calls `type_secret` exactly once and never calls
  `resolve` itself. Story 3.4.0 (native, plan.md:1104-1140) and Story
  4.3.0 (wasm, plan.md:1409-1430) add a `credential_store` field +
  setter to `NativeBrowser`/`WasmBrowser` respectively, injected by the
  daemon. The rewritten Story 5.2.1 (plan.md:1630-1717) makes this
  explicit in its header and ACs — `browser_type_secret` has only a `B:
  BrowserDriver` bound, no `CredentialStore` parameter, and one
  vault-touching call. Tasks 6.1.1a/b (native, plan.md:1814-1857) and
  6.1.2a/b/c (wasm, plan.md:1866-1887) have the daemon construct the
  store and inject it via the adapter's setter, never holding it to call
  `resolve` directly. Traced the full call path end to end
  (tool handler → `BrowserDriver::type_secret` → adapter-internal
  `CredentialStore::resolve` → DOM write) and found no remaining task
  where the tool handler calls `CredentialStore::resolve` directly.

- [x] **Missing in-flight de-duplication (ADR-003 item 3).** Fixed for
  both adapters. Story 3.2.5 (plan.md:983-1050) adds a real
  `pending: RefCell<HashMap<CredentialRef, Shared<...>>>` coalescing map
  to `NativeCredentialStore` (`crates/native/src/vault.rs`), distinct
  from Story 3.2.3's separate ID cache, with ACs asserting exactly-1
  underlying `op` call for concurrent identical requests and exactly-2
  for distinct ones, and confirming the map holds nothing once every
  waiter is served (dedup, not a cache). Story 4.2.2 (plan.md:1361-1405)
  mirrors it in `crates/wasm/src/glue/vault.js` with a module-level
  `pending` `Map` keyed by `` `${domain}::${field}` `` inside
  `jsResolveCredential`, same three ACs. Because Blocker 1's fix routes
  every resolve request through the one adapter-owned `CredentialStore`
  (`NativeCredentialStore::resolve` / `WasmCredentialStore::resolve` →
  `jsResolveCredential`), and both dedup maps live inside those exact
  `resolve()`/`jsResolveCredential` functions, the dedup logic sees every
  resolve call — there is no other path into the vault that could bypass
  it.

## Concerns

- [ ] **Dispatch-time refusal check (Task 3.4.1b) uses a narrower key than
  the plan's own redaction key.** For `field in {"password","username"}` it
  requires `type == "password"` only — it does not accept `autocomplete` in
  `{current-password, new-password}`, even though Epic 2.1/2.2/2.3 (per
  `architecture.md` §6's central finding) treat those `autocomplete` values
  as equally password-shaped for redaction purposes. A legitimate password
  field implemented as `type="text" autocomplete="current-password"` would
  be refused by `type_secret` even though the plan's own redaction logic —
  and real password-manager convention — recognizes it as a password
  field. Widen Task 3.4.1b's password/username branch to match the
  redaction key, or explicitly justify the narrower check. (Spot-checked
  2026-09-08: still open — Task 3.4.1b, plan.md:1178-1186, still requires
  `type == "password"` only for the Password/Username branch; the
  structural redaction key at plan.md:63/91 is `type == "password"` OR
  `autocomplete` in `{one-time-code, current-password, new-password}`.
  Not fixed by the repair pass.)

- [ ] **`CredentialUnauthenticated`'s stderr pattern (Task 3.2.4a) is only
  verified against the *interactive* "not currently signed in" message**
  (`ux.md`'s local verification ran `op vault list` with no active
  interactive session). `pitfalls.md` §2b's finding is specifically about
  *service-account* token expiry/revocation, which is only observable
  reactively and whose actual stderr text was never confirmed. Verify the
  real failure text for an expired/revoked `OP_SERVICE_ACCOUNT_TOKEN`
  before locking in the pattern-match strings in Task 3.2.4a/c — a mismatch
  here means a token-expiry failure silently falls through to a generic
  error instead of the actionable "re-provision the token" message the
  requirements call for.

- [ ] **No fallback plan if the `op item list` domain-filtering spike
  (Unresolved Questions #1, blocking Story 3.3.1) turns out to be
  impractical at real-vault scale.** The plan correctly calls this out as
  an explicit blocking spike, but records no contingency (e.g. a
  server-side filter flag, a different listing strategy, or a
  scope-narrowing fallback) if list-then-filter proves too slow or
  rate-costly — only "spike... before writing the final filtering logic."

- [ ] **No wasm-side vault/item ID cache is planned**, unlike native's
  Story 3.2.3 (ADR-003 item 1). Epic 4.2 covers the client singleton
  (Task 4.2.1a) and domain-scoped item listing (Task 4.2.1c) but never
  states whether `WasmCredentialStore` needs (or explicitly doesn't need,
  given a possibly different SDK cost model) the same per-domain identifier
  cache — leaving open whether every wasm resolve re-pays a full
  list-and-filter cost that native explicitly avoids after the first call.
  (Spot-checked 2026-09-08: still open — Epic 4.2 (plan.md:1301-1406) now
  has Story 4.2.1 and the new Story 4.2.2 (in-flight dedup), but still no
  wasm-side vault/item ID cache story. Story 4.2.2 is a different
  mechanism — in-flight coalescing, not a persistent ID cache — so it
  doesn't incidentally cover this. Not fixed by the repair pass.)

- [ ] **The "only probe form-control-shaped roles" optimization creates a
  redaction blind spot the fail-safe invariant doesn't cover.** Task
  2.2.1b (native) and Task 2.3.1b (wasm) both limit the type/autocomplete
  probe to nodes whose `role` "suggests a form control," to avoid probing
  every generic/button node. A password-type input with an unusual or
  script-overridden ARIA role would never be probed *at all* — not
  probed-and-failed, simply skipped — so it would never be redacted. The
  documented fail-safe invariant ("redact when uncertain," `pitfalls.md`
  §3c) only protects nodes that get probed and come back ambiguous; it
  says nothing about nodes filtered out of probing before that point.

## Minors

- Pattern Decisions cites "features.md §2.5 policy (a)" as the source for
  the chosen refuse-on-dispatch policy without noting that `features.md`
  §2.5 itself argued policy (b) (permit-but-redact) was "the better
  default." The override is defensible (matches the "never silent"
  ethos), but the citation reads as though the research recommended the
  chosen policy when it recommended the opposite — worth a one-line
  acknowledgment that this is a deliberate departure from the research's
  own preference.
- Story 2.2.3's headless-vs-headed empirical AX-masking test
  (`pitfalls.md` §3b) has no wasm-side equivalent, and the plan doesn't
  note whether that's because wasm's non-`getFullAXTree` approach makes
  the question inapplicable there or because it was simply not carried
  over.
- Story 7.1.4's cross-adapter parity table only checks `PortError`
  discriminants from `resolve()`, not the full `type_secret`
  redaction/dispatch-refusal behavior end-to-end — a narrower notion of
  "parity" than requirements' "same wire shape on both adapters" metric
  implies.
