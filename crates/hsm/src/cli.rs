//! Command line definition and dispatch.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use hsm_core::OpenTarget;

use crate::commands;
use crate::runtime::Ctx;

#[derive(Parser, Debug)]
#[command(
    name = "hsm",
    version,
    about = "Browse, search, restore and open agent sessions in herdr"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Open the session browser (the herdr plugin entrypoint).
    Browse {
        /// Running in a normal split pane instead of a popup: the pane to act
        /// on is the one that was focused before the split.
        #[arg(long)]
        pane_mode: bool,
    },

    /// Ask an agent session a question (the same screen, opening disabled).
    Ask {
        /// Running in a normal split pane instead of a popup.
        #[arg(long)]
        pane_mode: bool,
    },

    /// Restore a session into a pane and print what happened as JSON.
    Open {
        /// `<harness>:<id>`, or an id prefix of at least 8 characters.
        address: String,
        /// Where the session lands. Defaults to config `default_open`.
        #[arg(long, value_parser = parse_target)]
        target: Option<OpenTarget>,
        /// Start the agent here instead of in the session's own cwd.
        #[arg(long)]
        cwd: Option<PathBuf>,
    },

    /// Rescan the harness stores and refresh the index.
    Index {
        /// Re-read every transcript from the start instead of from the last
        /// byte offset.
        #[arg(long)]
        full: bool,
    },

    /// List sessions. `--json` is the agentmail session provider contract.
    Sessions {
        /// Print `{"sessions":[...]}` instead of a table.
        #[arg(long)]
        json: bool,
        /// Full-text query; empty lists the most recent sessions.
        #[arg(long)]
        query: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Only this harness, e.g. `claude` or `codex`.
        #[arg(long)]
        harness: Option<String>,
        /// Only this project (the last component of the session's cwd).
        #[arg(long)]
        project: Option<String>,
    },

    /// Post-restore hook: refresh the index in the background. Never fails.
    Startup,

    /// Install or remove the agentmail MCP server and hooks (runs `agentmail setup`).
    SetupChat,

    /// Report what is wrong, and nothing else. Exits 1 when it found something.
    Doctor,
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    // A popup's stderr is the popup itself, so log lines would smear the TUI;
    // only opt in when the user asked for logs.
    let quiet = matches!(cli.command, Command::Browse { .. } | Command::Ask { .. });
    init_logging(quiet);

    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("hsm: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let ctx = Ctx::load()?;
    match cli.command {
        Command::Browse { pane_mode } => commands::browse::run(&ctx, pane_mode)?,
        Command::Ask { pane_mode } => commands::browse::ask(&ctx, pane_mode)?,
        Command::Open {
            address,
            target,
            cwd,
        } => commands::open::run(&ctx, &address, target, cwd)?,
        Command::Index { full } => commands::index::run(&ctx, full)?,
        Command::Sessions {
            json,
            query,
            limit,
            harness,
            project,
        } => commands::sessions::run(
            &ctx,
            json,
            query.as_deref(),
            limit,
            harness.as_deref(),
            project.as_deref(),
        )?,
        // Exits 0 whatever happens: herdr reports a failing startup hook as a
        // broken plugin, and a stale index is not that.
        Command::Startup => return Ok(commands::startup::run(&ctx)),
        Command::SetupChat => return Ok(commands::setup_chat::run(&ctx)),
        // Non-zero when it printed a problem, so a script can act on it.
        Command::Doctor => {
            return Ok(match commands::doctor::run(&ctx)? {
                true => ExitCode::SUCCESS,
                false => ExitCode::FAILURE,
            })
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `split-down` is the herdr-facing spelling of hsm-core's `split-vertical`;
/// accept both so scripts can use either.
fn parse_target(raw: &str) -> Result<OpenTarget, String> {
    let normalised = match raw.trim().to_ascii_lowercase().as_str() {
        "split-down" | "down" => "split-vertical".to_string(),
        "split-right" | "right" => "split-horizontal".to_string(),
        other => other.to_string(),
    };
    normalised
        .parse()
        .map_err(|_| format!("expected current, split, split-down or tab, got {raw:?}"))
}

fn init_logging(quiet: bool) {
    use tracing_subscriber::EnvFilter;

    let filter =
        match EnvFilter::try_from_env("HSM_LOG").or_else(|_| EnvFilter::try_from_default_env()) {
            Ok(filter) => filter,
            Err(_) if quiet => return,
            Err(_) => EnvFilter::new("warn"),
        };
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use hsm_core::SplitDirection;

    #[test]
    fn command_line_is_well_formed() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn target_accepts_both_spellings() {
        assert_eq!(
            parse_target("current").expect("current"),
            OpenTarget::Current
        );
        assert_eq!(
            parse_target("split").expect("split"),
            OpenTarget::Split(SplitDirection::Horizontal)
        );
        assert_eq!(
            parse_target("split-down").expect("down"),
            OpenTarget::Split(SplitDirection::Vertical)
        );
        assert_eq!(parse_target("tab").expect("tab"), OpenTarget::Tab);
        assert!(parse_target("popup").is_err());
    }
}
