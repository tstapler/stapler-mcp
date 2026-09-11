// Node-harness unit tests for `crates/wasm/src/glue/browser.js`'s pure/mock-
// friendly pieces (Epic 4, Story 4.1/4.2) — no real Chromium/Playwright
// needed here; see `e2e.test.js` for the full real-daemon round trip and
// `crates/cli/tests/browser_session.rs` for the native equivalent.

const test = require("node:test");
const assert = require("node:assert");
const path = require("node:path");

const browserGlue = require(path.join(__dirname, "..", "..", "crates", "wasm", "src", "glue", "browser.js"));

test("jsBrowserSnapshot_should_parse_ref_annotated_node_when_aria_snapshot_string_given", () => {
    const node = browserGlue.parseAriaSnapshot('- button "Submit" [ref=e1]');

    assert.strictEqual(node.role, "button");
    assert.strictEqual(node.name, "Submit");
    assert.strictEqual(node.ref, "e1");
});

test("session_interval_should_evict_idle_session_when_last_used_exceeds_timeout_ms", () => {
    const id = "sess-test-1";
    let closed = false;
    browserGlue.sessions.set(id, {
        page: { close: async () => { closed = true; } },
        lastUsed: Date.now() - 301_000,
        blocked: undefined,
    });

    browserGlue.reapIdleSessions();

    assert.strictEqual(browserGlue.sessions.has(id), false);
    assert.strictEqual(closed, true);
});

// ---------------------------------------------------------------------------
// Task 4.2.1 AC2 (stale ref): a click/type dispatched against a `ref` id that
// no longer resolves to a live DOM node (Playwright's `aria-ref=` locator
// rejects the dispatch itself, e.g. "not attached to the DOM") must surface
// as a clear, actionable error naming the ref — never fall through to acting
// on some other, unrelated element that happens to share role+name.

test("jsBrowserClick_should_reject_with_actionable_ref_error_when_locator_click_rejects_stale_ref", async () => {
    const id = "sess-stale-click-1";
    const lowLevelError = new Error("locator.click: Error: element is not attached to the DOM");
    let clickCalls = 0;
    browserGlue.sessions.set(id, {
        page: {
            url: () => "https://example.com/before",
            locator: (selector) => {
                assert.strictEqual(selector, "aria-ref=e1");
                return {
                    click: async () => {
                        clickCalls += 1;
                        throw lowLevelError;
                    },
                };
            },
        },
        lastUsed: Date.now(),
        blocked: undefined,
    });

    await assert.rejects(
        () => browserGlue.jsBrowserClick(id, "e1", 5000),
        (err) => {
            assert.match(err.message, /^ref 'e1' not found or no longer attached:/);
            assert.match(err.message, /not attached to the DOM/);
            return true;
        },
    );
    // Exactly one dispatch attempt — no retry against a different element.
    assert.strictEqual(clickCalls, 1);

    browserGlue.sessions.delete(id);
});

test("jsBrowserType_should_reject_with_actionable_ref_error_when_locator_fill_rejects_stale_ref", async () => {
    const id = "sess-stale-type-1";
    const lowLevelError = new Error("locator.fill: Error: element is not attached to the DOM");
    browserGlue.sessions.set(id, {
        page: {
            url: () => "https://example.com/before",
            locator: (selector) => {
                assert.strictEqual(selector, "aria-ref=e7");
                return {
                    fill: async () => {
                        throw lowLevelError;
                    },
                };
            },
        },
        lastUsed: Date.now(),
        blocked: undefined,
    });

    await assert.rejects(
        () => browserGlue.jsBrowserType(id, "e7", "hello", 5000),
        (err) => {
            assert.match(err.message, /^ref 'e7' not found or no longer attached:/);
            assert.match(err.message, /not attached to the DOM/);
            return true;
        },
    );

    browserGlue.sessions.delete(id);
});

// ---------------------------------------------------------------------------
// isBlockedHost must reach parity with native's `blocked_host_reason`
// (crates/core/src/tools/webcrawl.rs) on IPv4-mapped IPv6 forms — a prior
// version only matched dotted-quad IPv6 (e.g. "::ffff:127.0.0.1"), but
// Node's URL parser normalizes these to compressed hex-hextet form
// (e.g. "::ffff:7f00:1"), so that form must be blocked too.

test("isBlockedHost_should_block_ipv4_mapped_ipv6_hosts_in_hex_hextet_form", () => {
    const prevEnv = process.env.STAPLER_MCP_ALLOW_PRIVATE_NETWORKS;
    delete process.env.STAPLER_MCP_ALLOW_PRIVATE_NETWORKS;
    try {
        assert.strictEqual(
            browserGlue.isBlockedHost(new URL("http://[::ffff:127.0.0.1]/").hostname),
            true,
        );
        assert.strictEqual(
            browserGlue.isBlockedHost(new URL("http://[::ffff:169.254.169.254]/").hostname),
            true,
        );
        assert.strictEqual(
            browserGlue.isBlockedHost(new URL("http://[::ffff:10.0.0.1]/").hostname),
            true,
        );
        assert.strictEqual(
            browserGlue.isBlockedHost(new URL("http://[::ffff:8.8.8.8]/").hostname),
            false,
        );
        assert.strictEqual(browserGlue.isBlockedHost("::ffff:127.0.0.1"), true);
    } finally {
        if (prevEnv === undefined) {
            delete process.env.STAPLER_MCP_ALLOW_PRIVATE_NETWORKS;
        } else {
            process.env.STAPLER_MCP_ALLOW_PRIVATE_NETWORKS = prevEnv;
        }
    }
});

// ---------------------------------------------------------------------------
// isBlockedHost must also block the legacy IPv4-*compatible* IPv6 form
// (RFC 4291 §2.5.5.1, deprecated but still parsed) and the full fe80::/10
// link-local range — not just the literal "fe80:" prefix. A prior version
// pattern-matched Node's normalized hostname string, which for
// "http://[::169.254.169.254]/" normalizes to "::a9fe:a9fe" (no "ffff:"
// prefix, so it slipped past the IPv4-mapped regex entirely) and for
// "http://[fe90::1]/" doesn't start with the literal "fe80:" despite being
// in fe80::/10. See `crates/core/src/tools/webcrawl.rs`'s `is_blocked_ipv6`
// for the canonical policy this must match.
test("isBlockedHost_should_block_legacy_ipv4_compatible_ipv6_and_full_fe80_range", () => {
    const prevEnv = process.env.STAPLER_MCP_ALLOW_PRIVATE_NETWORKS;
    delete process.env.STAPLER_MCP_ALLOW_PRIVATE_NETWORKS;
    try {
        // Legacy IPv4-compatible form, via URL normalization (no "ffff:").
        assert.strictEqual(
            browserGlue.isBlockedHost(new URL("http://[::169.254.169.254]/").hostname),
            true,
        );
        assert.strictEqual(
            browserGlue.isBlockedHost(new URL("http://[::127.0.0.1]/").hostname),
            true,
        );
        assert.strictEqual(
            browserGlue.isBlockedHost(new URL("http://[::10.0.0.1]/").hostname),
            true,
        );
        // Same forms as raw strings (not routed through URL()).
        assert.strictEqual(browserGlue.isBlockedHost("::169.254.169.254"), true);
        assert.strictEqual(browserGlue.isBlockedHost("::127.0.0.1"), true);

        // Full fe80::/10 range (first hextet 0xfe80-0xfebf), not just the
        // literal "fe80:" prefix.
        assert.strictEqual(browserGlue.isBlockedHost(new URL("http://[fe90::1]/").hostname), true);
        assert.strictEqual(browserGlue.isBlockedHost(new URL("http://[febf::1]/").hostname), true);
        assert.strictEqual(browserGlue.isBlockedHost(new URL("http://[fe80::1]/").hostname), true);
        // Just outside the range on both ends must stay allowed.
        assert.strictEqual(browserGlue.isBlockedHost(new URL("http://[fe7f::1]/").hostname), false);
        assert.strictEqual(browserGlue.isBlockedHost(new URL("http://[fec0::1]/").hostname), false);

        // Negative cases: a real-looking public IPv6 literal (Google DNS,
        // Cloudflare DNS) and a legacy-compatible-shaped literal for a
        // public IPv4 address must not be over-blocked.
        assert.strictEqual(
            browserGlue.isBlockedHost(new URL("http://[2001:4860:4860::8888]/").hostname),
            false,
        );
        assert.strictEqual(
            browserGlue.isBlockedHost(new URL("http://[2606:4700:4700::1111]/").hostname),
            false,
        );
        assert.strictEqual(browserGlue.isBlockedHost("::8.8.8.8"), false);
    } finally {
        if (prevEnv === undefined) {
            delete process.env.STAPLER_MCP_ALLOW_PRIVATE_NETWORKS;
        } else {
            process.env.STAPLER_MCP_ALLOW_PRIVATE_NETWORKS = prevEnv;
        }
    }
});

// ---------------------------------------------------------------------------
// Task 4.2.2 (FrameNavigatedGuard): a top-level in-session navigation to a
// blocked (private/loopback) host must set `session.blocked` to the exact
// Error-5 recoverable-block wording, byte-identical to native's Task 3.4.2 so
// callers see the same message regardless of daemon. A later legitimate
// re-navigate on that same session must clear the flag.

test("wireFrameNavigatedGuard_should_set_canonical_blocked_message_when_top_level_frame_navigates_to_blocked_host", () => {
    const prevEnv = process.env.STAPLER_MCP_ALLOW_PRIVATE_NETWORKS;
    delete process.env.STAPLER_MCP_ALLOW_PRIVATE_NETWORKS;
    try {
        const id = "sess-guard-1";
        let handler;
        const session = {
            page: {
                on: (event, cb) => {
                    assert.strictEqual(event, "framenavigated");
                    handler = cb;
                },
            },
            lastUsed: Date.now(),
            blocked: undefined,
        };

        browserGlue.wireFrameNavigatedGuard(id, session);
        assert.strictEqual(typeof handler, "function");

        // Simulate the page navigating in-page to a blocked (loopback) host,
        // as a top-level (main) frame — no parentFrame().
        handler({ parentFrame: () => undefined, url: () => "http://127.0.0.1/" });

        assert.strictEqual(
            session.blocked,
            "session 'sess-guard-1' navigated to a blocked host '127.0.0.1' during the last action; " +
                "call stapler_browser_navigate with this sessionId and a safe URL to recover it, or start a fresh session",
        );
    } finally {
        if (prevEnv === undefined) {
            delete process.env.STAPLER_MCP_ALLOW_PRIVATE_NETWORKS;
        } else {
            process.env.STAPLER_MCP_ALLOW_PRIVATE_NETWORKS = prevEnv;
        }
    }
});

test("wireFrameNavigatedGuard_should_ignore_subframe_navigation_to_blocked_host", () => {
    const prevEnv = process.env.STAPLER_MCP_ALLOW_PRIVATE_NETWORKS;
    delete process.env.STAPLER_MCP_ALLOW_PRIVATE_NETWORKS;
    try {
        const id = "sess-guard-2";
        let handler;
        const session = {
            page: { on: (_event, cb) => { handler = cb; } },
            lastUsed: Date.now(),
            blocked: undefined,
        };

        browserGlue.wireFrameNavigatedGuard(id, session);
        // An iframe embedding a private-looking URL is not the session
        // itself navigating — has a parentFrame(), so it's out of scope.
        handler({ parentFrame: () => ({}), url: () => "http://127.0.0.1/" });

        assert.strictEqual(session.blocked, undefined);
    } finally {
        if (prevEnv === undefined) {
            delete process.env.STAPLER_MCP_ALLOW_PRIVATE_NETWORKS;
        } else {
            process.env.STAPLER_MCP_ALLOW_PRIVATE_NETWORKS = prevEnv;
        }
    }
});

// ---------------------------------------------------------------------------
// BLOCKER 1 fix (crash detection): a page's `'crash'` event (Playwright's
// renderer-crash signal — the wasm-side counterpart of native's
// `Target.targetCrashed` listener in `crates/native/src/browser.rs`) must
// mark the session as crashed, and every subsequent call against that
// session must reject with a message containing "crashed" so it round-trips
// through `WasmBrowser::map_js_error` as `PortError::SessionCrashed`.

test("wireCrashListener_should_set_crashed_message_when_page_crash_event_fires", () => {
    const id = "sess-crash-1";
    let handler;
    const session = {
        page: {
            on: (event, cb) => {
                assert.strictEqual(event, "crash");
                handler = cb;
            },
        },
        lastUsed: Date.now(),
        blocked: undefined,
        crashed: undefined,
    };

    browserGlue.wireCrashListener(id, session);
    assert.strictEqual(typeof handler, "function");
    assert.strictEqual(session.crashed, undefined);

    handler();

    assert.strictEqual(session.crashed, browserGlue.crashedMessage(id));
    assert.match(session.crashed, /crashed/);
});

test("jsBrowserClick_should_reject_with_crashed_message_when_session_already_crashed", async () => {
    const id = "sess-crash-2";
    browserGlue.sessions.set(id, {
        page: { url: () => "https://example.com/", locator: () => ({ click: async () => {} }) },
        lastUsed: Date.now(),
        blocked: undefined,
        crashed: browserGlue.crashedMessage(id),
    });

    await assert.rejects(
        () => browserGlue.jsBrowserClick(id, "e1", 5000),
        (err) => {
            assert.match(err.message, /crashed/);
            return true;
        },
    );

    browserGlue.sessions.delete(id);
});

test("jsBrowserNavigate_should_reject_reused_session_with_crashed_message_when_session_already_crashed", async () => {
    const id = "sess-crash-3";
    browserGlue.sessions.set(id, {
        page: { goto: async () => {}, url: () => "https://example.com/", ariaSnapshot: async () => "" },
        lastUsed: Date.now(),
        blocked: undefined,
        crashed: browserGlue.crashedMessage(id),
    });

    await assert.rejects(
        () => browserGlue.jsBrowserNavigate("https://example.com/next", id, 5000),
        (err) => {
            assert.match(err.message, /crashed/);
            return true;
        },
    );

    browserGlue.sessions.delete(id);
});

// ---------------------------------------------------------------------------
// MUST FIX (reaper interval leak): `jsCloseBrowser` must clear the reaper's
// `setInterval` and drop every session, not just close the browser process.

test("jsCloseBrowser_should_clear_reaper_interval_and_sessions_when_reaper_was_running", async () => {
    // `ensureReaper` (and the `reaperTimer`/`browserPromise` singletons it
    // depends on) aren't exported, so the only way to actually start the
    // reaper is to drive it through the real `jsBrowserNavigate` "new
    // session" path — the one call site that invokes `ensureReaper()`. That
    // path calls the real `playwright-core` `chromium.launch`/`newPage`, so
    // those are stubbed out here with a fake browser/page rather than
    // spinning up a real Chromium instance.
    const { chromium } = require("playwright-core");
    const originalLaunch = chromium.launch;
    const originalClearInterval = global.clearInterval;
    const fakePage = {
        on: () => {},
        goto: async () => {},
        url: () => "https://example.com/",
        ariaSnapshot: async () => "",
        evaluate: async () => [],
    };
    const fakeBrowser = {
        newPage: async () => fakePage,
        close: async () => {},
    };
    chromium.launch = async () => fakeBrowser;
    let clearIntervalCalls = 0;
    global.clearInterval = (handle) => {
        clearIntervalCalls += 1;
        return originalClearInterval(handle);
    };

    try {
        await browserGlue.jsBrowserNavigate("https://example.com/", "", 5000);
        // The reaper only starts on the "new session" path above; confirm it
        // actually ran before trusting the `clearInterval` assertion below.
        assert.strictEqual(browserGlue.sessions.size, 1);

        await browserGlue.jsCloseBrowser();

        assert.strictEqual(clearIntervalCalls, 1, "jsCloseBrowser should clear the reaper's setInterval");
        assert.strictEqual(browserGlue.sessions.size, 0);
    } finally {
        // Unconditional cleanup so a failed assertion above (e.g. the
        // `sessions.size` check) doesn't leak the session/reaper/browserPromise
        // singletons into whichever test runs next in this process.
        // `jsCloseBrowser` is idempotent (`if (reaperTimer)`, `if
        // (browserPromise)`), so calling it again after the happy-path call
        // above is a safe no-op.
        await browserGlue.jsCloseBrowser().catch(() => {});
        chromium.launch = originalLaunch;
        global.clearInterval = originalClearInterval;
    }
});

test("reapIdleSessions_should_not_throw_when_pages_close_rejects", () => {
    const id = "sess-reap-reject-1";
    browserGlue.sessions.set(id, {
        page: { close: () => Promise.reject(new Error("already gone")) },
        lastUsed: Date.now() - 301_000,
        blocked: undefined,
        crashed: undefined,
    });

    assert.doesNotThrow(() => browserGlue.reapIdleSessions());
    assert.strictEqual(browserGlue.sessions.has(id), false);
});

test("jsBrowserNavigate_should_clear_blocked_flag_when_reused_session_navigates_to_safe_url", async () => {
    const id = "sess-clear-1";
    browserGlue.sessions.set(id, {
        page: {
            goto: async () => {},
            url: () => "https://example.com/safe",
            ariaSnapshot: async () => '- text "hi"',
            evaluate: async () => [],
        },
        lastUsed: Date.now() - 1000,
        blocked: browserGlue.blockedHostMessage(id, "127.0.0.1"),
    });

    const result = await browserGlue.jsBrowserNavigate("https://example.com/safe", id, 5000);

    assert.strictEqual(result.sessionId, id);
    assert.strictEqual(browserGlue.sessions.get(id).blocked, undefined);

    browserGlue.sessions.delete(id);
});

// ---------------------------------------------------------------------------
// BLOCKER fix (finding #1): `jsBrowserNavigate`'s own navigation must be
// checked against `session.blocked` before returning a snapshot — a
// same-call redirect to a blocked host (e.g. the cloud metadata service)
// must never leak a snapshot. `session.blocked` is set asynchronously by the
// `framenavigated` listener, so this also exercises the grace-period poll
// (finding #2) that gives that listener a chance to run before the check.

test("jsBrowserNavigate_should_reject_and_not_return_snapshot_when_navigate_itself_redirects_to_blocked_host", async () => {
    const { chromium } = require("playwright-core");
    const originalLaunch = chromium.launch;
    let framenavigatedHandler;
    const fakePage = {
        on: (event, cb) => {
            if (event === "framenavigated") {
                framenavigatedHandler = cb;
            }
        },
        goto: async () => {
            // Simulate the real timing: Playwright fires `framenavigated`
            // shortly *after* `goto()`'s own promise resolves, not before —
            // exactly the race the BLOCKER/CRITICAL fixes close.
            setTimeout(() => {
                framenavigatedHandler({
                    parentFrame: () => undefined,
                    url: () => "http://169.254.169.254/latest/meta-data/",
                });
            }, 5);
        },
        url: () => "https://example.com/",
        ariaSnapshot: async () => '- text "leaked metadata: should never be returned"',
    };
    const fakeBrowser = { newPage: async () => fakePage, close: async () => {} };
    chromium.launch = async () => fakeBrowser;

    try {
        await assert.rejects(
            () => browserGlue.jsBrowserNavigate("https://example.com/", "", 5000),
            (err) => {
                assert.match(err.message, /blocked host '169\.254\.169\.254'/);
                return true;
            },
        );
    } finally {
        await browserGlue.jsCloseBrowser().catch(() => {});
        chromium.launch = originalLaunch;
    }
});

// CRITICAL fix (finding #2): the same grace-period poll must apply to
// click's post-dispatch blocked check, not just navigate's.

test("jsBrowserClick_should_reject_when_click_triggered_navigation_to_blocked_host_fires_shortly_after_click_resolves", async () => {
    const id = "sess-click-race-1";
    let framenavigatedHandler;
    const session = {
        page: {
            on: (event, cb) => {
                if (event === "framenavigated") {
                    framenavigatedHandler = cb;
                }
            },
            url: () => "https://example.com/before",
            locator: () => ({
                click: async () => {
                    setTimeout(() => {
                        framenavigatedHandler({
                            parentFrame: () => undefined,
                            url: () => "http://169.254.169.254/latest/meta-data/",
                        });
                    }, 5);
                },
            }),
            ariaSnapshot: async () => '- text "leaked metadata: should never be returned"',
        },
        lastUsed: Date.now(),
        blocked: undefined,
    };
    browserGlue.wireFrameNavigatedGuard(id, session);
    browserGlue.sessions.set(id, session);

    try {
        await assert.rejects(
            () => browserGlue.jsBrowserClick(id, "e1", 5000),
            (err) => {
                assert.match(err.message, /blocked host '169\.254\.169\.254'/);
                return true;
            },
        );
    } finally {
        browserGlue.sessions.delete(id);
    }
});

// MAJOR fix (finding #3): only a genuine "ref missing" Playwright error gets
// rewritten into the "ref not found" wording; anything else (e.g. execution
// context destroyed by a same-click navigation) must pass through unchanged
// so `WasmBrowser::map_js_error` doesn't misclassify it as `NotFound`.

test("jsBrowserClick_should_not_rewrite_non_missing_ref_errors_into_ref_not_found_wording", async () => {
    const id = "sess-ctx-destroyed-1";
    const lowLevelError = new Error("Execution context was destroyed, most likely because of a navigation");
    browserGlue.sessions.set(id, {
        page: {
            url: () => "https://example.com/before",
            locator: () => ({
                click: async () => {
                    throw lowLevelError;
                },
            }),
        },
        lastUsed: Date.now(),
        blocked: undefined,
    });

    await assert.rejects(
        () => browserGlue.jsBrowserClick(id, "e1", 5000),
        (err) => {
            assert.strictEqual(err, lowLevelError);
            assert.doesNotMatch(err.message, /not found or no longer attached/);
            return true;
        },
    );

    browserGlue.sessions.delete(id);
});

// ---------------------------------------------------------------------------
// MAJOR fix (finding #5): snapshots must be capped at `MAX_SNAPSHOT_NODES`
// nodes, with `truncated` reflecting whether the cap was actually hit —
// previously unbounded, with `truncated` hardcoded to `false`.

test("capSnapshotNodes_should_cap_total_nodes_and_report_truncated_when_tree_exceeds_max", () => {
    const root = { role: "generic", name: "", ref: "root", children: [] };
    for (let i = 0; i < browserGlue.MAX_SNAPSHOT_NODES + 50; i++) {
        root.children.push({ role: "button", name: `n${i}`, ref: `e${i}`, children: [] });
    }

    const truncated = browserGlue.capSnapshotNodes(root, browserGlue.MAX_SNAPSHOT_NODES);

    assert.strictEqual(truncated, true);
    // The root itself counts against the cap, so only MAX - 1 children survive.
    assert.strictEqual(root.children.length, browserGlue.MAX_SNAPSHOT_NODES - 1);
});

test("capSnapshotNodes_should_report_not_truncated_when_tree_is_within_cap", () => {
    const root = {
        role: "generic",
        name: "",
        ref: "root",
        children: [{ role: "button", name: "ok", ref: "e1", children: [] }],
    };

    const truncated = browserGlue.capSnapshotNodes(root, browserGlue.MAX_SNAPSHOT_NODES);

    assert.strictEqual(truncated, false);
    assert.strictEqual(root.children.length, 1);
});

test("jsBrowserSnapshot_should_set_truncated_true_when_aria_snapshot_exceeds_node_cap", async () => {
    const id = "sess-snapshot-cap-1";
    const lines = [];
    for (let i = 0; i < browserGlue.MAX_SNAPSHOT_NODES + 10; i++) {
        lines.push(`- button "n${i}" [ref=e${i}]`);
    }
    browserGlue.sessions.set(id, {
        page: {
            url: () => "https://example.com/",
            ariaSnapshot: async () => lines.join("\n"),
            evaluate: async () => [],
        },
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserSnapshot(id, 5000);

    assert.strictEqual(snapshot.truncated, true);
    assert.strictEqual(snapshot.root.children.length, browserGlue.MAX_SNAPSHOT_NODES - 1);

    browserGlue.sessions.delete(id);
});

// ---------------------------------------------------------------------------
// MAJOR fix (finding #6): a hard cap on concurrently open sessions — without
// one, a caller could spawn unbounded tabs/pages before the idle reaper ever
// fires.

test("jsBrowserNavigate_should_reject_new_session_when_max_sessions_reached", async () => {
    const filler = [];
    for (let i = 0; i < browserGlue.MAX_SESSIONS; i++) {
        const id = `sess-cap-fill-${i}`;
        browserGlue.sessions.set(id, {
            page: { close: async () => {} },
            lastUsed: Date.now(),
            blocked: undefined,
        });
        filler.push(id);
    }

    try {
        await assert.rejects(
            () => browserGlue.jsBrowserNavigate("https://example.com/", "", 5000),
            /too many open browser sessions/,
        );
    } finally {
        for (const id of filler) {
            browserGlue.sessions.delete(id);
        }
    }
});

// ---------------------------------------------------------------------------
// Issue #12: session lifecycle + everyday interaction tools — jsCloseSession,
// jsBrowserTabs, jsBrowserHover, jsBrowserSelectOption, jsBrowserPressKey,
// jsBrowserWaitFor. Smoke-level coverage only (per task scope): one happy
// path per function plus the couple of error/edge branches that are cheap to
// hand-mock (last-tab close, out-of-range tab index, wait_for timeout).

function makeMockPage(overrides = {}) {
    return {
        url: () => "https://example.com/",
        title: async () => "Example",
        on: () => {},
        locator: (selector) => ({
            hover: async () => {},
            selectOption: async () => {},
            press: async () => {},
            ...overrides.locator,
            _selector: selector,
        }),
        keyboard: { press: async () => {} },
        ariaSnapshot: async () => '- text "hi"',
        // Default: no redactable fields on the live DOM (Epic 2.3's
        // `collectRedactionInfo` pass) — tests exercising redaction itself
        // override this via `overrides`.
        evaluate: async () => [],
        getByText: () => ({
            first: () => ({
                waitFor: async () => {},
            }),
        }),
        waitForTimeout: async () => {},
        close: async () => {},
        context: () => ({ close: async () => {}, newPage: async () => makeMockPage() }),
        ...overrides,
    };
}

test("jsCloseSession_should_close_context_and_remove_session_from_map", async () => {
    const id = "sess-close-1";
    let contextClosed = false;
    browserGlue.sessions.set(id, {
        page: makeMockPage({ context: () => ({ close: async () => { contextClosed = true; } }) }),
        lastUsed: Date.now(),
        blocked: undefined,
    });

    await browserGlue.jsCloseSession(id);

    assert.strictEqual(contextClosed, true);
    assert.strictEqual(browserGlue.sessions.has(id), false);
});

test("jsBrowserTabs_should_list_single_tab_by_default_when_session_never_called_tabs_before", async () => {
    const id = "sess-tabs-list-1";
    browserGlue.sessions.set(id, { page: makeMockPage(), lastUsed: Date.now(), blocked: undefined });

    const result = await browserGlue.jsBrowserTabs(id, JSON.stringify({ kind: "list" }), 5000);

    assert.strictEqual(result.tabs.length, 1);
    assert.strictEqual(result.activeIndex, 0);
    assert.strictEqual(result.snapshot, null);

    browserGlue.sessions.delete(id);
});

test("jsBrowserTabs_should_open_new_tab_navigate_and_make_it_active", async () => {
    const id = "sess-tabs-new-1";
    let gotoUrl;
    const newPage = makeMockPage({
        url: () => "https://example.com/new",
        goto: async (url) => { gotoUrl = url; },
    });
    const session = {
        page: makeMockPage({ context: () => ({ newPage: async () => newPage }) }),
        lastUsed: Date.now(),
        blocked: undefined,
    };
    browserGlue.sessions.set(id, session);

    const result = await browserGlue.jsBrowserTabs(
        id,
        JSON.stringify({ kind: "new", url: "https://example.com/new" }),
        5000,
    );

    assert.strictEqual(gotoUrl, "https://example.com/new");
    assert.strictEqual(result.tabs.length, 2);
    assert.strictEqual(result.activeIndex, 1);
    assert.notStrictEqual(result.snapshot, null);
    assert.strictEqual(browserGlue.sessions.get(id).page, newPage);

    browserGlue.sessions.delete(id);
});

test("jsBrowserTabs_should_select_tab_by_index_and_return_its_snapshot", async () => {
    const id = "sess-tabs-select-1";
    const pageZero = makeMockPage({ url: () => "https://example.com/zero" });
    const pageOne = makeMockPage({ url: () => "https://example.com/one" });
    const session = { page: pageZero, lastUsed: Date.now(), blocked: undefined };
    browserGlue.sessions.set(id, session);
    // Seed multi-tab state directly (as if a prior "new" action had run).
    session.pages = [pageZero, pageOne];
    session.activeIndex = 0;

    const result = await browserGlue.jsBrowserTabs(id, JSON.stringify({ kind: "select", index: 1 }), 5000);

    assert.strictEqual(result.activeIndex, 1);
    assert.strictEqual(browserGlue.sessions.get(id).page, pageOne);

    browserGlue.sessions.delete(id);
});

test("jsBrowserTabs_should_reject_select_with_out_of_range_index", async () => {
    const id = "sess-tabs-select-oob-1";
    browserGlue.sessions.set(id, { page: makeMockPage(), lastUsed: Date.now(), blocked: undefined });

    await assert.rejects(
        () => browserGlue.jsBrowserTabs(id, JSON.stringify({ kind: "select", index: 5 }), 5000),
        /out of range/,
    );

    browserGlue.sessions.delete(id);
});

test("jsBrowserTabs_should_close_tab_by_index_and_shift_active_index", async () => {
    const id = "sess-tabs-close-1";
    const pageZero = makeMockPage({ url: () => "https://example.com/zero" });
    const pageOne = makeMockPage({ url: () => "https://example.com/one" });
    const session = { page: pageOne, lastUsed: Date.now(), blocked: undefined };
    session.pages = [pageZero, pageOne];
    session.activeIndex = 1;
    browserGlue.sessions.set(id, session);

    const result = await browserGlue.jsBrowserTabs(id, JSON.stringify({ kind: "close", index: 0 }), 5000);

    assert.strictEqual(result.tabs.length, 1);
    assert.strictEqual(result.activeIndex, 0);
    assert.strictEqual(browserGlue.sessions.get(id).page, pageOne);

    browserGlue.sessions.delete(id);
});

test("jsBrowserTabs_should_reject_closing_the_only_remaining_tab", async () => {
    const id = "sess-tabs-close-last-1";
    browserGlue.sessions.set(id, { page: makeMockPage(), lastUsed: Date.now(), blocked: undefined });

    await assert.rejects(
        () => browserGlue.jsBrowserTabs(id, JSON.stringify({ kind: "close", index: 0 }), 5000),
        /close_session/,
    );

    browserGlue.sessions.delete(id);
});

test("jsBrowserHover_should_hover_located_element_and_return_snapshot", async () => {
    const id = "sess-hover-1";
    let hoverCalls = 0;
    browserGlue.sessions.set(id, {
        page: makeMockPage({ locator: (selector) => ({ hover: async () => { hoverCalls += 1; }, _selector: selector }) }),
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserHover(id, "e1", 5000);

    assert.strictEqual(hoverCalls, 1);
    assert.ok(snapshot.root);

    browserGlue.sessions.delete(id);
});

test("jsBrowserSelectOption_should_select_values_on_located_element_and_return_snapshot", async () => {
    const id = "sess-select-option-1";
    let receivedValues;
    browserGlue.sessions.set(id, {
        page: makeMockPage({
            locator: () => ({ selectOption: async (values) => { receivedValues = values; } }),
        }),
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserSelectOption(id, "e1", JSON.stringify(["a", "b"]), 5000);

    assert.deepStrictEqual(receivedValues, ["a", "b"]);
    assert.ok(snapshot.root);

    browserGlue.sessions.delete(id);
});

test("jsBrowserPressKey_should_press_on_page_keyboard_when_no_locator_given", async () => {
    const id = "sess-press-key-1";
    let pressedKey;
    browserGlue.sessions.set(id, {
        page: makeMockPage({ keyboard: { press: async (key) => { pressedKey = key; } } }),
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserPressKey(id, "Enter", undefined, 5000);

    assert.strictEqual(pressedKey, "Enter");
    assert.ok(snapshot.root);

    browserGlue.sessions.delete(id);
});

test("jsBrowserPressKey_should_press_on_located_element_when_locator_given", async () => {
    const id = "sess-press-key-2";
    let pressedKey;
    browserGlue.sessions.set(id, {
        page: makeMockPage({ locator: () => ({ press: async (key) => { pressedKey = key; } }) }),
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserPressKey(id, "Enter", "e1", 5000);

    assert.strictEqual(pressedKey, "Enter");
    assert.ok(snapshot.root);

    browserGlue.sessions.delete(id);
});

test("jsBrowserWaitFor_should_wait_for_text_to_appear_and_return_snapshot", async () => {
    const id = "sess-wait-appear-1";
    let waitedState;
    browserGlue.sessions.set(id, {
        page: makeMockPage({
            getByText: () => ({ first: () => ({ waitFor: async ({ state }) => { waitedState = state; } }) }),
        }),
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserWaitFor(
        id,
        JSON.stringify({ kind: "textAppears", text: "Loaded" }),
        5000,
    );

    assert.strictEqual(waitedState, "visible");
    assert.ok(snapshot.root);

    browserGlue.sessions.delete(id);
});

test("jsBrowserWaitFor_should_reject_with_timed_out_marker_when_text_appears_wait_rejects", async () => {
    const id = "sess-wait-appear-timeout-1";
    browserGlue.sessions.set(id, {
        page: makeMockPage({
            getByText: () => ({
                first: () => ({
                    waitFor: async () => { throw new Error("Timeout 5000ms exceeded"); },
                }),
            }),
        }),
        lastUsed: Date.now(),
        blocked: undefined,
    });

    await assert.rejects(
        () => browserGlue.jsBrowserWaitFor(id, JSON.stringify({ kind: "textAppears", text: "Loaded" }), 5000),
        /timed out waiting for text to appear/,
    );

    browserGlue.sessions.delete(id);
});

test("jsBrowserWaitFor_should_wait_for_text_to_disappear", async () => {
    const id = "sess-wait-disappear-1";
    let waitedState;
    browserGlue.sessions.set(id, {
        page: makeMockPage({
            getByText: () => ({ first: () => ({ waitFor: async ({ state }) => { waitedState = state; } }) }),
        }),
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserWaitFor(
        id,
        JSON.stringify({ kind: "textDisappears", text: "Loading" }),
        5000,
    );

    assert.strictEqual(waitedState, "hidden");
    assert.ok(snapshot.root);

    browserGlue.sessions.delete(id);
});

test("jsBrowserWaitFor_should_wait_a_fixed_delay_for_timeMs_condition", async () => {
    const id = "sess-wait-time-1";
    let waitedMs;
    browserGlue.sessions.set(id, {
        page: makeMockPage({ waitForTimeout: async (ms) => { waitedMs = ms; } }),
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserWaitFor(id, JSON.stringify({ kind: "timeMs", ms: 250 }), 5000);

    assert.strictEqual(waitedMs, 250);
    assert.ok(snapshot.root);

    browserGlue.sessions.delete(id);
});

test("jsBrowserScreenshot_should_pass_full_page_and_timeout_through_and_return_bytes", async () => {
    const id = "sess-screenshot-1";
    let receivedOptions;
    const fakeBytes = Buffer.from([1, 2, 3, 4]);
    browserGlue.sessions.set(id, {
        page: makeMockPage({
            screenshot: async (options) => {
                receivedOptions = options;
                return fakeBytes;
            },
        }),
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const bytes = await browserGlue.jsBrowserScreenshot(id, true, 5000);

    assert.deepStrictEqual(receivedOptions, { type: "png", fullPage: true, timeout: 5000 });
    assert.deepStrictEqual(Buffer.from(bytes), fakeBytes);

    browserGlue.sessions.delete(id);
});

test("jsBrowserEvaluate_should_call_function_at_page_scope_when_no_ref_given", async () => {
    const id = "sess-evaluate-page-1";
    let receivedFn;
    browserGlue.sessions.set(id, {
        page: makeMockPage({
            evaluate: async (fn) => {
                receivedFn = fn;
                return 42;
            },
        }),
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const value = await browserGlue.jsBrowserEvaluate(id, "() => 40 + 2", undefined, 5000);

    assert.strictEqual(typeof receivedFn, "function");
    assert.strictEqual(value, 42);

    browserGlue.sessions.delete(id);
});

test("jsBrowserEvaluate_should_call_function_on_located_element_when_ref_given", async () => {
    const id = "sess-evaluate-ref-1";
    let receivedOptions;
    browserGlue.sessions.set(id, {
        page: makeMockPage({
            locator: () => ({
                evaluate: async (fn, arg, options) => {
                    receivedOptions = options;
                    return "value-from-element";
                },
            }),
        }),
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const value = await browserGlue.jsBrowserEvaluate(id, "(el) => el.value", "e1", 5000);

    assert.deepStrictEqual(receivedOptions, { timeout: 5000 });
    assert.strictEqual(value, "value-from-element");

    browserGlue.sessions.delete(id);
});

test("jsBrowserEvaluate_should_return_null_when_function_returns_undefined", async () => {
    const id = "sess-evaluate-undefined-1";
    browserGlue.sessions.set(id, {
        page: makeMockPage({ evaluate: async () => undefined }),
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const value = await browserGlue.jsBrowserEvaluate(id, "() => {}", undefined, 5000);

    assert.strictEqual(value, null);

    browserGlue.sessions.delete(id);
});

test("jsBrowserHistory_should_go_back_and_return_snapshot", async () => {
    const id = "sess-history-back-1";
    let receivedOptions;
    browserGlue.sessions.set(id, {
        page: makeMockPage({
            goBack: async (options) => {
                receivedOptions = options;
                return {};
            },
        }),
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserHistory(id, "back", 5000);

    assert.deepStrictEqual(receivedOptions, { timeout: 5000 });
    assert.ok(snapshot.root);

    browserGlue.sessions.delete(id);
});

test("jsBrowserHistory_should_reject_when_going_back_with_no_previous_entry", async () => {
    const id = "sess-history-back-empty-1";
    browserGlue.sessions.set(id, {
        page: makeMockPage({ goBack: async () => null }),
        lastUsed: Date.now(),
        blocked: undefined,
    });

    await assert.rejects(
        () => browserGlue.jsBrowserHistory(id, "back", 5000),
        /cannot go back: no previous entry/,
    );

    browserGlue.sessions.delete(id);
});

test("jsBrowserHistory_should_go_forward_and_return_snapshot", async () => {
    const id = "sess-history-forward-1";
    let called = false;
    browserGlue.sessions.set(id, {
        page: makeMockPage({
            goForward: async () => {
                called = true;
                return {};
            },
        }),
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserHistory(id, "forward", 5000);

    assert.strictEqual(called, true);
    assert.ok(snapshot.root);

    browserGlue.sessions.delete(id);
});

test("jsBrowserHistory_should_reload_and_return_snapshot", async () => {
    const id = "sess-history-reload-1";
    let called = false;
    browserGlue.sessions.set(id, {
        page: makeMockPage({
            reload: async () => {
                called = true;
                return {};
            },
        }),
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserHistory(id, "reload", 5000);

    assert.strictEqual(called, true);
    assert.ok(snapshot.root);

    browserGlue.sessions.delete(id);
});

test("jsBrowserHistory_should_reject_unknown_action", async () => {
    const id = "sess-history-unknown-1";
    browserGlue.sessions.set(id, {
        page: makeMockPage(),
        lastUsed: Date.now(),
        blocked: undefined,
    });

    await assert.rejects(
        () => browserGlue.jsBrowserHistory(id, "sideways", 5000),
        /unknown history action/,
    );

    browserGlue.sessions.delete(id);
});

test("jsBrowserResize_should_set_viewport_size_and_return_snapshot", async () => {
    const id = "sess-resize-1";
    let receivedSize;
    browserGlue.sessions.set(id, {
        page: makeMockPage({
            setViewportSize: async (size) => {
                receivedSize = size;
            },
        }),
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserResize(id, 1024, 768, 5000);

    assert.deepStrictEqual(receivedSize, { width: 1024, height: 768 });
    assert.ok(snapshot.root);

    browserGlue.sessions.delete(id);
});

// ---------------------------------------------------------------------------
// Epic 2.3 (credential-vault): structural snapshot redaction. Playwright's
// `page.ariaSnapshot()` text carries no `type`/`autocomplete` attribute at
// all, so `captureSnapshot` merges in a second `page.evaluate()` pass
// (`collectRedactionInfo`) keyed by `ref`, with the identical redaction key
// and fail-safe rule as native's `AxNode.value` doc comment
// (`crates/core/src/ports.rs`): redact on `type === "password"` or
// `autocomplete` in `one-time-code`/`current-password`/`new-password`, and
// fail-safe-redact any textbox-like node the live-DOM pass couldn't resolve
// at all.

test("capture_snapshot_should_redact_value_when_dom_type_is_password", async () => {
    const id = "sess-redact-password-1";
    browserGlue.sessions.set(id, {
        page: {
            url: () => "https://example.com/",
            ariaSnapshot: async () => '- textbox "Password" [ref=e3]',
            locator: (selector) => {
                assert.strictEqual(selector, "aria-ref=e3");
                return { evaluate: async () => ({ type: "password", autocomplete: "" }) };
            },
        },
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserSnapshot(id, 5000);

    assert.strictEqual(snapshot.root.ref, "e3");
    assert.strictEqual(snapshot.root.value, "[REDACTED]");

    browserGlue.sessions.delete(id);
});

// A2 code review fix: Chromium can assign `searchbox`/`combobox` (not just
// `textbox`) to a password/OTP-shaped `<input>` — `FORM_CONTROL_ROLES` must
// cover both, matching native's `is_form_control_role`
// (`crates/native/src/ax.rs`), or a field with one of these roles would
// never even be probed for redaction.
test("capture_snapshot_should_redact_value_when_role_is_searchbox_and_dom_type_is_password", async () => {
    const id = "sess-redact-searchbox-1";
    browserGlue.sessions.set(id, {
        page: {
            url: () => "https://example.com/",
            ariaSnapshot: async () => '- searchbox "Password" [ref=e6]',
            locator: (selector) => {
                assert.strictEqual(selector, "aria-ref=e6");
                return { evaluate: async () => ({ type: "password", autocomplete: "" }) };
            },
        },
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserSnapshot(id, 5000);

    assert.strictEqual(snapshot.root.value, "[REDACTED]");

    browserGlue.sessions.delete(id);
});

test("capture_snapshot_should_redact_value_when_role_is_combobox_and_dom_autocomplete_is_current_password", async () => {
    const id = "sess-redact-combobox-1";
    browserGlue.sessions.set(id, {
        page: {
            url: () => "https://example.com/",
            ariaSnapshot: async () => '- combobox "Password" [ref=e7]',
            locator: (selector) => {
                assert.strictEqual(selector, "aria-ref=e7");
                return { evaluate: async () => ({ type: "", autocomplete: "current-password" }) };
            },
        },
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserSnapshot(id, 5000);

    assert.strictEqual(snapshot.root.value, "[REDACTED]");

    browserGlue.sessions.delete(id);
});

test("capture_snapshot_should_redact_value_when_dom_autocomplete_is_one_time_code", async () => {
    const id = "sess-redact-otc-1";
    browserGlue.sessions.set(id, {
        page: {
            url: () => "https://example.com/",
            ariaSnapshot: async () => '- textbox "Code" [ref=e5]',
            locator: (selector) => {
                assert.strictEqual(selector, "aria-ref=e5");
                return { evaluate: async () => ({ type: "", autocomplete: "one-time-code" }) };
            },
        },
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserSnapshot(id, 5000);

    assert.strictEqual(snapshot.root.value, "[REDACTED]");

    browserGlue.sessions.delete(id);
});

test("capture_snapshot_should_leave_value_unset_when_dom_pass_reports_ordinary_text_field", async () => {
    const id = "sess-redact-ordinary-1";
    browserGlue.sessions.set(id, {
        page: {
            url: () => "https://example.com/",
            ariaSnapshot: async () => '- textbox "Name" [ref=e7]',
            locator: (selector) => {
                assert.strictEqual(selector, "aria-ref=e7");
                return { evaluate: async () => ({ type: "text", autocomplete: "" }) };
            },
        },
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserSnapshot(id, 5000);

    assert.strictEqual(snapshot.root.value, undefined);

    browserGlue.sessions.delete(id);
});

// Fail-safe (Task 2.3.1b): a node the aria-snapshot parse assigned a
// form-control role but whose `aria-ref=` locator can't be resolved on the
// live DOM (e.g. removed between the two calls) must be redacted rather than
// left as whatever `ariaSnapshot()` reported — "can't confirm it's safe"
// redacts, it never leaks.
test("capture_snapshot_should_redact_value_when_textbox_ref_missing_from_live_dom_pass", async () => {
    const id = "sess-redact-missing-ref-1";
    browserGlue.sessions.set(id, {
        page: {
            url: () => "https://example.com/",
            ariaSnapshot: async () => '- textbox "Password" [ref=e3]',
            locator: (selector) => {
                assert.strictEqual(selector, "aria-ref=e3");
                return {
                    evaluate: async () => {
                        throw new Error("no node found for selector"); // ref gone by probe time
                    },
                };
            },
        },
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserSnapshot(id, 5000);

    assert.strictEqual(snapshot.root.value, "[REDACTED]");

    browserGlue.sessions.delete(id);
});

// Fail-safe (Task 2.3.2b): a closed shadow root's contents never appear as
// nodes in `ariaSnapshot()`'s tree at all — Playwright's own snapshot/ref
// engine only crosses *open* shadow boundaries — so in practice such a field
// is invisible to `parseAriaSnapshot` from the start and is never revealed
// as plaintext by this path. This covers the defense-in-depth case where a
// textbox node ends up in the tree regardless but its `aria-ref=` locator
// can't be resolved live: same missing-ref fail-safe rule as above, redact
// rather than leak.
test("capture_snapshot_should_redact_value_when_closed_shadow_root_hides_password_field_from_live_dom_pass", async () => {
    const id = "sess-redact-closed-shadow-1";
    browserGlue.sessions.set(id, {
        page: {
            url: () => "https://example.com/",
            ariaSnapshot: async () => '- textbox "Password" [ref=e9]',
            locator: (selector) => {
                assert.strictEqual(selector, "aria-ref=e9");
                return {
                    evaluate: async () => {
                        throw new Error("no node found for selector"); // closed shadow content: unresolvable
                    },
                };
            },
        },
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserSnapshot(id, 5000);

    assert.strictEqual(snapshot.root.value, "[REDACTED]");

    browserGlue.sessions.delete(id);
});

// ---------------------------------------------------------------------------
// Phase 7, Story 7.1.2: the literal acceptance test `requirements.md`'s
// Success Metrics section names — "verified by a test that autofills a
// password field out-of-band and then calls `stapler_browser_snapshot`."
// A real password-manager/browser autofill write reaches the live DOM the
// same way this mock's `type: "password"` shape does (never through
// `jsBrowserTypeSecret`), so `collectRedactionInfo` resolving that shape via
// `aria-ref=` is exactly what this scenario looks like from
// `captureSnapshot`'s side — this test's job is to prove the resulting
// snapshot value is redacted regardless.
test("snapshot_should_redact_value_when_field_was_autofilled_out_of_band", async () => {
    const id = "sess-autofill-redact-1";
    browserGlue.sessions.set(id, {
        page: {
            url: () => "https://example.com/",
            ariaSnapshot: async () => '- textbox "Password" [ref=e3]',
            // Standing in for a password-manager/browser autofill write that
            // never went through jsBrowserTypeSecret.
            locator: (selector) => {
                assert.strictEqual(selector, "aria-ref=e3");
                return { evaluate: async () => ({ type: "password", autocomplete: "" }) };
            },
        },
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserSnapshot(id, 5000);

    assert.strictEqual(snapshot.root.value, "[REDACTED]");

    browserGlue.sessions.delete(id);
});

// Exercises `collectRedactionInfo` directly (not just `captureSnapshot`'s
// merge). Playwright's `ariaSnapshot()`/`aria-ref=` machinery already
// crosses *open* shadow boundaries when building the ref-annotated tree
// (Task 2.3.2a), so a textbox that lives inside an open shadow root is
// indistinguishable in `root`'s shape from any other textbox — proving
// `collectRedactionInfo` resolves it is exactly proving it resolves any ref
// via the shared `refLocator` mechanism.
test("collect_redaction_info_should_find_password_field_when_shadow_root_is_open", async () => {
    const root = {
        role: "generic",
        name: "",
        ref: "root",
        children: [{ role: "textbox", name: "Password", ref: "e9", children: [] }],
    };
    const page = {
        locator: (selector) => {
            assert.strictEqual(selector, "aria-ref=e9");
            return { evaluate: async () => ({ type: "password", autocomplete: "" }) };
        },
    };

    const info = await browserGlue.collectRedactionInfo(page, root);

    assert.deepStrictEqual(info, [{ ref: "e9", redact: true }]);
});

// ---------------------------------------------------------------------------
// Epic 4.3 (credential-vault): `type_secret` dispatch support —
// `jsBrowserCurrentUrl` (Task 4.3.3a) and `jsBrowserTypeSecret` (Task 4.3.2).
// `WasmBrowser::type_secret` (`crates/wasm/src/browser.rs`) has already
// resolved the credential and domain-checked it by the time it calls
// `jsBrowserTypeSecret`, so these tests only exercise the dispatch-time
// re-check and redaction — not domain checking, which lives entirely on the
// Rust side against `jsBrowserCurrentUrl`'s return value.

test("jsBrowserCurrentUrl_should_return_live_url_without_building_a_tree", async () => {
    const id = "sess-current-url-1";
    let ariaSnapshotCalls = 0;
    browserGlue.sessions.set(id, {
        page: {
            url: () => "https://evil-example.com/",
            ariaSnapshot: async () => {
                ariaSnapshotCalls += 1;
                return "";
            },
        },
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const url = await browserGlue.jsBrowserCurrentUrl(id);

    assert.strictEqual(url, "https://evil-example.com/");
    assert.strictEqual(ariaSnapshotCalls, 0, "jsBrowserCurrentUrl must not build a snapshot tree");

    browserGlue.sessions.delete(id);
});

test("js_browser_type_secret_should_refuse_write_when_live_dom_type_is_plain_text", async () => {
    const id = "sess-secret-refuse-1";
    let fillCalls = 0;
    browserGlue.sessions.set(id, {
        page: {
            url: () => "https://example.com/",
            locator: (selector) => {
                assert.strictEqual(selector, "aria-ref=e14");
                return {
                    evaluate: async () => ({ type: "text", autocomplete: "" }),
                    fill: async () => {
                        fillCalls += 1;
                    },
                };
            },
        },
        lastUsed: Date.now(),
        blocked: undefined,
    });

    await assert.rejects(
        () => browserGlue.jsBrowserTypeSecret(id, "e14", "hunter2", 5000),
        (err) => {
            assert.match(
                err.message,
                /^type_secret refused: ref "e14" resolves to a plain text field \(role=textbox, no protected\/password state\)/,
            );
            return true;
        },
    );
    assert.strictEqual(fillCalls, 0, "fill must never be called when the dispatch-time re-check refuses the write");

    browserGlue.sessions.delete(id);
});

test("js_browser_type_secret_should_accept_totp_shaped_field_with_one_time_code_autocomplete", async () => {
    const id = "sess-secret-totp-1";
    let filledValue;
    browserGlue.sessions.set(id, {
        page: {
            url: () => "https://example.com/",
            locator: () => ({
                evaluate: async () => ({ type: "text", autocomplete: "one-time-code" }),
                fill: async (value) => {
                    filledValue = value;
                },
            }),
            ariaSnapshot: async () => '- textbox "Code" [ref=e2]',
        },
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserTypeSecret(id, "e2", "123456", 5000);

    assert.strictEqual(filledValue, "123456");
    assert.strictEqual(snapshot.root.value, browserGlue.REDACTED_PLACEHOLDER);

    browserGlue.sessions.delete(id);
});

// The dispatch-time re-check (before fill) must see a password-shaped field
// to accept the write at all, but the *general* redaction pass that runs
// afterward inside `captureSnapshot` must NOT be what causes the redaction
// here — otherwise this test can't tell force-redaction apart from the
// general pass doing its ordinary job. So the mock's second `evaluate()`
// call (the general pass, post-fill) reports a non-secret shape — as a real
// password-visibility-toggle widget would, flipping `type` from "password"
// back to "text" once filled — and the assertion still expects `[REDACTED]`,
// which is only possible because `forceRedactByRef` (Task 4.3.2c) overwrites
// the acted-on node unconditionally.
test("js_browser_type_secret_should_force_redact_acted_on_node_unconditionally", async () => {
    const id = "sess-secret-redact-1";
    let filledValue;
    let evaluateCalls = 0;
    browserGlue.sessions.set(id, {
        page: {
            url: () => "https://example.com/",
            locator: (selector) => {
                assert.strictEqual(selector, "aria-ref=e1");
                return {
                    evaluate: async () => {
                        evaluateCalls += 1;
                        return evaluateCalls === 1
                            ? { type: "password", autocomplete: "" } // dispatch-time re-check
                            : { type: "text", autocomplete: "" }; // general pass: NOT secret-shaped
                    },
                    fill: async (value) => {
                        filledValue = value;
                    },
                };
            },
            ariaSnapshot: async () => '- textbox "Password" [ref=e1]',
        },
        lastUsed: Date.now(),
        blocked: undefined,
    });

    const snapshot = await browserGlue.jsBrowserTypeSecret(id, "e1", "hunter2", 5000);

    assert.strictEqual(filledValue, "hunter2");
    assert.strictEqual(snapshot.root.ref, "e1");
    assert.strictEqual(snapshot.root.value, browserGlue.REDACTED_PLACEHOLDER);

    browserGlue.sessions.delete(id);
});

test("js_browser_type_secret_should_reject_with_actionable_ref_error_when_locator_fill_rejects_stale_ref", async () => {
    const id = "sess-secret-stale-1";
    const lowLevelError = new Error("locator.fill: Error: element is not attached to the DOM");
    browserGlue.sessions.set(id, {
        page: {
            url: () => "https://example.com/",
            locator: () => ({
                evaluate: async () => ({ type: "password", autocomplete: "" }),
                fill: async () => {
                    throw lowLevelError;
                },
            }),
        },
        lastUsed: Date.now(),
        blocked: undefined,
    });

    await assert.rejects(
        () => browserGlue.jsBrowserTypeSecret(id, "e3", "hunter2", 5000),
        (err) => {
            assert.match(err.message, /^ref 'e3' not found or no longer attached:/);
            return true;
        },
    );

    browserGlue.sessions.delete(id);
});
