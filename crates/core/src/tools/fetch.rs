use std::time::Duration;

use crate::ports::{BrowserDriver, FileStore};
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

    let extract = browser
        .navigate_and_extract(&input.url, timeout)
        .await
        .map_err(|e| format!("render {}: {e}", input.url))?;

    let mut out = FetchPageOutput {
        title: extract.title,
        text: extract.text,
        saved_to: None,
        final_url: extract.final_url,
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
