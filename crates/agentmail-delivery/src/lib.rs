//! agentmail-delivery — the adapters that actually move a message.
//!
//! `agentmail-core` defines `Deliverer`, `Directory` and the mailbox; this crate
//! implements them against the things that exist outside the process: harness hooks,
//! wake-up sockets, headless harness runs, and (behind the `herdr` feature) a herdr
//! multiplexer. Nothing here is aware of MCP.

pub mod error;
pub mod hooks;
pub mod pane;
pub mod poke_deliverer;
pub mod provider;
pub mod router;
pub mod spawn;

#[cfg(feature = "herdr")]
pub mod herdr;

pub use error::{Error, Result};
pub use hooks::{drain_stop, session_start, session_start_in, HookInput};
pub use pane::{pane_address, pane_of};
pub use poke_deliverer::PokeDeliverer;
pub use provider::{resolve_argv, resolve_argv_in};
pub use router::{default_deliverers, HerdrHandle};
pub use spawn::{
    CommandOutput, CommandRunner, CommandSpec, PaneRequest, PaneSpawner, Spawner,
    TokioCommandRunner,
};

#[cfg(feature = "herdr")]
pub use herdr::{address_of, HerdrDirectory, HerdrPaneSpawner, HerdrPromptDeliverer};
