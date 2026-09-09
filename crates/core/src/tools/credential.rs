//! Tool-layer function for `stapler_browser_type_secret`. Bridges
//! `BrowserDriver::type_secret` to the wire-facing `schema` types, mirroring
//! `tools/browser.rs`'s bridging role for every other browser tool.
//!
//! Deliberately holds no `CredentialStore` dependency: resolution happens
//! inside the `BrowserDriver` adapter (see `ports.rs`'s `type_secret` doc and
//! ADR-001), so this function calls `browser.type_secret` exactly once and
//! never touches `CredentialStore::resolve` itself — a second, tool-layer
//! resolve call would double the vault round-trips per request (worsening
//! the rate-limit exposure ADR-003 mitigates) and bypass the adapter's
//! in-flight dedup map, which only sees requests routed through its own
//! `CredentialStore` handle.

use crate::ports::{BrowserDriver, CredentialField, CredentialRef, Locator, PortError, SessionId};
use crate::schema::{
    BrowserActionOutput, BrowserTypeSecretInput, CredentialFieldInput, CredentialRefInput,
};
use crate::tools::browser::to_snapshot_output;

const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn resolve_timeout(timeout_seconds: Option<u32>) -> std::time::Duration {
    match timeout_seconds {
        Some(s) if s > 0 => std::time::Duration::from_secs(u64::from(s)),
        _ => DEFAULT_TIMEOUT,
    }
}

/// Fixed success note per `design/ux.md` §2 point 5: every other
/// `type`-family tool returns the real post-type value in its snapshot, so a
/// caller pattern-matching off that convention needs an explicit signal that
/// this call succeeded even though the visible value stays redacted.
const SUCCESS_NOTE: &str = "value redacted; typed successfully";

/// The 5 vault-rejection variants carry an already-actionable, `ux.md`-worded
/// sentence in their inner `String` (built by the adapter's own
/// `CredentialStore::resolve` call) — passed through verbatim, mirroring
/// `tools/browser.rs::map_error`'s `NotFound`/`SessionCrashed` convention.
/// Every other variant (including `NotFound`/`SessionCrashed` themselves,
/// which `type_secret` can also return for an unknown/dead session) falls to
/// a generic wrap naming the domain and the failing call.
fn map_error(domain: &str, err: PortError) -> String {
    match err {
        PortError::CredentialDomainMismatch(msg)
        | PortError::CredentialAmbiguous(msg)
        | PortError::CredentialUnauthenticated(msg)
        | PortError::CredentialRateLimited(msg)
        | PortError::CredentialExpired(msg) => msg,
        other => format!("type secret for {domain}: {other}"),
    }
}

pub async fn browser_type_secret<B: BrowserDriver>(
    browser: &B,
    input: BrowserTypeSecretInput,
) -> Result<BrowserActionOutput, String> {
    if input.session_id.is_empty() {
        return Err("sessionId must not be empty".to_string());
    }
    if input.ref_id.is_empty() {
        return Err("refId must not be empty".to_string());
    }
    let timeout = resolve_timeout(input.timeout_seconds);
    let session_id = SessionId(input.session_id.clone());
    let locator = Locator(input.ref_id.clone());
    let CredentialRefInput { domain, field } = input.credential;
    let field = match field {
        CredentialFieldInput::Username => CredentialField::Username,
        CredentialFieldInput::Password => CredentialField::Password,
        CredentialFieldInput::Totp => CredentialField::Totp,
    };
    let credential_ref = CredentialRef {
        domain: domain.clone(),
        field,
    };

    let snapshot = browser
        .type_secret(&session_id, &locator, &credential_ref, timeout)
        .await
        .map_err(|e| map_error(&domain, e))?;

    Ok(BrowserActionOutput {
        snapshot: to_snapshot_output(snapshot),
        note: Some(SUCCESS_NOTE.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{AxNode, AxSnapshot, NavigateResult, SessionSummary, TabAction};
    use crate::schema::CredentialRefInput;

    /// A minimal `BrowserDriver` fake exercising only `type_secret` — every
    /// other method is unreachable from this module's tests, mirroring the
    /// `todo!()`-bodied `StubBrowser` in `ports.rs`'s own test module.
    struct FakeBrowserDriver {
        type_secret_result: std::cell::RefCell<Option<Result<AxSnapshot, PortError>>>,
        type_secret_calls: std::cell::RefCell<u32>,
    }

    impl FakeBrowserDriver {
        fn new(result: Result<AxSnapshot, PortError>) -> Self {
            FakeBrowserDriver {
                type_secret_result: std::cell::RefCell::new(Some(result)),
                type_secret_calls: std::cell::RefCell::new(0),
            }
        }
    }

    impl BrowserDriver for FakeBrowserDriver {
        async fn navigate_and_extract(
            &self,
            _url: &str,
            _timeout: std::time::Duration,
        ) -> Result<crate::ports::PageExtract, PortError> {
            unreachable!("not exercised by browser_type_secret")
        }

        async fn navigate(
            &self,
            _url: &str,
            _session_id: Option<&SessionId>,
            _timeout: std::time::Duration,
        ) -> Result<NavigateResult, PortError> {
            unreachable!("not exercised by browser_type_secret")
        }

        async fn click(
            &self,
            _session_id: &SessionId,
            _locator: &Locator,
            _timeout: std::time::Duration,
        ) -> Result<AxSnapshot, PortError> {
            unreachable!("not exercised by browser_type_secret")
        }

        async fn type_text(
            &self,
            _session_id: &SessionId,
            _locator: &Locator,
            _text: &str,
            _timeout: std::time::Duration,
        ) -> Result<AxSnapshot, PortError> {
            unreachable!("not exercised by browser_type_secret")
        }

        async fn type_secret(
            &self,
            _session_id: &SessionId,
            _locator: &Locator,
            _credential_ref: &CredentialRef,
            _timeout: std::time::Duration,
        ) -> Result<AxSnapshot, PortError> {
            *self.type_secret_calls.borrow_mut() += 1;
            self.type_secret_result
                .borrow_mut()
                .take()
                .expect("type_secret called more than once in this test")
        }

        async fn snapshot(
            &self,
            _session_id: &SessionId,
            _timeout: std::time::Duration,
        ) -> Result<AxSnapshot, PortError> {
            unreachable!("not exercised by browser_type_secret")
        }

        async fn close_session(&self, _session_id: &SessionId) -> Result<(), PortError> {
            unreachable!("not exercised by browser_type_secret")
        }

        async fn list_sessions(&self) -> Result<Vec<SessionSummary>, PortError> {
            unreachable!("not exercised by browser_type_secret")
        }

        async fn tabs(
            &self,
            _session_id: &SessionId,
            _action: TabAction,
            _timeout: std::time::Duration,
        ) -> Result<crate::ports::TabsResult, PortError> {
            unreachable!("not exercised by browser_type_secret")
        }

        async fn hover(
            &self,
            _session_id: &SessionId,
            _locator: &Locator,
            _timeout: std::time::Duration,
        ) -> Result<AxSnapshot, PortError> {
            unreachable!("not exercised by browser_type_secret")
        }

        async fn select_option(
            &self,
            _session_id: &SessionId,
            _locator: &Locator,
            _values: &[String],
            _timeout: std::time::Duration,
        ) -> Result<AxSnapshot, PortError> {
            unreachable!("not exercised by browser_type_secret")
        }

        async fn press_key(
            &self,
            _session_id: &SessionId,
            _key: &str,
            _locator: Option<&Locator>,
            _timeout: std::time::Duration,
        ) -> Result<AxSnapshot, PortError> {
            unreachable!("not exercised by browser_type_secret")
        }

        async fn wait_for(
            &self,
            _session_id: &SessionId,
            _condition: crate::ports::WaitCondition,
            _timeout: std::time::Duration,
        ) -> Result<AxSnapshot, PortError> {
            unreachable!("not exercised by browser_type_secret")
        }

        async fn screenshot(
            &self,
            _session_id: &SessionId,
            _full_page: bool,
            _timeout: std::time::Duration,
        ) -> Result<Vec<u8>, PortError> {
            unreachable!("not exercised by browser_type_secret")
        }

        async fn evaluate(
            &self,
            _session_id: &SessionId,
            _function: &str,
            _locator: Option<&Locator>,
            _timeout: std::time::Duration,
        ) -> Result<serde_json::Value, PortError> {
            unreachable!("not exercised by browser_type_secret")
        }

        async fn history(
            &self,
            _session_id: &SessionId,
            _action: crate::ports::HistoryAction,
            _timeout: std::time::Duration,
        ) -> Result<AxSnapshot, PortError> {
            unreachable!("not exercised by browser_type_secret")
        }

        async fn resize(
            &self,
            _session_id: &SessionId,
            _width: u32,
            _height: u32,
            _timeout: std::time::Duration,
        ) -> Result<AxSnapshot, PortError> {
            unreachable!("not exercised by browser_type_secret")
        }
    }

    fn sample_snapshot() -> AxSnapshot {
        AxSnapshot {
            root: AxNode {
                node_ref: "e1".into(),
                role: "textbox".into(),
                name: "Password".into(),
                value: Some(crate::ports::REDACTED_PLACEHOLDER.to_string()),
                children: vec![],
            },
            url: "https://example.com/login".into(),
            truncated: false,
            navigated_from: None,
        }
    }

    fn sample_input() -> BrowserTypeSecretInput {
        BrowserTypeSecretInput {
            session_id: "sess-1".into(),
            ref_id: "e1".into(),
            credential: CredentialRefInput {
                domain: "example.com".into(),
                field: CredentialFieldInput::Password,
            },
            timeout_seconds: None,
        }
    }

    #[tokio::test]
    async fn browser_type_secret_should_set_note_to_fixed_success_string_when_type_secret_succeeds()
    {
        let driver = FakeBrowserDriver::new(Ok(sample_snapshot()));

        let result = browser_type_secret(&driver, sample_input()).await.unwrap();

        assert_eq!(
            result.note,
            Some("value redacted; typed successfully".to_string())
        );
    }

    #[tokio::test]
    async fn browser_type_secret_should_pass_through_credential_expired_message_verbatim_when_type_secret_errors(
    ) {
        let expired_message = "credential expired: TOTP code for example.com expired before it \
             could be typed (generated t, window closed t+30s) — not typed. Retry the same call; \
             a fresh code will be generated."
            .to_string();
        let driver =
            FakeBrowserDriver::new(Err(PortError::CredentialExpired(expired_message.clone())));

        let err = browser_type_secret(&driver, sample_input())
            .await
            .expect_err("credential-expired should surface as an error");

        assert_eq!(err, expired_message);
    }

    /// Structural/negative control (per Epic 5.2's AC): `browser_type_secret`
    /// makes exactly one vault-touching call — `browser.type_secret` — and
    /// never calls `CredentialStore::resolve`. `FakeBrowserDriver` above
    /// implements only `BrowserDriver` (no `CredentialStore` impl exists for
    /// it at all), and `browser_type_secret`'s signature has a single generic
    /// bound (`B: BrowserDriver`), so this test compiling — and observing
    /// `type_secret` called exactly once — is itself the verification: there
    /// is no `CredentialStore` type in scope this function could have called
    /// through.
    #[tokio::test]
    async fn browser_type_secret_should_reference_only_browser_driver_when_body_is_inspected() {
        let driver = FakeBrowserDriver::new(Ok(sample_snapshot()));

        let _ = browser_type_secret(&driver, sample_input()).await;

        assert_eq!(*driver.type_secret_calls.borrow(), 1);
    }

    #[tokio::test]
    async fn browser_type_secret_should_reject_empty_session_id() {
        let driver = FakeBrowserDriver::new(Ok(sample_snapshot()));
        let mut input = sample_input();
        input.session_id = String::new();

        let err = browser_type_secret(&driver, input)
            .await
            .expect_err("empty sessionId should be rejected");

        assert_eq!(err, "sessionId must not be empty");
        assert_eq!(*driver.type_secret_calls.borrow(), 0);
    }

    #[tokio::test]
    async fn browser_type_secret_should_reject_empty_ref_id() {
        let driver = FakeBrowserDriver::new(Ok(sample_snapshot()));
        let mut input = sample_input();
        input.ref_id = String::new();

        let err = browser_type_secret(&driver, input)
            .await
            .expect_err("empty refId should be rejected");

        assert_eq!(err, "refId must not be empty");
        assert_eq!(*driver.type_secret_calls.borrow(), 0);
    }
}
