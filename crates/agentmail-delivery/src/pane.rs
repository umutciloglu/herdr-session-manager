//! The placeholder address a message wears while its pane is still starting up.
//!
//! A message sent to `<harness>:new` has nowhere to live: the session does not exist
//! yet, and a brand new agent often sits in a trust or channel dialog for a while before
//! it reports a session id. Parking the row on `<harness>:pane:<pane id>` gives it a real
//! destination in the meantime — the pane — and the SessionStart hook, which is the first
//! thing that knows both the pane and the session id, moves it to the real address.

use agentmail_core::{Address, Harness};

const PANE: &str = "pane:";

/// `claude:pane:w1:p3`. The address parser splits on the first colon only, so the pane
/// id keeps its own colons and the whole thing round-trips.
pub fn pane_address(harness: &Harness, pane_id: &str) -> Address {
    Address::new(harness.clone(), format!("{PANE}{pane_id}"))
}

/// The pane a placeholder points at, or `None` for a real session address.
pub fn pane_of(addr: &Address) -> Option<&str> {
    addr.id.strip_prefix(PANE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_placeholder_survives_a_round_trip() {
        let addr = pane_address(&Harness::Claude, "w1:p3");
        assert_eq!(addr.to_string(), "claude:pane:w1:p3");
        assert_eq!(pane_of(&addr), Some("w1:p3"));

        let parsed: Address = "claude:pane:w1:p3".parse().expect("parse");
        assert_eq!(parsed, addr);
    }

    #[test]
    fn a_real_address_is_not_a_placeholder() {
        assert_eq!(
            pane_of(&Address::new(Harness::Codex, "01999b0e-2222")),
            None
        );
    }
}
