//! Claude Code's registry of running processes, `~/.claude/sessions/<pid>.json`,
//! one file per process.
//!
//! Observed shape (2026-09, cli 2.1.x):
//! ```json
//! {"pid":57845,"sessionId":"68cf2c3c-98cf-4dc9-9503-5c09fec06dc9","cwd":"/abs",
//!  "kind":"bg","status":"busy","name":"herdr search session linking",
//!  "jobId":"68cf2c3c","procStart":"Sat Sep 12 18:13:40 2026"}
//! ```
//! This is how a background job (`claude --bg`, `/jobs`) or a Claude running in
//! a terminal outside herdr becomes visible: it has a session of its own but
//! never gets a pane. Files outlive their process, so an entry counts only
//! while its pid is alive. No other harness keeps such a registry.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::domain::{ProcessKind, ProcessRef};

/// One live Claude process, keyed by the session it is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningRef {
    pub session_id: String,
    pub cwd: PathBuf,
    pub process: ProcessRef,
}

/// Everything in the registry, minus the fields we do not read. Lenient on
/// purpose: a refresh must not fail because one entry is from a newer cli.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    #[serde(default)]
    pid: u32,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// A pre-warmed worker with no session of its own.
    #[serde(default)]
    spare: bool,
}

pub fn registry_dir() -> Option<PathBuf> {
    crate::harness::registry::claude_home().map(|h| h.join("sessions"))
}

/// Every process in `dir` that is still alive. A missing directory means no
/// Claude has ever run here, which is not an error.
pub fn running(dir: &Path) -> Vec<RunningRef> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(e) = serde_json::from_str::<Entry>(&text) else {
            continue;
        };
        if e.spare || e.session_id.is_empty() || !pid_alive(e.pid) {
            continue;
        }
        out.push(RunningRef {
            session_id: e.session_id,
            cwd: PathBuf::from(e.cwd),
            process: ProcessRef {
                pid: e.pid,
                kind: match e.kind.as_str() {
                    "bg" => ProcessKind::Job,
                    _ => ProcessKind::Interactive,
                },
                status: e.status,
                name: e.name,
                // Filled by the refresh, which is the only pass that sees the
                // herdr panes alongside the registry.
                pane_id: None,
            },
        });
    }
    out
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // kill(2) reads a pid of 0 or less as "a whole process group", and -1 as
    // "everything I may signal", which would answer yes for a junk pid. Only a
    // positive pid asks about one process.
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // Signal 0 only checks: it delivers nothing. EPERM means the process is
    // there but owned by someone else, which still counts as alive.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: plain Win32 calls; the handle is closed on every path that opened one.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            // Denied means the process is there but belongs to someone else, which
            // still counts as alive, like EPERM on unix.
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        // An exited process stays openable while anyone holds a handle to it, so the
        // open alone does not prove it is running.
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        ok && code == STILL_ACTIVE as u32
    }
}

/// Neither unix nor Windows: keeping a stale row is milder than dropping every
/// running one.
#[cfg(not(any(unix, windows)))]
fn pid_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).expect("write");
    }

    #[test]
    fn reads_live_processes_and_skips_the_rest() {
        let dir = tempfile::tempdir().expect("tmp");
        let alive = std::process::id();
        write(
            dir.path(),
            &format!("{alive}.json"),
            &format!(
                r#"{{"pid":{alive},"sessionId":"68cf2c3c-98cf","cwd":"/Users/x/proj",
                    "kind":"bg","status":"busy","name":"session linking",
                    "jobId":"68cf2c3c","procStart":"Sat Sep 12 18:13:40 2026"}}"#
            ),
        );
        // A pid that cannot exist: kill(2) answers ESRCH.
        let dead = i32::MAX as u32;
        write(
            dir.path(),
            &format!("{dead}.json"),
            &format!(r#"{{"pid":{dead},"sessionId":"long-gone","kind":"interactive"}}"#),
        );
        write(
            dir.path(),
            "spare.json",
            &format!(r#"{{"pid":{alive},"sessionId":"warm-worker","spare":true}}"#),
        );
        write(dir.path(), "garbage.json", "not json at all");
        write(dir.path(), "notes.txt", "ignored");

        let got = running(dir.path());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].session_id, "68cf2c3c-98cf");
        assert_eq!(got[0].cwd, PathBuf::from("/Users/x/proj"));
        assert_eq!(got[0].process.pid, alive);
        assert_eq!(got[0].process.kind, ProcessKind::Job);
        assert_eq!(got[0].process.status.as_deref(), Some("busy"));
        assert_eq!(got[0].process.name.as_deref(), Some("session linking"));
    }

    /// A pid that does not fit an `i32` would wrap negative, and `kill(-1, 0)`
    /// answers for every process the caller may signal — a yes for junk. Windows
    /// rejects the same pids as invalid, and 0 is its idle process, never openable.
    #[cfg(any(unix, windows))]
    #[test]
    fn a_pid_no_process_could_have_is_dead() {
        assert!(!pid_alive(u32::MAX), "-1 as i32");
        assert!(!pid_alive(u32::MAX - 1), "-2 as i32");
        assert!(!pid_alive(0), "the caller's own process group");
        assert!(pid_alive(std::process::id()), "this test is running");
    }

    #[test]
    fn an_interactive_entry_keeps_its_kind() {
        let dir = tempfile::tempdir().expect("tmp");
        let alive = std::process::id();
        write(
            dir.path(),
            &format!("{alive}.json"),
            &format!(r#"{{"pid":{alive},"sessionId":"s1","kind":"interactive","status":"idle"}}"#),
        );
        let got = running(dir.path());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].process.kind, ProcessKind::Interactive);
    }

    #[test]
    fn a_missing_directory_is_empty_not_an_error() {
        let dir = tempfile::tempdir().expect("tmp");
        assert!(running(&dir.path().join("nope")).is_empty());
    }
}
