//! Generates or loads the HTTP transport's bearer token, persisted at
//! `~/.stapler-mcp/http-token` (`stapler_mcp_core::paths::http_token_path`).
//! Runs once at daemon startup, so plain blocking `std::fs` calls are fine
//! here — unlike `lock.rs`'s `spawn_blocking` dance, which exists to avoid
//! blocking the runtime on *every* lock acquisition during normal operation,
//! not a one-shot startup read/write.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use rand::rngs::OsRng;
use rand::TryRngCore;

use stapler_mcp_core::http_auth::BearerToken;

/// Reads the existing token at `path` if present and non-empty; otherwise
/// generates a new 32-byte (64 hex char) token, persists it at `path` with
/// mode `0600`, and returns it. The `bool` is `true` when a fresh token was
/// generated (the caller logs that), `false` when an existing one was
/// reused (nothing new to log).
pub async fn generate_or_load(path: &str) -> Result<(BearerToken, bool), std::io::Error> {
    if let Ok(existing) = fs::read_to_string(path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok((BearerToken::new(trimmed.to_string()), false));
        }
    }

    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent)?;
    }

    let mut bytes = [0u8; 32];
    // `rand_core` 0.9 only gives `OsRng` a fallible `TryRngCore` impl (unlike
    // 0.8's infallible `RngCore`) — a genuine OS RNG failure here is
    // unrecoverable for a security-sensitive token, so surface it as an
    // `io::Error` rather than silently falling back to a weaker source.
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();

    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(token.as_bytes())?;

    Ok((BearerToken::new(token), true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn generate_or_load_should_create_a_new_token_when_none_exists() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("http-token");
        let path_str = path.to_str().expect("utf8 path").to_string();

        let (token, generated) = generate_or_load(&path_str).await.expect("generate token");

        assert!(generated);
        assert_eq!(token.as_str().len(), 64);
        assert!(token.as_str().chars().all(|c| c.is_ascii_hexdigit()));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path)
                .expect("stat token file")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn generate_or_load_should_reuse_an_existing_token_when_file_present() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("http-token");
        let path_str = path.to_str().expect("utf8 path").to_string();
        fs::write(&path, "existing-token-value").expect("seed token file");

        let (token, generated) = generate_or_load(&path_str).await.expect("load token");

        assert!(!generated);
        assert_eq!(token.as_str(), "existing-token-value");
    }
}
