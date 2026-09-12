//! `agentmail setup` — the interactive checklist that plugs agentmail into Claude and
//! Codex. All file content goes through `edits`; this module only decides what to write
//! and where, and asks before it does.

pub mod edits;
pub mod tui;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::ctx::exe_path;

const MCP_NAME: &str = "agentmail";
const CHANNEL_FLAG: &str = "--dangerously-load-development-channels server:agentmail";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    ClaudeMcp,
    ClaudeStop,
    ClaudeSessionStart,
    ChannelHint,
    CodexMcp,
    CodexStop,
    CodexSessionStart,
}

const ITEMS: [(Kind, &str); 7] = [
    (Kind::ClaudeMcp, "Claude MCP server"),
    (Kind::ClaudeStop, "Claude Stop hook"),
    (Kind::ClaudeSessionStart, "Claude SessionStart hook"),
    (Kind::ChannelHint, "Claude channel flag (launch hint)"),
    (Kind::CodexMcp, "Codex MCP server"),
    (Kind::CodexStop, "Codex Stop hook"),
    (Kind::CodexSessionStart, "Codex SessionStart hook"),
];

pub struct Setup {
    home: PathBuf,
    /// False when `--home` points somewhere else: then the harness CLIs are off limits,
    /// because they would happily write to the real configuration instead.
    real_home: bool,
    exe: String,
}

impl Setup {
    pub fn new(home: Option<PathBuf>) -> anyhow::Result<Setup> {
        let real = dirs::home_dir();
        let home = match home {
            Some(h) => h,
            None => real
                .clone()
                .ok_or_else(|| anyhow::anyhow!("no home directory; pass --home"))?,
        };
        let real_home = real.as_deref() == Some(home.as_path());
        Ok(Setup {
            home,
            real_home,
            exe: exe_path().to_string_lossy().into_owned(),
        })
    }

    fn claude_settings(&self) -> PathBuf {
        self.home.join(".claude").join("settings.json")
    }

    fn claude_json(&self) -> PathBuf {
        self.home.join(".claude.json")
    }

    fn codex_hooks(&self) -> PathBuf {
        self.home.join(".codex").join("hooks.json")
    }

    fn codex_config(&self) -> PathBuf {
        self.home.join(".codex").join("config.toml")
    }

    fn file(&self, kind: Kind) -> PathBuf {
        match kind {
            Kind::ClaudeMcp => self.claude_json(),
            Kind::ClaudeStop | Kind::ClaudeSessionStart | Kind::ChannelHint => {
                self.claude_settings()
            }
            Kind::CodexMcp => self.codex_config(),
            Kind::CodexStop | Kind::CodexSessionStart => self.codex_hooks(),
        }
    }

    /// The exact command a harness runs. Quoted so a path with spaces survives the
    /// shell the harness runs it through.
    fn hook_command(&self, event: &str) -> String {
        let exe = if self.exe.contains(char::is_whitespace) {
            format!("'{}'", self.exe)
        } else {
            self.exe.clone()
        };
        format!("{exe} hook {event}")
    }

    fn installed(&self, kind: Kind) -> bool {
        let content = read(&self.file(kind));
        match kind {
            Kind::ClaudeMcp => edits::mcp_json_installed(&content, MCP_NAME),
            Kind::ClaudeStop => {
                edits::hook_installed(&content, "Stop", &self.hook_command("claude-stop"))
            }
            Kind::ClaudeSessionStart => edits::hook_installed(
                &content,
                "SessionStart",
                &self.hook_command("claude-session-start"),
            ),
            Kind::ChannelHint => false,
            Kind::CodexMcp => edits::mcp_toml_installed(&content, MCP_NAME),
            Kind::CodexStop => {
                edits::hook_installed(&content, "Stop", &self.hook_command("codex-stop"))
            }
            Kind::CodexSessionStart => edits::hook_installed(
                &content,
                "SessionStart",
                &self.hook_command("codex-session-start"),
            ),
        }
    }

    /// Every row, as the screens (and the checklist) see it.
    pub fn items(&self) -> Vec<tui::ItemState> {
        ITEMS
            .iter()
            .map(|(kind, label)| tui::ItemState {
                label: (*label).to_string(),
                file: short_path(&self.file(*kind), &self.home),
                installed: self.installed(*kind),
                informational: *kind == Kind::ChannelHint,
            })
            .collect()
    }

    fn kind_at(&self, index: usize) -> anyhow::Result<Kind> {
        ITEMS
            .get(index)
            .map(|(kind, _)| *kind)
            .ok_or_else(|| anyhow::anyhow!("no item {index}"))
    }

    fn install(&self, kind: Kind) -> anyhow::Result<()> {
        match kind {
            Kind::ClaudeMcp => self.install_claude_mcp(),
            Kind::ClaudeStop => self.install_hook("Stop", "claude-stop", None),
            // Claude's own SessionStart groups carry a matcher; herdr's installed group
            // is the proof that this shape is accepted.
            Kind::ClaudeSessionStart => {
                self.install_hook("SessionStart", "claude-session-start", Some("*"))
            }
            Kind::ChannelHint => {
                print_channel_hint();
                Ok(())
            }
            Kind::CodexMcp => self.install_codex_mcp(),
            Kind::CodexStop => self.install_hook("Stop", "codex-stop", None),
            Kind::CodexSessionStart => {
                self.install_hook("SessionStart", "codex-session-start", None)
            }
        }
    }

    fn uninstall(&self, kind: Kind) -> anyhow::Result<()> {
        let path = self.file(kind);
        let content = read(&path);
        let next = match kind {
            Kind::ClaudeMcp => edits::remove_mcp_json(&content, MCP_NAME, "~/.claude.json")?,
            Kind::ClaudeStop => edits::remove_hook(
                &content,
                "Stop",
                &self.hook_command("claude-stop"),
                "settings",
            )?,
            Kind::ClaudeSessionStart => edits::remove_hook(
                &content,
                "SessionStart",
                &self.hook_command("claude-session-start"),
                "settings",
            )?,
            Kind::ChannelHint => return Ok(()),
            Kind::CodexMcp => edits::remove_mcp_toml(&content, MCP_NAME)?,
            Kind::CodexStop => {
                edits::remove_hook(&content, "Stop", &self.hook_command("codex-stop"), "hooks")?
            }
            Kind::CodexSessionStart => edits::remove_hook(
                &content,
                "SessionStart",
                &self.hook_command("codex-session-start"),
                "hooks",
            )?,
        };
        write(&path, &next)
    }

    fn install_hook(&self, event: &str, arg: &str, matcher: Option<&str>) -> anyhow::Result<()> {
        let path = if arg.starts_with("claude") {
            self.claude_settings()
        } else {
            self.codex_hooks()
        };
        let content = read(&path);
        let next = edits::add_hook(&content, event, &self.hook_command(arg), matcher, "hooks")?;
        write(&path, &next)
    }

    /// The harness CLI knows its own file format best, so try it first and only edit
    /// JSON when it is missing or refuses.
    fn install_claude_mcp(&self) -> anyhow::Result<()> {
        if self.real_home
            && run_cli(
                "claude",
                &[
                    "mcp", "add", "--scope", "user", MCP_NAME, "--", &self.exe, "mcp",
                ],
            )
        {
            return Ok(());
        }
        let path = self.claude_json();
        let content = read(&path);
        let next = edits::add_mcp_json(&content, MCP_NAME, &self.exe, &["mcp"], "~/.claude.json")?;
        write(&path, &next)
    }

    fn install_codex_mcp(&self) -> anyhow::Result<()> {
        if self.real_home && run_cli("codex", &["mcp", "add", MCP_NAME, "--", &self.exe, "mcp"]) {
            return Ok(());
        }
        let path = self.codex_config();
        let content = read(&path);
        let next = edits::add_mcp_toml(&content, MCP_NAME, &self.exe, &["mcp"])?;
        write(&path, &next)
    }

    fn print(&self) {
        println!("agentmail setup · home {}", self.home.display());
        println!("binary {}\n", self.exe);
        for (n, (kind, label)) in ITEMS.iter().enumerate() {
            let mark = match kind {
                Kind::ChannelHint => "-".to_string(),
                _ if self.installed(*kind) => "x".to_string(),
                _ => " ".to_string(),
            };
            println!(
                "{:>2} [{mark}] {label:<34} {}",
                n + 1,
                short_path(&self.file(*kind), &self.home)
            );
        }
    }

    fn missing(&self) -> Vec<Kind> {
        ITEMS
            .iter()
            .map(|(k, _)| *k)
            .filter(|k| *k != Kind::ChannelHint && !self.installed(*k))
            .collect()
    }
}

impl tui::Installer for Setup {
    fn title(&self) -> (String, String) {
        (self.exe.clone(), self.home.display().to_string())
    }

    fn items(&self) -> Vec<tui::ItemState> {
        Setup::items(self)
    }

    fn install(&self, index: usize) -> anyhow::Result<()> {
        Setup::install(self, self.kind_at(index)?)
    }

    fn uninstall(&self, index: usize) -> anyhow::Result<()> {
        Setup::uninstall(self, self.kind_at(index)?)
    }

    fn hint(&self) -> Vec<String> {
        vec![
            format!("claude {CHANNEL_FLAG}"),
            format!("alias claude-mail='claude {CHANNEL_FLAG}'"),
        ]
    }
}

pub fn run(check: bool, yes: bool, home: Option<PathBuf>) -> anyhow::Result<i32> {
    let home_arg = home.clone();
    let setup = Setup::new(home)?;

    if check {
        setup.print();
        let missing = setup.missing();
        if missing.is_empty() {
            println!("\neverything is installed");
            return Ok(0);
        }
        println!("\n{} item(s) missing", missing.len());
        return Ok(1);
    }

    if yes {
        for kind in setup.missing() {
            setup.install(kind)?;
        }
        setup.print();
        print_channel_hint();
        return Ok(0);
    }

    // A pipe, a CI job or a harness action gets the plain list; only a real terminal
    // gets the screen, which would otherwise write escape codes into somebody's log.
    if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        match tui::run(Box::new(setup)) {
            Ok(()) => return Ok(0),
            // A terminal we cannot drive is not a reason to refuse to install anything.
            Err(e) => eprintln!("falling back to the plain checklist: {e}"),
        }
        let setup = Setup::new(home_arg)?;
        return plain_loop(setup);
    }
    plain_loop(setup)
}

/// The pre-TUI checklist, kept for pipes and for a terminal that will not cooperate.
fn plain_loop(setup: Setup) -> anyhow::Result<i32> {
    loop {
        setup.print();
        println!("\nnumber toggles · a installs everything · q quits");
        print!("> ");
        let _ = std::io::stdout().flush();

        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            return Ok(0);
        }
        match line.trim() {
            "" | "q" | "quit" => return Ok(0),
            "a" | "all" => {
                for kind in setup.missing() {
                    setup.install(kind)?;
                }
            }
            other => match other.parse::<usize>() {
                Ok(n) if (1..=ITEMS.len()).contains(&n) => {
                    let kind = ITEMS[n - 1].0;
                    if setup.installed(kind) {
                        setup.uninstall(kind)?;
                    } else {
                        setup.install(kind)?;
                    }
                }
                _ => println!("not an item: {other}"),
            },
        }
        println!();
    }
}

/// Channels are a launch-time flag, so there is nothing to install — only something to
/// tell the user. We never edit shell rc files.
fn print_channel_hint() {
    println!("\nClaude only opens a channel when it is launched with:\n");
    println!("  claude {CHANNEL_FLAG}\n");
    println!("Add this to your shell profile yourself if you want it by default:\n");
    println!("  alias claude-mail='claude {CHANNEL_FLAG}'\n");
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Silent on purpose: the caller owns the terminal, and inside the checklist screen a
/// stray line would land in the middle of the frame.
fn write(path: &Path, content: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

fn short_path(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

fn run_cli(program: &str, args: &[&str]) -> bool {
    match Command::new(program).args(args).status() {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}
