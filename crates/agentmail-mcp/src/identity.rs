//! Working out which session this MCP process belongs to.
//!
//! An MCP server is started by the harness with no arguments, so identity has to be
//! reconstructed from evidence — and the evidence is not equally good. Environment
//! variables are the weakest of it: a herdr pane split, or any shell started from an
//! agent, inherits `CLAUDECODE=1` and `CLAUDE_CODE_SESSION_ID` from whatever ran there
//! before, so a Codex session can be sitting in an environment that loudly claims to be
//! Claude. That is a real bug we shipped once; the order below exists to prevent it.
//!
//! Strongest to weakest:
//!
//! 1. `AGENTMAIL_HARNESS` / `AGENTMAIL_SESSION_ID` — someone stated it outright.
//! 2. The process tree. Our parent (or grandparent, since a harness may go through a
//!    shell) is the harness binary itself: `codex` means Codex, whatever the
//!    environment says. This decides the harness and nothing overrides it.
//! 3. herdr, when `HERDR_PANE_ID` is set: it knows which agent owns that pane and which
//!    session it is running. Exact when available, absent when the socket is not.
//! 4. The harness's own session variable — `CLAUDE_CODE_SESSION_ID` for Claude,
//!    `CODEX_THREAD_ID` for Codex — but only the one belonging to the harness decided
//!    above. A `CLAUDE_CODE_SESSION_ID` inside a Codex process is somebody else's id.
//! 5. The registry row a SessionStart hook wrote for this very pane (`HERDR_PANE_ID`),
//!    then the newest unclaimed hook row for this directory from the last two minutes.
//! 6. A provisional `unknown-<pid>` row, so the process still works and still shows up
//!    in `doctor`, just not addressably. It is retried later: see [`Identity::reresolve`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agentmail_core::{Address, Harness, Registration, Store};
use chrono::Utc;

pub const HARNESS_ENV: &str = "AGENTMAIL_HARNESS";
pub const SESSION_ENV: &str = "AGENTMAIL_SESSION_ID";
pub const HERDR_PANE_ENV: &str = "HERDR_PANE_ID";

/// How recent a hook row has to be before we are willing to believe it is us. Long
/// enough for a slow harness start, short enough that yesterday's session in the same
/// directory is never mistaken for this one.
pub const FRESH_ROW: Duration = Duration::from_secs(120);

/// How far up the process tree to look for a harness binary. The harness itself, or the
/// harness behind the shell it used to start us.
const PARENT_DEPTH: usize = 3;

/// Everything known about this process before the registry is consulted. Built from the
/// real world by [`Evidence::from_process`]; assembled by hand in tests.
#[derive(Debug, Clone, Default)]
pub struct Evidence {
    pub env: HashMap<String, String>,
    pub cwd: PathBuf,
    pub pid: u32,
    /// The harness proved by the process tree. Authoritative.
    pub parent_harness: Option<Harness>,
    /// What herdr says is running in our pane.
    pub herdr: Option<Address>,
}

impl Evidence {
    pub fn from_process() -> Evidence {
        let pid = std::process::id();
        Evidence {
            env: std::env::vars().collect(),
            cwd: std::env::current_dir().unwrap_or_default(),
            pid,
            parent_harness: parent_harness(pid),
            herdr: None,
        }
    }

    pub fn new(env: HashMap<String, String>, cwd: impl Into<PathBuf>, pid: u32) -> Evidence {
        Evidence {
            env,
            cwd: cwd.into(),
            pid,
            parent_harness: None,
            herdr: None,
        }
    }

    pub fn with_parent_harness(mut self, harness: Option<Harness>) -> Self {
        self.parent_harness = harness;
        self
    }

    /// The address herdr reports for our pane, if any.
    pub fn with_herdr(mut self, address: Option<Address>) -> Self {
        self.herdr = address;
        self
    }

    pub fn herdr_pane(&self) -> Option<String> {
        get(&self.env, HERDR_PANE_ENV)
    }

    /// Claude is the default only because it is the harness most likely to be running an
    /// MCP server at all; by then nothing has identified the session anyway.
    pub fn harness(&self) -> Harness {
        get(&self.env, HARNESS_ENV)
            .and_then(|raw| raw.parse::<Harness>().ok())
            .or_else(|| self.parent_harness.clone())
            .or_else(|| self.herdr.as_ref().map(|a| a.harness.clone()))
            .or_else(|| env_harness_guess(&self.env))
            .unwrap_or(Harness::Claude)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub harness: Harness,
    pub session_id: String,
    pub cwd: PathBuf,
    pub pid: u32,
    pub herdr_pane: Option<String>,
    /// True when nothing identified the session and the id was invented.
    pub provisional: bool,
}

impl Identity {
    pub fn detect(store: &Store) -> Identity {
        Identity::from_evidence(&Evidence::from_process(), store)
    }

    /// Environment-only detection, with no process tree and no herdr behind it.
    pub fn from_env(
        env: &HashMap<String, String>,
        cwd: &Path,
        pid: u32,
        store: &Store,
    ) -> Identity {
        Identity::from_evidence(&Evidence::new(env.clone(), cwd, pid), store)
    }

    pub fn from_evidence(ev: &Evidence, store: &Store) -> Identity {
        let harness = ev.harness();
        let herdr_pane = ev.herdr_pane();
        let settled = |session_id: String| Identity {
            harness: harness.clone(),
            session_id,
            cwd: ev.cwd.clone(),
            pid: ev.pid,
            herdr_pane: herdr_pane.clone(),
            provisional: false,
        };

        if let Some(id) = get(&ev.env, SESSION_ENV) {
            return settled(id);
        }

        // herdr watched this pane launch; it knows the session ref the harness reported.
        if let Some(addr) = ev.herdr.as_ref().filter(|a| a.harness == harness) {
            return settled(addr.id.clone());
        }

        // Only the variable belonging to the harness we settled on. A stale
        // CLAUDE_CODE_SESSION_ID inherited into a Codex pane is not our id.
        if let Some(id) = harness_session_id(&ev.env, &harness) {
            return settled(id);
        }

        if let Some(reg) = registry_match(store, &harness, herdr_pane.as_deref(), &ev.cwd) {
            return settled(reg.session_id);
        }

        Identity {
            harness,
            session_id: format!("unknown-{}", ev.pid),
            cwd: ev.cwd.clone(),
            pid: ev.pid,
            herdr_pane,
            provisional: true,
        }
    }

    /// Retry the registry lookup for a provisional identity. The SessionStart hook may
    /// simply not have run yet when this process started, and a session that becomes
    /// addressable a second later should not stay anonymous for its whole life.
    pub fn reresolve(&self, store: &Store) -> Option<Identity> {
        if !self.provisional {
            return None;
        }
        let reg = registry_match(store, &self.harness, self.herdr_pane.as_deref(), &self.cwd)?;
        Some(Identity {
            harness: self.harness.clone(),
            session_id: reg.session_id,
            cwd: self.cwd.clone(),
            pid: self.pid,
            herdr_pane: self.herdr_pane.clone(),
            provisional: false,
        })
    }

    pub fn address(&self) -> Address {
        Address::new(self.harness.clone(), self.session_id.clone())
    }

    pub fn registration(&self, poke_path: Option<String>) -> Registration {
        let mut reg = Registration::new(self.harness.clone(), &self.session_id, &self.cwd);
        reg.pid = Some(self.pid);
        reg.poke_path = poke_path;
        reg.herdr_pane = self.herdr_pane.clone();
        reg
    }
}

fn get(env: &HashMap<String, String>, key: &str) -> Option<String> {
    env.get(key)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The weakest signal there is: these variables survive into every child shell and every
/// pane split, so they only ever break a tie nothing else could.
pub fn env_harness_guess(env: &HashMap<String, String>) -> Option<Harness> {
    if env.keys().any(|k| k.starts_with("CODEX_")) {
        return Some(Harness::Codex);
    }
    if env
        .keys()
        .any(|k| k == "CLAUDECODE" || k.starts_with("CLAUDE_CODE_"))
    {
        return Some(Harness::Claude);
    }
    None
}

/// Kept for callers that only have an environment. Prefers an explicit
/// `AGENTMAIL_HARNESS` over the inherited-variable guess.
pub fn harness_from_env(env: &HashMap<String, String>) -> Option<Harness> {
    get(env, HARNESS_ENV)
        .and_then(|raw| raw.parse::<Harness>().ok())
        .or_else(|| env_harness_guess(env))
}

fn harness_session_id(env: &HashMap<String, String>, harness: &Harness) -> Option<String> {
    match harness {
        Harness::Claude => get(env, "CLAUDE_CODE_SESSION_ID"),
        Harness::Codex => get(env, "CODEX_THREAD_ID"),
        Harness::Other(_) => None,
    }
}

/// Walks up from `pid` looking for the harness that started us. A harness may put a
/// shell in between, so this climbs a few generations before giving up.
pub fn parent_harness(pid: u32) -> Option<Harness> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::Always),
    );

    let mut current = Pid::from_u32(pid);
    for _ in 0..PARENT_DEPTH {
        let parent = sys.process(current)?.parent()?;
        let process = sys.process(parent)?;
        let name = process.name().to_string_lossy().into_owned();
        let exe = process
            .exe()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned());
        if let Some(harness) = harness_of_process_name(&name)
            .or_else(|| exe.as_deref().and_then(harness_of_process_name))
        {
            return Some(harness);
        }
        current = parent;
    }
    None
}

/// `claude`, `claude-code`, `codex`, `codex.exe` — the harness binaries as they appear
/// in a process table. Anything else (a shell, a terminal, launchd) keeps the walk going.
pub fn harness_of_process_name(name: &str) -> Option<Harness> {
    let name = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim_end_matches(".exe")
        .to_ascii_lowercase();
    if name == "codex" || name.starts_with("codex-") {
        return Some(Harness::Codex);
    }
    if name == "claude" || name.starts_with("claude-") {
        return Some(Harness::Claude);
    }
    None
}

/// A hook row that could be this session: same pane if herdr told us one, otherwise the
/// freshest unclaimed row for this directory.
///
/// Rows that already carry a pid belong to another MCP process that identified itself,
/// so they are never taken over.
fn registry_match(
    store: &Store,
    harness: &Harness,
    herdr_pane: Option<&str>,
    cwd: &Path,
) -> Option<Registration> {
    let rows: Vec<Registration> = store
        .live()
        .ok()?
        .into_iter()
        .filter(|r| &r.harness == harness && r.pid.is_none())
        .collect();

    if let Some(pane) = herdr_pane {
        let mut in_pane: Vec<Registration> = rows
            .iter()
            .filter(|r| r.herdr_pane.as_deref() == Some(pane))
            .cloned()
            .collect();
        in_pane.sort_by_key(|r| std::cmp::Reverse(r.started_at));
        if let Some(reg) = in_pane.into_iter().next() {
            return Some(reg);
        }
    }

    let cutoff = Utc::now() - chrono::Duration::from_std(FRESH_ROW).unwrap_or_default();
    let mut fresh: Vec<Registration> = rows
        .into_iter()
        .filter(|r| r.cwd == cwd && r.started_at >= cutoff)
        .collect();
    fresh.sort_by_key(|r| std::cmp::Reverse(r.started_at));
    fresh.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn explicit_env_wins() {
        let store = Store::open_in_memory().expect("store");
        let id = Identity::from_env(
            &env(&[
                (HARNESS_ENV, "codex"),
                (SESSION_ENV, "01999b0e"),
                ("CLAUDECODE", "1"),
            ]),
            Path::new("/repo"),
            42,
            &store,
        );
        assert_eq!(id.harness, Harness::Codex);
        assert_eq!(id.session_id, "01999b0e");
        assert!(!id.provisional);
    }

    #[test]
    fn harness_variables_identify_the_session() {
        let store = Store::open_in_memory().expect("store");
        let claude = Identity::from_env(
            &env(&[
                ("CLAUDECODE", "1"),
                ("CLAUDE_CODE_SESSION_ID", "8890a685-1111"),
            ]),
            Path::new("/repo"),
            7,
            &store,
        );
        assert_eq!(claude.harness, Harness::Claude);
        assert_eq!(claude.session_id, "8890a685-1111");

        let codex = Identity::from_env(
            &env(&[("CODEX_HOME", "/h/.codex"), ("CODEX_THREAD_ID", "01999b0e")]),
            Path::new("/repo"),
            7,
            &store,
        );
        assert_eq!(codex.harness, Harness::Codex);
        assert_eq!(codex.session_id, "01999b0e");
    }

    /// The bug this order exists for: a Codex pane split from a Claude session still
    /// carries `CLAUDECODE=1` and `CLAUDE_CODE_SESSION_ID`, and once upon a time we
    /// answered mail as that Claude session.
    #[test]
    fn a_codex_process_never_adopts_an_inherited_claude_id() {
        let store = Store::open_in_memory().expect("store");
        store
            .register(&Registration::new(Harness::Codex, "01a09638", "/repo"))
            .expect("register");

        let stale = env(&[
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_SESSION_ID", "2c36e684-not-us"),
        ]);
        let ev = Evidence::new(stale, "/repo", 9).with_parent_harness(Some(Harness::Codex));

        let id = Identity::from_evidence(&ev, &store);
        assert_eq!(
            id.harness,
            Harness::Codex,
            "the process tree, not the environment"
        );
        assert_eq!(id.session_id, "01a09638");
    }

    #[test]
    fn without_a_codex_row_it_stays_anonymous_rather_than_wrong() {
        let store = Store::open_in_memory().expect("store");
        let stale = env(&[
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_SESSION_ID", "2c36e684-not-us"),
        ]);
        let ev = Evidence::new(stale, "/repo", 9).with_parent_harness(Some(Harness::Codex));

        let id = Identity::from_evidence(&ev, &store);
        assert_eq!(id.harness, Harness::Codex);
        assert!(id.provisional);
        assert_ne!(id.session_id, "2c36e684-not-us");
    }

    #[test]
    fn herdr_identifies_the_pane_when_the_environment_lies() {
        let store = Store::open_in_memory().expect("store");
        let stale = env(&[
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_SESSION_ID", "2c36e684-not-us"),
            (HERDR_PANE_ENV, "w9:p4"),
        ]);
        let ev = Evidence::new(stale, "/repo", 9)
            .with_herdr(Some(Address::new(Harness::Codex, "01a09638")));

        let id = Identity::from_evidence(&ev, &store);
        assert_eq!(id.harness, Harness::Codex);
        assert_eq!(id.session_id, "01a09638");
        assert_eq!(id.herdr_pane.as_deref(), Some("w9:p4"));
    }

    #[test]
    fn the_process_tree_outranks_herdr() {
        let store = Store::open_in_memory().expect("store");
        let ev = Evidence::new(env(&[]), "/repo", 9)
            .with_parent_harness(Some(Harness::Claude))
            // A pane herdr still believes belongs to the codex session that ran there
            // before: the session ref is not ours, so it is dropped, not adopted.
            .with_herdr(Some(Address::new(Harness::Codex, "01a09638")));

        let id = Identity::from_evidence(&ev, &store);
        assert_eq!(id.harness, Harness::Claude);
        assert!(id.provisional);
    }

    #[test]
    fn harness_binaries_are_recognised_in_a_process_table() {
        assert_eq!(harness_of_process_name("codex"), Some(Harness::Codex));
        assert_eq!(harness_of_process_name("Codex.exe"), Some(Harness::Codex));
        assert_eq!(harness_of_process_name("claude"), Some(Harness::Claude));
        assert_eq!(
            harness_of_process_name("claude-code"),
            Some(Harness::Claude)
        );
        assert_eq!(harness_of_process_name("/usr/bin/zsh"), None);
        assert_eq!(harness_of_process_name("node"), None);
    }

    #[test]
    fn falls_back_to_the_freshest_hook_row_for_this_cwd() {
        let store = Store::open_in_memory().expect("store");
        store
            .register(&Registration::new(Harness::Claude, "older", "/repo"))
            .expect("register");
        let mut newer = Registration::new(Harness::Claude, "newer", "/repo");
        newer.started_at += chrono::Duration::seconds(5);
        store.register(&newer).expect("register");
        store
            .register(&Registration::new(Harness::Claude, "elsewhere", "/other"))
            .expect("register");

        let id = Identity::from_env(&env(&[("CLAUDECODE", "1")]), Path::new("/repo"), 9, &store);
        assert_eq!(id.session_id, "newer");
        assert!(!id.provisional);
    }

    #[test]
    fn the_pane_wins_over_the_directory() {
        let store = Store::open_in_memory().expect("store");
        let mut elsewhere = Registration::new(Harness::Claude, "in-my-pane", "/other");
        elsewhere.herdr_pane = Some("w1:p3".into());
        store.register(&elsewhere).expect("register");
        let mut same_dir = Registration::new(Harness::Claude, "same-dir", "/repo");
        same_dir.started_at += chrono::Duration::seconds(5);
        store.register(&same_dir).expect("register");

        let id = Identity::from_env(
            &env(&[("CLAUDECODE", "1"), (HERDR_PANE_ENV, "w1:p3")]),
            Path::new("/repo"),
            9,
            &store,
        );
        assert_eq!(id.session_id, "in-my-pane");
    }

    #[test]
    fn a_stale_row_is_not_mistaken_for_this_session() {
        let store = Store::open_in_memory().expect("store");
        let mut yesterday = Registration::new(Harness::Claude, "yesterday", "/repo");
        yesterday.started_at -= chrono::Duration::hours(20);
        store.register(&yesterday).expect("register");

        let id = Identity::from_env(&env(&[("CLAUDECODE", "1")]), Path::new("/repo"), 9, &store);
        assert!(id.provisional, "an old row belongs to somebody else");
    }

    #[test]
    fn a_row_another_process_claimed_is_left_alone() {
        let store = Store::open_in_memory().expect("store");
        let mut claimed = Registration::new(Harness::Claude, "claimed", "/repo");
        claimed.pid = Some(std::process::id());
        store.register(&claimed).expect("register");

        let id = Identity::from_env(&env(&[("CLAUDECODE", "1")]), Path::new("/repo"), 9, &store);
        assert!(id.provisional);
    }

    #[test]
    fn a_provisional_identity_adopts_the_hook_row_when_it_appears() {
        let store = Store::open_in_memory().expect("store");
        let provisional =
            Identity::from_env(&env(&[("CLAUDECODE", "1")]), Path::new("/repo"), 9, &store);
        assert!(provisional.provisional);
        assert_eq!(provisional.reresolve(&store), None);

        store
            .register(&Registration::new(Harness::Claude, "late-hook", "/repo"))
            .expect("register");
        let found = provisional.reresolve(&store).expect("resolved");
        assert_eq!(found.session_id, "late-hook");
        assert!(!found.provisional);
        assert_eq!(found.pid, provisional.pid);
        // Already resolved identities never re-resolve.
        assert_eq!(found.reresolve(&store), None);
    }

    #[test]
    fn an_unidentifiable_process_still_gets_an_address() {
        let store = Store::open_in_memory().expect("store");
        let id = Identity::from_env(&env(&[]), Path::new("/repo"), 1234, &store);
        assert_eq!(id.session_id, "unknown-1234");
        assert!(id.provisional);
        // Nothing said otherwise, so assume the harness most likely to run us.
        assert_eq!(id.harness, Harness::Claude);
    }
}
