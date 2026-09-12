//! Request/response client for the herdr socket API.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, HerdrError, Result};
use crate::events::{EventStream, Subscription};
use crate::transport::{self, Transport};
use crate::types::*;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Slack added on top of a server-side `timeout_ms` so the socket timeout never
/// fires before herdr's own, which would cost us the real error.
const SERVER_WAIT_MARGIN: Duration = Duration::from_secs(5);

/// Handle on a herdr socket.
///
/// herdr 0.9 answers exactly one request per connection and then closes it, so
/// every call dials a fresh socket. That also makes concurrent calls on a shared
/// `&HerdrClient` safe: they never share a stream. Subscriptions are the one
/// long-lived case, and they get their own connection via [`HerdrClient::subscribe`].
#[derive(Debug, Clone)]
pub struct HerdrClient {
    path: PathBuf,
    timeout: Duration,
    next_id: Arc<AtomicU64>,
}

impl HerdrClient {
    /// Connect to the socket named by the environment. See [`transport::socket_path`].
    pub async fn connect() -> Result<Self> {
        Self::connect_path(transport::socket_path()?).await
    }

    /// Dials the socket once so an unreachable server fails here, not on the first call.
    pub async fn connect_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        drop(Transport::connect(&path).await?);
        Ok(HerdrClient {
            path,
            timeout: DEFAULT_TIMEOUT,
            next_id: Arc::new(AtomicU64::new(1)),
        })
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn socket_path(&self) -> &Path {
        &self.path
    }

    /// Raw call. Returns herdr's `result` object, tag field included.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_timeout(method, params, self.timeout).await
    }

    pub async fn request_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let id = format!("hc{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let request = json!({ "id": id, "method": method, "params": params });

        tracing::trace!(%id, method, "herdr request");
        let exchange = async {
            let mut conn = Transport::connect(&self.path).await?;
            conn.write_line(&request).await?;
            loop {
                let Some(line) = conn.read_line().await? else {
                    return Err(Error::Closed);
                };
                let envelope: Envelope = serde_json::from_value(line)?;
                // Ignore anything that is not the id we just sent rather than
                // mistaking another line for our answer.
                if envelope.id.as_deref() != Some(id.as_str()) {
                    tracing::debug!(expected = %id, got = ?envelope.id, "dropping unmatched herdr line");
                    continue;
                }
                if let Some(error) = envelope.error {
                    return Err(Error::Api(error));
                }
                return envelope.result.ok_or_else(|| Error::UnexpectedResult {
                    method: method.to_string(),
                    field: "result".to_string(),
                });
            }
        };

        match tokio::time::timeout(timeout, exchange).await {
            Ok(result) => result,
            Err(_) => Err(Error::Timeout {
                method: method.to_string(),
                timeout,
            }),
        }
    }

    async fn call<P: Serialize, T: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
        field: &str,
        timeout: Duration,
    ) -> Result<T> {
        let result = self
            .request_timeout(method, serde_json::to_value(params)?, timeout)
            .await?;
        take(method, result, field)
    }

    /// Liveness check; returns herdr's version string.
    pub async fn ping(&self) -> Result<String> {
        let result = self.request("ping", json!({})).await?;
        Ok(result
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    pub async fn session_snapshot(&self) -> Result<Snapshot> {
        self.call("session.snapshot", &json!({}), "snapshot", self.timeout)
            .await
    }

    pub async fn agent_list(&self) -> Result<Vec<AgentInfo>> {
        self.call("agent.list", &json!({}), "agents", self.timeout)
            .await
    }

    /// `target` is a pane id, an agent name, or another herdr agent target string.
    pub async fn agent_get(&self, target: &str) -> Result<AgentInfo> {
        self.call(
            "agent.get",
            &json!({ "target": target }),
            "agent",
            self.timeout,
        )
        .await
    }

    pub async fn agent_start(&self, params: &AgentStart) -> Result<AgentStarted> {
        let timeout = self.server_timeout(params.timeout_ms);
        let result = self
            .request_timeout("agent.start", serde_json::to_value(params)?, timeout)
            .await?;
        Ok(serde_json::from_value(result)?)
    }

    pub async fn agent_prompt(&self, params: &AgentPrompt) -> Result<AgentInfo> {
        let timeout = self.server_timeout(params.wait.as_ref().and_then(|w| w.timeout_ms));
        self.call("agent.prompt", params, "agent", timeout).await
    }

    pub async fn agent_wait(&self, params: &AgentWait) -> Result<AgentInfo> {
        let timeout = self.server_timeout(params.timeout_ms);
        self.call("agent.wait", params, "agent", timeout).await
    }

    /// Passing `None` clears the name.
    pub async fn agent_rename(&self, target: &str, name: Option<&str>) -> Result<AgentInfo> {
        self.call(
            "agent.rename",
            &json!({ "target": target, "name": name }),
            "agent",
            self.timeout,
        )
        .await
    }

    pub async fn pane_split(&self, params: &PaneSplit) -> Result<PaneInfo> {
        self.call("pane.split", params, "pane", self.timeout).await
    }

    pub async fn tab_create(&self, params: &TabCreate) -> Result<TabCreated> {
        let result = self
            .request("tab.create", serde_json::to_value(params)?)
            .await?;
        Ok(serde_json::from_value(result)?)
    }

    /// Types text into the pane without submitting it.
    pub async fn pane_send_text(&self, pane_id: &str, text: &str) -> Result<()> {
        self.request(
            "pane.send_text",
            json!({ "pane_id": pane_id, "text": text }),
        )
        .await?;
        Ok(())
    }

    /// Text plus key combos in one shot, e.g. `keys = ["enter"]` to submit.
    pub async fn pane_send_input(
        &self,
        pane_id: &str,
        text: Option<&str>,
        keys: &[&str],
    ) -> Result<()> {
        let mut params = json!({ "pane_id": pane_id });
        if let Some(text) = text {
            params["text"] = json!(text);
        }
        if !keys.is_empty() {
            params["keys"] = json!(keys);
        }
        self.request("pane.send_input", params).await?;
        Ok(())
    }

    /// Focuses the pane, bringing its tab and workspace forward with it.
    pub async fn pane_focus(&self, pane_id: &str) -> Result<()> {
        self.request("pane.focus", json!({ "pane_id": pane_id }))
            .await?;
        Ok(())
    }

    /// Without `caller_pane_id` this returns whatever herdr currently focuses.
    pub async fn pane_current(&self, caller_pane_id: Option<&str>) -> Result<PaneInfo> {
        self.call(
            "pane.current",
            &json!({ "caller_pane_id": caller_pane_id }),
            "pane",
            self.timeout,
        )
        .await
    }

    pub async fn pane_read(&self, params: &PaneRead) -> Result<PaneReadResult> {
        self.call("pane.read", params, "read", self.timeout).await
    }

    /// `None` means the pane is a popup: popups have no pane id and stay outside `pane.*`.
    pub async fn plugin_pane_open(
        &self,
        params: &PluginPaneOpen,
    ) -> Result<Option<PluginPaneInfo>> {
        let result = self
            .request("plugin.pane.open", serde_json::to_value(params)?)
            .await?;
        match result.get("plugin_pane") {
            Some(value) => Ok(Some(serde_json::from_value(value.clone())?)),
            None => Ok(None),
        }
    }

    /// Closes the active popup. Errors with `popup_not_open` when there is none.
    pub async fn popup_close(&self) -> Result<()> {
        self.request("popup.close", json!({})).await?;
        Ok(())
    }

    /// Opens a second connection for the event stream: a subscription keeps its
    /// connection busy, and this one still has to answer requests.
    pub async fn subscribe(&self, subscriptions: &[Subscription]) -> Result<EventStream> {
        EventStream::connect(&self.path, subscriptions).await
    }

    fn server_timeout(&self, server_ms: Option<u64>) -> Duration {
        match server_ms {
            Some(ms) => Duration::from_millis(ms) + SERVER_WAIT_MARGIN,
            None => self.timeout,
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct Envelope {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<HerdrError>,
}

fn take<T: DeserializeOwned>(method: &str, mut result: Value, field: &str) -> Result<T> {
    let value = result
        .get_mut(field)
        .map(Value::take)
        .ok_or_else(|| Error::UnexpectedResult {
            method: method.to_string(),
            field: field.to_string(),
        })?;
    Ok(serde_json::from_value(value)?)
}
