//! The message-facing subcommands: send, inbox, wait, sessions, alias.

use agentmail_core::{Message, Registration};
use serde_json::{json, Value};

use crate::cli::Mode;
use crate::ctx::Ctx;

pub async fn send(
    ctx: &Ctx,
    to: &str,
    text: &str,
    expects_reply: bool,
    mode: Option<Mode>,
    wait: Option<u64>,
) -> anyhow::Result<()> {
    let svc = ctx.service().await;
    let mut args = json!({"to": to, "text": text, "expects_reply": expects_reply});
    if let Some(mode) = mode {
        args["mode"] = json!(match mode {
            Mode::Ask => "ask",
            Mode::Background => "background",
            Mode::Pane => "pane",
        });
    }

    let out = svc.call_tool("agentmail_send", &args).await?;
    let outcome = out["outcome"].as_str().unwrap_or("?");
    let target = out["to"].as_str().unwrap_or(to);
    let id = out["message_id"].as_str().unwrap_or("-");
    println!("{outcome} {id} -> {target}");

    if let Some(reply) = out["reply"].as_str() {
        println!("\n{reply}");
        return Ok(());
    }
    if let Some(candidates) = out["candidates"].as_array() {
        println!("\nmore than one session matches:");
        for card in candidates {
            println!("  {}", card_line(card));
        }
        return Ok(());
    }
    if let Some(err) = out["error"].as_str() {
        anyhow::bail!("{err}");
    }
    if outcome == "use_native" {
        println!("{}", out["hint"].as_str().unwrap_or_default());
    }

    if let Some(secs) = wait {
        let answer = svc
            .call_tool(
                "agentmail_wait",
                &json!({"timeout_s": secs, "reply_to": id}),
            )
            .await?;
        match answer["message"]["text"].as_str() {
            Some(text) => println!("\n{text}"),
            None => println!("\n(no reply within {secs}s)"),
        }
    }
    Ok(())
}

pub fn inbox(ctx: &Ctx, addr: Option<&str>) -> anyhow::Result<()> {
    let me = match addr {
        Some(raw) => raw.parse()?,
        None => ctx.me(),
    };
    let messages = ctx.store.inbox(&me, false, 50)?;
    if messages.is_empty() {
        println!("{me}: no mail");
        return Ok(());
    }
    println!("{me}");
    for msg in messages {
        println!("  {}", message_line(&msg));
    }
    Ok(())
}

pub async fn wait(ctx: &Ctx, timeout: u64) -> anyhow::Result<()> {
    let svc = ctx.service().await;
    let out = svc
        .call_tool("agentmail_wait", &json!({ "timeout_s": timeout }))
        .await?;
    if out["timed_out"].as_bool().unwrap_or(false) {
        println!("(nothing within {timeout}s)");
        return Ok(());
    }
    let msg = &out["message"];
    println!(
        "from {} · id {}\n{}",
        msg["from"].as_str().unwrap_or("?"),
        msg["id"].as_str().unwrap_or("?"),
        msg["text"].as_str().unwrap_or("")
    );
    Ok(())
}

pub async fn sessions(ctx: &Ctx, as_json: bool) -> anyhow::Result<()> {
    let svc = ctx.service().await;
    let cards = svc.sessions().await;
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"sessions": cards}))?
        );
        return Ok(());
    }
    if cards.is_empty() {
        println!("no sessions");
        return Ok(());
    }
    for card in &cards {
        println!("{}", card_line(&serde_json::to_value(card)?));
    }
    Ok(())
}

pub fn alias(ctx: &Ctx, name: &str) -> anyhow::Result<()> {
    let id = &ctx.identity;
    // A session that never registered (a hookless setup, or a plain shell) still
    // deserves a name, so create the row before naming it.
    if ctx
        .store
        .get_registration(&id.harness, &id.session_id)?
        .is_none()
    {
        let mut reg = Registration::new(id.harness.clone(), &id.session_id, &id.cwd);
        reg.herdr_pane = id.herdr_pane.clone();
        ctx.store.register(&reg)?;
    }
    ctx.store
        .set_alias(&id.harness, &id.session_id, Some(name))?;
    println!("{} is now `{name}`", ctx.me());
    Ok(())
}

fn message_line(msg: &Message) -> String {
    let first = msg.text.lines().next().unwrap_or_default();
    format!(
        "[{}] {} from {} · {}",
        msg.status,
        msg.id,
        msg.from.short_display(),
        truncate(first, 80)
    )
}

fn card_line(card: &Value) -> String {
    format!(
        "{:<48} {:<10} {}",
        card["address"].as_str().unwrap_or("?"),
        card["state"].as_str().unwrap_or("-"),
        card["title"]
            .as_str()
            .or_else(|| card["first_prompt"].as_str())
            .or_else(|| card["cwd"].as_str())
            .unwrap_or("")
    )
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!("{}…", s.chars().take(max).collect::<String>())
}
