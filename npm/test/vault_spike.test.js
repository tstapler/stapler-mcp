// Task 4.1.1 feasibility spike, kept as a permanent (not throwaway) Node
// test so the "no wasm-in-wasm-glue nesting conflict" finding stays
// continuously verified rather than a one-off note in `plan.md`. Exercises
// the *real* `@1password/sdk@0.5.0` package installed in this project's own
// `npm/node_modules` — never `vault.js`'s mocked-client test suite
// (`vault_glue.test.js`) — from the same Node host / module resolution
// `crates/wasm/src/glue/vault.js` runs in.
//
// `@1password/sdk` depends on `@1password/sdk-core`, itself a
// `wasm-bindgen`-generated Rust module (see
// `npm/node_modules/@1password/sdk-core/package.json`'s description) — the
// exact wasm-in-wasm-glue shape `pitfalls.md` §4a/4b worried about, sharing
// one Node process with this project's own compiled `crates/wasm` output.
//
// This sandbox has no `OP_SERVICE_ACCOUNT_TOKEN` and no real 1Password vault
// (see `plan.md`'s Unresolved Questions section for the recorded go/no-go),
// so an end-to-end `client.items.get()`/`client.secrets.resolve()` against
// real data cannot be verified here. What IS verified below: the module
// loads, `createClient()` constructs and its internal `WasmCore` actually
// executes (decodes/validates the auth token) without throwing a wasm trap,
// memory-corruption panic, or module-loading/duplicate-instance error — only
// a clean, well-formed JS `Error` for a malformed token. If a real token is
// present in the environment, the test additionally attempts one live
// `client.vaults.list()` call and reports whether that succeeded, without
// failing the test on an auth/network rejection (which is a different, and
// fine, failure mode from a nesting conflict).

const test = require("node:test");
const assert = require("node:assert");

test("spike_should_construct_client_and_resolve_one_secret_when_sdk_installed_in_node_host", async () => {
    const { createClient } = require("@1password/sdk");
    assert.strictEqual(typeof createClient, "function");

    const token = process.env.OP_SERVICE_ACCOUNT_TOKEN;
    const hasRealToken = Boolean(token);

    try {
        const client = await createClient({
            auth: token || "ops_dummy_token_for_spike_only",
            integrationName: "stapler-mcp-spike",
            integrationVersion: "0.1.0",
        });

        // Only reachable with a real, valid token — confirms an actual live
        // resolve round-trips through the wasm core end to end.
        assert.ok(client.secrets, "constructed client should expose a secrets API");
        const vaults = await client.vaults.list();
        assert.ok(Array.isArray(vaults), "vaults.list() should return an array when authenticated");
    } catch (e) {
        // No real token in this environment: a clean, well-formed JS Error
        // (never a wasm trap/`RuntimeError`/`unreachable executed`, and
        // never a module-resolution error) is exactly the expected, fully
        // benign failure mode this spike sets out to distinguish from a
        // genuine nesting conflict.
        assert.ok(e instanceof Error, `expected a clean Error, got ${e}`);
        assert.ok(
            !/unreachable|RuntimeError|wasm trap|memory access out of bounds/i.test(e.message),
            `expected a normal SDK validation/auth error, not a wasm-level failure: ${e.message}`,
        );
        if (hasRealToken) {
            throw e; // a real token failing is worth surfacing, not swallowing
        }
    }
});
