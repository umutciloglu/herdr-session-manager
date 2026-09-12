//! Waking a live session that owns its own mailbox drain.

use agentmail_core::{Deliverer, DeliveryOutcome, DeliveryRequest, Poker, Resolved};
use async_trait::async_trait;

/// Rings the recipient's wake-up socket.
///
/// The outcome is `Queued`, not `Pushed`, and that is the whole point: a poke carries
/// no payload. It only tells the recipient's own process to look in the store, and the
/// recipient is the one that marks the row delivered once it has actually shown the
/// message to its model. Reporting `Pushed` here would mark the row delivered while it
/// is still unread, and the recipient's drain (`pending_for`) would then skip it.
///
/// A failed connect means nobody is listening, which is not an error: it returns
/// `Failed` so the next deliverer in the chain gets a turn.
#[derive(Debug, Default)]
pub struct PokeDeliverer;

impl PokeDeliverer {
    pub fn new() -> Self {
        PokeDeliverer
    }
}

#[async_trait]
impl Deliverer for PokeDeliverer {
    async fn deliver(&self, req: &DeliveryRequest<'_>) -> DeliveryOutcome {
        let Resolved::Live(reg, _) = req.resolved else {
            return DeliveryOutcome::Failed("not a live session".into());
        };
        if reg.poke_path.is_none() {
            return DeliveryOutcome::Failed(format!("{} has no poke socket", reg.address()));
        }
        match Poker::poke(reg).await {
            Ok(true) => DeliveryOutcome::Queued,
            Ok(false) => DeliveryOutcome::Failed(format!("{} is not listening", reg.address())),
            Err(e) => DeliveryOutcome::Failed(format!("poke {}: {e}", reg.address())),
        }
    }
}
