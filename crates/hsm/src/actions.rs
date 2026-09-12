//! `hsm_tui::Actions` over the index, the open service and the herdr socket.
//!
//! This is the only place where the screen's vocabulary meets herdr's: the TUI
//! asks for "open in a split", this blocks on the async client and answers.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;

use herdr_client::HerdrClient;
use hsm_core::{
    HarnessKind, Index, OpenContext, OpenReport, OpenService, OpenTarget, Query, Session,
};
use hsm_tui::actions::{Actions, Error, ReplyRow, Result, Sent};
use tokio::runtime::Handle;

use crate::herdr_adapter::HerdrPaneOps;
use crate::{agentmail, seen};

const NO_HERDR: &str = "herdr is not reachable; this only works inside a herdr session";

pub struct TuiActions {
    index: Index,
    /// `None` when the socket was unreachable at startup: browsing still works,
    /// only the pane actions are refused.
    open: Option<OpenService<HerdrPaneOps>>,
    client: Option<HerdrClient>,
    handle: Handle,
    invoking_pane: Option<String>,
    /// Project of the pane the popup was opened from; boosts its sessions.
    project: Option<String>,
    agentmail: Option<PathBuf>,
    /// Who sends: always the human, never the agent in the invoking pane.
    from: Option<String>,
    /// Our own mailbox as it looked when the last question went out, so a poll
    /// can tell an answer from what was already sitting there.
    inbox_baseline: RefCell<String>,
}

impl TuiActions {
    pub fn new(index: Index, client: Option<HerdrClient>, handle: Handle) -> TuiActions {
        let open = client
            .clone()
            .map(|c| OpenService::new(HerdrPaneOps::new(c)));
        TuiActions {
            index,
            open,
            client,
            handle,
            invoking_pane: None,
            project: None,
            agentmail: None,
            from: None,
            inbox_baseline: RefCell::new(String::new()),
        }
    }

    pub fn invoking_pane(mut self, pane_id: Option<String>) -> TuiActions {
        self.invoking_pane = pane_id;
        self
    }

    pub fn project(mut self, project: Option<String>) -> TuiActions {
        self.project = project;
        self
    }

    /// `from` is the address messages are sent as; without it agentmail falls
    /// back to its own idea of the sender.
    pub fn agentmail(mut self, bin: Option<PathBuf>, from: Option<String>) -> TuiActions {
        self.agentmail = bin;
        self.from = from;
        self
    }
}

impl Actions for TuiActions {
    fn search(
        &self,
        query: &str,
        harness: Option<&HarnessKind>,
        limit: usize,
    ) -> Result<Vec<Session>> {
        let q = Query {
            text: query.to_string(),
            harness: harness.cloned(),
            project: self.project.clone(),
            limit,
        };
        self.index.search(&q).map_err(Error::action)
    }

    fn recent(&self, limit: usize) -> Result<Vec<Session>> {
        self.index.recent(limit).map_err(Error::action)
    }

    fn get(&self, harness: &HarnessKind, id: &str) -> Result<Option<Session>> {
        self.index.get(Some(harness), id).map_err(Error::action)
    }

    fn pin(&self, harness: &HarnessKind, id: &str, pinned: bool) -> Result<()> {
        let changed = if pinned {
            self.index.pin(harness, id)
        } else {
            self.index.unpin(harness, id)
        }
        .map_err(Error::action)?;
        if !changed {
            return Err(Error::Action("session is no longer in the index".into()));
        }
        Ok(())
    }

    fn open(&self, session: &Session, target: OpenTarget) -> Result<OpenReport> {
        let service = self
            .open
            .as_ref()
            .ok_or_else(|| Error::Action(NO_HERDR.into()))?;
        let ctx = OpenContext {
            context_pane_id: self.invoking_pane.clone(),
            cwd_override: None,
        };
        self.handle
            .block_on(service.open(session, target, &ctx))
            .map_err(Error::action)
    }

    fn insert_address(&self, session: &Session) -> Result<()> {
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| Error::Action(NO_HERDR.into()))?;
        let pane = self
            .invoking_pane
            .as_deref()
            .ok_or_else(|| Error::Action("no invoking pane to insert into".into()))?;
        // Trailing space so the agent's prompt keeps the address one word.
        let text = format!("{} ", session.address().short());
        self.handle
            .block_on(client.pane_send_text(pane, &text))
            .map_err(Error::action)
    }

    fn replies(&self) -> Result<Vec<ReplyRow>> {
        let (Some(bin), Some(me)) = (self.agentmail.as_ref(), self.from.as_deref()) else {
            return Ok(Vec::new());
        };
        let listing = agentmail::inbox(bin, me).map_err(Error::action)?;
        let seen = seen::load();
        let mut rows: Vec<ReplyRow> = agentmail::inbox_rows(&listing)
            .into_iter()
            .map(|r| self.reply_row(r, &seen))
            .collect();
        // Newest first: the one they are waiting for is at the top.
        rows.reverse();
        Ok(rows)
    }

    fn mark_seen(&self, ids: &[String]) -> Result<()> {
        seen::add(ids).map_err(Error::action)
    }

    fn message(&self, session: &Session, text: &str) -> Result<String> {
        let bin = self
            .agentmail
            .as_ref()
            .ok_or_else(|| Error::Action("no agentmail binary".into()))?;
        agentmail::send(
            bin,
            &session.address().to_string(),
            text,
            self.from.as_deref(),
        )
        .map_err(Error::action)
    }

    fn ask(&self, session: &Session, text: &str) -> Result<Sent> {
        let bin = self
            .agentmail
            .as_ref()
            .ok_or_else(|| Error::Action("no agentmail binary".into()))?;
        let address = session.address().to_string();
        let line =
            agentmail::ask(bin, &address, text, self.from.as_deref()).map_err(Error::action)?;

        // Only a peer that is running and at rest can answer inside the popup's
        // patience, and only a known mailbox can be watched.
        let mut awaiting_reply = false;
        if let Some(from) = self.from.as_deref().filter(|_| can_answer_now(session)) {
            // Remember the mailbox as it is now; anything past this is an answer.
            match agentmail::inbox(bin, from) {
                Ok(baseline) => {
                    *self.inbox_baseline.borrow_mut() = baseline;
                    awaiting_reply = true;
                }
                Err(error) => tracing::warn!(%error, "cannot watch the inbox for a reply"),
            }
        }

        Ok(Sent {
            message_id: agentmail::message_id_of(&line),
            address,
            awaiting_reply,
        })
    }

    fn poll_reply(&self, sent: &Sent) -> Result<Option<ReplyRow>> {
        let (Some(bin), Some(me)) = (self.agentmail.as_ref(), self.from.as_deref()) else {
            return Ok(None);
        };
        let current = agentmail::inbox(bin, me).map_err(Error::action)?;
        let baseline = self.inbox_baseline.borrow().clone();
        if current == baseline {
            return Ok(None);
        }

        // Structured mail tells us who answered and when; a plain listing only
        // gives the new tail, which still beats nothing.
        let before: Vec<String> = agentmail::inbox_rows(&baseline)
            .into_iter()
            .map(|r| r.id)
            .collect();
        let answer = agentmail::inbox_rows(&current).into_iter().find(|r| {
            r.reply_to.as_deref() == Some(sent.message_id.as_str()) || !before.contains(&r.id)
        });
        if let Some(row) = answer {
            return Ok(Some(self.reply_row(row, &Default::default())));
        }
        Ok(
            agentmail::reply_from(&baseline, &current, &sent.message_id).map(|text| ReplyRow {
                id: format!("tail-{}", sent.message_id),
                from: sent.address.clone(),
                title: self.session_title(&sent.address),
                when: Some(chrono::Utc::now()),
                text,
                reply_to: Some(sent.message_id.clone()),
                seen: false,
            }),
        )
    }

    fn can_message(&self) -> bool {
        self.agentmail.is_some()
    }
}

impl TuiActions {
    /// A mailbox row with what the index knows about its sender added: an
    /// address alone says nothing about what that session was doing.
    fn reply_row(&self, row: agentmail::InboxRow, seen: &HashSet<String>) -> ReplyRow {
        ReplyRow {
            title: self.session_title(&row.from),
            seen: seen.contains(&row.id),
            id: row.id,
            from: row.from,
            when: row.when,
            text: row.text,
            reply_to: row.reply_to,
        }
    }

    fn session_title(&self, address: &str) -> Option<String> {
        let (harness, id) = address.split_once(':')?;
        let session = self
            .index
            .get(Some(&HarnessKind::from_name(harness)), id)
            .ok()??;
        session.title.or(session.first_prompt).map(|t| {
            // One line: this lands in a table column.
            t.lines().next().unwrap_or_default().to_string()
        })
    }
}

/// herdr calls a finished agent `done` and a waiting one `idle`; both are
/// sitting at a prompt and can answer now. `working` and `blocked` cannot, and
/// a session with no live pane is not running at all.
fn can_answer_now(session: &Session) -> bool {
    session
        .last_pane
        .as_ref()
        .is_some_and(|p| p.live && matches!(p.status.as_deref(), Some("idle") | Some("done")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hsm_core::{PaneRef, Tier};

    fn with_pane(live: bool, status: &str) -> Session {
        let mut s = Session::new(HarnessKind::Claude, "8890a685", "/p/demo");
        s.tier = Tier::Hot;
        s.last_pane = Some(PaneRef {
            pane_id: "w6:p1".into(),
            live,
            status: Some(status.to_string()),
            ..PaneRef::default()
        });
        s
    }

    #[test]
    fn only_a_running_agent_at_rest_can_answer_while_we_wait() {
        assert!(can_answer_now(&with_pane(true, "idle")));
        assert!(can_answer_now(&with_pane(true, "done")));
        assert!(!can_answer_now(&with_pane(true, "working")));
        assert!(!can_answer_now(&with_pane(true, "blocked")));
        assert!(!can_answer_now(&with_pane(false, "idle")), "not running");
        assert!(!can_answer_now(&Session::new(
            HarnessKind::Claude,
            "x",
            "/p/demo"
        )));
    }
}
