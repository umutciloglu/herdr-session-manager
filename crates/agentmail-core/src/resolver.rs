//! Turning what a caller typed into something deliverable.
//!
//! Sources are consulted cheapest-first: our own registry, then the terminal
//! multiplexer, then an external session index. Anything that matches more than
//! once stops as `Ambiguous` rather than guessing.

use std::sync::Arc;

use crate::domain::{Address, AddressTarget, Harness, Registration, SessionCard, MIN_PREFIX};
use crate::error::Result;
use crate::store::{Liveness, Store};
use crate::traits::{Directory, DirectoryEntry, SessionProvider};

/// How many provider rows we are willing to look at before calling it ambiguous.
const PROVIDER_LIMIT: usize = 10;

pub struct ResolveCtx<'a> {
    pub from: &'a Address,
    pub directory: Option<&'a dyn Directory>,
    pub provider: Option<&'a dyn SessionProvider>,
}

impl<'a> ResolveCtx<'a> {
    pub fn new(from: &'a Address) -> Self {
        ResolveCtx {
            from,
            directory: None,
            provider: None,
        }
    }

    pub fn with_directory(mut self, dir: &'a dyn Directory) -> Self {
        self.directory = Some(dir);
        self
    }

    pub fn with_provider(mut self, provider: &'a dyn SessionProvider) -> Self {
        self.provider = Some(provider);
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Resolved {
    /// A session that is running now. The `DirectoryEntry` is present when the
    /// multiplexer can also see it, which is what tells a deliverer whether it is idle.
    Live(Registration, Option<DirectoryEntry>),
    /// A real address with nothing listening. Delivery queues; a hook drains later.
    Offline(Address, Option<SessionCard>),
    Spawn(Harness),
    Ambiguous(Vec<SessionCard>),
    NotFound,
}

impl Resolved {
    pub fn address(&self) -> Option<Address> {
        match self {
            Resolved::Live(reg, _) => Some(reg.address()),
            Resolved::Offline(addr, _) => Some(addr.clone()),
            _ => None,
        }
    }

    pub fn harness(&self) -> Option<Harness> {
        match self {
            Resolved::Live(reg, _) => Some(reg.harness.clone()),
            Resolved::Offline(addr, _) => Some(addr.harness.clone()),
            Resolved::Spawn(h) => Some(h.clone()),
            _ => None,
        }
    }
}

pub struct Resolver {
    store: Arc<Store>,
}

impl Resolver {
    pub fn new(store: Arc<Store>) -> Self {
        Resolver { store }
    }

    pub fn resolve(&self, target: &AddressTarget, ctx: &ResolveCtx<'_>) -> Result<Resolved> {
        match target {
            AddressTarget::New(h) => Ok(Resolved::Spawn(h.clone())),
            AddressTarget::Alias(name) => self.resolve_alias(name, ctx),
            AddressTarget::Address(addr) => self.resolve_address(addr, ctx),
        }
    }

    fn resolve_address(&self, addr: &Address, ctx: &ResolveCtx<'_>) -> Result<Resolved> {
        // Every row, not just the live ones: an offline session is a valid target, and
        // liveness is decided per match below.
        let regs = self.store.registrations()?;
        let dir = directory_entries(ctx);

        if let Some(reg) = regs.iter().find(|r| r.address() == *addr) {
            return Ok(self.judge(reg.clone(), &dir, ctx.directory.is_some()));
        }

        let usable_prefix = addr.id.len() >= MIN_PREFIX;
        if usable_prefix {
            let hits: Vec<_> = regs
                .iter()
                .filter(|r| r.harness == addr.harness && r.address().matches_prefix(&addr.id))
                .collect();
            match hits.len() {
                1 => {
                    return Ok(self.judge(hits[0].clone(), &dir, ctx.directory.is_some()));
                }
                0 => {}
                _ => {
                    return Ok(Resolved::Ambiguous(
                        hits.iter().map(|r| card_of_reg(r)).collect(),
                    ))
                }
            }

            let hits: Vec<_> = dir
                .iter()
                .filter(|e| {
                    e.address
                        .as_ref()
                        .is_some_and(|a| a.harness == addr.harness && a.matches_prefix(&addr.id))
                })
                .collect();
            match hits.len() {
                1 => return Ok(live_from_entry(hits[0])),
                0 => {}
                _ => {
                    return Ok(Resolved::Ambiguous(
                        hits.iter().map(|e| card_of_entry(e)).collect(),
                    ))
                }
            }

            let cards = provider_search(ctx, &addr.id, Some(&addr.harness));
            let hits: Vec<_> = cards
                .into_iter()
                .filter(|c| {
                    c.parsed_address()
                        .is_some_and(|a| a.harness == addr.harness && a.matches_prefix(&addr.id))
                })
                .collect();
            match hits.len() {
                1 => {
                    let card = hits.into_iter().next().unwrap_or_default();
                    let a = card.parsed_address().unwrap_or_else(|| addr.clone());
                    return Ok(Resolved::Offline(a, Some(card)));
                }
                0 => {}
                _ => return Ok(Resolved::Ambiguous(hits)),
            }
        }

        // A full-looking address nobody knows yet is still worth queuing: a SessionStart
        // hook registers the session later and drains it. Anything shorter cannot be a
        // valid id or a legal prefix, so it is a typo.
        if usable_prefix {
            Ok(Resolved::Offline(addr.clone(), None))
        } else {
            Ok(Resolved::NotFound)
        }
    }

    fn resolve_alias(&self, name: &str, ctx: &ResolveCtx<'_>) -> Result<Resolved> {
        let regs = self.store.registrations()?;
        let dir = directory_entries(ctx);

        if let Some(reg) = regs.iter().find(|r| {
            r.alias
                .as_deref()
                .is_some_and(|a| a.eq_ignore_ascii_case(name))
        }) {
            return Ok(self.judge(reg.clone(), &dir, ctx.directory.is_some()));
        }

        let hits: Vec<_> = dir
            .iter()
            .filter(|e| {
                e.alias
                    .as_deref()
                    .is_some_and(|a| a.eq_ignore_ascii_case(name))
            })
            .collect();
        match hits.len() {
            1 => return Ok(live_from_entry(hits[0])),
            0 => {}
            _ => {
                return Ok(Resolved::Ambiguous(
                    hits.iter().map(|e| card_of_entry(e)).collect(),
                ))
            }
        }

        let cards = provider_search(ctx, name, None);
        match cards.len() {
            1 => {
                let card = cards.into_iter().next().unwrap_or_default();
                match card.parsed_address() {
                    Some(a) => Ok(Resolved::Offline(a, Some(card))),
                    None => Ok(Resolved::NotFound),
                }
            }
            0 => Ok(Resolved::NotFound),
            _ => Ok(Resolved::Ambiguous(cards)),
        }
    }
}

impl Resolver {
    /// Turns a registry row into a verdict.
    ///
    /// A hook row is a claim, not a process: the SessionStart hook records that a
    /// session existed and nothing tells us when its pane closed. So an `Unproven` row
    /// is `Live` only when the multiplexer confirms it. With no multiplexer wired up
    /// (standalone use) the freshness window is all we have, so we trust it.
    fn judge(&self, reg: Registration, dir: &[DirectoryEntry], has_directory: bool) -> Resolved {
        let addr = reg.address();
        let entry = entry_for(dir, &addr).or_else(|| entry_for_pane(dir, &reg));
        match self.store.liveness(&reg) {
            Liveness::Proven => Resolved::Live(reg, entry),
            Liveness::Unproven if entry.is_some() || !has_directory => Resolved::Live(reg, entry),
            _ => {
                let card = card_of_reg(&reg);
                Resolved::Offline(addr, Some(card))
            }
        }
    }
}

fn directory_entries(ctx: &ResolveCtx<'_>) -> Vec<DirectoryEntry> {
    ctx.directory.map(|d| d.live_agents()).unwrap_or_default()
}

/// A provider that errors out is a provider that is not there. Callers never see it.
fn provider_search(
    ctx: &ResolveCtx<'_>,
    query: &str,
    harness: Option<&Harness>,
) -> Vec<SessionCard> {
    ctx.provider
        .and_then(|p| p.search(query, harness, None, PROVIDER_LIMIT).ok())
        .unwrap_or_default()
}

fn entry_for(dir: &[DirectoryEntry], addr: &Address) -> Option<DirectoryEntry> {
    dir.iter()
        .find(|e| e.address.as_ref() == Some(addr))
        .cloned()
}

/// Herdr knows the pane even when it cannot report the harness session id, so a pane
/// match confirms a hook row just as well as an address match.
fn entry_for_pane(dir: &[DirectoryEntry], reg: &Registration) -> Option<DirectoryEntry> {
    let pane = reg.herdr_pane.as_deref()?;
    dir.iter()
        .find(|e| e.pane.as_deref() == Some(pane))
        .cloned()
}

/// The multiplexer sees an agent that never registered with agentmail. Stand in a
/// registration for it so delivery still has something addressable.
fn live_from_entry(entry: &DirectoryEntry) -> Resolved {
    match Registration::from_directory(entry) {
        Some(reg) => Resolved::Live(reg, Some(entry.clone())),
        None => Resolved::NotFound,
    }
}

fn card_of_reg(reg: &Registration) -> SessionCard {
    SessionCard {
        address: reg.address().to_string(),
        harness: reg.harness.to_string(),
        cwd: Some(reg.cwd.to_string_lossy().into_owned()),
        title: reg.alias.clone(),
        started: Some(crate::ids::to_rfc3339(&reg.started_at)),
        last_active: Some(crate::ids::to_rfc3339(&reg.last_seen)),
        resumable: true,
        ..SessionCard::default()
    }
}

fn card_of_entry(entry: &DirectoryEntry) -> SessionCard {
    let addr = entry.address.clone();
    SessionCard {
        address: addr.as_ref().map(|a| a.to_string()).unwrap_or_default(),
        harness: addr.map(|a| a.harness.to_string()).unwrap_or_default(),
        cwd: entry.cwd.as_ref().map(|c| c.to_string_lossy().into_owned()),
        title: entry.title.clone().or_else(|| entry.alias.clone()),
        state: Some(format!("{:?}", entry.state).to_ascii_lowercase()),
        resumable: true,
        ..SessionCard::default()
    }
}
