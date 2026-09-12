//! `hsm index` — rescan the harness stores.

use anyhow::Result;
use hsm_core::{Config, Index, LiveSessions, NoLive, RefreshOptions, RefreshReport};

use crate::herdr_adapter::HerdrLive;
use crate::runtime::Ctx;

/// Shared by `index`, `startup` and the first `browse` of a fresh install.
pub fn refresh(
    index: &mut Index,
    config: &Config,
    full: bool,
    live: Option<&HerdrLive>,
) -> Result<RefreshReport> {
    let offline = NoLive;
    let live: &dyn LiveSessions = match live {
        Some(l) => l,
        None => &offline,
    };
    let opts = RefreshOptions {
        hot_days: config.hot_days,
        full,
        disabled: &config.disabled_harnesses,
        live,
        extra_transcript_roots: &config.extra_transcript_roots,
        store_roots_override: None,
        registry_dir_override: None,
    };
    Ok(index.refresh(&opts)?)
}

pub fn run(ctx: &Ctx, full: bool) -> Result<()> {
    let live = ctx.live(ctx.herdr());
    let mut index = ctx.index()?;
    let report = refresh(&mut index, &ctx.config, full, live.as_ref())?;

    println!(
        "{}",
        summary(&report, index.count()?, index.message_count()?)
    );
    for problem in report.errors.iter().take(5) {
        eprintln!("hsm index: {problem}");
    }
    if report.errors.len() > 5 {
        eprintln!("hsm index: {} more problems", report.errors.len() - 5);
    }
    Ok(())
}

/// One line, because that is what the startup hook logs too.
pub fn summary(r: &RefreshReport, sessions: u64, messages: u64) -> String {
    format!(
        "{sessions} sessions, {messages} messages indexed \
         ({} files scanned, {} parsed, {} upserted, {} new messages, {} panes, \
          {} processes, {} gone, {} errors) \
         in {} ms",
        r.files_scanned,
        r.files_parsed,
        r.sessions_upserted,
        r.messages_indexed,
        r.panes_seen,
        r.processes_seen,
        r.marked_gone,
        r.errors.len(),
        r.elapsed_ms
    )
}
