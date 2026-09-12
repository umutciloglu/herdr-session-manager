//! Long-lived `events.subscribe` stream.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::client::Envelope;
use crate::error::{Error, Result};
use crate::transport::Transport;
use crate::types::{AgentStatus, ReadSource};

/// Subscription kinds herdr accepts in `events.subscribe`.
///
/// Note the separator: a *subscription* is `pane.updated`, while the *event*
/// herdr pushes back names the same thing `pane_updated` (schema `EventKind`).
/// [`Event::is`] compares the two forms so callers can use one spelling.
pub mod kind {
    pub const WORKSPACE_CREATED: &str = "workspace.created";
    pub const WORKSPACE_UPDATED: &str = "workspace.updated";
    pub const WORKSPACE_CLOSED: &str = "workspace.closed";
    pub const WORKSPACE_FOCUSED: &str = "workspace.focused";
    pub const TAB_CREATED: &str = "tab.created";
    pub const TAB_CLOSED: &str = "tab.closed";
    pub const TAB_FOCUSED: &str = "tab.focused";
    pub const PANE_CREATED: &str = "pane.created";
    pub const PANE_UPDATED: &str = "pane.updated";
    pub const PANE_CLOSED: &str = "pane.closed";
    pub const PANE_FOCUSED: &str = "pane.focused";
    pub const PANE_EXITED: &str = "pane.exited";
    pub const PANE_AGENT_DETECTED: &str = "pane.agent_detected";
    pub const PANE_AGENT_STATUS_CHANGED: &str = "pane.agent_status_changed";
    pub const PANE_OUTPUT_MATCHED: &str = "pane.output_matched";
    pub const PANE_SCROLL_CHANGED: &str = "pane.scroll_changed";
    pub const LAYOUT_UPDATED: &str = "layout.updated";
}

/// One entry of `events.subscribe { subscriptions }`.
///
/// Kept as a struct with a free-form `type` rather than an enum of the 27 variants,
/// so a herdr that grows a new event kind needs no change here.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Subscription {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_status: Option<AgentStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ReadSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip_ansi: Option<bool>,
    /// `pane.output_matched` matcher, passed through untyped.
    #[serde(rename = "match", default, skip_serializing_if = "Option::is_none")]
    pub match_: Option<Value>,
}

impl Subscription {
    pub fn new(kind: impl Into<String>) -> Self {
        Subscription {
            kind: kind.into(),
            ..Subscription::default()
        }
    }

    pub fn pane(mut self, pane_id: impl Into<String>) -> Self {
        self.pane_id = Some(pane_id.into());
        self
    }

    pub fn agent_status(mut self, status: AgentStatus) -> Self {
        self.agent_status = Some(status);
        self
    }

    pub fn source(mut self, source: ReadSource) -> Self {
        self.source = Some(source);
        self
    }

    pub fn lines(mut self, lines: u32) -> Self {
        self.lines = Some(lines);
        self
    }

    pub fn strip_ansi(mut self, strip: bool) -> Self {
        self.strip_ansi = Some(strip);
        self
    }

    pub fn match_output(mut self, matcher: Value) -> Self {
        self.match_ = Some(matcher);
        self
    }
}

/// A pushed event line: `{"event": "<kind>", "data": {...}}`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Event {
    /// herdr's event name, e.g. `pane_updated`. Kept as a string so a newer
    /// herdr can add kinds without breaking the stream.
    pub kind: String,
    pub payload: Value,
}

impl Event {
    /// True for either spelling of the name, `pane.updated` or `pane_updated`.
    pub fn is(&self, kind: &str) -> bool {
        self.kind.len() == kind.len()
            && self
                .kind
                .bytes()
                .zip(kind.bytes())
                .all(|(a, b)| a == b || (matches!(a, b'.' | b'_') && matches!(b, b'.' | b'_')))
    }

    pub fn pane_id(&self) -> Option<&str> {
        self.payload.get("pane_id").and_then(Value::as_str)
    }

    pub fn workspace_id(&self) -> Option<&str> {
        self.payload.get("workspace_id").and_then(Value::as_str)
    }

    pub fn tab_id(&self) -> Option<&str> {
        self.payload.get("tab_id").and_then(Value::as_str)
    }
}

/// A connection that has been handed over to a subscription. Only events come back.
#[derive(Debug)]
pub struct EventStream {
    conn: Transport,
}

impl EventStream {
    /// Subscribes and waits for herdr's `subscription_started` acknowledgement,
    /// so the caller knows from which point events are guaranteed.
    pub async fn connect(path: impl AsRef<Path>, subscriptions: &[Subscription]) -> Result<Self> {
        let mut conn = Transport::connect(path).await?;
        let id = "sub1";
        conn.write_line(&json!({
            "id": id,
            "method": "events.subscribe",
            "params": { "subscriptions": subscriptions },
        }))
        .await?;

        loop {
            let Some(line) = conn.read_line().await? else {
                return Err(Error::Closed);
            };
            let envelope: Envelope = serde_json::from_value(line)?;
            if envelope.id.as_deref() != Some(id) {
                continue;
            }
            if let Some(error) = envelope.error {
                return Err(Error::Api(error));
            }
            tracing::debug!(count = subscriptions.len(), "herdr subscription started");
            return Ok(EventStream { conn });
        }
    }

    /// Next pushed event, or `None` when herdr closes the stream.
    pub async fn next_event(&mut self) -> Result<Option<Event>> {
        loop {
            let Some(line) = self.conn.read_line().await? else {
                return Ok(None);
            };
            match parse_event(line)? {
                Some(event) => return Ok(Some(event)),
                None => continue,
            }
        }
    }

    /// Same as [`EventStream::next_event`], but gives up after `timeout`.
    pub async fn next_event_timeout(&mut self, timeout: Duration) -> Result<Option<Event>> {
        match tokio::time::timeout(timeout, self.next_event()).await {
            Ok(result) => result,
            Err(_) => Err(Error::Timeout {
                method: "events.subscribe".to_string(),
                timeout,
            }),
        }
    }
}

/// `Ok(None)` for lines that are not events (a late response echo), so the
/// stream keeps going instead of dying on protocol noise.
fn parse_event(mut line: Value) -> Result<Option<Event>> {
    if let Some(error) = line.get_mut("error").map(Value::take) {
        return Err(Error::Api(serde_json::from_value(error)?));
    }
    let Some(kind) = line
        .get("event")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        tracing::debug!(?line, "ignoring non-event line on subscription");
        return Ok(None);
    };
    let payload = line
        .get_mut("data")
        .map(Value::take)
        .unwrap_or(Value::Object(Default::default()));
    Ok(Some(Event { kind, payload }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_event_envelope() {
        let line = json!({ "event": "pane_updated", "data": { "pane_id": "w1:p1" } });
        let event = parse_event(line).expect("parse").expect("event");
        assert_eq!(event.kind, "pane_updated");
        assert_eq!(event.pane_id(), Some("w1:p1"));
        assert!(event.is(kind::PANE_UPDATED));
        assert!(event.is("pane_updated"));
        assert!(!event.is("pane_closed"));
    }

    #[test]
    fn skips_response_echo() {
        let line = json!({ "id": "sub1", "result": { "type": "subscription_started" } });
        assert!(parse_event(line).expect("parse").is_none());
    }

    #[test]
    fn subscription_serialises_only_what_it_has() {
        let sub = Subscription::new(kind::PANE_AGENT_STATUS_CHANGED)
            .pane("w1:p1")
            .agent_status(AgentStatus::Blocked);
        let json = serde_json::to_value(&sub).expect("serialise");
        assert_eq!(
            json,
            json!({ "type": "pane.agent_status_changed", "pane_id": "w1:p1", "agent_status": "blocked" })
        );
    }
}
