use stapler_mcp_core::ports::{HttpClient, HttpResponse, PortError};

pub struct NativeHttp {
    client: reqwest::Client,
}

impl NativeHttp {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for NativeHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClient for NativeHttp {
    async fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<HttpResponse, PortError> {
        let mut req = self.client.get(url);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(|e| PortError::Io(e.to_string()))?;
        let status = resp.status().as_u16();
        let final_url = resp.url().to_string();
        let body = resp
            .bytes()
            .await
            .map_err(|e| PortError::Io(e.to_string()))?
            .to_vec();
        Ok(HttpResponse {
            status,
            body,
            final_url,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Minimal hand-rolled HTTP mock: a `TcpListener` that 301-redirects
    /// `/old-page` to `/new-page` (served on the same listener) and serves a
    /// small body for `/new-page`. Enough to exercise `reqwest`'s real
    /// redirect-following, unlike a pure-logic unit test.
    async fn spawn_redirecting_mock() -> (String, tokio::sync::oneshot::Sender<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock server");
        let addr = listener.local_addr().expect("mock server addr");
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => return,
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else { return };
                        tokio::spawn(async move {
                            let mut buf = [0u8; 4096];
                            let n = stream.read(&mut buf).await.unwrap_or(0);
                            let request = String::from_utf8_lossy(&buf[..n]);
                            let path = request
                                .lines()
                                .next()
                                .and_then(|line| line.split_whitespace().nth(1))
                                .unwrap_or("/")
                                .to_string();

                            let resp = if path == "/old-page" {
                                "HTTP/1.1 301 Moved Permanently\r\nLocation: /new-page\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                            } else if path == "/new-page" {
                                let body = "new page body";
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                    body.len(),
                                    body
                                )
                            } else {
                                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                            };
                            let _ = stream.write_all(resp.as_bytes()).await;
                            let _ = stream.shutdown().await;
                        });
                    }
                }
            }
        });

        (format!("http://{addr}"), shutdown_tx)
    }

    #[tokio::test]
    async fn should_populate_final_url_from_response_url_when_native_http_get_follows_redirect() {
        let (base_url, shutdown) = spawn_redirecting_mock().await;
        let http = NativeHttp::new();

        let response = http
            .get(&format!("{base_url}/old-page"), &[])
            .await
            .expect("GET through redirect should succeed");

        assert_eq!(response.status, 200);
        assert_eq!(response.final_url, format!("{base_url}/new-page"));
        assert_eq!(response.body, b"new page body");

        let _ = shutdown.send(());
    }
}
