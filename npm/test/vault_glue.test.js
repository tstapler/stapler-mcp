// Node-harness unit tests for `crates/wasm/src/glue/vault.js` (Epic 4.2) —
// exercises the pure/mockable pieces against a hand-built fake
// `@1password/sdk` client injected via `__setClientFactoryForTesting`, never
// the real SDK/network. See `npm/test/vault_spike.test.js` for the Task
// 4.1.1 feasibility spike against the real installed SDK.

const test = require("node:test");
const assert = require("node:assert");
const path = require("node:path");

const vaultGlue = require(path.join(__dirname, "..", "..", "crates", "wasm", "src", "glue", "vault.js"));

test.beforeEach(() => {
    vaultGlue.__resetForTesting();
});

// Builds one `ItemOverview`-shaped fixture (the shape `items.list()` returns)
// with a single website URL, so every test below constructs its Login-item
// fixtures the same way rather than repeating the object literal.
function loginItem(id, vaultId, title, url) {
    return { id, vaultId, title, category: "Login", websites: [{ url }] };
}

// A single vault ("vault1") holding one Login item ("item1", "Example
// Login") whose website matches "example.com" — the shared fixture most
// tests below start from.
function singleMatchClient(overrides = {}) {
    return {
        vaults: { list: async () => [{ id: "vault1" }] },
        items: {
            list: async () => [loginItem("item1", "vault1", "Example Login", "https://example.com/login")],
            get: async () => ({
                fields: [
                    { id: "username", value: "alice" },
                    { id: "password", value: "hunter2" },
                ],
            }),
        },
        secrets: { resolve: async () => "should not be called for non-totp fields" },
        ...overrides,
    };
}

test("js_resolve_credential_should_call_secrets_resolve_with_otp_attribute_when_field_is_totp", async () => {
    const resolveCalls = [];
    const client = singleMatchClient({
        secrets: {
            resolve: async (ref) => {
                resolveCalls.push(ref);
                return "123456";
            },
        },
    });
    vaultGlue.__setClientFactoryForTesting(async () => client);

    const value = await vaultGlue.jsResolveCredential("example.com", "totp");

    assert.strictEqual(value, "123456");
    assert.strictEqual(resolveCalls.length, 1);
    assert.strictEqual(resolveCalls[0], "op://vault1/item1/totp?attribute=otp");
});

test("js_resolve_credential_should_resolve_password_field_from_item_get_when_field_is_password", async () => {
    const client = singleMatchClient();
    vaultGlue.__setClientFactoryForTesting(async () => client);

    const value = await vaultGlue.jsResolveCredential("example.com", "password");

    assert.strictEqual(value, "hunter2");
});

test("js_resolve_credential_should_reject_when_no_item_matches_domain", async () => {
    const client = {
        vaults: { list: async () => [{ id: "vault1" }] },
        items: {
            list: async () => [loginItem("item1", "vault1", "Other Login", "https://other.example/login")],
            get: async () => {
                throw new Error("should not be called on a domain mismatch");
            },
        },
        secrets: { resolve: async () => {
            throw new Error("should not be called on a domain mismatch");
        } },
    };
    vaultGlue.__setClientFactoryForTesting(async () => client);

    await assert.rejects(
        () => vaultGlue.jsResolveCredential("example.com", "password"),
        (err) => {
            assert.match(err.message, /no vault entry for domain "example\.com"/);
            return true;
        },
    );
});

test("js_resolve_credential_should_reject_ambiguous_when_two_items_match_domain", async () => {
    const client = {
        vaults: { list: async () => [{ id: "vault1" }] },
        items: {
            list: async () => [
                loginItem("item1", "vault1", "Example Login A", "https://example.com/a"),
                loginItem("item2", "vault1", "Example Login B", "https://example.com/b"),
            ],
            get: async () => {
                throw new Error("should not be called on an ambiguous match");
            },
        },
        secrets: { resolve: async () => {
            throw new Error("should not be called on an ambiguous match");
        } },
    };
    vaultGlue.__setClientFactoryForTesting(async () => client);

    await assert.rejects(
        () => vaultGlue.jsResolveCredential("example.com", "password"),
        (err) => {
            assert.match(err.message, /2 vault items match domain "example\.com" — ambiguous, not typed/);
            assert.match(err.message, /Example Login A/);
            assert.match(err.message, /Example Login B/);
            return true;
        },
    );
});

// -- Story 4.2.1: client singleton --

test("js_resolve_credential_should_construct_vault_client_at_most_once_across_multiple_resolves", async () => {
    let createCount = 0;
    vaultGlue.__setClientFactoryForTesting(async () => {
        createCount += 1;
        return singleMatchClient();
    });

    await vaultGlue.jsResolveCredential("example.com", "username");
    await vaultGlue.jsResolveCredential("example.com", "password");

    assert.strictEqual(createCount, 1);
});

// -- Story 4.2.2: in-flight dedup (ADR-003 item 3) --

test("js_resolve_credential_should_issue_one_sdk_call_when_two_concurrent_resolves_share_same_key", async () => {
    let listCalls = 0;
    let releaseGate;
    const gate = new Promise((resolve) => {
        releaseGate = resolve;
    });
    const client = singleMatchClient({
        items: {
            list: async () => {
                listCalls += 1;
                await gate;
                return [loginItem("item1", "vault1", "Example Login", "https://example.com/login")];
            },
            get: async () => ({ fields: [{ id: "password", value: "hunter2" }] }),
        },
    });
    vaultGlue.__setClientFactoryForTesting(async () => client);

    // Both calls are issued synchronously, back-to-back, before either has
    // had a chance to await anything — `jsResolveCredential`'s dedup check
    // is itself synchronous (see its doc comment), so this alone is enough
    // to force the overlap deterministically without timing-dependent waits.
    const p1 = vaultGlue.jsResolveCredential("example.com", "password");
    const p2 = vaultGlue.jsResolveCredential("example.com", "password");

    releaseGate();
    const [secret1, secret2] = await Promise.all([p1, p2]);

    assert.strictEqual(secret1, "hunter2");
    assert.strictEqual(secret2, "hunter2");
    assert.strictEqual(listCalls, 1, "two concurrent resolves for the same key must issue exactly one SDK call");
});

test("js_resolve_credential_should_issue_two_sdk_calls_when_concurrent_resolves_target_different_keys", async () => {
    let listCalls = 0;
    const client = singleMatchClient({
        items: {
            list: async () => {
                listCalls += 1;
                return [loginItem("item1", "vault1", "Example Login", "https://example.com/login")];
            },
            get: async () => ({ fields: [{ id: "username", value: "alice" }, { id: "password", value: "hunter2" }] }),
        },
    });
    vaultGlue.__setClientFactoryForTesting(async () => client);

    const [username, password] = await Promise.all([
        vaultGlue.jsResolveCredential("example.com", "username"),
        vaultGlue.jsResolveCredential("example.com", "password"),
    ]);

    assert.strictEqual(username, "alice");
    assert.strictEqual(password, "hunter2");
    assert.strictEqual(listCalls, 2, "distinct domain+field keys must not be deduped together");
});

test("js_resolve_credential_pending_map_should_be_empty_when_all_waiters_served", async () => {
    const client = singleMatchClient();
    vaultGlue.__setClientFactoryForTesting(async () => client);

    const p1 = vaultGlue.jsResolveCredential("example.com", "password");
    const p2 = vaultGlue.jsResolveCredential("example.com", "password");
    await Promise.all([p1, p2]);

    // Re-issuing after both waiters have settled must trigger a fresh SDK
    // call (dedup-only, never a value cache) — asserted indirectly via a
    // fresh client factory call count, since `pending` itself is private.
    let secondRoundCalls = 0;
    vaultGlue.__setClientFactoryForTesting(async () => {
        secondRoundCalls += 1;
        return client;
    });
    await vaultGlue.jsResolveCredential("example.com", "password");
    assert.strictEqual(secondRoundCalls, 1);
});

// -- Error mapping (Task 4.3.1b's JS-side source of the markers it greps for) --

test("js_resolve_credential_should_normalize_rate_limit_error_when_sdk_throws_rate_limit_exceeded", async () => {
    const { RateLimitExceededError } = require("@1password/sdk");
    const client = singleMatchClient({
        items: {
            list: async () => {
                throw new RateLimitExceededError("account-wide rate limit hit");
            },
            get: async () => {
                throw new Error("unreachable");
            },
        },
    });
    vaultGlue.__setClientFactoryForTesting(async () => client);

    await assert.rejects(
        () => vaultGlue.jsResolveCredential("example.com", "password"),
        (err) => {
            assert.match(err.message, /rate limit exceeded — not typed/i);
            return true;
        },
    );
});

test("js_resolve_credential_should_normalize_unauthenticated_error_when_create_client_rejects_invalid_token", async () => {
    vaultGlue.__setClientFactoryForTesting(async () => {
        throw new Error(
            "invalid service account token, please make sure you provide a valid service account token as parameter",
        );
    });

    await assert.rejects(
        () => vaultGlue.jsResolveCredential("example.com", "password"),
        (err) => {
            assert.match(err.message, /not authenticated — not typed/i);
            return true;
        },
    );
});
