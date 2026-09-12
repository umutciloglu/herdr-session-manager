//! Restoring a session into a herdr pane.
//!
//! The service never talks to herdr; it drives a `PaneOps` implementation so
//! the wiring (and the socket) lives in the binary.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::domain::{HarnessKind, OpenTarget, Session, SessionRef, SplitDirection, Tier};
use crate::error::{Error, Result};
use crate::harness::registry;

/// herdr's `agent.start` rejects a timeout outside (3000, 300000].
const AGENT_START_TIMEOUT_MS: u64 = 30_000;

#[async_trait]
pub trait PaneOps: Send + Sync {
    async fn send_input(&self, pane_id: &str, text: &str, press_enter: bool) -> Result<()>;

    /// Returns the new pane id. `Horizontal` means a pane beside the target,
    /// `Vertical` one below it.
    async fn split(
        &self,
        direction: SplitDirection,
        target_pane_id: Option<&str>,
        cwd: Option<&Path>,
        focus: bool,
    ) -> Result<String>;

    /// Returns the pane id of the new tab's first pane.
    async fn create_tab(
        &self,
        cwd: Option<&Path>,
        label: Option<&str>,
        focus: bool,
    ) -> Result<String>;

    async fn agent_start(
        &self,
        name: &str,
        kind: &HarnessKind,
        pane_id: &str,
        args: &[String],
        timeout_ms: Option<u64>,
    ) -> Result<()>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenContext {
    /// The pane the user triggered the plugin from. Required for `Current`,
    /// used as the split target otherwise.
    pub context_pane_id: Option<String>,
    pub cwd_override: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMethod {
    /// herdr launched the agent and owns the session ref for the pane.
    AgentStart,
    /// We typed the command at a shell prompt.
    SendInput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenReport {
    pub pane_id: String,
    pub method: OpenMethod,
}

pub struct OpenService<P: PaneOps> {
    ops: P,
    agent_start_timeout_ms: u64,
}

impl<P: PaneOps> OpenService<P> {
    pub fn new(ops: P) -> Self {
        OpenService {
            ops,
            agent_start_timeout_ms: AGENT_START_TIMEOUT_MS,
        }
    }

    pub fn with_timeout(ops: P, timeout_ms: u64) -> Self {
        OpenService {
            ops,
            agent_start_timeout_ms: timeout_ms,
        }
    }

    pub async fn open(
        &self,
        session: &Session,
        target: OpenTarget,
        ctx: &OpenContext,
    ) -> Result<OpenReport> {
        let cwd = ctx
            .cwd_override
            .clone()
            .unwrap_or_else(|| session.cwd.clone());
        let cwd = (!cwd.as_os_str().is_empty()).then_some(cwd);
        let (exe, args) = command_for(session);

        match target {
            OpenTarget::Current => {
                let pane_id = ctx.context_pane_id.clone().ok_or(Error::NoContextPane)?;
                self.ops
                    .send_input(&pane_id, &command_line(&exe, &args), true)
                    .await?;
                Ok(OpenReport {
                    pane_id,
                    method: OpenMethod::SendInput,
                })
            }
            OpenTarget::Split(direction) => {
                let pane_id = self
                    .ops
                    .split(
                        direction,
                        ctx.context_pane_id.as_deref(),
                        cwd.as_deref(),
                        true,
                    )
                    .await?;
                self.launch(session, &pane_id, &exe, &args).await
            }
            OpenTarget::Tab => {
                let label = agent_name(session);
                let pane_id = self
                    .ops
                    .create_tab(cwd.as_deref(), Some(&label), true)
                    .await?;
                self.launch(session, &pane_id, &exe, &args).await
            }
        }
    }

    /// `agent.start` is preferred because herdr then owns the pane's session
    /// ref and can restore it later; typing the command is the fallback when
    /// the pane is not at a shell prompt.
    async fn launch(
        &self,
        session: &Session,
        pane_id: &str,
        exe: &str,
        args: &[String],
    ) -> Result<OpenReport> {
        let name = agent_name(session);
        let started = self
            .ops
            .agent_start(
                &name,
                &session.harness,
                pane_id,
                args,
                Some(self.agent_start_timeout_ms),
            )
            .await;
        match started {
            Ok(()) => Ok(OpenReport {
                pane_id: pane_id.to_string(),
                method: OpenMethod::AgentStart,
            }),
            Err(_) => {
                self.ops
                    .send_input(pane_id, &command_line(exe, args), true)
                    .await?;
                Ok(OpenReport {
                    pane_id: pane_id.to_string(),
                    method: OpenMethod::SendInput,
                })
            }
        }
    }
}

/// `<harness>-<id8>`, the name herdr shows for the agent.
pub fn agent_name(session: &Session) -> String {
    format!("{}-{}", session.harness, session.short_id())
}

/// A `Gone` session has no transcript left to resume, so it opens as a fresh
/// agent in the old cwd instead.
pub fn command_for(session: &Session) -> (String, Vec<String>) {
    let exe = registry::executable(&session.harness).to_string();
    if session.tier == Tier::Gone {
        return (exe, Vec::new());
    }
    let session_ref = SessionRef::id(session.harness.clone(), session.id.clone());
    let args = registry::resume_args(&session.harness, &session_ref).unwrap_or_default();
    (exe, args)
}

fn command_line(exe: &str, args: &[String]) -> String {
    let mut out = String::from(exe);
    for a in args {
        out.push(' ');
        out.push_str(&shell_quote(a));
    }
    out
}

/// POSIX single-quoting; session ids never need it, but a path-kind ref can.
fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@+,".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    struct Recorder {
        calls: Mutex<Vec<String>>,
        agent_start_fails: bool,
    }

    impl Recorder {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().map(|c| c.clone()).unwrap_or_default()
        }

        fn push(&self, s: String) {
            if let Ok(mut c) = self.calls.lock() {
                c.push(s);
            }
        }
    }

    #[async_trait]
    impl PaneOps for Recorder {
        async fn send_input(&self, pane_id: &str, text: &str, press_enter: bool) -> Result<()> {
            self.push(format!("send_input({pane_id}, {text:?}, {press_enter})"));
            Ok(())
        }

        async fn split(
            &self,
            direction: SplitDirection,
            target_pane_id: Option<&str>,
            cwd: Option<&Path>,
            focus: bool,
        ) -> Result<String> {
            self.push(format!(
                "split({direction}, {target_pane_id:?}, {:?}, {focus})",
                cwd.map(|c| c.display().to_string())
            ));
            Ok("99".to_string())
        }

        async fn create_tab(
            &self,
            cwd: Option<&Path>,
            label: Option<&str>,
            focus: bool,
        ) -> Result<String> {
            self.push(format!(
                "create_tab({:?}, {label:?}, {focus})",
                cwd.map(|c| c.display().to_string())
            ));
            Ok("100".to_string())
        }

        async fn agent_start(
            &self,
            name: &str,
            kind: &HarnessKind,
            pane_id: &str,
            args: &[String],
            timeout_ms: Option<u64>,
        ) -> Result<()> {
            self.push(format!(
                "agent_start({name}, {kind}, {pane_id}, {args:?}, {timeout_ms:?})"
            ));
            if self.agent_start_fails {
                Err(Error::Backend("pane is not at a shell prompt".into()))
            } else {
                Ok(())
            }
        }
    }

    fn session() -> Session {
        let mut s = Session::new(
            HarnessKind::Claude,
            "8890a685-1111-2222-3333-444455556666",
            "/Users/x/Projects/demo",
        );
        s.tier = Tier::Warm;
        s.transcript_present = true;
        s
    }

    #[tokio::test]
    async fn current_types_the_resume_command() {
        let svc = OpenService::new(Recorder::default());
        let ctx = OpenContext {
            context_pane_id: Some("4".into()),
            ..OpenContext::default()
        };
        let report = svc
            .open(&session(), OpenTarget::Current, &ctx)
            .await
            .expect("open");
        assert_eq!(
            report,
            OpenReport {
                pane_id: "4".into(),
                method: OpenMethod::SendInput
            }
        );
        assert_eq!(
            svc.ops.calls(),
            vec!["send_input(4, \"claude --resume 8890a685-1111-2222-3333-444455556666\", true)"]
        );
    }

    #[tokio::test]
    async fn current_without_a_context_pane_is_an_error() {
        let svc = OpenService::new(Recorder::default());
        let err = svc
            .open(&session(), OpenTarget::Current, &OpenContext::default())
            .await;
        assert!(matches!(err, Err(Error::NoContextPane)));
    }

    #[tokio::test]
    async fn split_starts_the_agent_through_herdr() {
        let svc = OpenService::new(Recorder::default());
        let ctx = OpenContext {
            context_pane_id: Some("4".into()),
            ..OpenContext::default()
        };
        let report = svc
            .open(
                &session(),
                OpenTarget::Split(SplitDirection::Vertical),
                &ctx,
            )
            .await
            .expect("open");
        assert_eq!(report.method, OpenMethod::AgentStart);
        assert_eq!(report.pane_id, "99");
        let calls = svc.ops.calls();
        assert_eq!(
            calls[0],
            "split(vertical, Some(\"4\"), Some(\"/Users/x/Projects/demo\"), true)"
        );
        assert_eq!(
            calls[1],
            "agent_start(claude-8890a685, claude, 99, [\"--resume\", \
             \"8890a685-1111-2222-3333-444455556666\"], Some(30000))"
        );
    }

    #[tokio::test]
    async fn agent_start_failure_falls_back_to_typing() {
        let svc = OpenService::new(Recorder {
            agent_start_fails: true,
            ..Recorder::default()
        });
        let report = svc
            .open(&session(), OpenTarget::Tab, &OpenContext::default())
            .await
            .expect("open");
        assert_eq!(
            report,
            OpenReport {
                pane_id: "100".into(),
                method: OpenMethod::SendInput
            }
        );
        let calls = svc.ops.calls();
        assert_eq!(
            calls[0],
            "create_tab(Some(\"/Users/x/Projects/demo\"), Some(\"claude-8890a685\"), true)"
        );
        assert!(calls[2].starts_with("send_input(100, \"claude --resume"));
    }

    #[tokio::test]
    async fn gone_sessions_open_a_fresh_agent() {
        let mut s = session();
        s.tier = Tier::Gone;
        assert_eq!(command_for(&s), ("claude".to_string(), Vec::new()));

        let svc = OpenService::new(Recorder::default());
        let ctx = OpenContext {
            context_pane_id: Some("4".into()),
            ..OpenContext::default()
        };
        svc.open(&s, OpenTarget::Current, &ctx).await.expect("open");
        assert_eq!(svc.ops.calls(), vec!["send_input(4, \"claude\", true)"]);
    }

    #[test]
    fn unresumable_harnesses_launch_bare() {
        let mut s = Session::new(HarnessKind::Gemini, "abc", "/p/demo");
        s.transcript_present = true;
        assert_eq!(command_for(&s), ("gemini".to_string(), Vec::new()));
    }

    #[test]
    fn arguments_with_spaces_are_quoted() {
        assert_eq!(
            command_line("pi", &["--session".into(), "/a b/c.json".into()]),
            "pi --session '/a b/c.json'"
        );
    }
}
