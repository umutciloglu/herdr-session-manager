//! Reaching a session that is not running: headless one-shots, background sessions,
//! and (with the `herdr` feature) a fresh pane.

use std::path::PathBuf;
use std::sync::Arc;

use agentmail_core::{
    Address, Deliverer, DeliveryOutcome, DeliveryRequest, Envelope, Harness, Message, Resolved,
    SendMode, SpawnConfig, Store,
};
use async_trait::async_trait;

pub const HARNESS_ENV: &str = "AGENTMAIL_HARNESS";
pub const SESSION_ENV: &str = "AGENTMAIL_SESSION_ID";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

/// Every process launch goes through this so tests never shell out.
#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(&self, spec: &CommandSpec) -> std::io::Result<CommandOutput>;
}

#[derive(Debug, Default)]
pub struct TokioCommandRunner;

#[async_trait]
impl CommandRunner for TokioCommandRunner {
    async fn run(&self, spec: &CommandSpec) -> std::io::Result<CommandOutput> {
        let mut cmd = tokio::process::Command::new(&spec.program);
        cmd.args(&spec.args);
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        if let Some(cwd) = &spec.cwd {
            cmd.current_dir(cwd);
        }
        // stdin is closed: a headless harness run must never wait on a terminal.
        cmd.stdin(std::process::Stdio::null());
        let out = cmd.output().await?;
        Ok(CommandOutput {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// What `Spawner` hands to a pane adapter. Kept as a struct so the `herdr` feature can
/// grow the request without touching this module's signature.
pub struct PaneRequest<'a> {
    pub harness: &'a Harness,
    pub message: &'a Message,
    pub envelope: &'a str,
    /// Set when the target is a known-but-offline session that should be resumed.
    pub resume: Option<&'a Address>,
    pub extra_args: &'a [String],
}

#[async_trait]
pub trait PaneSpawner: Send + Sync {
    async fn spawn_pane(&self, req: PaneRequest<'_>) -> DeliveryOutcome;
}

/// Starts harness processes to deliver a message nobody is around to receive.
pub struct Spawner {
    runner: Arc<dyn CommandRunner>,
    store: Arc<Store>,
    spawn: SpawnConfig,
    pane: Option<Arc<dyn PaneSpawner>>,
}

impl Spawner {
    pub fn new(store: Arc<Store>, spawn: SpawnConfig) -> Self {
        Spawner {
            runner: Arc::new(TokioCommandRunner),
            store,
            spawn,
            pane: None,
        }
    }

    pub fn with_runner(mut self, runner: Arc<dyn CommandRunner>) -> Self {
        self.runner = runner;
        self
    }

    pub fn with_pane_spawner(mut self, pane: Arc<dyn PaneSpawner>) -> Self {
        self.pane = Some(pane);
        self
    }

    fn extra_args(&self, harness: &Harness) -> &[String] {
        match harness {
            Harness::Claude => &self.spawn.claude_extra_args,
            Harness::Codex => &self.spawn.codex_extra_args,
            Harness::Other(_) => &[],
        }
    }

    async fn ask(
        &self,
        harness: &Harness,
        message: &Message,
        envelope: &str,
        resume: Option<&Address>,
    ) -> DeliveryOutcome {
        match harness {
            Harness::Claude => self.ask_claude(envelope, resume).await,
            Harness::Codex => self.ask_codex(message, envelope, resume).await,
            Harness::Other(h) => DeliveryOutcome::Failed(format!("cannot spawn a {h} session")),
        }
    }

    async fn ask_claude(&self, envelope: &str, resume: Option<&Address>) -> DeliveryOutcome {
        let mut args = vec![
            "-p".to_string(),
            envelope.to_string(),
            "--output-format".to_string(),
            "json".to_string(),
        ];
        if let Some(addr) = resume {
            // --fork-session keeps the peer's own history intact: an inbound message is
            // not something the human's session should find in its transcript later.
            args.push("--resume".to_string());
            args.push(addr.id.clone());
            args.push("--fork-session".to_string());
        }
        args.extend(self.extra_args(&Harness::Claude).iter().cloned());

        let spec = CommandSpec {
            program: "claude".into(),
            args,
            // The forked session gets a new id, so there is no session id to announce.
            env: vec![(HARNESS_ENV.to_string(), "claude".to_string())],
            cwd: None,
        };
        let out = match self.runner.run(&spec).await {
            Ok(out) => out,
            Err(e) => return DeliveryOutcome::Failed(format!("claude -p: {e}")),
        };
        if !out.ok() {
            return DeliveryOutcome::Failed(format!(
                "claude -p exited with {}: {}",
                out.code,
                first_line(&out.stderr)
            ));
        }

        match serde_json::from_str::<serde_json::Value>(out.stdout.trim()) {
            Ok(v) => {
                let reply = v
                    .get("result")
                    .and_then(|r| r.as_str())
                    .unwrap_or_else(|| out.stdout.trim())
                    .to_string();
                let from = v
                    .get("session_id")
                    .and_then(|s| s.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| Address::new(Harness::Claude, s));
                DeliveryOutcome::Replied { reply, from }
            }
            // A harness that printed plain text still answered; report the answer
            // rather than failing a delivery that actually happened.
            Err(_) => DeliveryOutcome::Replied {
                reply: out.stdout.trim().to_string(),
                from: None,
            },
        }
    }

    /// `codex exec` (and `codex exec resume`) both take `--json` and
    /// `-o/--output-last-message <FILE>`, so the answer can be the model's final message
    /// alone instead of the whole run transcript, and the thread id can be read off the
    /// event stream — which is the only way a *fresh* codex session can report an
    /// address back.
    async fn ask_codex(
        &self,
        message: &Message,
        envelope: &str,
        resume: Option<&Address>,
    ) -> DeliveryOutcome {
        let last_message = std::env::temp_dir().join(format!("agentmail-{}.txt", message.id));

        let mut args = vec!["exec".to_string()];
        let mut env = vec![(HARNESS_ENV.to_string(), "codex".to_string())];
        if resume.is_some() {
            args.push("resume".to_string());
        }
        args.push("--json".to_string());
        args.push("--output-last-message".to_string());
        args.push(last_message.to_string_lossy().into_owned());
        args.extend(self.extra_args(&Harness::Codex).iter().cloned());
        if let Some(addr) = resume {
            // `codex exec resume <id> <prompt>` continues the same thread, so the
            // spawned process really is that session and may say so.
            args.push(addr.id.clone());
            env.push((SESSION_ENV.to_string(), addr.id.clone()));
        }
        args.push(envelope.to_string());

        let spec = CommandSpec {
            program: "codex".into(),
            args,
            env,
            cwd: None,
        };
        let out = match self.runner.run(&spec).await {
            Ok(out) => out,
            Err(e) => return DeliveryOutcome::Failed(format!("codex exec: {e}")),
        };

        // The file is the authoritative answer; the event stream and raw stdout are
        // fallbacks for a codex that wrote nothing there.
        let from_file = std::fs::read_to_string(&last_message)
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        let _ = std::fs::remove_file(&last_message);

        if !out.ok() {
            return DeliveryOutcome::Failed(format!(
                "codex exec exited with {}: {}",
                out.code,
                first_line(&out.stderr)
            ));
        }

        let reply = from_file
            .or_else(|| codex_last_message(&out.stdout))
            .unwrap_or_else(|| out.stdout.trim().to_string());
        let from = resume
            .cloned()
            .or_else(|| codex_thread_id(&out.stdout).map(|id| Address::new(Harness::Codex, id)));
        DeliveryOutcome::Replied { reply, from }
    }

    async fn background(
        &self,
        harness: &Harness,
        message: &Message,
        resume: Option<&Address>,
    ) -> DeliveryOutcome {
        match harness {
            Harness::Claude => self.background_claude(message, resume).await,
            // Codex has no detached mode; failing lets the chain (or an explicit
            // --mode ask) pick something that works.
            Harness::Codex => DeliveryOutcome::Failed("codex has no background mode".into()),
            Harness::Other(h) => {
                DeliveryOutcome::Failed(format!("{h} has no background mode we know of"))
            }
        }
    }

    async fn background_claude(
        &self,
        message: &Message,
        resume: Option<&Address>,
    ) -> DeliveryOutcome {
        let mut args = vec!["--bg".to_string()];
        let mut env = vec![(HARNESS_ENV.to_string(), "claude".to_string())];
        if let Some(addr) = resume {
            args.push("--resume".to_string());
            args.push(addr.id.clone());
            env.push((SESSION_ENV.to_string(), addr.id.clone()));
        }
        args.extend(self.extra_args(&Harness::Claude).iter().cloned());

        let spec = CommandSpec {
            program: "claude".into(),
            args,
            env,
            cwd: None,
        };
        let out = match self.runner.run(&spec).await {
            Ok(out) => out,
            Err(e) => return DeliveryOutcome::Failed(format!("claude --bg: {e}")),
        };
        if !out.ok() {
            return DeliveryOutcome::Failed(format!(
                "claude --bg exited with {}: {}",
                out.code,
                first_line(&out.stderr)
            ));
        }

        let Some(id) = session_id_in(&out.stdout).or_else(|| resume.map(|a| a.id.clone())) else {
            return DeliveryOutcome::Failed("claude --bg printed no session id".into());
        };

        // The row still says `claude:new`; re-point it at the session that was just
        // started, then leave it Pending. That new session's MCP process or Stop hook
        // is what finally hands the message to the model — which is also why this is
        // `Queued` and not `Spawned`: `Spawned` would mark the row delivered and the
        // drain would skip it.
        let addr = Address::new(Harness::Claude, id);
        if let Err(e) = self.store.retarget(&message.id, &addr) {
            return DeliveryOutcome::Failed(format!("retarget {}: {e}", message.id));
        }
        DeliveryOutcome::Queued
    }
}

/// `Auto` is the only mode a model ever needs to think about; this is where it turns
/// into something concrete. A message that wants an answer gets a one-shot run whose
/// reply comes back inline; anything else prefers a real session the peer can keep.
fn effective_mode(mode: SendMode, harness: &Harness, expects_reply: bool) -> SendMode {
    match mode {
        SendMode::Auto if expects_reply => SendMode::Ask,
        SendMode::Auto => match harness {
            Harness::Claude => SendMode::Background,
            _ => SendMode::Ask,
        },
        explicit => explicit,
    }
}

/// Claude prints the new session id on its own line; be liberal about what surrounds it.
fn session_id_in(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || c == '"' || c == ',')
        .map(|t| t.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-'))
        .find(|t| looks_like_session_id(t))
        .map(|t| t.to_string())
}

fn looks_like_session_id(s: &str) -> bool {
    let uuid = s.len() == 36
        && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        && s.split('-').map(str::len).eq([8, 4, 4, 4, 12]);
    let ulid = s.len() == 26 && s.chars().all(|c| c.is_ascii_alphanumeric());
    uuid || ulid
}

/// Codex's JSONL is versioned and we pin nothing about its shape: walk it and take the
/// last thing that looks like an agent message, whatever nests it.
fn codex_last_message(stdout: &str) -> Option<String> {
    let mut last = None;
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        walk(&value, &mut |obj| {
            let is_message = obj
                .get("type")
                .and_then(|t| t.as_str())
                .is_some_and(|t| t.ends_with("agent_message"));
            if !is_message {
                return;
            }
            if let Some(text) = ["text", "message", "content"]
                .iter()
                .find_map(|k| obj.get(*k).and_then(|v| v.as_str()))
            {
                last = Some(text.trim().to_string());
            }
        });
    }
    last.filter(|t| !t.is_empty())
}

/// The id under any of codex's names for it, taking only uuid-shaped values so an
/// event's own sequence id can never be mistaken for a thread.
fn codex_thread_id(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let mut found: Option<String> = None;
        walk(&value, &mut |obj| {
            if found.is_some() {
                return;
            }
            found = ["thread_id", "session_id", "conversation_id", "id"]
                .iter()
                .find_map(|k| obj.get(*k).and_then(|v| v.as_str()))
                .filter(|id| looks_like_session_id(id))
                .map(str::to_string);
        });
        if found.is_some() {
            return found;
        }
    }
    None
}

fn walk(
    value: &serde_json::Value,
    visit: &mut impl FnMut(&serde_json::Map<String, serde_json::Value>),
) {
    match value {
        serde_json::Value::Object(map) => {
            visit(map);
            for v in map.values() {
                walk(v, visit);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                walk(v, visit);
            }
        }
        _ => {}
    }
}

fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

#[async_trait]
impl Deliverer for Spawner {
    async fn deliver(&self, req: &DeliveryRequest<'_>) -> DeliveryOutcome {
        let (harness, resume) = match req.resolved {
            Resolved::Spawn(h) => (h.clone(), None),
            Resolved::Offline(addr, _) => {
                // Auto never resurrects a sleeping session on its own: the row is
                // addressed correctly, so its SessionStart or Stop hook will drain it
                // the next time the human runs it. Only an explicit mode spends money.
                if req.mode == SendMode::Auto {
                    return DeliveryOutcome::Queued;
                }
                (addr.harness.clone(), Some(addr))
            }
            // A live peer is somebody else's job; leaving the row Pending is correct
            // because that peer drains its own mailbox.
            _ => return DeliveryOutcome::Queued,
        };

        let envelope = Envelope::render(req.message, None);
        match effective_mode(req.mode, &harness, req.message.expects_reply) {
            SendMode::Ask => self.ask(&harness, req.message, &envelope, resume).await,
            SendMode::Background => self.background(&harness, req.message, resume).await,
            SendMode::Pane => match &self.pane {
                Some(pane) => {
                    pane.spawn_pane(PaneRequest {
                        harness: &harness,
                        message: req.message,
                        envelope: &envelope,
                        resume,
                        extra_args: self.extra_args(&harness),
                    })
                    .await
                }
                None => DeliveryOutcome::Failed(
                    "pane mode needs agentmail built with the herdr feature".into(),
                ),
            },
            SendMode::Auto => unreachable!("effective_mode never returns Auto"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_picks_a_mode_per_harness() {
        assert_eq!(
            effective_mode(SendMode::Auto, &Harness::Claude, true),
            SendMode::Ask
        );
        assert_eq!(
            effective_mode(SendMode::Auto, &Harness::Claude, false),
            SendMode::Background
        );
        assert_eq!(
            effective_mode(SendMode::Auto, &Harness::Codex, false),
            SendMode::Ask
        );
        assert_eq!(
            effective_mode(SendMode::Pane, &Harness::Claude, true),
            SendMode::Pane
        );
    }

    #[test]
    fn finds_a_session_id_in_noisy_output() {
        assert_eq!(
            session_id_in("Started background session 8890a685-1111-2222-3333-444444444444\n"),
            Some("8890a685-1111-2222-3333-444444444444".to_string())
        );
        assert_eq!(session_id_in("no id here"), None);
    }
}
