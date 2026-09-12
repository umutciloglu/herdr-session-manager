//! The `hook` subcommand. Reads the harness payload on stdin, writes at most one JSON
//! object on stdout, and always exits 0: a hook that fails must never wedge a session.

use std::io::Read;

use agentmail_core::{Harness, Paths, Store};

use crate::cli::HookEvent;

pub fn run(event: HookEvent) {
    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("agentmail hook: cannot read stdin: {e}");
        return;
    }
    if let Err(e) = handle(event, &input) {
        eprintln!("agentmail hook: {e}");
    }
}

fn handle(event: HookEvent, input: &str) -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let store = Store::open_at(&paths)?;

    match event {
        HookEvent::ClaudeStop => drain(&store, &Harness::Claude, input),
        HookEvent::CodexStop => drain(&store, &Harness::Codex, input),
        HookEvent::ClaudeSessionStart => start(&store, &Harness::Claude, input),
        HookEvent::CodexSessionStart => start(&store, &Harness::Codex, input),
    }
}

fn drain(store: &Store, harness: &Harness, input: &str) -> anyhow::Result<()> {
    if let Some(json) = agentmail_delivery::drain_stop(store, harness, input)? {
        println!("{json}");
    }
    Ok(())
}

fn start(store: &Store, harness: &Harness, input: &str) -> anyhow::Result<()> {
    let addr = agentmail_delivery::session_start(store, harness, input)?;
    // Stderr only: SessionStart output would be fed to the model.
    eprintln!("agentmail: registered {addr}");
    Ok(())
}
