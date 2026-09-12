//! The deliverer chain. Order matters: cheapest and least intrusive first.

use std::sync::Arc;

use agentmail_core::{Config, Deliverer, Store};

use crate::poke_deliverer::PokeDeliverer;
use crate::spawn::Spawner;

/// A herdr connection when the feature is on, and an uninhabitable type when it is not,
/// so callers can pass `None` from the same call site either way.
#[cfg(feature = "herdr")]
pub type HerdrHandle = herdr_client::HerdrClient;
#[cfg(not(feature = "herdr"))]
pub type HerdrHandle = std::convert::Infallible;

/// 1. poke a session that owns its own drain,
/// 2. type into an idle non-Claude agent herdr can see,
/// 3. start something.
///
/// `Mailbox` takes the first non-`Failed` outcome, so every step is free to bow out.
pub fn default_deliverers(
    cfg: &Config,
    store: Arc<Store>,
    herdr: Option<HerdrHandle>,
) -> Vec<Box<dyn Deliverer>> {
    let mut chain: Vec<Box<dyn Deliverer>> = vec![Box::new(PokeDeliverer::new())];
    #[allow(unused_mut)] // only the herdr feature reassigns it
    let mut spawner = Spawner::new(Arc::clone(&store), cfg.spawn.clone());

    #[cfg(feature = "herdr")]
    if let Some(client) = herdr {
        // `codex_idle` decides whether herdr is allowed to judge idleness at all.
        if cfg.codex_idle == agentmail_core::CodexIdle::Herdr {
            chain.push(Box::new(crate::herdr::HerdrPromptDeliverer::new(
                client.clone(),
            )));
        }
        spawner = spawner.with_pane_spawner(Arc::new(crate::herdr::HerdrPaneSpawner::new(client)));
    }
    #[cfg(not(feature = "herdr"))]
    let _ = herdr;

    chain.push(Box::new(spawner));
    chain
}
