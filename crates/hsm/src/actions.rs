//! `hsm_tui::Actions` over the index, the open service and the herdr socket.
//!
//! This is the only place where the screen's vocabulary meets herdr's: the TUI
//! asks for "open in a split", this blocks on the async client and answers.

use std::path::PathBuf;

use herdr_client::HerdrClient;
use hsm_core::{
    HarnessKind, Index, OpenContext, OpenReport, OpenService, OpenTarget, Query, Session,
};
use hsm_tui::actions::{Actions, Error, Result};
use tokio::runtime::Handle;

use crate::agentmail;
use crate::herdr_adapter::HerdrPaneOps;

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
    /// Session running in the invoking pane, i.e. who a message is from.
    from: Option<String>,
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

    fn can_message(&self) -> bool {
        self.agentmail.is_some()
    }
}
