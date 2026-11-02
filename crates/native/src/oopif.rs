//! Child CDP sessions for cross-origin (out-of-process) iframes.
//!
//! chromiumoxide 0.9's `Page` is hard-wired to one session and offers no
//! public way to wrap another, so `Browser::get_page` can't serve an OOPIF
//! target (Task 0.1 finding, `project_plans/oopif-ax-snapshot/research/stack.md`
//! §Feasibility). Instead `OopifHub` opens its own browser-level
//! `chromiumoxide::Connection`, attaches (flattened) to the iframe target
//! over it, and speaks raw typed commands on the resulting session. It is
//! opened lazily — only when a capture actually meets a cross-origin
//! iframe — and sessions are always torn down by `detach_all`.

use std::time::Duration;

use chromiumoxide::cdp::browser_protocol::target::{
    AttachToTargetParams, DetachFromTargetParams, SessionId, TargetId,
};
use chromiumoxide::types::{CdpJsonEventMessage, Command, Message};
use chromiumoxide::Connection;
use futures::StreamExt;
use tokio::sync::Mutex;

use stapler_mcp_core::ports::PortError;

const CALL_TIMEOUT: Duration = Duration::from_secs(5);

struct HubConn {
    conn: Connection<CdpJsonEventMessage>,
    sessions: Vec<SessionId>,
}

pub(crate) struct OopifHub {
    ws_url: Option<String>,
    state: Mutex<Option<HubConn>>,
}

impl OopifHub {
    /// `None` disables OOPIF capture: every `attach` fails and the caller
    /// degrades to a childless `Iframe` leaf.
    pub(crate) fn new(ws_url: Option<&str>) -> Self {
        Self {
            ws_url: ws_url.map(str::to_owned),
            state: Mutex::new(None),
        }
    }

    pub(crate) async fn attach(&self, target_id: &str) -> Result<SessionId, PortError> {
        let mut guard = self.state.lock().await;
        let hub = self.ensure_connected(&mut guard).await?;
        let params = AttachToTargetParams {
            target_id: TargetId::new(target_id),
            flatten: Some(true),
        };
        let resp = roundtrip(hub, None, &params).await?;
        let session_id = resp
            .get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| PortError::Other("attachToTarget returned no sessionId".into()))?;
        let session_id = SessionId::new(session_id);
        hub.sessions.push(session_id.clone());
        Ok(session_id)
    }

    pub(crate) async fn call<C: Command>(
        &self,
        session: &SessionId,
        cmd: C,
    ) -> Result<C::Response, PortError> {
        let mut guard = self.state.lock().await;
        let hub = guard
            .as_mut()
            .ok_or_else(|| PortError::Other("no child CDP connection".into()))?;
        let value = roundtrip(hub, Some(session.clone()), &cmd).await?;
        C::response_from_value(value).map_err(|e| PortError::Other(e.to_string()))
    }

    /// Detaches every session attached so far and closes the connection
    /// (closing it makes Chrome drop anything a failed detach left behind).
    pub(crate) async fn detach_all(&self) {
        let Some(mut hub) = self.state.lock().await.take() else {
            return;
        };
        for session_id in std::mem::take(&mut hub.sessions) {
            let params = DetachFromTargetParams {
                session_id: Some(session_id),
            };
            let _ = roundtrip(&mut hub, None, &params).await;
        }
    }

    async fn ensure_connected<'a>(
        &self,
        slot: &'a mut Option<HubConn>,
    ) -> Result<&'a mut HubConn, PortError> {
        if slot.is_none() {
            let url = self
                .ws_url
                .as_deref()
                .ok_or_else(|| PortError::Other("OOPIF capture unavailable".into()))?;
            let conn = tokio::time::timeout(CALL_TIMEOUT, Connection::connect(url))
                .await
                .map_err(|_| PortError::Other("child CDP connect timed out".into()))?
                .map_err(|e| PortError::Other(e.to_string()))?;
            *slot = Some(HubConn {
                conn,
                sessions: Vec::new(),
            });
        }
        slot.as_mut()
            .ok_or_else(|| PortError::Other("child CDP connection unavailable".into()))
    }
}

async fn roundtrip<C: Command>(
    hub: &mut HubConn,
    session: Option<SessionId>,
    cmd: &C,
) -> Result<serde_json::Value, PortError> {
    let params = serde_json::to_value(cmd).map_err(|e| PortError::Other(e.to_string()))?;
    let id = hub
        .conn
        .submit_command(cmd.identifier(), session, params)
        .map_err(|e| PortError::Other(e.to_string()))?;
    let wait = async {
        while let Some(msg) = hub.conn.next().await {
            if let Message::Response(resp) = msg.map_err(|e| PortError::Other(e.to_string()))? {
                if resp.id != id {
                    continue;
                }
                if let Some(err) = resp.error {
                    return Err(PortError::Other(err.to_string()));
                }
                return Ok(resp.result.unwrap_or(serde_json::Value::Null));
            }
        }
        Err(PortError::Other("child CDP connection closed".into()))
    };
    tokio::time::timeout(CALL_TIMEOUT, wait)
        .await
        .map_err(|_| PortError::Other(format!("{} timed out", cmd.identifier())))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn attach_should_fail_when_hub_has_no_websocket_url() {
        let hub = OopifHub::new(None);
        assert!(hub.attach("some-target").await.is_err());
    }

    #[tokio::test]
    async fn attach_should_fail_when_websocket_is_unreachable() {
        let hub = OopifHub::new(Some("ws://127.0.0.1:1/devtools/browser/x"));
        assert!(hub.attach("some-target").await.is_err());
    }

    #[tokio::test]
    async fn detach_all_should_be_a_noop_when_never_connected() {
        OopifHub::new(None).detach_all().await;
    }
}
