//! `hsm` — the herdr session manager binary (see docs/PLAN.md "Product 1").
//!
//! Everything below is wiring: hsm-core owns the index and the open service,
//! hsm-tui owns the screen, herdr-client owns the socket. This crate supplies
//! the adapters that join them and the command line that picks one.

mod actions;
mod agentmail;
mod cli;
mod commands;
mod herdr_adapter;
mod runtime;
mod seen;

fn main() -> std::process::ExitCode {
    cli::main()
}
