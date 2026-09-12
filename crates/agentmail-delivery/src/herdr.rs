//! herdr adapters: what the multiplexer can see, and how to talk to it.
//!
//! Only compiled with the `herdr` feature. agentmail works without herdr; this just
//! makes it better at finding and reaching sessions that are on screen right now.

use std::path::PathBuf;
use std::sync::Mutex;

use agentmail_core::{
    Address, AgentState, Deliverer, DeliveryOutcome, DeliveryRequest, Directory, DirectoryEntry,
    Harness, Resolved,
};
use async_trait::async_trait;
use herdr_client::{AgentInfo, AgentStart, AgentStatus, HerdrClient, PaneSplit, SplitDirection};

use crate::spawn::{PaneRequest, PaneSpawner};

/// herdr must be given a bounded window to get a shell prompt ready; its API rejects
/// anything at or below 3 s.
const AGENT_START_TIMEOUT_MS: u64 = 15_000;

/// The agents herdr can see, as a `Directory` the resolver can consult.
///
/// `Directory::live_agents` is synchronous and the herdr API is not, so the list is a
/// cache: whoever is about to resolve calls `refresh().await` first. That keeps the
/// blocking-in-async problem out of the resolver instead of hiding a `block_on` in it.
pub struct HerdrDirectory {
    client: HerdrClient,
    cache: Mutex<Vec<DirectoryEntry>>,
}

impl HerdrDirectory {
    pub fn new(client: HerdrClient) -> Self {
        HerdrDirectory {
            client,
            cache: Mutex::new(Vec::new()),
        }
    }

    pub fn client(&self) -> &HerdrClient {
        &self.client
    }

    /// Re-reads `agent.list`. A herdr that is gone leaves the last known list in place
    /// rather than blanking the directory mid-send.
    pub async fn refresh(&self) -> herdr_client::Result<()> {
        let agents = self.client.agent_list().await?;
        let entries: Vec<DirectoryEntry> = agents.iter().map(entry_of).collect();
        if let Ok(mut cache) = self.cache.lock() {
            *cache = entries;
        }
        Ok(())
    }

    pub fn entries(&self) -> Vec<DirectoryEntry> {
        self.cache
            .lock()
            .map(|c| c.clone())
            .unwrap_or_else(|e| e.into_inner().clone())
    }
}

impl Directory for HerdrDirectory {
    fn live_agents(&self) -> Vec<DirectoryEntry> {
        self.entries()
    }
}

/// herdr's richer status collapses onto the three states delivery cares about.
/// `done` counts as idle: a finished turn is a free agent.
pub fn state_of(status: AgentStatus) -> AgentState {
    match status {
        AgentStatus::Idle | AgentStatus::Done => AgentState::Idle,
        AgentStatus::Working => AgentState::Working,
        AgentStatus::Blocked => AgentState::Blocked,
        AgentStatus::Unknown => AgentState::Unknown,
    }
}

pub fn entry_of(agent: &AgentInfo) -> DirectoryEntry {
    DirectoryEntry {
        address: address_of(agent),
        alias: agent.name.clone(),
        state: state_of(agent.agent_status),
        cwd: agent.cwd.clone().map(PathBuf::from),
        title: agent
            .terminal_title_stripped
            .clone()
            .or_else(|| agent.title.clone()),
    }
}

/// A pane only has an address once its harness integration has reported a session ref.
/// Public because identity detection asks herdr who is running in its own pane.
pub fn address_of(agent: &AgentInfo) -> Option<Address> {
    let session = agent.agent_session.as_ref()?;
    if session.value.is_empty() {
        return None;
    }
    let label = if session.agent.is_empty() {
        agent.agent.clone().unwrap_or_default()
    } else {
        session.agent.clone()
    };
    let harness: Harness = label.parse().ok()?;
    Some(Address::new(harness, session.value.clone()))
}

async fn find_agent(client: &HerdrClient, addr: &Address) -> Option<AgentInfo> {
    let agents = client.agent_list().await.ok()?;
    agents
        .into_iter()
        .find(|a| address_of(a).as_ref() == Some(addr))
}

/// What herdr should be told to talk to: a pane id if we have one, else the agent name.
fn target_of(agent: &AgentInfo) -> Option<String> {
    if !agent.pane_id.is_empty() {
        return Some(agent.pane_id.clone());
    }
    agent.name.clone().filter(|n| !n.is_empty())
}

/// A terminal submits on newline, so the envelope is flattened into a single line
/// before it is typed into somebody else's prompt.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Types a message straight into a live agent's prompt.
///
/// Claude sessions are excluded on purpose: they have a channel their own MCP process
/// owns, and typing into their prompt would race it.
pub struct HerdrPromptDeliverer {
    client: HerdrClient,
}

impl HerdrPromptDeliverer {
    pub fn new(client: HerdrClient) -> Self {
        HerdrPromptDeliverer { client }
    }
}

#[async_trait]
impl Deliverer for HerdrPromptDeliverer {
    async fn deliver(&self, req: &DeliveryRequest<'_>) -> DeliveryOutcome {
        let Resolved::Live(reg, entry) = req.resolved else {
            return DeliveryOutcome::Failed("not a live session".into());
        };
        if reg.harness == Harness::Claude {
            return DeliveryOutcome::Failed("claude peers are reached over their channel".into());
        }

        let addr = reg.address();
        let agent = match find_agent(&self.client, &addr).await {
            Some(agent) => agent,
            None => return DeliveryOutcome::Failed(format!("herdr does not see {addr}")),
        };
        let state = entry
            .as_ref()
            .map(|e| e.state)
            .unwrap_or(state_of(agent.agent_status));
        if state != AgentState::Idle {
            // Busy or in a dialog: the row stays Pending and the Stop hook hands it
            // over when the current turn ends.
            return DeliveryOutcome::Queued;
        }
        let Some(target) = reg.herdr_pane.clone().or_else(|| target_of(&agent)) else {
            return DeliveryOutcome::Failed(format!("no herdr target for {addr}"));
        };

        let text = one_line(&agentmail_core::Envelope::render(req.message, None));
        match self
            .client
            .agent_prompt(&herdr_client::AgentPrompt::new(target, text))
            .await
        {
            Ok(_) => DeliveryOutcome::Pushed,
            // The agent sat in a dialog after all; queue rather than lose it.
            Err(e) if e.is_code("agent_blocked") => DeliveryOutcome::Queued,
            Err(e) => DeliveryOutcome::Failed(format!("agent.prompt: {e}")),
        }
    }
}

/// Splits a pane and starts a harness in it. Used by `Spawner` for `--mode pane`.
pub struct HerdrPaneSpawner {
    client: HerdrClient,
}

impl HerdrPaneSpawner {
    pub fn new(client: HerdrClient) -> Self {
        HerdrPaneSpawner { client }
    }
}

#[async_trait]
impl PaneSpawner for HerdrPaneSpawner {
    async fn spawn_pane(&self, req: PaneRequest<'_>) -> DeliveryOutcome {
        let mut split = PaneSplit::new(SplitDirection::Right);
        if let Ok(cwd) = std::env::current_dir() {
            split = split.cwd(cwd.to_string_lossy().into_owned());
        }
        let pane = match self.client.pane_split(&split).await {
            Ok(pane) => pane,
            Err(e) => return DeliveryOutcome::Failed(format!("pane.split: {e}")),
        };

        let short = req.resume.map(|a| a.short()).unwrap_or_else(|| {
            req.message
                .id
                .chars()
                .take(8)
                .collect::<String>()
                .to_lowercase()
        });
        let name = format!("{}-{}", req.harness, short);

        let mut args: Vec<String> = Vec::new();
        match (req.resume, req.harness) {
            (Some(addr), Harness::Claude) => {
                args.push("--resume".into());
                args.push(addr.id.clone());
            }
            (Some(addr), Harness::Codex) => {
                args.push("resume".into());
                args.push(addr.id.clone());
            }
            (Some(_), Harness::Other(_)) | (None, _) => {}
        }
        args.extend(req.extra_args.iter().cloned());
        // A resumed session owns the message row already, so it will drain it itself.
        // A brand new session has no address yet — nothing could ever drain a row
        // addressed to `<harness>:new` — so the envelope goes in as its first prompt.
        if req.resume.is_none() {
            args.push(req.envelope.to_string());
        }

        let start = AgentStart::new(name, req.harness.as_str(), &pane.pane_id)
            .args(args)
            .timeout_ms(AGENT_START_TIMEOUT_MS);
        if let Err(e) = self.client.agent_start(&start).await {
            return DeliveryOutcome::Failed(format!("agent.start: {e}"));
        }

        if req.resume.is_some() {
            return DeliveryOutcome::Queued;
        }
        // The harness integration may already have reported the new session id; if so
        // the row can be re-pointed at a real address instead of `<harness>:new`.
        match self.client.agent_get(&pane.pane_id).await {
            Ok(agent) => match address_of(&agent) {
                Some(addr) => DeliveryOutcome::Spawned(addr),
                None => DeliveryOutcome::Pushed,
            },
            Err(_) => DeliveryOutcome::Pushed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_an_agent_row_into_a_directory_entry() {
        let agent: AgentInfo = serde_json::from_value(serde_json::json!({
            "pane_id": "w1:p3",
            "agent": "codex",
            "name": "reviewer",
            "agent_status": "done",
            "cwd": "/repo",
            "terminal_title_stripped": "fixing the parser",
            "agent_session": {"source": "herdr:codex", "agent": "codex", "kind": "id", "value": "01999b0e"}
        }))
        .expect("agent info");

        let entry = entry_of(&agent);
        assert_eq!(
            entry.address,
            Some(Address::new(Harness::Codex, "01999b0e"))
        );
        assert_eq!(entry.alias.as_deref(), Some("reviewer"));
        assert_eq!(entry.state, AgentState::Idle);
        assert_eq!(entry.title.as_deref(), Some("fixing the parser"));
    }

    #[test]
    fn an_agent_without_a_session_ref_has_no_address() {
        let agent: AgentInfo = serde_json::from_value(serde_json::json!({
            "pane_id": "w1:p4",
            "agent": "claude",
            "agent_status": "working"
        }))
        .expect("agent info");
        assert_eq!(entry_of(&agent).address, None);
    }

    #[test]
    fn prompts_are_flattened_to_one_line() {
        assert_eq!(one_line("a\nb   c\n\nd"), "a b c d");
    }
}
