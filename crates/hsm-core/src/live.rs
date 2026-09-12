use std::path::PathBuf;

use crate::domain::SessionRef;

/// A pane herdr reports as running an agent right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivePane {
    pub pane_id: String,
    pub workspace_id: Option<String>,
    pub tab_id: Option<String>,
    pub agent: String,
    pub session: SessionRef,
    pub cwd: PathBuf,
    pub title: Option<String>,
    pub status: String,
}

/// The index's only view of the running herdr server. The herdr-backed
/// implementation lives in the `hsm` binary so hsm-core stays offline and
/// testable.
pub trait LiveSessions {
    fn live(&self) -> Vec<LivePane>;
}

/// Use when herdr is not reachable (CLI runs outside a herdr session, tests).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoLive;

impl LiveSessions for NoLive {
    fn live(&self) -> Vec<LivePane> {
        Vec::new()
    }
}

impl<T: LiveSessions + ?Sized> LiveSessions for &T {
    fn live(&self) -> Vec<LivePane> {
        (**self).live()
    }
}

impl<T: LiveSessions + ?Sized> LiveSessions for Box<T> {
    fn live(&self) -> Vec<LivePane> {
        (**self).live()
    }
}
