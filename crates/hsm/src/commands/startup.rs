//! `hsm startup` — the manifest's one-shot hook after herdr restores a session.

use std::process::ExitCode;

use anyhow::Result;

use crate::commands::index::{refresh, summary};
use crate::runtime::Ctx;

/// Always exits 0: herdr shows a failing startup hook as a broken plugin, and
/// "the index is a bit stale" is not that.
pub fn run(ctx: &Ctx) -> ExitCode {
    match go(ctx) {
        Ok(line) => eprintln!("hsm startup: {line}"),
        Err(error) => eprintln!("hsm startup: skipped, {error:#}"),
    }
    ExitCode::SUCCESS
}

fn go(ctx: &Ctx) -> Result<String> {
    let live = ctx.live(ctx.herdr());
    let mut index = ctx.index()?;
    let report = refresh(&mut index, &ctx.config, false, live.as_ref())?;
    Ok(summary(
        &report,
        index.count().unwrap_or_default(),
        index.message_count().unwrap_or_default(),
    ))
}
