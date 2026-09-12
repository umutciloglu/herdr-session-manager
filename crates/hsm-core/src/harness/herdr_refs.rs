//! herdr's persisted pane -> session refs, `~/.config/herdr/session.json`.
//!
//! Observed shape (version 3, 2026-09-12):
//! ```json
//! {"version":3,"workspaces":[{"id":"w1","identity_cwd":"/abs",
//!   "tabs":[{"custom_name":null,"panes":{"1":{"cwd":"/abs","label":"Files",
//!     "agent_session":{"source":"herdr:claude","agent":"claude","kind":"id",
//!                      "value":"<session id>"}}}}]}]}
//! ```
//! Tabs carry no id of their own, so the tab index within the workspace is used.
//! This is tier 1: it works for every harness, including ones whose transcripts
//! we cannot read.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::domain::{HarnessKind, PaneRef, RefKind, SessionRef};
use crate::error::{Error, Result};
use crate::live::LivePane;

/// One pane that herdr remembers as holding an agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HerdrRef {
    pub pane: PaneRef,
    pub session: SessionRef,
    pub cwd: PathBuf,
    pub label: Option<String>,
}

pub fn from_session_file(path: &Path) -> Result<Vec<HerdrRef>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        // No herdr state yet is normal, not an error.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(path, e)),
    };
    let root: Value = serde_json::from_str(&text).map_err(|e| Error::json(path, e))?;
    Ok(parse(&root))
}

fn parse(root: &Value) -> Vec<HerdrRef> {
    let mut out = Vec::new();
    let Some(workspaces) = root.get("workspaces").and_then(Value::as_array) else {
        return out;
    };
    for ws in workspaces {
        let ws_id = ws.get("id").and_then(Value::as_str).map(str::to_string);
        let Some(tabs) = ws.get("tabs").and_then(Value::as_array) else {
            continue;
        };
        for (tab_idx, tab) in tabs.iter().enumerate() {
            let tab_id = tab
                .get("custom_name")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| tab_idx.to_string());
            let Some(panes) = tab.get("panes").and_then(Value::as_object) else {
                continue;
            };
            for (pane_id, pane) in panes {
                let Some(agent) = pane.get("agent_session") else {
                    continue;
                };
                let Some(session) = session_ref(agent) else {
                    continue;
                };
                out.push(HerdrRef {
                    pane: PaneRef {
                        pane_id: pane_id.clone(),
                        workspace_id: ws_id.clone(),
                        tab_id: Some(tab_id.clone()),
                        live: false,
                        status: None,
                    },
                    session,
                    cwd: pane
                        .get("cwd")
                        .and_then(Value::as_str)
                        .map(PathBuf::from)
                        .unwrap_or_default(),
                    label: pane
                        .get("label")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                });
            }
        }
    }
    out
}

fn session_ref(agent: &Value) -> Option<SessionRef> {
    let value = agent.get("value").and_then(Value::as_str)?;
    if value.is_empty() {
        return None;
    }
    // `agent` is the herdr kind; `source` looks like "herdr:claude".
    let name = agent.get("agent").and_then(Value::as_str).or_else(|| {
        agent
            .get("source")
            .and_then(Value::as_str)
            .and_then(|s| s.split(':').next_back())
    })?;
    Some(SessionRef {
        harness: HarnessKind::from_name(name),
        kind: agent
            .get("kind")
            .and_then(Value::as_str)
            .map(RefKind::from_name)
            .unwrap_or(RefKind::Id),
        value: value.to_string(),
    })
}

impl From<&LivePane> for HerdrRef {
    fn from(p: &LivePane) -> Self {
        HerdrRef {
            pane: PaneRef {
                pane_id: p.pane_id.clone(),
                workspace_id: p.workspace_id.clone(),
                tab_id: p.tab_id.clone(),
                live: true,
                status: Some(p.status.clone()),
            },
            session: p.session.clone(),
            cwd: p.cwd.clone(),
            label: p.title.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION_JSON: &str = r#"{
      "version": 3,
      "workspaces": [
        {
          "id": "w1",
          "identity_cwd": "/Users/x/Projects/demo",
          "tabs": [
            {
              "custom_name": null,
              "panes": {
                "1": {
                  "cwd": "/Users/x/Projects/demo",
                  "agent_session": {"source":"herdr:claude","agent":"claude","kind":"id","value":"7713f9f6-e951-4b3b-b920-1810655be372"}
                },
                "2": {"cwd": "/Users/x/Projects/demo"}
              }
            },
            {
              "custom_name": "ecc",
              "panes": {
                "4": {
                  "cwd": "/Users/x/Projects/demo",
                  "label": "Files",
                  "agent_session": {"source":"herdr:codex","agent":"codex","kind":"id","value":"019ffa5d-8515-71f0-bb83-0c02d2b9ceb6"}
                }
              }
            }
          ]
        }
      ]
    }"#;

    #[test]
    fn reads_agent_panes_only() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("session.json");
        std::fs::write(&p, SESSION_JSON).expect("write");

        let mut refs = from_session_file(&p).expect("parse");
        refs.sort_by(|a, b| a.pane.pane_id.cmp(&b.pane.pane_id));
        assert_eq!(refs.len(), 2, "the pane without agent_session is skipped");

        assert_eq!(refs[0].session.harness, HarnessKind::Claude);
        assert_eq!(
            refs[0].session.value,
            "7713f9f6-e951-4b3b-b920-1810655be372"
        );
        assert_eq!(refs[0].pane.workspace_id.as_deref(), Some("w1"));
        assert_eq!(refs[0].pane.tab_id.as_deref(), Some("0"));
        assert!(!refs[0].pane.live);

        assert_eq!(refs[1].session.harness, HarnessKind::Codex);
        assert_eq!(refs[1].pane.tab_id.as_deref(), Some("ecc"));
        assert_eq!(refs[1].label.as_deref(), Some("Files"));
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tmp");
        assert!(from_session_file(&dir.path().join("nope.json"))
            .expect("ok")
            .is_empty());
    }
}
