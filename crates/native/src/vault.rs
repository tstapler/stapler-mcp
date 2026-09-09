//! Native `CredentialStore` adapter: resolves a `CredentialRef` to a
//! `SecretValue` by invoking the 1Password CLI (`op`) via `ProcessSpawner`,
//! with a vault/item ID cache (ADR-003 item 1) and in-flight dedup of
//! concurrent identical `resolve()` calls (ADR-003 item 3 — the chosen
//! mitigation for `op`'s account-wide rate limit, `pitfalls.md` §2a).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use futures::future::{FutureExt, LocalBoxFuture, Shared};
use stapler_mcp_core::ports::{
    CredentialField, CredentialRef, CredentialStore, PortError, ProcessOutput, ProcessSpawner,
    SecretValue,
};

/// One waiter's view of an in-flight `op` call. `Rc`-wrapped on both sides
/// (never a bare `Result<SecretValue, PortError>`) because `Shared`'s
/// `Output` must be `Clone` and neither `SecretValue` (zeroize design, Task
/// 1.1.1c) nor `PortError` (no `Clone` impl, `crates/core/src/ports.rs`) is
/// — see `clone_port_error` below for how each waiter still gets its own
/// independently owned value out of the shared result.
type PendingOutput = Result<Rc<SecretValue>, Rc<PortError>>;
type PendingFuture = Shared<LocalBoxFuture<'static, PendingOutput>>;

struct State<S: ProcessSpawner> {
    spawner: S,
    // Read once at construction (Phase 6, `main.rs`, via `EnvPort`) and
    // handed in, keeping this struct free of a direct `EnvPort` dependency.
    // Not currently read by the `op` invocation path below: `NativeSpawner
    // ::spawn_and_capture` (crates/native/src/spawn.rs) already forwards
    // `OP_SERVICE_ACCOUNT_TOKEN` from the daemon's own process environment
    // to each `op` child process. Retained per the planned struct shape —
    // a future epic may use it for pre-flight validation.
    #[allow(dead_code)]
    service_account_token: String,
    /// domain -> (vault_id, item_id). ADR-003: this cache exists purely to
    /// avoid repeat *name -> ID* lookups (`op`'s 3x-request-cost list/filter
    /// path, `stack.md`) — it never holds a resolved value. Its value type,
    /// `(String, String)`, makes that structural, not just conventional:
    /// there is no code path that could insert a `SecretValue` here.
    id_cache: RefCell<HashMap<String, (String, String)>>,
    /// In-flight dedup (ADR-003 item 3) — distinct from `id_cache` above:
    /// this map coalesces concurrent *value* resolves for the identical
    /// `CredentialRef`, not repeat ID lookups. An entry is removed the
    /// moment its `op` call finishes, success or failure, so this is
    /// deliberately not a value cache — the next `resolve()` for a
    /// previously-seen ref always triggers a fresh `op` invocation.
    pending: RefCell<HashMap<CredentialRef, PendingFuture>>,
}

impl<S: ProcessSpawner> State<S> {
    // TODO(Epic 3.3): implement the real domain lookup — `op item list
    // --categories Login --format json`, filtered by the shared `same_host`
    // helper (Epic 1.4) matching each item's `urls[].href` host against
    // `domain`. Zero matches -> `PortError::CredentialDomainMismatch`; more
    // than one -> `PortError::CredentialAmbiguous` (candidate titles,
    // comma-separated); exactly one -> `Ok((vault_id, item_id))`. This stub
    // keeps a synchronous signature only so the crate compiles for this
    // epic's isolated testing — the real implementation will need
    // `spawn_and_capture`, so Epic 3.3 will likely need to make this an
    // `async fn` (and thread that change through `resolve_uncached` below).
    fn lookup_domain(&self, domain: &str) -> Result<(String, String), PortError> {
        let _ = domain;
        todo!("Epic 3.3: op item list --categories Login --format json, filtered by same_host")
    }
}

pub struct NativeCredentialStore<S: ProcessSpawner> {
    state: Rc<State<S>>,
}

impl<S: ProcessSpawner> NativeCredentialStore<S> {
    pub fn new(spawner: S, service_account_token: String) -> Self {
        NativeCredentialStore {
            state: Rc::new(State {
                spawner,
                service_account_token,
                id_cache: RefCell::new(HashMap::new()),
                pending: RefCell::new(HashMap::new()),
            }),
        }
    }
}

impl<S: ProcessSpawner + 'static> CredentialStore for NativeCredentialStore<S> {
    async fn resolve(&self, credential_ref: &CredentialRef) -> Result<SecretValue, PortError> {
        let shared = {
            let mut pending = self.state.pending.borrow_mut();
            if let Some(existing) = pending.get(credential_ref) {
                existing.clone()
            } else {
                let state = Rc::clone(&self.state);
                let cref = credential_ref.clone();
                let cleanup_cref = credential_ref.clone();
                let work: LocalBoxFuture<'static, PendingOutput> = Box::pin(async move {
                    let result = resolve_uncached(&state, &cref).await;
                    // Dedup only, never a value cache (ADR-003 item 3): the
                    // entry is cleared the instant the real `op` call
                    // finishes, success or failure, so a later `resolve()`
                    // for this ref always issues a fresh invocation.
                    state.pending.borrow_mut().remove(&cleanup_cref);
                    result.map(Rc::new).map_err(Rc::new)
                });
                let shared: PendingFuture = work.shared();
                pending.insert(credential_ref.clone(), shared.clone());
                // Drive the shared future to completion as its own task so
                // it always runs through to (and clears) the `pending`
                // entry independent of whether any individual `resolve()`
                // caller is still awaiting it — matches this daemon's
                // single-threaded `current_thread` + `LocalSet` runtime.
                tokio::task::spawn_local(shared.clone().map(|_| ()));
                shared
            }
        };

        match shared.await {
            Ok(rc) => Ok(SecretValue::new(rc.expose().to_string())),
            Err(rc_err) => Err(clone_port_error(&rc_err)),
        }
    }
}

/// `PortError` has no `Clone` impl (`crates/core/src/ports.rs`, out of this
/// epic's scope to change) but every variant's payload is a plain `String`
/// (or unit, for `Timeout`) — so each dedup waiter can get its own owned
/// `PortError` of the same variant/message by cloning that inner `String`,
/// without needing the enum itself to be `Clone`. Exhaustive so a future
/// variant added to `PortError` fails this crate's build until handled here.
fn clone_port_error(e: &PortError) -> PortError {
    match e {
        PortError::Io(s) => PortError::Io(s.clone()),
        PortError::Timeout => PortError::Timeout,
        PortError::Other(s) => PortError::Other(s.clone()),
        PortError::NotFound(s) => PortError::NotFound(s.clone()),
        PortError::SessionCrashed(s) => PortError::SessionCrashed(s.clone()),
        PortError::NotActionable(s) => PortError::NotActionable(s.clone()),
        PortError::CredentialDomainMismatch(s) => PortError::CredentialDomainMismatch(s.clone()),
        PortError::CredentialAmbiguous(s) => PortError::CredentialAmbiguous(s.clone()),
        PortError::CredentialUnauthenticated(s) => PortError::CredentialUnauthenticated(s.clone()),
        PortError::CredentialRateLimited(s) => PortError::CredentialRateLimited(s.clone()),
        PortError::CredentialExpired(s) => PortError::CredentialExpired(s.clone()),
    }
}

/// The actual (uncached-at-the-`pending`-level) `op` resolve: cache
/// check-then-populate around the domain lookup (Story 3.2.3), argv build
/// (Stories 3.2.1/3.2.2), the `spawn_and_capture` call, error mapping
/// (Story 3.2.4), and the audit-log line — in that order.
async fn resolve_uncached<S: ProcessSpawner>(
    state: &State<S>,
    credential_ref: &CredentialRef,
) -> Result<SecretValue, PortError> {
    let domain = credential_ref.domain.as_str();

    let cached = state.id_cache.borrow().get(domain).cloned();
    let (vault_id, item_id) = match cached {
        Some(ids) => ids,
        None => {
            let ids = state.lookup_domain(domain)?;
            state
                .id_cache
                .borrow_mut()
                .insert(domain.to_string(), ids.clone());
            ids
        }
    };

    let argv_owned = build_argv(credential_ref.field, &vault_id, &item_id);
    let argv: Vec<&str> = argv_owned.iter().map(String::as_str).collect();

    let result = match state.spawner.spawn_and_capture(&argv).await {
        Ok(ProcessOutput {
            stdout,
            exit_code: 0,
            ..
        }) => {
            // The resolved value reaches `SecretValue` via one buffer,
            // trimmed of a single trailing newline — never routed through
            // `serde_json::Value` (pitfalls.md §1d has no `zeroize` impl and
            // would add an untracked intermediate copy).
            let text = String::from_utf8_lossy(&stdout).into_owned();
            let trimmed = text.strip_suffix('\n').unwrap_or(&text);
            Ok(SecretValue::new(trimmed.to_string()))
        }
        // Built from `stderr` only, never `stdout` (pitfalls.md §1b) — even
        // `stderr` is untrusted text, mapped to a fixed `PortError` variant
        // rather than `format!()`-ed wholesale into an error message.
        Ok(ProcessOutput { stderr, .. }) => Err(build_op_error(&state.spawner, &stderr).await),
        Err(port_err) => Err(port_err),
    };

    log_resolve_outcome(domain, credential_ref.field, &result);
    result
}

/// Converts `field` to its lowercase `op://` wire segment. `Totp` never
/// reaches this — it resolves via `op item get --otp` (its own branch in
/// `build_argv`), not `op read`.
fn field_wire_segment(field: CredentialField) -> &'static str {
    match field {
        CredentialField::Username => "username",
        CredentialField::Password => "password",
        CredentialField::Totp => unreachable!("Totp resolves via op item get, not op read"),
    }
}

/// Builds the exact `op` argv for `field` against a resolved `(vault_id,
/// item_id)` — always literal `Vec` entries, never a shell string. Vault/item
/// IDs, not names, are used throughout (`stack.md`'s 3x-request-cost note);
/// `--vault` is always passed explicitly since service accounts have no
/// default-vault inference (`architecture.md` §6).
fn build_argv(field: CredentialField, vault_id: &str, item_id: &str) -> Vec<String> {
    match field {
        CredentialField::Totp => vec![
            "op".to_string(),
            "item".to_string(),
            "get".to_string(),
            "--vault".to_string(),
            vault_id.to_string(),
            item_id.to_string(),
            "--otp".to_string(),
        ],
        CredentialField::Username | CredentialField::Password => vec![
            "op".to_string(),
            "read".to_string(),
            "--vault".to_string(),
            vault_id.to_string(),
            format!("op://{vault_id}/{item_id}/{}", field_wire_segment(field)),
        ],
    }
}

/// Verbatim per `ux.md` §4 example 3 (confirmed against the real `op` CLI
/// locally, `op` 2.34.1) minus the `PortError::CredentialUnauthenticated`
/// `Display` impl's own `"vault unauthenticated: "` prefix, which is added
/// by `Display` itself (`crates/core/src/ports.rs`) rather than baked into
/// this constant, matching every other `Credential*` variant's convention.
const UNAUTHENTICATED_MESSAGE: &str = "1Password CLI reports not signed in — not typed. This requires human action (run `op signin` or unlock the desktop app); the agent cannot resolve this itself.";

const RATE_LIMIT_UNSPECIFIED_DELAY: &str = "retry after an unspecified delay";

fn stderr_contains(stderr_text: &str, needle: &str) -> bool {
    stderr_text.to_ascii_lowercase().contains(&needle.to_ascii_lowercase())
}

/// Maps a failed `op` invocation's `stderr` to one of the 5 vault
/// `PortError` variants (Story 3.2.4). A small, fixed set of substring
/// checks — never `format!()`-ing the raw `stderr` buffer into the error
/// (pitfalls.md §1b) — only fixed, hand-written strings per matched pattern.
async fn build_op_error<S: ProcessSpawner>(spawner: &S, stderr: &[u8]) -> PortError {
    let stderr_text = String::from_utf8_lossy(stderr);

    if stderr_contains(&stderr_text, "not currently signed in") || stderr_contains(&stderr_text, "op signin") {
        return PortError::CredentialUnauthenticated(UNAUTHENTICATED_MESSAGE.to_string());
    }
    if stderr_contains(&stderr_text, "too many requests") || stderr_contains(&stderr_text, "rate limit") {
        let detail = rate_limit_retry_after(spawner).await;
        return PortError::CredentialRateLimited(detail);
    }

    // Unrecognized `op` failure: a fixed, generic message, never the raw
    // `stderr` buffer (pitfalls.md §1b).
    PortError::Other("op invocation failed".to_string())
}

/// Best-effort `op service-account ratelimit` diagnostic (pitfalls.md §2a)
/// to extract a retry-after duration. `op service-account ratelimit`'s exact
/// output shape is unverified against a real account (`plan.md`'s
/// Unresolved Questions) — any failure of this secondary call (non-zero
/// exit, unparsable stdout) falls back to the unspecified-delay phrasing
/// rather than failing the whole error-mapping path.
async fn rate_limit_retry_after<S: ProcessSpawner>(spawner: &S) -> String {
    match spawner
        .spawn_and_capture(&["op", "service-account", "ratelimit"])
        .await
    {
        Ok(ProcessOutput {
            stdout,
            exit_code: 0,
            ..
        }) => match parse_retry_after_seconds(&stdout) {
            Some(secs) => format!("retry after {secs}s"),
            None => RATE_LIMIT_UNSPECIFIED_DELAY.to_string(),
        },
        _ => RATE_LIMIT_UNSPECIFIED_DELAY.to_string(),
    }
}

/// Heuristic best-effort parse: the first whitespace-delimited token that
/// parses as a plain (optionally `s`-suffixed) integer.
fn parse_retry_after_seconds(stdout: &[u8]) -> Option<u64> {
    String::from_utf8_lossy(stdout)
        .split_whitespace()
        .find_map(|token| token.trim_end_matches('s').parse::<u64>().ok())
}

#[cfg(test)]
thread_local! {
    /// Test-only override for `log_resolve_outcome`'s destination — lets
    /// tests assert the audit line's shape without OS-level stderr capture.
    /// `None` (production, and any test that never sets it) falls through
    /// to the real `eprintln!`.
    static TEST_LOG_SINK: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// One `eprintln!` line per `resolve()` attempt (REQ-19/Epic 5.4's
/// observability requirement), mirroring `webcrawl.rs`'s SSRF-deny-log
/// convention (`crates/core/src/tools/webcrawl.rs:266`) — same stderr
/// channel, since stdout carries the MCP JSON-RPC stream. Always contains
/// the domain, field, and outcome in plain text; never the resolved value —
/// `result` is only ever inspected by discriminant here, relying on
/// `SecretValue`'s non-leaking `Debug` rather than manually formatting the
/// exposed value. A rejection is logged identically to a success (`ux.md`
/// §3: both are equally audit-worthy).
fn log_resolve_outcome(domain: &str, field: CredentialField, result: &Result<SecretValue, PortError>) {
    let field_str = match field {
        CredentialField::Username => "username",
        CredentialField::Password => "password",
        CredentialField::Totp => "totp",
    };
    let outcome = match result {
        Ok(_) => "resolved",
        Err(PortError::CredentialDomainMismatch(_)) => "rejected-domain-mismatch",
        Err(PortError::CredentialAmbiguous(_)) => "rejected-ambiguous",
        Err(PortError::CredentialRateLimited(_)) => "rate-limited",
        Err(PortError::CredentialExpired(_)) => "totp-expired",
        // `CredentialUnauthenticated` and any other unexpected `PortError`
        // (Io/Other/...) all mean the vault lookup itself couldn't
        // complete — per plan.md's Observability Plan outcome set.
        Err(_) => "vault-lookup-failed",
    };
    let line = format!(
        "stapler-mcp: credential resolve domain='{domain}' field={field_str} outcome={outcome}"
    );

    #[cfg(test)]
    {
        let captured = TEST_LOG_SINK.with(|sink| {
            let mut sink = sink.borrow_mut();
            match sink.as_mut() {
                Some(lines) => {
                    lines.push(line.clone());
                    true
                }
                None => false,
            }
        });
        if captured {
            return;
        }
    }

    eprintln!("{line}");
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use tokio::sync::Notify;

    use super::*;

    /// Fake `ProcessSpawner` mirroring `FakeBrowserDriver`'s queued-result
    /// shape (`crates/core/src/tools/browser.rs`): a `VecDeque` of canned
    /// `spawn_and_capture` results consumed in call order, plus a record of
    /// every argv actually passed — no real `op` binary needed.
    #[derive(Default)]
    struct FakeProcessSpawner {
        responses: RefCell<VecDeque<Result<ProcessOutput, PortError>>>,
        calls: RefCell<Vec<Vec<String>>>,
        /// When set, `spawn_and_capture` notifies `started` then waits on
        /// `gate` before consuming its queued response — lets a test force
        /// two concurrent `resolve()` calls to overlap deterministically
        /// (Task 3.2.5c) instead of relying on timing.
        gate: Option<Rc<Notify>>,
        started: Option<Rc<Notify>>,
    }

    impl FakeProcessSpawner {
        fn with_responses(responses: Vec<Result<ProcessOutput, PortError>>) -> Self {
            FakeProcessSpawner {
                responses: RefCell::new(responses.into()),
                ..Default::default()
            }
        }

        fn gated(mut self, gate: Rc<Notify>, started: Rc<Notify>) -> Self {
            self.gate = Some(gate);
            self.started = Some(started);
            self
        }
    }

    impl ProcessSpawner for FakeProcessSpawner {
        async fn spawn_daemon(
            &self,
            _exe_hint: Option<&str>,
            _log_path: &str,
        ) -> Result<(), PortError> {
            unimplemented!("not exercised by vault.rs's tests")
        }

        async fn spawn_and_capture(&self, argv: &[&str]) -> Result<ProcessOutput, PortError> {
            self.calls
                .borrow_mut()
                .push(argv.iter().map(|s| s.to_string()).collect());

            if let (Some(gate), Some(started)) = (&self.gate, &self.started) {
                started.notify_one();
                gate.notified().await;
            }

            self.responses
                .borrow_mut()
                .pop_front()
                .expect("FakeProcessSpawner: more spawn_and_capture calls than queued responses")
        }
    }

    fn ok_output(stdout: &str) -> Result<ProcessOutput, PortError> {
        Ok(ProcessOutput {
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
        })
    }

    fn err_output(stderr: &str) -> Result<ProcessOutput, PortError> {
        Ok(ProcessOutput {
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
            exit_code: 1,
        })
    }

    fn seed_cache<S: ProcessSpawner>(
        store: &NativeCredentialStore<S>,
        domain: &str,
        vault_id: &str,
        item_id: &str,
    ) {
        store
            .state
            .id_cache
            .borrow_mut()
            .insert(domain.to_string(), (vault_id.to_string(), item_id.to_string()));
    }

    fn password_ref(domain: &str) -> CredentialRef {
        CredentialRef {
            domain: domain.to_string(),
            field: CredentialField::Password,
        }
    }

    // -- Story 3.2.1/3.2.2: argv shape --

    #[test]
    fn native_credential_store_resolve_should_build_op_read_argv_when_field_is_password() {
        let argv = build_argv(CredentialField::Password, "v1", "i1");
        assert_eq!(
            argv,
            vec!["op", "read", "--vault", "v1", "op://v1/i1/password"]
        );
    }

    #[test]
    fn native_credential_store_resolve_should_build_op_item_get_otp_argv_when_field_is_totp() {
        let argv = build_argv(CredentialField::Totp, "v1", "i1");
        assert_eq!(argv, vec!["op", "item", "get", "--vault", "v1", "i1", "--otp"]);
    }

    // -- Story 3.2.4: error mapping --

    #[tokio::test(flavor = "current_thread")]
    async fn native_credential_store_resolve_should_return_credential_unauthenticated_when_op_reports_not_signed_in(
    ) {
        tokio::task::LocalSet::new()
            .run_until(async {
                let spawner = FakeProcessSpawner::with_responses(vec![err_output(
                    "[ERROR] 2026/09/08 You are not currently signed in. Please run `op signin --help` for instructions.\n",
                )]);
                let store = NativeCredentialStore::new(spawner, "token".to_string());
                seed_cache(&store, "example.com", "v1", "i1");

                let err = store.resolve(&password_ref("example.com")).await.unwrap_err();

                match err {
                    PortError::CredentialUnauthenticated(msg) => {
                        assert_eq!(msg, UNAUTHENTICATED_MESSAGE);
                    }
                    other => panic!("expected CredentialUnauthenticated, got {other:?}"),
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_credential_store_resolve_should_return_credential_rate_limited_with_retry_after_when_op_rate_limit_hit(
    ) {
        tokio::task::LocalSet::new()
            .run_until(async {
                let spawner = FakeProcessSpawner::with_responses(vec![
                    err_output("[ERROR] 2026/09/08 You've made too many requests. Please try again later.\n"),
                    ok_output("15\n"),
                ]);
                let store = NativeCredentialStore::new(spawner, "token".to_string());
                seed_cache(&store, "example.com", "v1", "i1");

                let err = store.resolve(&password_ref("example.com")).await.unwrap_err();
                let rendered = format!("{err}");
                assert!(rendered.contains("retry after"), "rendered = {rendered}");
                assert!(matches!(err, PortError::CredentialRateLimited(_)));
            })
            .await;
    }

    // -- Story 3.2.3: ID cache --

    #[tokio::test(flavor = "current_thread")]
    async fn native_credential_store_resolve_should_skip_domain_lookup_when_id_cache_already_populated(
    ) {
        tokio::task::LocalSet::new()
            .run_until(async {
                // `lookup_domain` is Epic 3.3's `todo!()` stub — if the cache
                // check above it were broken, this call would panic instead
                // of returning a value, failing this test loudly.
                let spawner = FakeProcessSpawner::with_responses(vec![ok_output("hunter2\n")]);
                let store = NativeCredentialStore::new(spawner, "token".to_string());
                seed_cache(&store, "example.com", "v1", "i1");

                let secret = store
                    .resolve(&password_ref("example.com"))
                    .await
                    .expect("cache hit should resolve without invoking lookup_domain");

                assert_eq!(secret.expose(), "hunter2");
                assert_eq!(store.state.spawner.calls.borrow().len(), 1);
            })
            .await;
    }

    #[test]
    fn id_cache_should_only_hold_identifiers_when_typed_as_hash_map_of_string_pairs() {
        let store = NativeCredentialStore::new(FakeProcessSpawner::default(), "token".to_string());
        store
            .state
            .id_cache
            .borrow_mut()
            .insert("example.com".to_string(), ("vault1".to_string(), "item1".to_string()));

        let cached = store.state.id_cache.borrow().get("example.com").cloned();
        assert_eq!(cached, Some(("vault1".to_string(), "item1".to_string())));
    }

    // -- Story 3.2.5: in-flight dedup --

    #[tokio::test(flavor = "current_thread")]
    async fn native_credential_store_resolve_should_issue_one_op_call_when_two_concurrent_resolves_share_same_ref(
    ) {
        tokio::task::LocalSet::new()
            .run_until(async {
                let gate = Rc::new(Notify::new());
                let started = Rc::new(Notify::new());
                let spawner = FakeProcessSpawner::with_responses(vec![ok_output("hunter2\n")])
                    .gated(Rc::clone(&gate), Rc::clone(&started));
                let store = Rc::new(NativeCredentialStore::new(spawner, "token".to_string()));
                seed_cache(&store, "example.com", "v1", "i1");

                let store1 = Rc::clone(&store);
                let task1 =
                    tokio::task::spawn_local(async move { store1.resolve(&password_ref("example.com")).await });

                // Wait until the first call is actually blocked inside
                // `spawn_and_capture` before starting the second.
                started.notified().await;

                let store2 = Rc::clone(&store);
                let task2 =
                    tokio::task::spawn_local(async move { store2.resolve(&password_ref("example.com")).await });

                // Let task2 run far enough to hit the `pending` dedup check
                // (and start awaiting the shared future) before releasing
                // the gate.
                tokio::task::yield_now().await;
                tokio::task::yield_now().await;

                gate.notify_one();

                let (r1, r2) = tokio::join!(task1, task2);
                let secret1 = r1.unwrap().unwrap();
                let secret2 = r2.unwrap().unwrap();

                assert_eq!(secret1.expose(), "hunter2");
                assert_eq!(secret2.expose(), "hunter2");
                assert_eq!(
                    store.state.spawner.calls.borrow().len(),
                    1,
                    "two concurrent resolves for the same ref must issue exactly one op call"
                );
                assert!(
                    store.state.pending.borrow().is_empty(),
                    "pending map must be empty once every waiter has been served"
                );
            })
            .await;
    }

    #[test]
    fn pending_map_should_have_no_entry_when_all_waiters_have_been_served() {
        // Direct structural check on an empty store: nothing is ever
        // inserted into `pending` outside of an in-flight `resolve()` call,
        // so a freshly constructed store's map starts (and, per the
        // dedup test above, ends) empty.
        let store = NativeCredentialStore::new(FakeProcessSpawner::default(), "token".to_string());
        assert!(store.state.pending.borrow().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_credential_store_resolve_should_issue_two_op_calls_when_concurrent_resolves_target_different_refs(
    ) {
        tokio::task::LocalSet::new()
            .run_until(async {
                let spawner = FakeProcessSpawner::with_responses(vec![
                    ok_output("secret-a\n"),
                    ok_output("secret-b\n"),
                ]);
                let store = Rc::new(NativeCredentialStore::new(spawner, "token".to_string()));
                seed_cache(&store, "a.example", "va", "ia");
                seed_cache(&store, "b.example", "vb", "ib");

                let store_a = Rc::clone(&store);
                let task_a =
                    tokio::task::spawn_local(async move { store_a.resolve(&password_ref("a.example")).await });
                let store_b = Rc::clone(&store);
                let task_b =
                    tokio::task::spawn_local(async move { store_b.resolve(&password_ref("b.example")).await });

                let (ra, rb) = tokio::join!(task_a, task_b);
                ra.unwrap().unwrap();
                rb.unwrap().unwrap();

                let calls = store.state.spawner.calls.borrow();
                assert_eq!(calls.len(), 2, "distinct refs must not be deduped together");
                assert!(calls.iter().any(|c| c.contains(&"op://va/ia/password".to_string())));
                assert!(calls.iter().any(|c| c.contains(&"op://vb/ib/password".to_string())));
            })
            .await;
    }

    // -- Observability --

    #[tokio::test(flavor = "current_thread")]
    async fn native_credential_store_resolve_should_emit_log_line_with_domain_field_and_outcome_when_rejected(
    ) {
        tokio::task::LocalSet::new()
            .run_until(async {
                let spawner = FakeProcessSpawner::with_responses(vec![err_output(
                    "[ERROR] 2026/09/08 You are not currently signed in. Please run `op signin --help` for instructions.\n",
                )]);
                let store = NativeCredentialStore::new(spawner, "token".to_string());
                seed_cache(&store, "example.com", "v1", "i1");

                TEST_LOG_SINK.with(|s| *s.borrow_mut() = Some(Vec::new()));
                let _ = store.resolve(&password_ref("example.com")).await;
                let lines = TEST_LOG_SINK.with(|s| s.borrow_mut().take().unwrap());

                assert_eq!(lines.len(), 1);
                assert!(lines[0].contains("example.com"));
                assert!(lines[0].contains("field=password"));
                assert!(lines[0].contains("outcome=vault-lookup-failed"));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_credential_store_resolve_should_never_emit_resolved_value_in_log_line_when_resolve_succeeds(
    ) {
        tokio::task::LocalSet::new()
            .run_until(async {
                let spawner = FakeProcessSpawner::with_responses(vec![ok_output("hunter2\n")]);
                let store = NativeCredentialStore::new(spawner, "token".to_string());
                seed_cache(&store, "example.com", "v1", "i1");

                TEST_LOG_SINK.with(|s| *s.borrow_mut() = Some(Vec::new()));
                let secret = store.resolve(&password_ref("example.com")).await.unwrap();
                let lines = TEST_LOG_SINK.with(|s| s.borrow_mut().take().unwrap());

                assert_eq!(secret.expose(), "hunter2");
                assert_eq!(lines.len(), 1);
                assert!(lines[0].contains("outcome=resolved"));
                assert!(!lines[0].contains("hunter2"));
            })
            .await;
    }
}
