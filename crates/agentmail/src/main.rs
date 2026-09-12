//! agentmail — cross-harness agent-to-agent messaging.

mod cli;
mod ctx;
mod doctor;
mod hooks;
mod mail;
mod setup;

use clap::Parser;

use crate::cli::{Cli, Command};
use crate::ctx::Ctx;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing();

    match cli.command {
        // stdout is the MCP protocol from here on; nothing else may print to it.
        Command::Mcp => agentmail_mcp::run().await.map_err(anyhow::Error::from),

        // A hook that fails must never wedge the session that called it.
        Command::Hook { event } => {
            hooks::run(event);
            Ok(())
        }

        Command::Send {
            to,
            text,
            from,
            expects_reply,
            mode,
            wait,
        } => {
            let ctx = Ctx::open(from.as_deref()).await?;
            mail::send(&ctx, &to, &text, expects_reply, mode, wait).await
        }

        Command::Inbox { addr } => {
            let ctx = Ctx::open(None).await?;
            mail::inbox(&ctx, addr.as_deref())
        }

        Command::Wait { timeout } => {
            let ctx = Ctx::open(None).await?;
            mail::wait(&ctx, timeout).await
        }

        Command::Sessions { json } => {
            let ctx = Ctx::open(None).await?;
            mail::sessions(&ctx, json).await
        }

        Command::Alias { name } => {
            let ctx = Ctx::open(None).await?;
            mail::alias(&ctx, &name)
        }

        Command::Setup { check, yes, home } => {
            let code = setup::run(check, yes, home)?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }

        Command::Doctor => {
            let ctx = Ctx::open(None).await?;
            doctor::run(&ctx).await
        }
    }
}

/// Logs go to stderr, always: in `mcp` mode stdout carries JSON-RPC frames.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter =
        EnvFilter::try_from_env("AGENTMAIL_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
}
