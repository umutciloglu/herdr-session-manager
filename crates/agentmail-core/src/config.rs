//! `<state>/config.toml`. Every field has a default, so a missing file is a valid config.

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::paths::Paths;
use crate::traits::default_argv;

/// How we decide whether a Codex session is idle enough to be prompted.
///
/// There is deliberately no app-server option: the Codex app-server daemon does
/// not share state with the interactive TUI (verified 2026-09-12 — the TUI runs
/// its own embedded server and never attaches to the daemon socket), so a turn
/// started through it would run a hidden second turn on the same thread instead
/// of waking the session the user is watching. herdr, which types into the
/// visible pane, is the only real idle-wake path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodexIdle {
    #[default]
    Herdr,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SpawnConfig {
    pub claude_extra_args: Vec<String>,
    pub codex_extra_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `None` means "use the built-in hsm argv"; an empty vec disables the provider.
    pub session_provider: Option<Vec<String>>,
    pub codex_idle: CodexIdle,
    pub claude_channel: bool,
    pub spawn: SpawnConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            session_provider: None,
            codex_idle: CodexIdle::default(),
            claude_channel: true,
            spawn: SpawnConfig::default(),
        }
    }
}

impl Config {
    pub fn load(paths: &Paths) -> Result<Config> {
        let path = paths.config();
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// The argv to shell out to, or `None` when the provider is switched off.
    pub fn provider_argv(&self) -> Option<Vec<String>> {
        match &self.session_provider {
            None => Some(default_argv()),
            Some(v) if v.is_empty() => None,
            Some(v) => Some(v.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = Config::load(&Paths::new(dir.path())).expect("load");
        assert_eq!(cfg, Config::default());
        assert_eq!(cfg.provider_argv(), Some(default_argv()));
    }

    #[test]
    fn parses_a_full_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::new(dir.path());
        paths.ensure().expect("ensure");
        std::fs::write(
            paths.config(),
            r#"
session_provider = ["hsm", "sessions", "--json"]
codex_idle = "none"
claude_channel = false

[spawn]
claude_extra_args = ["--model", "opus"]
"#,
        )
        .expect("write");

        let cfg = Config::load(&paths).expect("load");
        assert_eq!(cfg.codex_idle, CodexIdle::None);
        assert!(!cfg.claude_channel);
        assert_eq!(cfg.spawn.claude_extra_args, vec!["--model", "opus"]);
        assert!(cfg.spawn.codex_extra_args.is_empty());
    }

    #[test]
    fn empty_provider_argv_disables_it() {
        let cfg = Config {
            session_provider: Some(vec![]),
            ..Config::default()
        };
        assert_eq!(cfg.provider_argv(), None);
    }
}
