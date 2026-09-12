use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::domain::{HarnessKind, OpenTarget};
use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Sessions newer than this get their message text indexed for FTS.
    pub hot_days: u32,
    pub disabled_harnesses: Vec<HarnessKind>,
    /// Extra directories to scan for Claude-shaped transcripts.
    pub extra_transcript_roots: Vec<PathBuf>,
    pub default_open: OpenTarget,
    pub agentmail_bin: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hot_days: 30,
            disabled_harnesses: Vec::new(),
            extra_transcript_roots: Vec::new(),
            default_open: OpenTarget::default(),
            agentmail_bin: "agentmail".to_string(),
        }
    }
}

impl Config {
    /// A missing file means "all defaults" — the plugin must work the moment it
    /// is installed, without the user writing any config.
    pub fn load(path: &Path) -> Result<Config> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(Error::io(path, e)),
        };
        toml::from_str(&text).map_err(|source| Error::Config {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn is_disabled(&self, kind: &HarnessKind) -> bool {
        self.disabled_harnesses.contains(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SplitDirection;

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().expect("tmp");
        let c = Config::load(&dir.path().join("config.toml")).expect("load");
        assert_eq!(c, Config::default());
        assert_eq!(c.hot_days, 30);
        assert_eq!(
            c.default_open,
            OpenTarget::Split(SplitDirection::Horizontal)
        );
        assert_eq!(c.agentmail_bin, "agentmail");
    }

    #[test]
    fn partial_file_keeps_the_other_defaults() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("config.toml");
        std::fs::write(
            &p,
            "hot_days = 7\ndisabled_harnesses = [\"codex\", \"gemini\"]\ndefault_open = \"tab\"\n",
        )
        .expect("write");
        let c = Config::load(&p).expect("load");
        assert_eq!(c.hot_days, 7);
        assert_eq!(c.default_open, OpenTarget::Tab);
        assert!(c.is_disabled(&HarnessKind::Codex));
        assert!(!c.is_disabled(&HarnessKind::Claude));
        assert_eq!(c.agentmail_bin, "agentmail");
    }

    #[test]
    fn bad_open_target_is_reported() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "default_open = \"popup\"\n").expect("write");
        assert!(Config::load(&p).is_err());
    }
}
