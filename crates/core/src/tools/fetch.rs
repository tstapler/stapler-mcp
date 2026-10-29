use std::time::Duration;

use crate::ports::{BrowserDriver, FileStore, PortError};
use crate::schema::{FetchPageInput, FetchPageOutput};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

pub async fn fetch_page<B: BrowserDriver, F: FileStore>(
    browser: &B,
    fs: &F,
    input: FetchPageInput,
) -> Result<FetchPageOutput, String> {
    if input.url.is_empty() {
        return Err("url must not be empty".to_string());
    }
    let timeout = match input.timeout_seconds {
        Some(s) if s > 0 => Duration::from_secs(u64::from(s)),
        _ => DEFAULT_TIMEOUT,
    };

    let extract = match browser.navigate_and_extract(&input.url, timeout).await {
        Ok(extract) => extract,
        Err(PortError::AuthRequired(info)) => {
            return Ok(FetchPageOutput {
                title: String::new(),
                text: String::new(),
                saved_to: None,
                final_url: info.final_url.clone(),
                auth_required: Some(info),
            });
        }
        Err(e) => return Err(format!("render {}: {e}", input.url)),
    };

    let mut out = FetchPageOutput {
        title: extract.title,
        text: extract.text,
        saved_to: None,
        final_url: extract.final_url,
        auth_required: None,
    };

    // Note: unlike the original Go function (which could return a partially
    // populated output alongside a save error to its direct caller), the IPC
    // `Response` envelope is strictly result-xor-error — a save failure here
    // was never actually observable as "partial success" through the tool
    // interface, only within the Go function's own unit tests. So this just
    // surfaces the error, matching real external behavior rather than the
    // internal Go signature.
    if let Some(save_path) = input.save_path {
        fs.write_file(&save_path, extract.html.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        out.saved_to = Some(save_path);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{FileStore, PortError};
    use crate::schema::AuthRequiredInfo;
    use crate::tools::test_support::FakeBrowserDriver;

    struct NoFs;
    impl FileStore for NoFs {
        async fn write_file(&self, _: &str, _: &[u8]) -> Result<(), PortError> {
            panic!("unused")
        }
        async fn read_file(&self, _: &str) -> Result<Option<Vec<u8>>, PortError> {
            panic!("unused")
        }
        async fn delete_file(&self, _: &str) -> Result<(), PortError> {
            panic!("unused")
        }
    }

    fn input() -> FetchPageInput {
        FetchPageInput {
            url: "https://wiki.example.com/p".into(),
            save_path: None,
            timeout_seconds: None,
        }
    }

    #[tokio::test]
    async fn fetch_page_should_return_auth_required_instead_of_error_when_login_wall() {
        let info = AuthRequiredInfo {
            requested_url: "https://wiki.example.com/p".into(),
            final_url: "https://idp.example.com/login".into(),
            auth_host: "idp.example.com".into(),
            hint: None,
        };
        let browser =
            FakeBrowserDriver::new().with_extract(Err(PortError::AuthRequired(info.clone())));
        let out = fetch_page(&browser, &NoFs, input()).await.unwrap();
        assert_eq!(out.auth_required, Some(info));
        assert!(out.text.is_empty());
        assert_eq!(out.final_url, "https://idp.example.com/login");
    }

    #[tokio::test]
    async fn fetch_page_should_keep_timeout_as_a_plain_error() {
        let browser = FakeBrowserDriver::new().with_extract(Err(PortError::Timeout));
        let err = fetch_page(&browser, &NoFs, input()).await.unwrap_err();
        assert!(err.contains("timed out"), "{err}");
    }
}
