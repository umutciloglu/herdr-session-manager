//! `hsm browse` — the popup entrypoint.

use std::path::Path;

use anyhow::Result;
use herdr_client::{HerdrClient, PluginEnv};
use hsm_core::domain::project_of;
use hsm_tui::BrowseContext;

use crate::actions::TuiActions;
use crate::agentmail;
use crate::commands::index::refresh;
use crate::herdr_adapter;
use crate::runtime::Ctx;

pub fn run(ctx: &Ctx, pane_mode: bool) -> Result<()> {
    let client = ctx.herdr();
    let mut index = ctx.index()?;

    // Refresh before drawing. Liveness is stored, not computed, so without this
    // the list would show green dots for agents that exited since the startup
    // hook ran. An incremental pass is a few hundred ms on a full store, and a
    // failing one only costs freshness, never the popup.
    let live = ctx.live(client.clone());
    match refresh(&mut index, &ctx.config, false, live.as_ref()) {
        Ok(report) => tracing::info!(ms = report.elapsed_ms, "index refreshed"),
        Err(error) => tracing::warn!(%error, "could not refresh the index"),
    }

    let invoking_pane = invoking_pane(&ctx.plugin, pane_mode);
    let browse = BrowseContext {
        invoking_pane: invoking_pane.clone(),
        // Typing a resume command into a running agent would feed it a prompt.
        invoking_pane_has_agent: ctx.plugin.agent().is_some(),
        default_open: ctx.config.default_open,
    };
    let actions = TuiActions::new(index, client.clone(), ctx.handle())
        .invoking_pane(invoking_pane.clone())
        .project(ctx.plugin.cwd().map(|c| project_of(Path::new(c))))
        .agentmail(
            agentmail::find(&ctx.config.agentmail_bin),
            sender(ctx, client.as_ref(), invoking_pane.as_deref()),
        );

    let status = hsm_tui::run(Box::new(actions), browse)?;
    // A popup vanishes with its process; a `--pane-mode` split does not, so the
    // last line is worth printing.
    if !status.text.is_empty() {
        println!("{}", status.text);
    }
    Ok(())
}

/// Who a message from here is from: the session herdr runs in the invoking
/// pane. The context says whether that pane holds an agent at all, and the
/// snapshot carries its session ref. `None` leaves the sender to agentmail.
fn sender(ctx: &Ctx, client: Option<&HerdrClient>, invoking_pane: Option<&str>) -> Option<String> {
    ctx.plugin.agent()?;
    let client = client?;
    let pane = invoking_pane?;
    let snapshot = ctx.block_on(client.session_snapshot()).ok()?;
    herdr_adapter::pane_address(&snapshot, pane)
}

/// A popup has no `HERDR_PANE_ID`, so the context's focused pane is the pane to
/// act on. In pane mode `HERDR_PANE_ID` is *our own* split, which is exactly
/// the pane we must not type into, so only the context counts there.
fn invoking_pane(env: &PluginEnv, pane_mode: bool) -> Option<String> {
    if pane_mode {
        env.context
            .as_ref()
            .and_then(|c| c.focused_pane_id.clone())
            .filter(|id| Some(id.as_str()) != env.pane_id.as_deref())
    } else {
        env.focused_pane_id().map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use herdr_client::PluginContext;

    fn env(pane_id: Option<&str>, focused: Option<&str>) -> PluginEnv {
        PluginEnv {
            pane_id: pane_id.map(str::to_string),
            context: Some(PluginContext {
                focused_pane_id: focused.map(str::to_string),
                ..PluginContext::default()
            }),
            ..PluginEnv::default()
        }
    }

    #[test]
    fn a_popup_acts_on_the_focused_pane() {
        assert_eq!(
            invoking_pane(&env(None, Some("w1:p3")), false),
            Some("w1:p3".to_string())
        );
    }

    #[test]
    fn pane_mode_uses_the_pane_focused_before_the_split() {
        // scripts/browse.ps1 splits a pane for us and hands the action's own
        // context over with `pane split --env`; the user came from w1:p3.
        assert_eq!(
            invoking_pane(&env(Some("w1:p7"), Some("w1:p3")), true),
            Some("w1:p3".to_string())
        );
    }

    #[test]
    fn pane_mode_never_returns_its_own_pane() {
        // If the context ever names the split we are running in, acting on it
        // would type the resume command into this very browser.
        assert_eq!(
            invoking_pane(&env(Some("w1:p7"), Some("w1:p7")), true),
            None
        );
    }

    #[test]
    fn pane_mode_without_a_context_has_no_pane() {
        // HERDR_PANE_ID alone is our own split, so there is nothing safe to
        // type into and `c`/`i` say so instead of guessing.
        assert_eq!(invoking_pane(&env(Some("w1:p7"), None), true), None);
        assert_eq!(
            invoking_pane(
                &PluginEnv {
                    pane_id: Some("w1:p7".into()),
                    context: None,
                    ..PluginEnv::default()
                },
                true
            ),
            None
        );
    }
}
