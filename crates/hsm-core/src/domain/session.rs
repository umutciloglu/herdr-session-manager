use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::harness::HarnessKind;

/// How reachable a session is right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Recent and on disk; message text is indexed.
    Hot,
    /// On disk but older than `hot_days`; metadata only.
    Warm,
    /// Transcript is gone (Claude prunes after `cleanupPeriodDays`); we can
    /// still open a fresh agent in the old cwd.
    Gone,
}

impl Tier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::Hot => "hot",
            Tier::Warm => "warm",
            Tier::Gone => "gone",
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The last herdr pane that carried this session. `live` is true only for the
/// current snapshot, so it is recomputed on every refresh.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneRef {
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default)]
    pub live: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// A harness process that is alive right now but has no herdr pane: a Claude
/// background job, or an interactive session in some other terminal. Snapshot
/// only, recomputed on every refresh like `PaneRef::live`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessRef {
    pub pid: u32,
    pub kind: ProcessKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The herdr pane whose interactive agent is currently showing this job,
    /// matched by title; `None` when nobody is looking at it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessKind {
    /// Started detached, with no terminal of its own.
    Job,
    /// A person is typing at it, just not in a herdr pane.
    Interactive,
}

impl ProcessKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProcessKind::Job => "job",
            ProcessKind::Interactive => "interactive",
        }
    }
}

impl fmt::Display for ProcessKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub harness: HarnessKind,
    pub id: String,
    pub cwd: PathBuf,
    pub project: String,
    pub title: Option<String>,
    pub first_prompt: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub last_active_at: Option<DateTime<Utc>>,
    pub size_bytes: u64,
    pub transcript_path: Option<PathBuf>,
    pub transcript_present: bool,
    pub tier: Tier,
    pub last_pane: Option<PaneRef>,
    pub process: Option<ProcessRef>,
    pub pinned: bool,
}

impl Session {
    pub fn new(harness: HarnessKind, id: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        let cwd = crate::paths::without_verbatim_prefix(cwd.into());
        let project = project_of(&cwd);
        Session {
            harness,
            id: id.into(),
            cwd,
            project,
            title: None,
            first_prompt: None,
            started_at: None,
            last_active_at: None,
            size_bytes: 0,
            transcript_path: None,
            transcript_present: false,
            tier: Tier::Warm,
            last_pane: None,
            process: None,
            pinned: false,
        }
    }

    pub fn address(&self) -> Address {
        Address {
            harness: self.harness.clone(),
            id: self.id.clone(),
        }
    }

    /// First 8 characters of the id — the prefix the UI shows and the protocol
    /// accepts when resolving.
    pub fn short_id(&self) -> String {
        short_id(&self.id)
    }

    pub fn is_live(&self) -> bool {
        self.last_pane.as_ref().is_some_and(|p| p.live)
    }

    /// Alive in any form: a herdr pane, or a process of its own. Jumping still
    /// needs a pane, so it asks `jump_pane`.
    pub fn is_running(&self) -> bool {
        self.is_live() || self.process.is_some()
    }

    /// The pane `Enter` focuses: this session's own live pane, or the pane
    /// where someone is watching it run as a job. `None` opens instead.
    pub fn jump_pane(&self) -> Option<&str> {
        match self.last_pane.as_ref() {
            Some(p) if p.live => Some(&p.pane_id),
            _ => self.process.as_ref()?.pane_id.as_deref(),
        }
    }
}

/// Last path component of the cwd; empty when the cwd is `/` or unknown.
pub fn project_of(cwd: &Path) -> String {
    cwd.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// `<harness>:<id>` as defined in docs/protocol.md.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Address {
    pub harness: HarnessKind,
    pub id: String,
}

impl Address {
    pub fn new(harness: HarnessKind, id: impl Into<String>) -> Self {
        Address {
            harness,
            id: id.into(),
        }
    }

    /// `<harness>:<id8>` — the short form used in lists and inserted into panes.
    pub fn short(&self) -> String {
        format!("{}:{}", self.harness, short_id(&self.id))
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.harness, self.id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("address must look like <harness>:<id>")]
pub struct AddressParseError;

impl FromStr for Address {
    type Err = AddressParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (h, id) = s.split_once(':').ok_or(AddressParseError)?;
        if h.is_empty() || id.is_empty() {
            return Err(AddressParseError);
        }
        Ok(Address {
            harness: HarnessKind::from_name(h),
            id: id.to_string(),
        })
    }
}

impl Serialize for Address {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

/// The provider JSON row from docs/protocol.md "Session provider".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCard {
    pub address: String,
    pub harness: String,
    pub project: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_active: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    pub resumable: bool,
    /// The herdr pane this session last ran in, when we know of one. A reader
    /// can focus it instead of starting a second copy of the session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane: Option<PaneCard>,
    /// A process running this session outside herdr. Alive, but there is no
    /// pane to focus.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<ProcessCard>,
}

/// [`PaneRef`] as it goes over the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneCard {
    pub pane_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    pub live: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// [`ProcessRef`] as it goes over the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessCard {
    pub pid: u32,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The pane showing this job, so a reader can focus it instead of opening
    /// a session that is already on someone's screen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
}

impl From<&ProcessRef> for ProcessCard {
    fn from(p: &ProcessRef) -> Self {
        ProcessCard {
            pid: p.pid,
            kind: p.kind.as_str().to_string(),
            status: p.status.clone(),
            name: p.name.clone(),
            pane_id: p.pane_id.clone(),
        }
    }
}

impl From<&PaneRef> for PaneCard {
    fn from(p: &PaneRef) -> Self {
        PaneCard {
            pane_id: p.pane_id.clone(),
            workspace_id: p.workspace_id.clone(),
            tab_id: p.tab_id.clone(),
            live: p.live,
            status: p.status.clone(),
        }
    }
}

impl From<&Session> for SessionCard {
    fn from(s: &Session) -> Self {
        SessionCard {
            address: s.address().to_string(),
            harness: s.harness.to_string(),
            project: s.project.clone(),
            cwd: s.cwd.to_string_lossy().into_owned(),
            title: s.title.clone(),
            started: s.started_at.map(|t| t.to_rfc3339()),
            last_active: s.last_active_at.map(|t| t.to_rfc3339()),
            first_prompt: s.first_prompt.clone(),
            transcript_path: s
                .transcript_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            // A Gone session cannot be resumed, only restarted in its old cwd.
            resumable: s.tier != Tier::Gone && crate::harness::registry::is_resumable(&s.harness),
            pane: s.last_pane.as_ref().map(PaneCard::from),
            process: s.process.as_ref().map(ProcessCard::from),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_round_trip_and_short() {
        let a: Address = "claude:8890a685-1111-2222-3333-444455556666"
            .parse()
            .expect("parse");
        assert_eq!(a.harness, HarnessKind::Claude);
        assert_eq!(a.short(), "claude:8890a685");
        assert_eq!(a.to_string(), "claude:8890a685-1111-2222-3333-444455556666");
    }

    #[test]
    fn address_rejects_junk() {
        assert!("claude".parse::<Address>().is_err());
        assert!(":x".parse::<Address>().is_err());
    }

    #[test]
    fn project_is_last_path_component() {
        assert_eq!(
            project_of(Path::new("/Users/x/Projects/trade-help")),
            "trade-help"
        );
        assert_eq!(project_of(Path::new("/")), "");
    }

    #[test]
    fn a_card_carries_the_pane_only_when_there_is_one() {
        let mut s = Session::new(HarnessKind::Claude, "abc", "/tmp/proj");
        assert_eq!(SessionCard::from(&s).pane, None);
        let json = serde_json::to_string(&SessionCard::from(&s)).expect("json");
        assert!(!json.contains("pane"), "{json}");

        s.last_pane = Some(PaneRef {
            pane_id: "w6:p1".into(),
            workspace_id: Some("w6".into()),
            tab_id: Some("w6:t1".into()),
            live: true,
            status: Some("idle".into()),
        });
        let pane = SessionCard::from(&s).pane.expect("pane");
        assert_eq!(pane.pane_id, "w6:p1");
        assert_eq!(pane.workspace_id.as_deref(), Some("w6"));
        assert!(pane.live);
    }

    #[test]
    fn a_card_carries_the_process_only_when_there_is_one() {
        let mut s = Session::new(HarnessKind::Claude, "abc", "/tmp/proj");
        assert_eq!(SessionCard::from(&s).process, None);
        assert!(!s.is_running());

        s.process = Some(ProcessRef {
            pid: 57845,
            kind: ProcessKind::Job,
            status: Some("busy".into()),
            name: Some("herdr search session linking".into()),
            pane_id: None,
        });
        assert!(s.is_running(), "a process with no pane still runs");
        assert!(!s.is_live(), "but there is nothing to jump to");
        assert_eq!(s.jump_pane(), None);

        let card = SessionCard::from(&s);
        let process = card.process.as_ref().expect("process");
        assert_eq!(process.pid, 57845);
        assert_eq!(process.kind, "job");
        assert_eq!(process.status.as_deref(), Some("busy"));
        assert_eq!(process.pane_id, None);

        let json = serde_json::to_string(&card).expect("json");
        assert!(
            json.contains(r#""process":{"pid":57845,"kind":"job""#),
            "{json}"
        );
        assert!(!json.contains("pane_id"), "{json}");

        // Someone is watching the job from their own pane: that pane is what
        // Enter focuses, even though the job process has none of its own.
        s.process = Some(ProcessRef {
            pane_id: Some("w9:p7".into()),
            ..s.process.clone().expect("process")
        });
        assert_eq!(s.jump_pane(), Some("w9:p7"));
        assert_eq!(
            SessionCard::from(&s)
                .process
                .and_then(|p| p.pane_id)
                .as_deref(),
            Some("w9:p7")
        );

        // A pane of its own still wins.
        s.last_pane = Some(PaneRef {
            pane_id: "w6:p1".into(),
            live: true,
            ..PaneRef::default()
        });
        assert_eq!(s.jump_pane(), Some("w6:p1"));
    }

    #[test]
    fn card_marks_gone_sessions_unresumable() {
        let mut s = Session::new(HarnessKind::Claude, "abc", "/tmp/proj");
        s.tier = Tier::Gone;
        assert!(!SessionCard::from(&s).resumable);
        s.tier = Tier::Warm;
        assert!(SessionCard::from(&s).resumable);
    }
}
