//! Wire types for the herdr socket API.
//!
//! Everything here is deliberately tolerant: herdr adds fields between protocol
//! revisions, so nothing uses `deny_unknown_fields` and every optional field
//! defaults instead of failing the whole response.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Semantic agent state. Unknown strings from a newer herdr map to [`AgentStatus::Unknown`]
/// rather than failing the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    #[default]
    Unknown,
}

impl AgentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentStatus::Idle => "idle",
            AgentStatus::Working => "working",
            AgentStatus::Blocked => "blocked",
            AgentStatus::Done => "done",
            AgentStatus::Unknown => "unknown",
        }
    }

    pub fn from_wire(s: &str) -> Self {
        match s {
            "idle" => AgentStatus::Idle,
            "working" => AgentStatus::Working,
            "blocked" => AgentStatus::Blocked,
            "done" => AgentStatus::Done,
            _ => AgentStatus::Unknown,
        }
    }
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for AgentStatus {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AgentStatus {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Ok(AgentStatus::from_wire(&raw))
    }
}

/// herdr splits along `right` or `down`, not horizontal/vertical.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Right,
    Down,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginPanePlacement {
    Overlay,
    Popup,
    Split,
    Tab,
    Zoomed,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadSource {
    Visible,
    Recent,
    RecentUnwrapped,
    Detection,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReadFormat {
    #[default]
    Text,
    Ansi,
}

/// Popup dimension: terminal cells, or a percentage string such as `"80%"`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub enum PopupSize {
    Cells(u16),
    Percent(String),
}

impl PopupSize {
    pub fn percent(value: u8) -> Self {
        PopupSize::Percent(format!("{value}%"))
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentSessionRefKind {
    Id,
    Path,
}

/// Native session reference a harness integration reported for a pane.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionInfo {
    pub source: String,
    pub agent: String,
    pub kind: AgentSessionRefKind,
    pub value: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PaneScrollInfo {
    #[serde(default)]
    pub offset_from_bottom: u64,
    #[serde(default)]
    pub max_offset_from_bottom: u64,
    #[serde(default)]
    pub viewport_rows: u64,
}

impl PaneScrollInfo {
    pub fn at_bottom(&self) -> bool {
        self.offset_from_bottom == 0
    }
}

/// A pane that herdr considers to hold an agent. Superset of [`PaneInfo`] minus `label`/`scroll`.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AgentInfo {
    #[serde(default)]
    pub terminal_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub pane_id: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub display_agent: Option<String>,
    /// User-assigned agent name, the addressable handle for `agent.*` targets.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub agent_session: Option<AgentSessionInfo>,
    #[serde(default)]
    pub agent_status: AgentStatus,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub terminal_title: Option<String>,
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub interactive_ready: bool,
    #[serde(default)]
    pub launch_pending: bool,
    #[serde(default)]
    pub screen_detection_skipped: bool,
    #[serde(default)]
    pub state_change_seq: u64,
    #[serde(default)]
    pub state_labels: BTreeMap<String, String>,
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
    #[serde(default)]
    pub revision: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PaneInfo {
    #[serde(default)]
    pub pane_id: String,
    #[serde(default)]
    pub terminal_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub display_agent: Option<String>,
    #[serde(default)]
    pub agent_session: Option<AgentSessionInfo>,
    #[serde(default)]
    pub agent_status: AgentStatus,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub terminal_title: Option<String>,
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub scroll: Option<PaneScrollInfo>,
    #[serde(default)]
    pub state_labels: BTreeMap<String, String>,
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
    #[serde(default)]
    pub revision: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct TabInfo {
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub number: u64,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub pane_count: u64,
    #[serde(default)]
    pub agent_status: AgentStatus,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct WorkspaceInfo {
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub number: u64,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub pane_count: u64,
    #[serde(default)]
    pub tab_count: u64,
    #[serde(default)]
    pub active_tab_id: String,
    #[serde(default)]
    pub agent_status: AgentStatus,
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
    /// Worktree provenance; left untyped because only herdr's own UI reads it.
    #[serde(default)]
    pub worktree: Option<Value>,
}

/// One-time bootstrap from `session.snapshot`.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Snapshot {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub protocol: u32,
    #[serde(default)]
    pub focused_workspace_id: Option<String>,
    #[serde(default)]
    pub focused_tab_id: Option<String>,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceInfo>,
    #[serde(default)]
    pub tabs: Vec<TabInfo>,
    #[serde(default)]
    pub panes: Vec<PaneInfo>,
    #[serde(default)]
    pub agents: Vec<AgentInfo>,
    /// Per-tab BSP layout snapshots, passed through untyped.
    #[serde(default)]
    pub layouts: Vec<Value>,
}

impl Snapshot {
    pub fn pane(&self, pane_id: &str) -> Option<&PaneInfo> {
        self.panes.iter().find(|p| p.pane_id == pane_id)
    }

    pub fn tab(&self, tab_id: &str) -> Option<&TabInfo> {
        self.tabs.iter().find(|t| t.tab_id == tab_id)
    }

    pub fn workspace(&self, workspace_id: &str) -> Option<&WorkspaceInfo> {
        self.workspaces
            .iter()
            .find(|w| w.workspace_id == workspace_id)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PaneReadResult {
    #[serde(default)]
    pub pane_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub source: Option<ReadSource>,
    #[serde(default)]
    pub format: ReadFormat,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PluginPaneInfo {
    #[serde(default)]
    pub plugin_id: String,
    #[serde(default)]
    pub entrypoint: String,
    #[serde(default)]
    pub pane: PaneInfo,
}

/// `tab.create` result: the tab plus the pane it was born with.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct TabCreated {
    pub tab: TabInfo,
    pub root_pane: PaneInfo,
}

/// `agent.start` result: the agent record plus the argv herdr actually launched.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AgentStarted {
    pub agent: AgentInfo,
    #[serde(default)]
    pub argv: Vec<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Params for `agent.start`. The target pane must be sitting at a shell prompt.
#[derive(Serialize, Debug, Clone, Default)]
pub struct AgentStart {
    pub name: String,
    pub kind: String,
    pub pane_id: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl AgentStart {
    pub fn new(
        name: impl Into<String>,
        kind: impl Into<String>,
        pane_id: impl Into<String>,
    ) -> Self {
        AgentStart {
            name: name.into(),
            kind: kind.into(),
            pane_id: pane_id.into(),
            args: Vec::new(),
            timeout_ms: None,
        }
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// herdr accepts 3001..=300000 ms.
    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = Some(ms);
        self
    }
}

/// Submit-and-wait options bundled into `agent.prompt`, avoiding a race with a separate wait.
#[derive(Serialize, Debug, Clone, Default)]
pub struct PromptWait {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub until: Vec<AgentStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl PromptWait {
    pub fn until(statuses: impl IntoIterator<Item = AgentStatus>) -> Self {
        PromptWait {
            until: statuses.into_iter().collect(),
            timeout_ms: None,
        }
    }

    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = Some(ms);
        self
    }
}

/// Params for `agent.prompt`. Fails with `agent_blocked` if the agent sits in a dialog.
#[derive(Serialize, Debug, Clone, Default)]
pub struct AgentPrompt {
    pub target: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<PromptWait>,
}

impl AgentPrompt {
    pub fn new(target: impl Into<String>, text: impl Into<String>) -> Self {
        AgentPrompt {
            target: target.into(),
            text: text.into(),
            wait: None,
        }
    }

    pub fn wait(mut self, wait: PromptWait) -> Self {
        self.wait = Some(wait);
        self
    }
}

/// Params for `agent.wait`.
#[derive(Serialize, Debug, Clone, Default)]
pub struct AgentWait {
    pub target: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub until: Vec<AgentStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl AgentWait {
    pub fn new(target: impl Into<String>) -> Self {
        AgentWait {
            target: target.into(),
            until: Vec::new(),
            timeout_ms: None,
        }
    }

    pub fn until(mut self, statuses: impl IntoIterator<Item = AgentStatus>) -> Self {
        self.until = statuses.into_iter().collect();
        self
    }

    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = Some(ms);
        self
    }
}

/// Params for `pane.split`. Omitting `target_pane_id` splits the focused pane.
#[derive(Serialize, Debug, Clone)]
pub struct PaneSplit {
    pub direction: SplitDirection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_pane_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f32>,
    #[serde(skip_serializing_if = "is_false")]
    pub focus: bool,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

impl PaneSplit {
    pub fn new(direction: SplitDirection) -> Self {
        PaneSplit {
            direction,
            target_pane_id: None,
            workspace_id: None,
            cwd: None,
            ratio: None,
            focus: false,
            env: BTreeMap::new(),
        }
    }

    pub fn target_pane_id(mut self, pane_id: impl Into<String>) -> Self {
        self.target_pane_id = Some(pane_id.into());
        self
    }

    pub fn workspace_id(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = Some(workspace_id.into());
        self
    }

    pub fn cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn ratio(mut self, ratio: f32) -> Self {
        self.ratio = Some(ratio);
        self
    }

    pub fn focus(mut self, focus: bool) -> Self {
        self.focus = focus;
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }
}

/// Params for `tab.create`.
#[derive(Serialize, Debug, Clone, Default)]
pub struct TabCreate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub focus: bool,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

impl TabCreate {
    pub fn new() -> Self {
        TabCreate::default()
    }

    pub fn workspace_id(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = Some(workspace_id.into());
        self
    }

    pub fn cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn focus(mut self, focus: bool) -> Self {
        self.focus = focus;
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }
}

/// Params for `pane.read`.
#[derive(Serialize, Debug, Clone)]
pub struct PaneRead {
    pub pane_id: String,
    pub source: ReadSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strip_ansi: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<ReadFormat>,
}

impl PaneRead {
    pub fn new(pane_id: impl Into<String>, source: ReadSource) -> Self {
        PaneRead {
            pane_id: pane_id.into(),
            source,
            lines: None,
            strip_ansi: None,
            format: None,
        }
    }

    pub fn lines(mut self, lines: u32) -> Self {
        self.lines = Some(lines);
        self
    }

    pub fn strip_ansi(mut self, strip: bool) -> Self {
        self.strip_ansi = Some(strip);
        self
    }

    pub fn format(mut self, format: ReadFormat) -> Self {
        self.format = Some(format);
        self
    }
}

/// Params for `plugin.pane.open`.
#[derive(Serialize, Debug, Clone)]
pub struct PluginPaneOpen {
    pub plugin_id: String,
    pub entrypoint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placement: Option<PluginPanePlacement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<SplitDirection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<PopupSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<PopupSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_pane_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub focus: bool,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

impl PluginPaneOpen {
    pub fn new(plugin_id: impl Into<String>, entrypoint: impl Into<String>) -> Self {
        PluginPaneOpen {
            plugin_id: plugin_id.into(),
            entrypoint: entrypoint.into(),
            placement: None,
            direction: None,
            width: None,
            height: None,
            target_pane_id: None,
            workspace_id: None,
            cwd: None,
            focus: false,
            env: BTreeMap::new(),
        }
    }

    pub fn placement(mut self, placement: PluginPanePlacement) -> Self {
        self.placement = Some(placement);
        self
    }

    pub fn direction(mut self, direction: SplitDirection) -> Self {
        self.direction = Some(direction);
        self
    }

    pub fn width(mut self, width: PopupSize) -> Self {
        self.width = Some(width);
        self
    }

    pub fn height(mut self, height: PopupSize) -> Self {
        self.height = Some(height);
        self
    }

    pub fn target_pane_id(mut self, pane_id: impl Into<String>) -> Self {
        self.target_pane_id = Some(pane_id.into());
        self
    }

    pub fn workspace_id(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = Some(workspace_id.into());
        self
    }

    pub fn cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn focus(mut self, focus: bool) -> Self {
        self.focus = focus;
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }
}
