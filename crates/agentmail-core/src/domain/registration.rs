use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::address::Address;
use super::harness::Harness;
use crate::ids;
use crate::traits::DirectoryEntry;

/// A session that announced itself to agentmail: either its MCP process registered,
/// or a SessionStart hook did. `(harness, session_id)` is the primary key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    pub harness: Harness,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// `None` for hook-created rows: we cannot prove liveness, so `live()` keeps them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poke_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub herdr_pane: Option<String>,
    pub started_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

impl Registration {
    pub fn new(harness: Harness, session_id: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        let now = ids::now();
        Registration {
            harness,
            session_id: session_id.into(),
            alias: None,
            pid: None,
            cwd: cwd.into(),
            poke_path: None,
            herdr_pane: None,
            started_at: now,
            last_seen: now,
        }
    }

    pub fn address(&self) -> Address {
        Address::new(self.harness.clone(), self.session_id.clone())
    }

    /// A registration standing in for an agent herdr can see but that never registered
    /// with agentmail. It has no pid and no poke socket, so delivery falls through to
    /// whatever transport the directory adapter provides.
    pub fn from_directory(entry: &DirectoryEntry) -> Option<Registration> {
        let addr = entry.address.clone()?;
        let now = ids::now();
        Some(Registration {
            harness: addr.harness,
            session_id: addr.id,
            alias: entry.alias.clone(),
            pid: None,
            cwd: entry.cwd.clone().unwrap_or_default(),
            poke_path: None,
            herdr_pane: None,
            started_at: now,
            last_seen: now,
        })
    }
}
