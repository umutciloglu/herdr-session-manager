//! The command surface. Everything here is parsing only; the work lives in the
//! sibling modules.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "agentmail",
    version,
    about = "Cross-harness agent-to-agent messaging",
    long_about = "Send messages between Claude, Codex and other agent sessions. \
                  Runs as an MCP server inside a session, as a harness hook, or as this CLI."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Serve MCP on stdio. Started by the harness, not by hand.
    Mcp,

    /// Handle a harness hook: hook JSON on stdin, answer on stdout.
    Hook {
        #[arg(value_enum)]
        event: HookEvent,
    },

    /// Send a message to another agent session.
    Send {
        /// <harness>:<id>, an 8+ character id prefix, an alias, or claude:new.
        to: String,
        /// The message body.
        text: String,
        /// Who it is from. Defaults to this session, else human:<user>.
        #[arg(long)]
        from: Option<String>,
        /// Ask the peer to answer.
        #[arg(long)]
        expects_reply: bool,
        /// How hard to try when the peer is not running.
        #[arg(long, value_enum)]
        mode: Option<Mode>,
        /// Wait this many seconds for a reply before returning.
        #[arg(long, value_name = "SECS")]
        wait: Option<u64>,
    },

    /// Show the messages waiting for a session.
    Inbox {
        /// Whose inbox. Defaults to this session.
        #[arg(long)]
        addr: Option<String>,
        /// Machine-readable: {"address", "messages": [...]}, oldest first.
        #[arg(long)]
        json: bool,
    },

    /// Block until the next message for this session arrives.
    Wait {
        #[arg(long, value_name = "SECS", default_value_t = 300)]
        timeout: u64,
    },

    /// List the agent sessions agentmail can see.
    Sessions {
        #[arg(long)]
        json: bool,
    },

    /// Give this session a name others can write to.
    Alias { name: String },

    /// Install agentmail into Claude and Codex.
    Setup {
        /// Print what is installed and exit non-zero if anything is missing.
        #[arg(long)]
        check: bool,
        /// Install everything without asking.
        #[arg(long)]
        yes: bool,
        /// Treat this directory as the home directory (for tests).
        #[arg(long, value_name = "DIR", env = "AGENTMAIL_HOME")]
        home: Option<PathBuf>,
    },

    /// Print what agentmail knows about its own state.
    Doctor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HookEvent {
    ClaudeStop,
    CodexStop,
    ClaudeSessionStart,
    CodexSessionStart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Ask,
    Background,
    Pane,
}

impl From<Mode> for agentmail_core::SendMode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Ask => agentmail_core::SendMode::Ask,
            Mode::Background => agentmail_core::SendMode::Background,
            Mode::Pane => agentmail_core::SendMode::Pane,
        }
    }
}
