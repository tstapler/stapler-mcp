# ADR-003: No persistent `SecretValue` cache — in-flight dedup only

**Status**: Accepted
**Date**: 2026-09-08

## Context

`requirements.md`'s Risk Control section states credentials are "resolved
fresh from 1Password per call" and there is "no persistent state to unwind."
Taken literally, this means every `type_secret` call — and every retry —
issues its own independent `op`/SDK round-trip.

`research/pitfalls.md` §2a documents a real, specific risk with that literal
reading: 1Password service-account tokens are rate-limited to approximately
15 requests per 10 minutes, **account-wide** (every service account under
the account is blocked together, for 15+ minutes, sometimes longer than
reported). A retry-heavy login flow, or multiple browser tabs/sessions
resolving the *same* `CredentialRef` concurrently with zero caching or
de-duplication, can trip this limit — a real, documented crash-loop pattern
(cited: `openclaw#56217`). Pitfalls.md explicitly recommends this be decided
in planning rather than left to fall out of the literal requirements
wording, and notes that a short-lived in-memory mechanism is not the same
kind of "persistent state" the Risk Control section is actually warning
about (durable, cross-restart state — a Unix socket file, a lockfile, a
written credential — none of which this design has either way).

## Decision

Two separate caching questions get two separate answers:

1. **Vault/item *identifier* cache (not secret values): yes.**
   `research/stack.md` notes `op read`/`op item get` cost 3 API requests each
   unless called with vault/item **IDs**, not names. `NativeCredentialStore`
   maintains an in-memory `HashMap<domain, (vault_id, item_id)>`, populated
   on first successful resolve per domain and reused for the rest of the
   daemon's process lifetime. This cache holds only opaque identifiers, never
   a `SecretValue` — it does not weaken the "resolved fresh every call"
   guarantee for the actual secret material.

2. **`SecretValue` cache: no.** No secret value is ever cached or reused
   across calls, for either the password/username or TOTP case — every
   `type_secret` call triggers a real vault round-trip for the value itself,
   preserving `requirements.md`'s literal guarantee for the thing that
   guarantee is actually about.

3. **In-flight de-duplication: yes, as the rate-limit mitigation.** If two or
   more `resolve()` calls for the *identical* `CredentialRef` are in flight
   concurrently within the same daemon process (e.g. two tabs both driving a
   login to the same site), they share one underlying `op`/SDK call rather
   than each issuing its own. Nothing persists once every waiter has been
   served — this is not a cache in the sense Risk Control is warning about,
   it only prevents *redundant simultaneous* requests. TOTP resolution is
   explicitly excluded from any caching *value*-reuse (per
   `architecture.md` §6: resolve immediately before dispatch, no
   intermediate holding), but is still eligible for in-flight dedup of
   concurrent identical requests, since dedup doesn't change how fresh the
   eventually-typed value is relative to when it was generated.

## Consequences

- The single-caller, single-login-flow common case behaves exactly as
  `requirements.md` describes: fresh resolution every call.
- The specific crash-loop risk pitfalls.md flagged (concurrent/retry-heavy
  resolution of the same credential) is mitigated without introducing
  value-at-rest anywhere in the daemon.
- `requirements.md`'s Risk Control wording ("resolved fresh from 1Password
  per call") is preserved for secret material; the ID cache is a
  deliberate, narrow, explicitly-justified deviation from a maximally
  literal reading, not a silent one — this ADR is that explicit record.
- If the in-flight-dedup mitigation proves insufficient in practice (e.g.
  sequential, not concurrent, retries still trip the limit), a short-TTL
  value cache remains an explicitly available future option per
  `requirements.md`'s own NFR section — this ADR does not foreclose it, it
  only declines to build it now.

## Alternatives Considered

- **Fully stateless, zero dedup** (the most literal reading of
  requirements.md). Rejected: leaves the documented rate-limit crash-loop
  risk for concurrent multi-tab/same-credential resolution completely
  unaddressed, for a cost (an in-memory dedup map) far short of "persistent
  state."
- **Short-TTL value cache** (cache the resolved `SecretValue` itself for a
  few seconds). Rejected for this pass: increases the TOTP-staleness risk
  `architecture.md` §6 already flags (a cached TOTP code sitting unused
  narrows the safety margin against the ~30s window), and deviates from
  requirements' "resolved fresh every call" language more than in-flight
  dedup of literally-concurrent requests does.
