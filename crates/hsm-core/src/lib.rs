//! Session index and restore service for `hsm` (see docs/PLAN.md).
//!
//! Layering: `domain` holds plain types, `harness` adapts each agent's own
//! storage into them, `index` persists and ranks them, `open` restores one into
//! a herdr pane through the `PaneOps` trait. Nothing here talks to herdr or to
//! the network; the binary supplies the adapters.

pub mod config;
pub mod domain;
pub mod error;
pub mod harness;
pub mod index;
pub mod live;
pub mod open;
pub mod paths;

pub use config::Config;
pub use domain::{
    Address, HarnessKind, KeyBinding, Keys, OpenTarget, PaneCard, PaneRef, RefKind, Session,
    SessionCard, SessionRef, SplitDirection, Tier,
};
pub use error::{Error, Result};
pub use index::{Index, Query, RefreshOptions, RefreshReport};
pub use live::{LivePane, LiveSessions, NoLive};
pub use open::{OpenContext, OpenMethod, OpenReport, OpenService, PaneOps};
