//! State directory layout. Everything agentmail owns lives under one root so the
//! whole product can be wiped with a single `rm -rf`.

use std::path::{Path, PathBuf};

use crate::domain::Harness;
use crate::error::{Error, Result};

pub const STATE_DIR_ENV: &str = "AGENTMAIL_STATE_DIR";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Paths { root: root.into() }
    }

    pub fn from_env() -> Result<Self> {
        Ok(Paths::new(state_dir()?))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn db(&self) -> PathBuf {
        self.root.join("agentmail.sqlite")
    }

    pub fn config(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    pub fn poke_dir(&self) -> PathBuf {
        self.root.join("poke")
    }

    pub fn poke_socket(&self, harness: &Harness, session_id: &str) -> PathBuf {
        self.poke_dir()
            .join(format!("{}-{}.sock", harness, sanitize(session_id)))
    }

    /// Create the root (and `poke/`) if missing.
    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::create_dir_all(self.poke_dir())?;
        Ok(())
    }
}

/// `AGENTMAIL_STATE_DIR` → `$XDG_STATE_HOME/agentmail` → `~/.local/state/agentmail`
/// (`%LOCALAPPDATA%\agentmail` on Windows).
pub fn state_dir() -> Result<PathBuf> {
    if let Some(dir) = non_empty_env(STATE_DIR_ENV) {
        return Ok(PathBuf::from(dir));
    }

    #[cfg(windows)]
    {
        if let Some(dir) = dirs::data_local_dir() {
            return Ok(dir.join("agentmail"));
        }
    }

    if let Some(dir) = non_empty_env("XDG_STATE_HOME") {
        return Ok(PathBuf::from(dir).join("agentmail"));
    }

    let home = dirs::home_dir()
        .ok_or_else(|| Error::not_found("no home directory; set AGENTMAIL_STATE_DIR"))?;
    Ok(home.join(".local").join("state").join("agentmail"))
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

/// Windows named pipe leaf, used where there is no socket file to place.
pub fn poke_name(harness: &Harness, session_id: &str) -> String {
    format!("agentmail-{}-{}", harness, sanitize(session_id))
}

/// Session ids are harness-controlled; keep them usable as a filename component.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_hangs_off_the_root() {
        let p = Paths::new("/tmp/am");
        assert_eq!(p.db(), PathBuf::from("/tmp/am/agentmail.sqlite"));
        assert_eq!(p.config(), PathBuf::from("/tmp/am/config.toml"));
        assert_eq!(
            p.poke_socket(&Harness::Claude, "88/90:a6"),
            PathBuf::from("/tmp/am/poke/claude-88_90_a6.sock")
        );
    }
}
