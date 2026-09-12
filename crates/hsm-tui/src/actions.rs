//! What the screen is allowed to do, and what it knows about its caller.

use std::time::Duration;

use chrono::{DateTime, Utc};
use hsm_core::{HarnessKind, Keys, OpenReport, OpenTarget, Session};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Whatever the binary's adapter failed at: sqlite, the herdr socket, a
    /// subprocess. The screen only ever prints it, so one string is enough and
    /// hsm-tui stays free of those dependencies.
    #[error("{0}")]
    Action(String),

    // No `{0}`: the io error is the source, and printing it here too would
    // double it in anyhow's chain.
    #[error("terminal io")]
    Io(#[from] std::io::Error),
}

impl Error {
    pub fn action(e: impl std::fmt::Display) -> Error {
        Error::Action(e.to_string())
    }
}

/// Every effect the browser can have. Sync on purpose: the screen is a
/// crossterm loop, so the binary blocks on its async client behind these.
pub trait Actions {
    fn search(
        &self,
        query: &str,
        harness: Option<&HarnessKind>,
        limit: usize,
    ) -> Result<Vec<Session>>;

    fn recent(&self, limit: usize) -> Result<Vec<Session>>;

    fn get(&self, harness: &HarnessKind, id: &str) -> Result<Option<Session>>;

    fn pin(&self, harness: &HarnessKind, id: &str, pinned: bool) -> Result<()>;

    fn open(&self, session: &Session, target: OpenTarget) -> Result<OpenReport>;

    /// Focus the herdr pane that runs this session. Only meaningful for a live
    /// row.
    fn jump(&self, session: &Session) -> Result<JumpReport> {
        let _ = session;
        Err(Error::Action("jumping is not available here".into()))
    }

    /// Type `<harness>:<id8> ` into the pane the popup was invoked from.
    fn insert_address(&self, session: &Session) -> Result<()>;

    /// Runs `agentmail send <address> <text>`; returns the line to show.
    fn message(&self, session: &Session, text: &str) -> Result<String>;

    /// Sends a question that expects an answer back.
    fn ask(&self, session: &Session, text: &str) -> Result<Sent> {
        let _ = (session, text);
        Err(Error::Action("asking is not available here".into()))
    }

    /// One cheap look for the answer to `sent`. The screen drives the schedule
    /// so it can keep drawing and stay interruptible while it waits.
    fn poll_reply(&self, sent: &Sent) -> Result<Option<ReplyRow>> {
        let _ = sent;
        Ok(None)
    }

    /// Everything in the human's mailbox, newest first, each row saying whether
    /// it has been shown before.
    fn replies(&self) -> Result<Vec<ReplyRow>> {
        Ok(Vec::new())
    }

    /// Remembers that these replies have been put in front of the human, so the
    /// unread count stops counting them.
    fn mark_seen(&self, ids: &[String]) -> Result<()> {
        let _ = ids;
        Ok(())
    }

    /// False hides the `m` key: without an agentmail binary there is nothing
    /// to send with.
    fn can_message(&self) -> bool {
        false
    }
}

/// The pane a jump put the user in front of.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JumpReport {
    pub pane_id: String,
}

/// What a question turned into.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sent {
    pub message_id: String,
    pub address: String,
    /// The peer is live and idle, so an answer may come back while we watch.
    /// Otherwise it lands in the sender's own session later.
    pub awaiting_reply: bool,
}

/// One message in the human's mailbox.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplyRow {
    pub id: String,
    /// Address that sent it, e.g. `claude:8890a685-…`.
    pub from: String,
    /// That session's title, when the index knows the sender.
    pub title: Option<String>,
    pub when: Option<DateTime<Utc>>,
    pub text: String,
    /// The question it answers, when the mailbox says.
    pub reply_to: Option<String>,
    /// False until this machine has put it in front of the human.
    pub seen: bool,
}

impl ReplyRow {
    /// The row's one-line summary: mail is usually a paragraph and the list has
    /// one line per message.
    pub fn first_line(&self) -> &str {
        self.text
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
    }
}

/// One popup, three panels, switched with `s`, `a` and `r`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Panel {
    /// Pick a session and open it in a pane.
    #[default]
    Search,
    /// Pick a session and ask it something. Opening is off here.
    Ask,
    /// Everything that answered, newest first.
    Replies,
}

impl Panel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Panel::Search => "search",
            Panel::Ask => "ask",
            Panel::Replies => "replies",
        }
    }
}

/// Where the popup was opened from. `invoking_pane` is the tiled pane the user
/// was on (a popup has no pane id of its own).
#[derive(Debug, Clone)]
pub struct BrowseContext {
    pub invoking_pane: Option<String>,
    /// True when that pane is already running an agent. Typing a resume command
    /// into a running agent would feed it a prompt, so `c` is refused.
    pub invoking_pane_has_agent: bool,
    pub default_open: OpenTarget,
    /// Panel the popup opens on. `hsm browse` starts on search, `hsm ask` on
    /// ask; they are otherwise the same app.
    pub start_panel: Panel,
    /// How long to watch for an answer (`ask_wait_secs`).
    pub ask_wait: Duration,
    /// The rebindable browser keys, so the screen can both match and name them.
    pub keys: Keys,
    /// Who the question goes out as, for the screen to show. Sends are always
    /// from the human, never from the agent in the invoking pane: a reply
    /// addressed to that agent would be injected into it instead of reaching
    /// the person who asked.
    pub sender_note: Option<String>,
}

impl Default for BrowseContext {
    fn default() -> Self {
        BrowseContext {
            invoking_pane: None,
            invoking_pane_has_agent: false,
            default_open: OpenTarget::default(),
            start_panel: Panel::default(),
            ask_wait: Duration::from_secs(120),
            keys: Keys::default(),
            sender_note: None,
        }
    }
}
