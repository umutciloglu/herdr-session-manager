//! Times a real refresh against the stores on this machine.
//!
//! ```text
//! cargo run -p hsm-core --release --example refresh -- [--full] [--state-dir DIR]
//! ```
//! Read-only against `~/.claude`, `~/.codex` and `~/.config/herdr`.

use std::path::PathBuf;

use hsm_core::{Config, Index, NoLive, Query, RefreshOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut full = false;
    let mut state_dir: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--full" => full = true,
            "--state-dir" => state_dir = args.next().map(PathBuf::from),
            other => return Err(format!("unknown argument {other}").into()),
        }
    }

    let config = Config::load(&hsm_core::paths::config_path())?;
    let path = state_dir
        .map(|d| d.join("index.sqlite"))
        .unwrap_or_else(hsm_core::paths::index_path);
    println!("index: {}", path.display());

    let mut index = Index::open(&path)?;
    let live = NoLive;
    let report = index.refresh(&RefreshOptions {
        hot_days: config.hot_days,
        full,
        disabled: &config.disabled_harnesses,
        live: &live,
        extra_transcript_roots: &config.extra_transcript_roots,
        store_roots_override: None,
        registry_dir_override: None,
    })?;

    println!("{report:#?}");
    println!("sessions: {}", index.count()?);
    println!("messages: {}", index.message_count()?);

    let t = std::time::Instant::now();
    let hits = index.search(&Query::text("auth"))?;
    println!("search \"auth\": {} hits in {:?}", hits.len(), t.elapsed());
    for s in hits.iter().take(5) {
        println!(
            "  {:<24} {:<14} {:<6} {}",
            s.address().short(),
            s.project,
            s.tier,
            s.title.as_deref().unwrap_or("")
        );
    }
    Ok(())
}
