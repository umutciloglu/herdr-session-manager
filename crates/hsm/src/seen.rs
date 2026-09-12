//! Which replies this machine has already put in front of the human.
//!
//! agentmail has no read state we may set (`inbox` takes no `--mark-read`), so
//! the popup keeps its own list. It only drives the unread count; losing the
//! file costs nothing but a re-shown reply.

use std::collections::HashSet;
use std::path::PathBuf;

use hsm_core::paths;

/// Plenty for a count that only matters between two popups, and small enough
/// that the file stays a file.
const KEEP: usize = 500;

fn path() -> PathBuf {
    paths::state_dir().join("seen_replies")
}

pub fn load() -> HashSet<String> {
    let Ok(text) = std::fs::read_to_string(path()) else {
        return HashSet::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Appends `ids`, keeping the newest `KEEP`. Errors are the caller's to ignore:
/// a reply shown twice is a much smaller problem than a popup that will not
/// open.
pub fn add(ids: &[String]) -> std::io::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let path = path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let mut kept: Vec<String> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    for id in ids {
        if !kept.iter().any(|k| k == id) {
            kept.push(id.clone());
        }
    }
    if kept.len() > KEEP {
        kept.drain(..kept.len() - KEEP);
    }
    kept.push(String::new());
    std::fs::write(&path, kept.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_survive_a_round_trip_and_stay_unique() {
        let dir = tempfile::tempdir().expect("tmp");
        // `paths::state_dir` reads the env, so this test owns it.
        temp_env(dir.path(), || {
            assert!(load().is_empty());
            add(&["01A".to_string(), "01B".to_string()]).expect("add");
            add(&["01B".to_string(), "01C".to_string()]).expect("add again");

            let seen = load();
            assert_eq!(seen.len(), 3);
            assert!(seen.contains("01A") && seen.contains("01B") && seen.contains("01C"));

            let raw = std::fs::read_to_string(dir.path().join("seen_replies")).expect("read");
            assert_eq!(raw.matches("01B").count(), 1, "no duplicates");
        });
    }

    #[test]
    fn the_list_is_capped() {
        let dir = tempfile::tempdir().expect("tmp");
        temp_env(dir.path(), || {
            let ids: Vec<String> = (0..KEEP + 25).map(|i| format!("{i:04}")).collect();
            add(&ids).expect("add");
            let seen = load();
            assert_eq!(seen.len(), KEEP);
            assert!(!seen.contains("0000"), "the oldest fell off");
            assert!(seen.contains(&format!("{:04}", KEEP + 24)));
        });
    }

    /// `HSM_STATE_DIR` is process-wide, so these two tests share one lock.
    fn temp_env(dir: &std::path::Path, body: impl FnOnce()) {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("HSM_STATE_DIR");
        std::env::set_var("HSM_STATE_DIR", dir);
        body();
        match previous {
            Some(v) => std::env::set_var("HSM_STATE_DIR", v),
            None => std::env::remove_var("HSM_STATE_DIR"),
        }
    }
}
