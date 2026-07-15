use std::time::Duration;

use chromiumoxide::{Browser, BrowserConfig};
use futures::StreamExt;

use stapler_mcp_core::ports::{BrowserDriver, PageExtract, PortError};

/// One shared browser-process allocator for the daemon's whole lifetime.
/// `Page` methods below take `&self`, so a fresh page (chromedp's "fresh tab
/// per call") is created per `navigate_and_extract` call without needing a
/// mutex — concurrent calls get independent tabs on the same browser process.
pub struct NativeBrowser {
    browser: Browser,
}

impl NativeBrowser {
    pub async fn launch() -> Result<Self, PortError> {
        let config = BrowserConfig::builder()
            .build()
            .map_err(|e| PortError::Other(e.to_string()))?;
        let (browser, mut handler) = Browser::launch(config)
            .await
            .map_err(|e| PortError::Other(e.to_string()))?;

        // Drains the CDP websocket event stream for the daemon's whole
        // lifetime; dropping this JoinHandle does not stop the task.
        tokio::spawn(async move { while handler.next().await.is_some() {} });

        Ok(NativeBrowser { browser })
    }
}

impl NativeBrowser {
    /// Must be called once, explicitly, at daemon shutdown (after every
    /// `Rc<NativeBrowser>` clone held by registered tool handlers has been
    /// dropped, so this is the sole remaining reference) — otherwise the
    /// Chrome subprocess and its CDP connection keep the process alive.
    pub async fn close(&mut self) {
        let _ = self.browser.close().await;
    }
}

impl BrowserDriver for NativeBrowser {
    async fn navigate_and_extract(&self, url: &str, timeout: Duration) -> Result<PageExtract, PortError> {
        let fut = async {
            let page = self
                .browser
                .new_page(url)
                .await
                .map_err(|e| PortError::Other(e.to_string()))?;
            page.wait_for_navigation()
                .await
                .map_err(|e| PortError::Other(e.to_string()))?;

            let title: String = page
                .evaluate("document.title")
                .await
                .map_err(|e| PortError::Other(e.to_string()))?
                .into_value()
                .map_err(|e| PortError::Other(e.to_string()))?;
            let text: String = page
                .evaluate("document.body ? document.body.innerText : ''")
                .await
                .map_err(|e| PortError::Other(e.to_string()))?
                .into_value()
                .map_err(|e| PortError::Other(e.to_string()))?;
            let html = page
                .content()
                .await
                .map_err(|e| PortError::Other(e.to_string()))?;
            let final_url = page
                .url()
                .await
                .map_err(|e| PortError::Other(e.to_string()))?
                .unwrap_or_else(|| url.to_string());

            Ok(PageExtract {
                title,
                html,
                text,
                final_url,
            })
        };

        tokio::time::timeout(timeout, fut)
            .await
            .map_err(|_| PortError::Timeout)?
    }
}
