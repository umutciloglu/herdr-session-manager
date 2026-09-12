//! Async client for the herdr local socket API.
//!
//! Newline-delimited JSON over a Unix domain socket (named pipe on Windows).
//! One [`HerdrClient`] owns one connection; subscriptions get their own.
//!
//! ```no_run
//! use herdr_client::{HerdrClient, PaneRead, ReadSource};
//!
//! # async fn run() -> Result<(), herdr_client::Error> {
//! let client = HerdrClient::connect().await?;
//! for agent in client.agent_list().await? {
//!     println!("{} {} {}", agent.pane_id, agent.agent_status, agent.cwd.unwrap_or_default());
//! }
//! let screen = client
//!     .pane_read(&PaneRead::new("w1:p1", ReadSource::Recent).lines(50))
//!     .await?;
//! println!("{}", screen.text);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod client;
pub mod env;
pub mod error;
pub mod events;
pub mod transport;
pub mod types;

pub use client::{HerdrClient, DEFAULT_TIMEOUT};
pub use env::{PluginContext, PluginEnv};
pub use error::{Error, HerdrError, Result};
pub use events::{kind, Event, EventStream, Subscription};
pub use transport::{config_dir, herdr_bin, socket_path, Transport};
pub use types::*;
