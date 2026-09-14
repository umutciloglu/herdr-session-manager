use std::path::PathBuf;

/// One index for every entrypoint. herdr offers plugins their own state dir,
/// but `hsm` is also run outside the plugin environment (agentmail shells out
/// to `hsm sessions`), and two index files would drift apart. So the plugin
/// state dir is ignored on purpose; `HSM_STATE_DIR` is the only override.
pub fn state_dir() -> PathBuf {
    env_dir("HSM_STATE_DIR").unwrap_or_else(default_state_dir)
}

/// Same reasoning as [`state_dir`]: config is read from one place regardless
/// of who launched `hsm`.
pub fn config_dir() -> PathBuf {
    env_dir("HSM_CONFIG_DIR").unwrap_or_else(default_config_dir)
}

pub fn index_path() -> PathBuf {
    state_dir().join("index.sqlite")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// herdr's persisted pane -> session refs. herdr keeps `session.json` in its config
/// dir, or in `sessions/<name>` under it for a named session.
pub fn herdr_session_file() -> Option<PathBuf> {
    if let Some(p) = env_dir("HERDR_SESSION_FILE") {
        return Some(p);
    }
    let dir = herdr_config_dir()?;
    // herdr treats a session named "default" as the unnamed one.
    let dir = match std::env::var("HERDR_SESSION") {
        Ok(name) if !name.is_empty() && name != "default" => dir.join("sessions").join(name),
        _ => dir,
    };
    Some(dir.join("session.json"))
}

/// Mirrors herdr's `config::config_dir`, as `herdr_client::config_dir` does; hsm-core
/// stays free of the client crate for one path rule. `XDG_CONFIG_HOME` wins on every
/// platform, Windows included.
fn herdr_config_dir() -> Option<PathBuf> {
    if let Some(xdg) = env_dir("XDG_CONFIG_HOME") {
        return Some(xdg.join("herdr"));
    }
    #[cfg(windows)]
    {
        env_dir("APPDATA")
            .or_else(dirs::config_dir)
            .map(|dir| dir.join("herdr"))
    }
    #[cfg(not(windows))]
    {
        dirs::home_dir().map(|h| h.join(".config").join("herdr"))
    }
}

fn env_dir(key: &str) -> Option<PathBuf> {
    match std::env::var_os(key) {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => None,
    }
}

#[cfg(windows)]
fn default_state_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hsm")
}

#[cfg(not(windows))]
fn default_state_dir() -> PathBuf {
    match dirs::home_dir() {
        Some(h) => h.join(".local/state/hsm"),
        None => PathBuf::from(".hsm"),
    }
}

#[cfg(windows)]
fn default_config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hsm")
}

#[cfg(not(windows))]
fn default_config_dir() -> PathBuf {
    match dirs::home_dir() {
        Some(h) => h.join(".config/hsm"),
        None => PathBuf::from(".hsm"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_lives_under_state_dir() {
        assert_eq!(index_path().parent(), Some(state_dir().as_path()));
    }
}
