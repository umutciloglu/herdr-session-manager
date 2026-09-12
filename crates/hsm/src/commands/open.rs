//! `hsm open` — restore one session from the command line.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use hsm_core::{OpenContext, OpenMethod, OpenService, OpenTarget};
use serde_json::json;

use crate::commands::split_address;
use crate::herdr_adapter::HerdrPaneOps;
use crate::runtime::Ctx;

pub fn run(
    ctx: &Ctx,
    address: &str,
    target: Option<OpenTarget>,
    cwd: Option<PathBuf>,
) -> Result<()> {
    let index = ctx.index()?;
    let (harness, id) = split_address(address);
    let session = index
        .get(harness.as_ref(), &id)?
        .ok_or_else(|| anyhow!("no indexed session matches {address:?}; try `hsm index`"))?;

    let client = ctx
        .herdr()
        .ok_or_else(|| anyhow!("herdr is not running, so there is no pane to open into"))?;
    let target = target.unwrap_or(ctx.config.default_open);
    let service = OpenService::new(HerdrPaneOps::new(client));
    let open_ctx = OpenContext {
        context_pane_id: ctx.plugin.focused_pane_id().map(str::to_string),
        cwd_override: cwd,
    };

    let report = ctx.block_on(service.open(&session, target, &open_ctx))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "address": session.address().to_string(),
            "target": target.as_str(),
            "pane_id": report.pane_id,
            "method": match report.method {
                OpenMethod::AgentStart => "agent_start",
                OpenMethod::SendInput => "send_input",
            },
            "resumed": session.tier != hsm_core::Tier::Gone,
        }))?
    );
    Ok(())
}
