//! Bearer-token type for the HTTP transport's auth middleware
//! (`crates/cli/src/http_server.rs`). Lives in `core` rather than `native`
//! or `cli` because it must stay usable from wasm targets too (no OS-level
//! token generation/storage here — that's `crates/native/src/http_token.rs`).

use subtle::ConstantTimeEq;

/// Wraps the raw bearer token string. Equality is constant-time to avoid
/// leaking the real token's contents through response-timing side channels,
/// and `Debug` is redacted so the token never lands in a log line by
/// accident (e.g. via `{:?}` in a `dbg!` or panic message).
#[derive(Clone)]
pub struct BearerToken(String);

impl BearerToken {
    pub fn new(token: String) -> Self {
        Self(token)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl PartialEq for BearerToken {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_bytes().ct_eq(other.0.as_bytes()).into()
    }
}

impl Eq for BearerToken {}

impl std::fmt::Debug for BearerToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BearerToken(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_consider_equal_tokens_with_identical_contents_equal() {
        let a = BearerToken::new("abc123".to_string());
        let b = BearerToken::new("abc123".to_string());

        assert_eq!(a, b);
    }

    #[test]
    fn should_consider_tokens_with_different_contents_unequal() {
        let a = BearerToken::new("abc123".to_string());
        let b = BearerToken::new("xyz789".to_string());

        assert_ne!(a, b);
    }

    #[test]
    fn should_consider_tokens_of_different_lengths_unequal() {
        let a = BearerToken::new("short".to_string());
        let b = BearerToken::new("a-much-longer-token".to_string());

        assert_ne!(a, b);
    }

    #[test]
    fn debug_should_never_print_the_raw_token_value() {
        let token = BearerToken::new("super-secret-value".to_string());

        let formatted = format!("{token:?}");

        assert_eq!(formatted, "BearerToken(***)");
        assert!(!formatted.contains("super-secret-value"));
    }

    #[test]
    fn as_str_should_return_the_underlying_token() {
        let token = BearerToken::new("abc123".to_string());

        assert_eq!(token.as_str(), "abc123");
    }
}
