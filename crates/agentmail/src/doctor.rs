//! `agentmail doctor` — everything you need to know why a message did not arrive.

use agentmail_core::{CommandSessionProvider, SessionProvider};

use crate::ctx::Ctx;

pub async fn run(ctx: &Ctx) -> anyhow::Result<()> {
    println!("state dir   {}", ctx.paths.root().display());
    println!("database    {}", ctx.paths.db().display());
    println!("config      {}", ctx.paths.config().display());
    println!("identity    {}", ctx.me());
    println!(
        "harness     {} (pid {}{})",
        ctx.identity.harness,
        ctx.identity.pid,
        ctx.identity
            .herdr_pane
            .as_deref()
            .map(|p| format!(", herdr pane {p}"))
            .unwrap_or_default()
    );

    println!("\nconfig");
    println!("  claude_channel   {}", ctx.cfg.claude_channel);
    println!("  codex_idle       {:?}", ctx.cfg.codex_idle);
    println!(
        "  session_provider {}",
        provider_argv(ctx)
            .map(|a| a.join(" "))
            .unwrap_or_else(|| "(disabled)".into())
    );
    println!("  spawn.claude     {:?}", ctx.cfg.spawn.claude_extra_args);
    println!("  spawn.codex      {:?}", ctx.cfg.spawn.codex_extra_args);

    match ctx.store.prune(chrono::Utc::now()) {
        Ok(n) => println!("\npruned      {n} stale registry row(s)"),
        Err(e) => println!("\npruned      failed: {e}"),
    }

    println!("\nlive registrations");
    let live = ctx.store.live()?;
    if live.is_empty() {
        println!("  (none)");
    }
    for reg in &live {
        println!(
            "  {:<48} pid {:<8} {}{}",
            reg.address().to_string(),
            reg.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            reg.cwd.display(),
            reg.alias
                .as_deref()
                .map(|a| format!("  ({a})"))
                .unwrap_or_default()
        );
    }

    println!("\nsession provider");
    match provider_argv(ctx) {
        Some(argv) => {
            let provider = CommandSessionProvider::new(argv.clone());
            match provider.recent(1) {
                Ok(cards) => println!("  ok, {} session(s) from `{}`", cards.len(), argv.join(" ")),
                Err(e) => println!("  unavailable: {e}"),
            }
        }
        None => println!("  disabled"),
    }

    println!("\nherdr");
    print_herdr().await;
    Ok(())
}

/// The same argv the service will use, sibling lookup included, so `doctor` never
/// reports a provider the running server would have found.
fn provider_argv(ctx: &Ctx) -> Option<Vec<String>> {
    ctx.cfg
        .provider_argv()
        .map(agentmail_delivery::resolve_argv)
}

#[cfg(feature = "herdr")]
async fn print_herdr() {
    match herdr_client::socket_path() {
        Ok(path) => {
            println!("  socket {}", path.display());
            match herdr_client::HerdrClient::connect_path(&path).await {
                Ok(client) => match client.agent_list().await {
                    Ok(agents) => println!("  reachable, {} agent pane(s)", agents.len()),
                    Err(e) => println!("  connected but agent.list failed: {e}"),
                },
                Err(e) => println!("  unreachable: {e}"),
            }
        }
        Err(e) => println!("  no socket: {e}"),
    }
}

#[cfg(not(feature = "herdr"))]
async fn print_herdr() {
    println!("  built without the herdr feature");
}
