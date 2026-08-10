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
