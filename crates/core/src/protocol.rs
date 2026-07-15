//! Wire shape, preserved from the original Go implementation: `{tool, params}`
//! request / `{result|error}` response, newline-delimited JSON, one request
//! then one response per connection. Framing (the newline) is the transport
//! adapter's job (`Conn::read_frame`/`write_frame`); this module only handles
//! the JSON payload itself.

use serde::{Deserialize, Serialize};

pub const PING_TOOL: &str = "ping";
pub const SHUTDOWN_TOOL: &str = "shutdown";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Request {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Response {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(result: serde_json::Value) -> Self {
        Response {
            result: Some(result),
            error: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Response {
            result: None,
            error: Some(msg.into()),
        }
    }
}
