//! Wasm `CredentialStore` adapter (Epic 4.3): wraps
//! `crates/wasm/src/glue/vault.js`'s `jsResolveCredential` via a
//! `wasm-bindgen` extern binding, mirroring `crates/wasm/src/browser.rs`'s
//! existing `#[wasm_bindgen(module = "/src/glue/browser.js")]` pattern and
//! `map_js_error`'s message-substring dispatch — one crate-boundary crossing
//! per `resolve()` call, same as every other wasm port adapter.

use stapler_mcp_core::ports::{
    CredentialField, CredentialRef, CredentialStore, PortError, SecretValue,
};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::js_util::js_err_to_string;

#[wasm_bindgen(module = "/src/glue/vault.js")]
extern "C" {
    #[wasm_bindgen(js_name = jsResolveCredential)]
    fn js_resolve_credential(domain: &str, field: &str) -> js_sys::Promise;
}

/// Converts `field` to its lowercase wire string before crossing the
/// `wasm_bindgen` extern boundary — JS has no enum type, so `vault.js`'s
/// `jsResolveCredential` receives a plain string, never the Rust enum.
/// Mirrors native's `field_wire_segment` (`crates/native/src/vault.rs`),
/// except `Totp` is a real, reachable wire value here (native's
/// `field_wire_segment` deliberately never handles it, since native routes
/// TOTP through a different `op` subcommand entirely) — `vault.js`'s own
/// field-conditional branch is what dispatches on it. `pub(crate)` so
/// `browser.rs`'s pre-resolve domain-mismatch audit-log call (A1 code review
/// fix) can reuse the same wire strings rather than re-deriving them.
pub(crate) fn field_wire_string(field: CredentialField) -> &'static str {
    match field {
        CredentialField::Username => "username",
        CredentialField::Password => "password",
        CredentialField::Totp => "totp",
    }
}

/// Maps a JS-thrown/rejected error's message to one of the 5 vault
/// `PortError` variants (Task 4.3.1b), mirroring
/// `crates/wasm/src/browser.rs`'s `map_js_error` substring-dispatch
/// convention and `crates/native/src/vault.rs`'s `build_op_error` fixed-variant
/// mapping. The marker substrings matched here are produced by
/// `crates/wasm/src/glue/vault.js`'s `pickUniqueMatch`/`normalizeVaultError` —
/// see those functions' doc comments for why each message is worded the way
/// it is. Checked in an order where no legitimate message can match more
/// than one branch (see `normalizeVaultError`'s doc comment on why "expired"
/// never appears in its unauthenticated-error wording).
fn map_vault_js_error(message: String) -> PortError {
    let lower = message.to_ascii_lowercase();
    if lower.contains("ambiguous") {
        PortError::CredentialAmbiguous(message)
    } else if lower.contains("no vault entry for domain") {
        PortError::CredentialDomainMismatch(message)
    } else if lower.contains("not authenticated") || lower.contains("not signed in") {
        PortError::CredentialUnauthenticated(message)
    } else if lower.contains("rate limit") {
        PortError::CredentialRateLimited(message)
    } else if lower.contains("expired") {
        PortError::CredentialExpired(message)
    } else {
        PortError::Other(message)
    }
}

pub struct WasmCredentialStore;

impl CredentialStore for WasmCredentialStore {
    async fn resolve(&self, credential_ref: &CredentialRef) -> Result<SecretValue, PortError> {
        let field = field_wire_string(credential_ref.field);
        let result = JsFuture::from(js_resolve_credential(&credential_ref.domain, field))
            .await
            .map_err(|e| map_vault_js_error(js_err_to_string(&e)))?;

        let value = result.as_string().ok_or_else(|| {
            PortError::Other("jsResolveCredential resolved with a non-string value".to_string())
        })?;
        Ok(SecretValue::new(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_wire_string_should_produce_lowercase_wire_values_for_every_credential_field_variant() {
        assert_eq!(field_wire_string(CredentialField::Username), "username");
        assert_eq!(field_wire_string(CredentialField::Password), "password");
        assert_eq!(field_wire_string(CredentialField::Totp), "totp");
    }

    /// Task 4.3.1 AC / required test: a JS rejection whose message matches
    /// `vault.js`'s `pickUniqueMatch` ambiguous-match wording must map to
    /// `PortError::CredentialAmbiguous`, carrying the `ux.md` §4 example-2
    /// verbatim message through unchanged.
    #[test]
    fn wasm_credential_store_resolve_should_map_ambiguous_error_when_sdk_rejects_with_multiple_matches(
    ) {
        let message = "2 vault items match domain \"example.com\" — ambiguous, not typed. Ask the user which item to use, or scope the request further. Candidates: Example Login A, Example Login B".to_string();

        let err = map_vault_js_error(message.clone());

        match err {
            PortError::CredentialAmbiguous(m) => assert_eq!(m, message),
            other => panic!("expected PortError::CredentialAmbiguous, got {other:?}"),
        }
    }

    #[test]
    fn map_vault_js_error_should_map_domain_mismatch_marker_to_credential_domain_mismatch() {
        let message = "no vault entry for domain \"example.com\" — not typed".to_string();

        let err = map_vault_js_error(message.clone());

        match err {
            PortError::CredentialDomainMismatch(m) => assert_eq!(m, message),
            other => panic!("expected PortError::CredentialDomainMismatch, got {other:?}"),
        }
    }

    #[test]
    fn map_vault_js_error_should_map_unauthenticated_marker_to_credential_unauthenticated() {
        let message = "1Password SDK reports not authenticated — not typed. This requires human action (verify OP_SERVICE_ACCOUNT_TOKEN); the agent cannot resolve this itself. Detail: invalid service account token".to_string();

        let err = map_vault_js_error(message.clone());

        match err {
            PortError::CredentialUnauthenticated(m) => assert_eq!(m, message),
            other => panic!("expected PortError::CredentialUnauthenticated, got {other:?}"),
        }
    }

    #[test]
    fn map_vault_js_error_should_map_rate_limit_marker_to_credential_rate_limited() {
        let message =
            "1Password rate limit exceeded — not typed. Retry after an unspecified delay. Detail: too many requests"
                .to_string();

        let err = map_vault_js_error(message.clone());

        match err {
            PortError::CredentialRateLimited(m) => assert_eq!(m, message),
            other => panic!("expected PortError::CredentialRateLimited, got {other:?}"),
        }
    }

    /// C2 code review fix: the domain-mismatch/unauthenticated/rate-limit
    /// branches above all had coverage, but nothing exercised the "expired"
    /// branch (`PortError::CredentialExpired`) — checked last in
    /// `map_vault_js_error`'s precedence order specifically so it never
    /// shadows the unauthenticated branch (see that function's doc comment).
    #[test]
    fn map_vault_js_error_should_map_expired_marker_to_credential_expired() {
        let message = "TOTP code for example.com expired before it could be typed".to_string();

        let err = map_vault_js_error(message.clone());

        match err {
            PortError::CredentialExpired(m) => assert_eq!(m, message),
            other => panic!("expected PortError::CredentialExpired, got {other:?}"),
        }
    }

    #[test]
    fn map_vault_js_error_should_return_other_when_message_has_no_recognizable_marker() {
        let err = map_vault_js_error("boom: something unrelated broke".to_string());

        match err {
            PortError::Other(_) => {}
            other => panic!("expected PortError::Other, got {other:?}"),
        }
    }
}
