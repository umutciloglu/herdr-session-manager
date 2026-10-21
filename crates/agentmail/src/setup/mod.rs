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
/// The Claude Code plugin's id: its name in claude-plugin/agentmail/.claude-plugin/
/// plugin.json, at the marketplace name in .claude-plugin/marketplace.json.
const PLUGIN_ID: &str = "agentmail@herdr-session-manager";
const CHANNEL_FLAG: &str = "--dangerously-load-development-channels";

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
    /// The agentmail Claude Code plugin's id, when it is enabled. It brings
    /// the Claude MCP server and hooks itself, so ours would only make every message
    /// arrive twice.
    plugin: Option<String>,
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
        let plugin = edits::claude_plugin_enabled(
            &read(&home.join(".claude").join("settings.json")),
            &read(
                &home
                    .join(".claude")
                    .join("plugins")
                    .join("installed_plugins.json"),
            ),
            PLUGIN_ID,
        )
        .then(|| PLUGIN_ID.to_string());
        Ok(Setup {
            home,
            real_home,
            exe: exe_path().to_string_lossy().into_owned(),
            plugin,
        })
    }

    /// Rows the plugin takes over once it is enabled.
    fn plugin_provides(&self, kind: Kind) -> bool {
        self.plugin.is_some()
            && matches!(
                kind,
                Kind::ClaudeMcp | Kind::ClaudeStop | Kind::ClaudeSessionStart
            )
    }

    /// Entries an earlier `setup` installed that the plugin now duplicates.
    fn duplicates(&self) -> Vec<Kind> {
        ITEMS
            .iter()
            .map(|(k, _)| *k)
            .filter(|k| self.plugin_provides(*k) && self.installed(*k))
            .collect()
    }

    /// Claude names a plugin's channel by plugin id, a hand-registered one by server.
    fn channel_flag(&self) -> String {
        match &self.plugin {
            Some(id) => format!("{CHANNEL_FLAG} plugin:{id}"),
            None => format!("{CHANNEL_FLAG} server:{MCP_NAME}"),
        }
    }

    fn label(&self, kind: Kind, label: &str) -> String {
        match (self.plugin_provides(kind), self.installed(kind)) {
            (true, true) => format!("{label} (duplicate)"),
            (true, false) => format!("{label} (plugin)"),
            _ => label.to_string(),
        }
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
    #[cfg(not(windows))]
    fn hook_command(&self, event: &str) -> String {
        let exe = if self.exe.contains(char::is_whitespace) {
            format!("'{}'", self.exe)
        } else {
            self.exe.clone()
        };
        format!("{exe} hook {event}")
    }

    /// Codex runs a hook line through PowerShell on Windows, or cmd.exe when it finds no
    /// PowerShell, and the two quote differently. A short (8.3) directory name usually
    /// removes the spaces, so one unquoted line serves both.
    #[cfg(windows)]
    fn hook_command(&self, event: &str) -> String {
        windows_hook_line(&short_dir_path(&self.exe), event)
    }

    /// Claude on Windows runs a hook line through Git Bash, which eats the backslashes
    /// of `C:\...`, or PowerShell. Its exec form skips the shell, and `agentmail.exe` is
    /// the real executable that form requires.
    fn exec_form(kind: Kind) -> bool {
        cfg!(windows) && matches!(kind, Kind::ClaudeStop | Kind::ClaudeSessionStart)
    }

    /// What this binary is called, whatever directory it was run from. Entries are
    /// recognised by this plus their arguments, so a second `setup` from another path
    /// updates the entry instead of installing a rival one that fires as well.
    fn basename(&self) -> String {
        Path::new(&self.exe)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "agentmail".to_string())
    }

    /// Event, arguments and matcher for the hook rows; `None` for everything else.
    fn hook_spec(kind: Kind) -> Option<(&'static str, [&'static str; 2], Option<&'static str>)> {
        match kind {
            Kind::ClaudeStop => Some(("Stop", ["hook", "claude-stop"], None)),
            // Claude's own SessionStart groups carry a matcher; herdr's installed group
            // is the proof that this shape is accepted.
            Kind::ClaudeSessionStart => {
                Some(("SessionStart", ["hook", "claude-session-start"], Some("*")))
            }
            Kind::CodexStop => Some(("Stop", ["hook", "codex-stop"], None)),
            Kind::CodexSessionStart => {
                Some(("SessionStart", ["hook", "codex-session-start"], None))
            }
            _ => None,
        }
    }

    fn installed(&self, kind: Kind) -> bool {
        let content = read(&self.file(kind));
        if let Some((event, args, _)) = Self::hook_spec(kind) {
            return edits::hook_installed(&content, event, &self.basename(), &args);
        }
        match kind {
            Kind::ClaudeMcp => edits::mcp_json_installed(&content, MCP_NAME, &self.basename()),
            Kind::CodexMcp => edits::mcp_toml_installed(&content, MCP_NAME, &self.basename()),
            _ => false,
        }
    }

    /// Every row, as the screens (and the checklist) see it.
    pub fn items(&self) -> Vec<tui::ItemState> {
        ITEMS
            .iter()
            .map(|(kind, label)| {
                let installed = self.installed(*kind);
                tui::ItemState {
                    label: self.label(*kind, label),
                    file: short_path(&self.file(*kind), &self.home),
                    installed,
                    // A duplicate stays a real row, so it can be unticked and removed.
                    informational: *kind == Kind::ChannelHint
                        || (self.plugin_provides(*kind) && !installed),
                }
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
        if let (true, Some(id)) = (self.plugin_provides(kind), &self.plugin) {
            anyhow::bail!("the Claude Code plugin {id} already provides this");
        }
        if Self::hook_spec(kind).is_some() {
            self.write_hook(kind, true)?;
            return Ok(());
        }
        match kind {
            Kind::ClaudeMcp => self.install_claude_mcp(),
            Kind::CodexMcp => self.install_codex_mcp(),
            Kind::ChannelHint => {
                print_channel_hint(&self.channel_flag());
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn uninstall(&self, kind: Kind) -> anyhow::Result<()> {
        let path = self.file(kind);
        let content = read(&path);
        if let Some((event, args, _)) = Self::hook_spec(kind) {
            let next = edits::remove_hook(&content, event, &self.basename(), &args, "hooks")?;
            return write(&path, &next);
        }
        let next = match kind {
            Kind::ClaudeMcp => edits::remove_mcp_json(&content, MCP_NAME, "~/.claude.json")?,
            Kind::CodexMcp => edits::remove_mcp_toml(&content, MCP_NAME)?,
            _ => return Ok(()),
        };
        write(&path, &next)
    }

    /// Writes our hook entry and reports how many duplicates of it were folded away.
    fn write_hook(&self, kind: Kind, add_if_missing: bool) -> anyhow::Result<usize> {
        let Some((event, args, matcher)) = Self::hook_spec(kind) else {
            return Ok(0);
        };
        let path = self.file(kind);
        let content = read(&path);
        let exec = Self::exec_form(kind);
        let command = if exec {
            self.exe.clone()
        } else {
            self.hook_command(args[1])
        };
        let edit = edits::set_hook(
            &content,
            &edits::HookSpec {
                event,
                command: &command,
                exec,
                basename: &self.basename(),
                args: &args,
                matcher,
                add_if_missing,
                path: "hooks",
            },
        )?;
        if edit.content != content {
            write(&path, &edit.content)?;
        }
        Ok(edit.collapsed)
    }

    /// Folds away entries an earlier `setup` left behind when it was run from another
    /// directory: same binary, same arguments, different path — and every one of them
    /// fires, so the model reads every message twice.
    pub fn migrate(&self) -> anyhow::Result<usize> {
        let mut collapsed = 0;
        for (kind, _) in ITEMS {
            if Self::hook_spec(kind).is_some() && self.installed(kind) {
                collapsed += self.write_hook(kind, false)?;
            }
        }
        Ok(collapsed)
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
        println!("binary {}", self.exe);
        if let Some(id) = &self.plugin {
            println!("plugin {id} provides the Claude MCP server and hooks");
        }
        println!();
        for (n, (kind, label)) in ITEMS.iter().enumerate() {
            let mark = match kind {
                Kind::ChannelHint => "-",
                _ if self.installed(*kind) => "x",
                _ if self.plugin_provides(*kind) => "-",
                _ => " ",
            };
            println!(
                "{:>2} [{mark}] {:<34} {}",
                n + 1,
                self.label(*kind, label),
                short_path(&self.file(*kind), &self.home)
            );
        }
    }

    fn missing(&self) -> Vec<Kind> {
        ITEMS
            .iter()
            .map(|(k, _)| *k)
            .filter(|k| *k != Kind::ChannelHint && !self.plugin_provides(*k) && !self.installed(*k))
            .collect()
    }

    fn remove_duplicates(&self) -> anyhow::Result<()> {
        for kind in self.duplicates() {
            self.uninstall(kind)?;
            println!("removed {} (the plugin provides it)", label_of(kind));
        }
        Ok(())
    }

    /// Asks once, before any screen opens. Enter means yes: two copies always deliver
    /// every message twice, so keeping them is the choice that needs typing.
    fn offer_to_remove_duplicates(&self) -> anyhow::Result<()> {
        let Some(id) = &self.plugin else {
            return Ok(());
        };
        let dups = self.duplicates();
        if dups.is_empty() {
            return Ok(());
        }
        println!("The Claude Code plugin {id} brings its own MCP server and hooks.");
        println!("An earlier `agentmail setup` installed these too, so every message would arrive twice:");
        for kind in &dups {
            println!("  {}", label_of(*kind));
        }
        print!("Remove them? [Y/n] ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        // No answer at all (a closed stdin) changes nothing.
        if std::io::stdin().read_line(&mut line)? == 0 {
            println!();
            return Ok(());
        }
        if matches!(line.trim(), "" | "y" | "Y" | "yes") {
            self.remove_duplicates()?;
        }
        println!();
        Ok(())
    }
}

fn label_of(kind: Kind) -> &'static str {
    ITEMS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, label)| *label)
        .unwrap_or("")
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
        let flag = self.channel_flag();
        vec![format!("claude {flag}"), profile_shortcut(&flag)]
    }
}

pub fn run(check: bool, yes: bool, home: Option<PathBuf>) -> anyhow::Result<i32> {
    let home_arg = home.clone();
    let setup = Setup::new(home)?;

    // An older setup, run from a different directory, may have left a second copy of
    // every hook behind. Fold them together before showing anyone the state.
    match setup.migrate() {
        Ok(0) => {}
        Ok(n) => println!(
            "collapsed {n} duplicate hook entr{}\n",
            if n == 1 { "y" } else { "ies" }
        ),
        Err(e) => eprintln!("could not collapse duplicate hooks: {e}"),
    }

    if check {
        setup.print();
        let missing = setup.missing();
        let duplicates = setup.duplicates();
        if missing.is_empty() && duplicates.is_empty() {
            println!("\neverything is installed");
            return Ok(0);
        }
        if !missing.is_empty() {
            println!("\n{} item(s) missing", missing.len());
        }
        if !duplicates.is_empty() {
            println!(
                "\n{} item(s) duplicate the Claude Code plugin; run `agentmail setup` to remove them",
                duplicates.len()
            );
        }
        return Ok(1);
    }

    if yes {
        // "Without asking" covers the duplicates too: keeping them is never what
        // someone who installed the plugin wants.
        setup.remove_duplicates()?;
        for kind in setup.missing() {
            setup.install(kind)?;
        }
        setup.print();
        print_codex_trust_note();
        print_channel_hint(&setup.channel_flag());
        return Ok(0);
    }

    setup.offer_to_remove_duplicates()?;

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
                    let result = if setup.installed(kind) {
                        setup.uninstall(kind)
                    } else {
                        setup.install(kind)
                    };
                    // A row the plugin provides refuses to install; say so and stay.
                    if let Err(e) = result {
                        println!("{e}");
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
fn print_codex_trust_note() {
    println!("\nCodex asks you to trust its hooks once after each change to hooks.json.");
}

fn print_channel_hint(flag: &str) {
    println!("\nClaude only opens a channel when it is launched with:\n");
    println!("  claude {flag}\n");
    println!("Add this to your shell profile yourself if you want it by default:\n");
    println!("  {}\n", profile_shortcut(flag));
}

/// The launch shortcut for the user's own profile. Windows means PowerShell, where an
/// alias cannot carry arguments, so it gets a function instead.
fn profile_shortcut(flag: &str) -> String {
    if cfg!(windows) {
        format!("function claude-mail {{ claude {flag} @args }}")
    } else {
        format!("alias claude-mail='claude {flag}'")
    }
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

/// A line PowerShell and cmd.exe both read as "this program, these words". A path made
/// of plain characters needs no quoting in either. Anything else gets PowerShell's call
/// operator, because PowerShell is the shell Codex picks whenever one exists.
#[cfg(any(windows, test))]
fn windows_hook_line(exe: &str, event: &str) -> String {
    let plain = !exe.is_empty()
        && exe
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '\\' | ':' | '.' | '-' | '_' | '~'));
    if plain {
        format!("{exe} hook {event}")
    } else {
        format!("& '{}' hook {event}", exe.replace('\'', "''"))
    }
}

/// The exe with its directory in 8.3 form, which drops spaces where the volume keeps
/// short names. The file name stays long so the entry still reads `agentmail.exe`.
/// Any failure hands the path back unchanged.
#[cfg(windows)]
fn short_dir_path(exe: &str) -> String {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;

    let path = Path::new(exe);
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return exe.to_string();
    };
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is nul-terminated; a zero-length buffer only asks for the size.
    let needed = unsafe { GetShortPathNameW(wide.as_ptr(), std::ptr::null_mut(), 0) };
    if needed == 0 {
        return exe.to_string();
    }
    let mut buf = vec![0u16; needed as usize];
    // SAFETY: `buf` holds `needed` units, the size the first call asked for.
    let written = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), needed) };
    if written == 0 || written >= needed {
        return exe.to_string();
    }
    buf.truncate(written as usize);
    Path::new(&OsString::from_wide(&buf))
        .join(name)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_windows_path_runs_unquoted_in_both_shells() {
        assert_eq!(
            windows_hook_line(r"C:\Users\FIRSTL~1\herdr\agentmail.exe", "codex-stop"),
            r"C:\Users\FIRSTL~1\herdr\agentmail.exe hook codex-stop"
        );
    }

    #[test]
    fn a_path_with_spaces_uses_the_powershell_call_operator() {
        assert_eq!(
            windows_hook_line(r"C:\Users\First Last\agentmail.exe", "codex-stop"),
            r"& 'C:\Users\First Last\agentmail.exe' hook codex-stop"
        );
        assert_eq!(
            windows_hook_line(r"C:\Users\O'Neil\agentmail.exe", "codex-stop"),
            r"& 'C:\Users\O''Neil\agentmail.exe' hook codex-stop"
        );
    }

    #[test]
    fn claude_uses_exec_form_only_on_windows() {
        assert_eq!(Setup::exec_form(Kind::ClaudeStop), cfg!(windows));
        assert_eq!(Setup::exec_form(Kind::ClaudeSessionStart), cfg!(windows));
        assert!(!Setup::exec_form(Kind::CodexStop));
    }
}
