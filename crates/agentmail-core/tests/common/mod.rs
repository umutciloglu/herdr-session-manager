//! Fakes shared by the integration tests.

// Each test binary uses a different subset of these.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use agentmail_core::domain::{Address, Harness, SendMode, SessionCard};
use agentmail_core::traits::{
    AgentState, Deliverer, DeliveryOutcome, DeliveryRequest, Directory, DirectoryEntry,
    ProcessProbe, SessionProvider,
};
use agentmail_core::{Error, Result};
use async_trait::async_trait;

/// Declares a fixed set of pids alive; everything else is dead.
pub struct FakeProbe(pub Vec<u32>);

impl ProcessProbe for FakeProbe {
    fn is_alive(&self, pid: u32) -> bool {
        self.0.contains(&pid)
    }
}

#[derive(Default)]
pub struct FakeDirectory(pub Vec<DirectoryEntry>);

impl Directory for FakeDirectory {
    fn live_agents(&self) -> Vec<DirectoryEntry> {
        self.0.clone()
    }
}

pub fn entry(address: Option<Address>, alias: Option<&str>, state: AgentState) -> DirectoryEntry {
    DirectoryEntry {
        address,
        alias: alias.map(str::to_string),
        state,
        cwd: Some("/work".into()),
        title: Some("some title".into()),
        pane: None,
    }
}

pub fn pane_entry(pane: &str, state: AgentState) -> DirectoryEntry {
    DirectoryEntry {
        pane: Some(pane.to_string()),
        ..entry(None, None, state)
    }
}

pub struct FakeProvider {
    pub cards: Vec<SessionCard>,
    pub available: bool,
}

impl FakeProvider {
    pub fn new(cards: Vec<SessionCard>) -> Self {
        FakeProvider {
            cards,
            available: true,
        }
    }

    pub fn unavailable() -> Self {
        FakeProvider {
            cards: Vec::new(),
            available: false,
        }
    }
}

impl SessionProvider for FakeProvider {
    fn search(
        &self,
        query: &str,
        harness: Option<&Harness>,
        _project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SessionCard>> {
        if !self.available {
            return Err(Error::provider("fake is down"));
        }
        let q = query.to_ascii_lowercase();
        Ok(self
            .cards
            .iter()
            .filter(|c| harness.is_none_or(|h| c.harness == h.as_str()))
            .filter(|c| {
                c.address.to_ascii_lowercase().contains(&q)
                    || c.title
                        .as_deref()
                        .is_some_and(|t| t.to_ascii_lowercase().contains(&q))
            })
            .take(limit)
            .cloned()
            .collect())
    }

    fn recent(&self, limit: usize) -> Result<Vec<SessionCard>> {
        if !self.available {
            return Err(Error::provider("fake is down"));
        }
        Ok(self.cards.iter().take(limit).cloned().collect())
    }

    fn transcript(&self, _address: &Address) -> Result<Option<String>> {
        Ok(None)
    }
}

pub fn card(address: &str, harness: &str, title: &str) -> SessionCard {
    SessionCard {
        address: address.into(),
        harness: harness.into(),
        title: Some(title.into()),
        resumable: true,
        ..SessionCard::default()
    }
}

/// Returns a scripted outcome and records the (message id, mode) it was handed.
pub struct FakeDeliverer {
    outcome: DeliveryOutcome,
    pub seen: Arc<Mutex<Vec<(String, SendMode)>>>,
}

impl FakeDeliverer {
    pub fn new(outcome: DeliveryOutcome) -> Self {
        FakeDeliverer {
            outcome,
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl Deliverer for FakeDeliverer {
    async fn deliver(&self, req: &DeliveryRequest<'_>) -> DeliveryOutcome {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push((req.message.id.clone(), req.mode));
        }
        self.outcome.clone()
    }
}
