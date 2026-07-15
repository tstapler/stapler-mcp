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
    async fn get(&self, url: &str, headers: &[(String, String)]) -> Result<HttpResponse, PortError> {
        let mut req = self.client.get(url);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(|e| PortError::Io(e.to_string()))?;
        let status = resp.status().as_u16();
        let body = resp
            .bytes()
            .await
            .map_err(|e| PortError::Io(e.to_string()))?
            .to_vec();
        Ok(HttpResponse { status, body })
    }
}
