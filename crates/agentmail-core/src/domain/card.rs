use serde::{Deserialize, Serialize};

use super::address::Address;

/// One row of `{"sessions":[...]}` as emitted by a session provider (docs/protocol.md).
/// Field names are the wire contract; everything but `address` and `harness` is optional
/// so a thin provider can answer with what it has.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCard {
    pub address: String,
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_user_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub resumable: bool,
}

impl SessionCard {
    pub fn parsed_address(&self) -> Option<Address> {
        self.address.parse().ok()
    }

    /// What an envelope header shows for this session.
    pub fn label(&self) -> Option<&str> {
        self.title
            .as_deref()
            .or(self.first_prompt.as_deref())
            .or(self.project.as_deref())
    }
}

/// The provider stdout envelope.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionList {
    #[serde(default)]
    pub sessions: Vec<SessionCard>,
}
