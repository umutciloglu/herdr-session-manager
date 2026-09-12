//! agentmail-core — cross-harness agent-to-agent messaging.
//!
//! Layering: `domain` has no dependencies of its own, `store`/`resolver`/`mailbox` are
//! services over it, and everything harness- or multiplexer-specific lives behind the
//! traits in `traits`. This crate knows nothing about herdr, MCP, or spawning agents.

pub mod config;
pub mod domain;
pub mod error;
pub mod ids;
pub mod mailbox;
pub mod paths;
pub mod poke;
pub mod resolver;
pub mod store;
pub mod traits;

pub use config::{CodexIdle, Config, SpawnConfig};
pub use domain::{
    Address, AddressTarget, Envelope, Harness, Message, MessageStatus, Registration, SendMode,
    SessionCard, SessionList, MIN_PREFIX,
};
pub use error::{Error, Result};
pub use mailbox::{Mailbox, SendOpts, SendOutcome, SendResult};
pub use paths::Paths;
pub use poke::{PokeListener, Poker};
pub use resolver::{ResolveCtx, Resolved, Resolver};
pub use store::{Liveness, Store, HOOK_ROW_TTL, PRUNE_AFTER};
pub use traits::{
    AgentState, CommandSessionProvider, Deliverer, DeliveryOutcome, DeliveryRequest, Directory,
    DirectoryEntry, ProcessProbe, SessionProvider,
};
