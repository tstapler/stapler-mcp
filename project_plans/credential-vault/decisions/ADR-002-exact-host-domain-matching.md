# ADR-002: `CredentialRef.domain` matches by exact host-string equality, not eTLD+1

**Status**: Accepted
**Date**: 2026-09-08

## Context

`CredentialRef.domain` must be checked against the page a `type_secret` call
is actually acting on, to stop a credential from ever being typed into the
wrong site. Two research passes gave apparently conflicting guidance:

- `research/build-vs-buy.md` gives a blanket recommendation that domain
  matching should use `psl`/`publicsuffix` for eTLD+1-aware comparison (e.g.
  treating `accounts.example.com` and `example.com` as related, or correctly
  handling multi-part public suffixes like `co.uk`).
- `research/architecture.md` §7 recommends **exact host-string equality**,
  citing this codebase's own existing precedent: the SSRF guard's `same_host`
  (`crates/core/src/tools/webcrawl.rs:125-126`) is `a.host_str() == b.host_str()`
  — no suffix logic at all — and its one suffix-matching case (`.localhost`)
  does it explicitly, with a documented leading-dot bug this codebase already
  had to fix once (`webcrawl.rs:248-250`'s IPv6-bracket comment records a
  related class of exact-match footgun).

## Decision

`CredentialRef.domain` matching uses **exact host-string equality**, sourced
from the daemon's own freshly-queried live navigation URL for the active tab
(never a caller-supplied or cached URL — see the staleness-bug precedent in
`6b6b56a`, "race SSRF redirect detection against goto"). It reuses (or
extracts a shared helper from) the SSRF guard's `same_host`, rather than a
second, independently-written comparison. No `psl`/`publicsuffix` dependency
is added.

This resolves the tension between the two research passes explicitly:
`build-vs-buy.md`'s recommendation is generic guidance not grounded in this
codebase's specific precedent or its specific chosen algorithm;
`architecture.md` §7 is more specific — it accounts for the existing
`same_host` precedent, the "confidential/regulated" NFR classification
(which favors the strictest correct default over a more permissive one), and
the fact that this codebase has already been bitten once by a
suffix/substring-matching bug class (`.localhost`'s leading-dot fix). The
more specific, more grounded recommendation wins.

## Consequences

- A login page hosted on a subdomain that doesn't exactly match the vault
  item's own recorded URL (e.g. `accounts.example.com` vs. `example.com`)
  is rejected, not silently widened — a usability cost, not a correctness
  bug, and the safer failure direction.
- No new dependency (`psl`/`publicsuffix`) is added to any crate.
- The credential-domain guard and the SSRF guard share one comparison
  implementation, so a future "helpful" loosening (e.g. an `.ends_with()`
  added without a dot-boundary guard) has to be caught in one place, not two
  independently-drifting ones.
- 1Password vault items themselves must be scoped precisely — an item saved
  against `www.example.com` will not match a page at `example.com` under
  this rule either (this applies on the vault-item-lookup side identically
  to the page-URL side).

## Alternatives Considered

- **eTLD+1 / public-suffix-aware matching** (`build-vs-buy.md`'s blanket
  recommendation). Rejected for this pass: adds a new dependency and
  correctness surface (public suffix lists require periodic updates) this
  design doesn't need under the exact-match rule, and is a strictly more
  permissive (hence riskier) default for a feature explicitly classified
  confidential/regulated.
