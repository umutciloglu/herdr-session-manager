use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::address::Address;
use crate::error::Error;
use crate::ids;

/// How far a send may go to reach its target. Deliverers read this to decide
/// between prompting a live pane, a one-shot headless run, and a background session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SendMode {
    #[default]
    Auto,
    Ask,
    Background,
    Pane,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageStatus {
    Pending,
    Delivered,
    Read,
    Failed,
}

impl MessageStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            MessageStatus::Pending => "pending",
            MessageStatus::Delivered => "delivered",
            MessageStatus::Read => "read",
            MessageStatus::Failed => "failed",
        }
    }
}

impl fmt::Display for MessageStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MessageStatus {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "pending" => MessageStatus::Pending,
            "delivered" => MessageStatus::Delivered,
            "read" => MessageStatus::Read,
            "failed" => MessageStatus::Failed,
            other => return Err(Error::parse(format!("unknown message status {other:?}"))),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub from: Address,
    pub to: Address,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub expects_reply: bool,
    pub status: MessageStatus,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Message {
    /// A fresh Pending message with a generated ulid.
    pub fn new(from: Address, to: Address, text: impl Into<String>) -> Self {
        Message {
            id: ids::new_id(),
            from,
            to,
            text: text.into(),
            reply_to: None,
            expects_reply: false,
            status: MessageStatus::Pending,
            created_at: ids::now(),
            delivered_at: None,
            error: None,
        }
    }

    pub fn expecting_reply(mut self, yes: bool) -> Self {
        self.expects_reply = yes;
        self
    }

    pub fn in_reply_to(mut self, id: Option<String>) -> Self {
        self.reply_to = id;
        self
    }
}
