//! What the screen is allowed to do, and what it knows about its caller.

use hsm_core::{HarnessKind, OpenReport, OpenTarget, Session};

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

    /// Type `<harness>:<id8> ` into the pane the popup was invoked from.
    fn insert_address(&self, session: &Session) -> Result<()>;

    /// Runs `agentmail send <address> <text>`; returns the line to show.
    fn message(&self, session: &Session, text: &str) -> Result<String>;

    /// False hides the `m` key: without an agentmail binary there is nothing
    /// to send with.
    fn can_message(&self) -> bool {
        false
    }
}

/// Where the popup was opened from. `invoking_pane` is the tiled pane the user
/// was on (a popup has no pane id of its own).
#[derive(Debug, Clone, Default)]
pub struct BrowseContext {
    pub invoking_pane: Option<String>,
    /// True when that pane is already running an agent. Typing a resume command
    /// into a running agent would feed it a prompt, so `c` is refused.
    pub invoking_pane_has_agent: bool,
    pub default_open: OpenTarget,
}
