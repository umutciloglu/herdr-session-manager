//! A fake `Actions` so the screen can be driven without herdr, sqlite or a tty.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use chrono::{TimeZone, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hsm_core::{
    HarnessKind, Keys, OpenMethod, OpenReport, OpenTarget, PaneRef, Session, SplitDirection, Tier,
};

use crate::actions::{Actions, BrowseContext, Error, JumpReport, Panel, ReplyRow, Result, Sent};
use crate::app::App;

thread_local! {
    /// Each test runs on its own thread, so a thread local keeps the fake's
    /// call log per-test without threading a handle through every helper.
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn log() -> Vec<String> {
    LOG.with(|l| l.borrow().clone())
}

fn record(entry: String) {
    LOG.with(|l| l.borrow_mut().push(entry));
}

pub(crate) fn session(harness: &str, id: &str, project: &str, title: &str) -> Session {
    let mut s = Session::new(
        HarnessKind::from_name(harness),
        id,
        format!("/Users/x/Projects/{project}"),
    );
    s.title = Some(title.to_string());
    s.first_prompt = Some(format!("first prompt of {title}"));
    s.started_at = Utc.with_ymd_and_hms(2026, 9, 10, 8, 0, 0).single();
    s.last_active_at = Utc.with_ymd_and_hms(2026, 9, 12, 7, 55, 0).single();
    s.transcript_present = true;
    s.transcript_path = Some(format!("/transcripts/{id}.jsonl").into());
    s.tier = Tier::Hot;
    s
}

#[derive(Default)]
pub(crate) struct Fake {
    pub sessions: Vec<Session>,
    pub agentmail: bool,
    pub open_fails: bool,
    /// The pane herdr no longer has, so a jump has to fall back to opening.
    pub jump_fails: bool,
    /// What `ask` reports about the peer being able to answer right now.
    pub awaiting: bool,
    /// Poll number that finally carries the answer, if any.
    pub reply_on_poll: Option<u32>,
    /// Mail already waiting for the human when the popup opens.
    pub waiting_replies: Vec<ReplyRow>,
    polls: Cell<u32>,
    /// Pins the index would have persisted, so `get` answers like the real one.
    pins: RefCell<HashMap<String, bool>>,
}

impl Fake {
    pub fn with_sessions() -> Fake {
        let mut live = session(
            "claude",
            "8890a685-a0f1-4a9e-949d-f7f386bc4cb6",
            "trade-help",
            "API authentication",
        );
        live.last_pane = Some(PaneRef {
            pane_id: "w6:p1".into(),
            workspace_id: Some("w6".into()),
            tab_id: Some("w6:t1".into()),
            live: true,
            status: Some("idle".into()),
        });

        let old = session(
            "claude",
            "43901a13-7735-465b-9e08-86e55b01c4c5",
            "flip-to-screen",
            "fold animation",
        );

        let mut gone = session(
            "codex",
            "01a08ad8-1a08-7013-97e7-1053ecf353fe",
            "Documents",
            "codex thread",
        );
        gone.tier = Tier::Gone;
        gone.transcript_present = false;

        Fake {
            sessions: vec![live, old, gone],
            ..Fake::default()
        }
    }
}

impl Actions for Fake {
    fn search(
        &self,
        query: &str,
        harness: Option<&HarnessKind>,
        limit: usize,
    ) -> Result<Vec<Session>> {
        let q = query.to_ascii_lowercase();
        let hit = |s: &Session| {
            let hay = format!(
                "{} {} {}",
                s.title.clone().unwrap_or_default(),
                s.first_prompt.clone().unwrap_or_default(),
                s.project
            )
            .to_ascii_lowercase();
            hay.contains(&q)
        };
        Ok(self
            .sessions
            .iter()
            .filter(|s| hit(s) && harness.is_none_or(|h| &s.harness == h))
            .take(limit)
            .cloned()
            .collect())
    }

    fn recent(&self, limit: usize) -> Result<Vec<Session>> {
        Ok(self.sessions.iter().take(limit).cloned().collect())
    }

    fn get(&self, harness: &HarnessKind, id: &str) -> Result<Option<Session>> {
        Ok(self
            .sessions
            .iter()
            .find(|s| &s.harness == harness && s.id == id)
            .cloned()
            .map(|mut s| {
                s.pinned = self.pins.borrow().get(id).copied().unwrap_or(s.pinned);
                s
            }))
    }

    fn pin(&self, _harness: &HarnessKind, id: &str, pinned: bool) -> Result<()> {
        record(format!("pin({}, {pinned})", &id[..8]));
        self.pins.borrow_mut().insert(id.to_string(), pinned);
        Ok(())
    }

    fn open(&self, session: &Session, target: OpenTarget) -> Result<OpenReport> {
        record(format!("open({}, {target})", session.address().short()));
        if self.open_fails {
            return Err(Error::Action("herdr is not reachable".into()));
        }
        Ok(OpenReport {
            pane_id: "w1:p9".into(),
            method: OpenMethod::AgentStart,
        })
    }

    fn jump(&self, session: &Session) -> Result<JumpReport> {
        let pane_id = session
            .last_pane
            .as_ref()
            .map(|p| p.pane_id.clone())
            .unwrap_or_default();
        record(format!("jump({}, {pane_id})", session.address().short()));
        if self.jump_fails {
            return Err(Error::Action("no such pane".into()));
        }
        Ok(JumpReport { pane_id })
    }

    fn insert_address(&self, session: &Session) -> Result<()> {
        record(format!("insert({})", session.address().short()));
        Ok(())
    }

    fn message(&self, session: &Session, text: &str) -> Result<String> {
        record(format!("message({}, {text})", session.address().short()));
        Ok(format!("queued for {}", session.address().short()))
    }

    fn ask(&self, session: &Session, text: &str) -> Result<Sent> {
        record(format!("ask({}, {text})", session.address().short()));
        Ok(Sent {
            message_id: "01JASKTESTID0000000000000".into(),
            address: session.address().to_string(),
            awaiting_reply: self.awaiting,
        })
    }

    fn poll_reply(&self, _sent: &Sent) -> Result<Option<ReplyRow>> {
        let n = self.polls.get() + 1;
        self.polls.set(n);
        record(format!("poll({n})"));
        Ok(match self.reply_on_poll {
            Some(want) if want == n => Some(ReplyRow {
                id: "01JREPLY0000000000000000A".into(),
                from: "claude:8890a685-a0f1-4a9e-949d-f7f386bc4cb6".into(),
                title: Some("API authentication".into()),
                when: Utc.with_ymd_and_hms(2026, 9, 12, 8, 0, 0).single(),
                text: "yes, the token refreshes hourly".into(),
                reply_to: Some("01JASKTESTID0000000000000".into()),
                seen: false,
            }),
            _ => None,
        })
    }

    fn replies(&self) -> Result<Vec<ReplyRow>> {
        Ok(self.waiting_replies.clone())
    }

    fn mark_seen(&self, ids: &[String]) -> Result<()> {
        record(format!("mark_seen({})", ids.join(",")));
        Ok(())
    }

    fn can_message(&self) -> bool {
        self.agentmail
    }
}

/// The ask popup: same list, but Enter composes a question.
pub(crate) fn ask_app(fake: Fake) -> App {
    LOG.with(|l| l.borrow_mut().clear());
    App::new(
        Box::new(fake),
        BrowseContext {
            invoking_pane: Some("w1:p1".into()),
            invoking_pane_has_agent: true,
            default_open: OpenTarget::Split(SplitDirection::Horizontal),
            start_panel: Panel::Ask,
            sender_note: Some("asking as human:tester from pane w9:p7".into()),
            ..BrowseContext::default()
        },
    )
}

/// A popup opened from a plain shell pane: `c` is allowed, nothing is running.
pub(crate) fn fake_app(fake: Fake) -> App {
    keys_app(fake, Keys::default())
}

/// The same popup with the browser keys rebound.
pub(crate) fn keys_app(fake: Fake, keys: Keys) -> App {
    LOG.with(|l| l.borrow_mut().clear());
    App::new(
        Box::new(fake),
        BrowseContext {
            invoking_pane: None,
            invoking_pane_has_agent: false,
            default_open: OpenTarget::Split(SplitDirection::Horizontal),
            start_panel: Panel::Search,
            keys,
            ..BrowseContext::default()
        },
    )
}

pub(crate) fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}
