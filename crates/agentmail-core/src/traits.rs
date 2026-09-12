//! The seams. agentmail-core knows nothing about herdr, MCP, or spawning agents;
//! adapters implement these and the binary wires them together.

use std::path::PathBuf;
use std::process::Command;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::domain::{Address, Harness, Message, SendMode, SessionCard, SessionList};
use crate::error::{Error, Result};
use crate::resolver::Resolved;

/// How busy an agent looks right now. Drives whether we push a prompt or queue.
/// Herdr's richer `agent_status` collapses into this: `done` is `Idle`, since for
/// delivery purposes a finished turn is a free one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    Idle,
    Working,
    Blocked,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    /// `None` when the multiplexer knows about an agent but not its session id.
    pub address: Option<Address>,
    pub alias: Option<String>,
    pub state: AgentState,
    pub cwd: Option<PathBuf>,
    pub title: Option<String>,
}

impl DirectoryEntry {
    pub fn new(state: AgentState) -> Self {
        DirectoryEntry {
            address: None,
            alias: None,
            state,
            cwd: None,
            title: None,
        }
    }
}

/// Agents a terminal multiplexer can see right now (the herdr adapter implements this).
pub trait Directory: Send + Sync {
    fn live_agents(&self) -> Vec<DirectoryEntry>;
}

/// Sessions agentmail itself never saw — old transcripts, other projects.
pub trait SessionProvider: Send + Sync {
    fn search(
        &self,
        query: &str,
        harness: Option<&Harness>,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SessionCard>>;

    fn recent(&self, limit: usize) -> Result<Vec<SessionCard>>;

    fn transcript(&self, address: &Address) -> Result<Option<String>>;
}

/// Everything a deliverer needs to decide what to do. Passed by reference so adding
/// context later does not churn every implementation.
#[derive(Debug, Clone, Copy)]
pub struct DeliveryRequest<'a> {
    pub message: &'a Message,
    pub resolved: &'a Resolved,
    pub mode: SendMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// The recipient has the message now.
    Pushed,
    /// Left in the store; a hook or the recipient's own MCP process will drain it.
    Queued,
    /// A new session was started and owns the message.
    Spawned(Address),
    /// A one-shot peer answered inline (ask mode). `from` is the new session's address
    /// when the deliverer knows one — a headless run that leaves no resumable session
    /// leaves it `None` rather than inventing an address.
    Replied {
        reply: String,
        from: Option<Address>,
    },
    /// This deliverer cannot handle the target; the next one is tried.
    Failed(String),
}

#[async_trait]
pub trait Deliverer: Send + Sync {
    async fn deliver(&self, req: &DeliveryRequest<'_>) -> DeliveryOutcome;
}

/// Liveness check for registration pids, behind a trait so tests can lie about it.
pub trait ProcessProbe: Send + Sync {
    fn is_alive(&self, pid: u32) -> bool;
}

#[derive(Debug, Default)]
pub struct SysinfoProbe;

impl ProcessProbe for SysinfoProbe {
    fn is_alive(&self, pid: u32) -> bool {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

        let pid = Pid::from_u32(pid);
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        sys.process(pid).is_some()
    }
}

/// Treats every pid as alive. Useful where liveness is enforced elsewhere.
#[derive(Debug, Default)]
pub struct AlwaysAliveProbe;

impl ProcessProbe for AlwaysAliveProbe {
    fn is_alive(&self, _pid: u32) -> bool {
        true
    }
}

pub const DEFAULT_PROVIDER_ARGV: [&str; 3] = ["hsm", "sessions", "--json"];

/// Shells out to a configured argv and parses `{"sessions":[...]}` from stdout.
/// Any failure is `ProviderUnavailable`: agentmail keeps working off registry + directory.
#[derive(Debug, Clone)]
pub struct CommandSessionProvider {
    argv: Vec<String>,
}

impl Default for CommandSessionProvider {
    fn default() -> Self {
        CommandSessionProvider::new(default_argv())
    }
}

pub fn default_argv() -> Vec<String> {
    DEFAULT_PROVIDER_ARGV
        .iter()
        .map(|s| s.to_string())
        .collect()
}

impl CommandSessionProvider {
    pub fn new(argv: Vec<String>) -> Self {
        CommandSessionProvider { argv }
    }

    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    fn run(&self, extra: &[String]) -> Result<Vec<SessionCard>> {
        let (exe, base) = self
            .argv
            .split_first()
            .ok_or_else(|| Error::provider("empty provider argv"))?;

        let out = Command::new(exe)
            .args(base)
            .args(extra)
            .output()
            .map_err(|e| Error::provider(format!("{exe}: {e}")))?;

        if !out.status.success() {
            return Err(Error::provider(format!(
                "{exe} exited with {}",
                out.status.code().unwrap_or(-1)
            )));
        }

        let list: SessionList = serde_json::from_slice(&out.stdout)
            .map_err(|e| Error::provider(format!("{exe}: malformed json: {e}")))?;
        Ok(list.sessions)
    }
}

impl SessionProvider for CommandSessionProvider {
    fn search(
        &self,
        query: &str,
        harness: Option<&Harness>,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SessionCard>> {
        let mut args = vec![
            "--query".to_string(),
            query.to_string(),
            "--limit".to_string(),
            limit.to_string(),
        ];
        if let Some(h) = harness {
            args.push("--harness".to_string());
            args.push(h.to_string());
        }
        if let Some(p) = project {
            args.push("--project".to_string());
            args.push(p.to_string());
        }
        self.run(&args)
    }

    fn recent(&self, limit: usize) -> Result<Vec<SessionCard>> {
        self.run(&["--limit".to_string(), limit.to_string()])
    }

    fn transcript(&self, address: &Address) -> Result<Option<String>> {
        let cards = self.search(&address.id, Some(&address.harness), None, 5)?;
        let Some(card) = cards
            .iter()
            .find(|c| c.parsed_address().as_ref() == Some(address))
        else {
            return Ok(None);
        };
        match card.transcript_path.as_deref() {
            Some(path) => Ok(std::fs::read_to_string(path).ok()),
            None => Ok(None),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(script: &str) -> CommandSessionProvider {
        CommandSessionProvider::new(vec!["sh".into(), "-c".into(), script.into(), "sh".into()])
    }

    #[test]
    fn parses_the_provider_contract() {
        let p = sh(
            r#"printf '{"sessions":[{"address":"claude:8890a685","harness":"claude","project":"trade-help","title":"API authentication","resumable":true}]}'"#,
        );
        let cards = p
            .search("auth", Some(&Harness::Claude), Some("trade-help"), 5)
            .expect("search");
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].title.as_deref(), Some("API authentication"));
        assert!(cards[0].resumable);
        assert_eq!(
            cards[0].parsed_address(),
            Some(Address::new(Harness::Claude, "8890a685"))
        );
    }

    #[test]
    fn passes_the_documented_args() {
        // `$@` echoes the appended args so the contract is visible in the assertion.
        let p = sh(
            r#"printf '{"sessions":[{"address":"claude:x","harness":"claude","title":"'"$*"'"}]}'"#,
        );
        let cards = p
            .search("auth", Some(&Harness::Codex), Some("proj"), 3)
            .expect("search");
        assert_eq!(
            cards[0].title.as_deref(),
            Some("--query auth --limit 3 --harness codex --project proj")
        );
    }

    #[test]
    fn non_zero_exit_is_unavailable() {
        let p = sh("exit 2");
        assert!(matches!(p.recent(5), Err(Error::ProviderUnavailable(_))));
    }

    #[test]
    fn malformed_output_is_unavailable() {
        let p = sh("printf 'not json'");
        assert!(matches!(
            p.search("x", None, None, 5),
            Err(Error::ProviderUnavailable(_))
        ));
    }

    #[test]
    fn a_missing_binary_is_unavailable() {
        let p = CommandSessionProvider::new(vec!["agentmail-no-such-binary".into()]);
        assert!(matches!(p.recent(5), Err(Error::ProviderUnavailable(_))));
    }

    #[test]
    fn default_argv_is_the_hsm_provider() {
        assert_eq!(
            CommandSessionProvider::default().argv(),
            ["hsm", "sessions", "--json"]
        );
    }
}
