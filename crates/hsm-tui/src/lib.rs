//! The session browser popup (see docs/PLAN.md "hsm-tui").
//!
//! Deliberately ignorant of herdr and of sqlite: everything the screen can do
//! goes through the [`Actions`] trait, which the `hsm` binary implements over
//! `Index` + `OpenService`. That keeps the whole screen testable with a fake.

pub mod actions;
pub mod app;
pub mod run;
pub mod ui;

#[cfg(test)]
mod testing;

pub use actions::{Actions, BrowseContext, Error, Result};
pub use app::{App, Mode, Status};
pub use run::run;
