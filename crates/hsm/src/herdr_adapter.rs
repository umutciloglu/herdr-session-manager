//! herdr implementations of the two traits hsm-core defines.
//!
//! `LiveSessions` is sync because the index is sync, so it blocks on the shared
//! runtime; `PaneOps` stays async and is awaited by `OpenService`.

use std::path::Path;

use async_trait::async_trait;
use herdr_client::{
    AgentInfo, AgentSessionInfo, AgentSessionRefKind, AgentStart, HerdrClient, PaneInfo, PaneSplit,
    Snapshot, SplitDirection as HerdrSplit, TabCreate,
};
use hsm_core::{
    Error as CoreError, HarnessKind, LivePane, LiveSessions, PaneOps, RefKind,
    Result as CoreResult, SessionRef, SplitDirection,
};
use tokio::runtime::Handle;

/// Reads the running herdr server for the index refresh.
pub struct HerdrLive {
    client: HerdrClient,
    handle: Handle,
}

impl HerdrLive {
    pub fn new(client: HerdrClient, handle: Handle) -> HerdrLive {
        HerdrLive { client, handle }
    }
}

impl LiveSessions for HerdrLive {
    fn live(&self) -> Vec<LivePane> {
        let client = self.client.clone();
        let fetched = self.handle.block_on(async move {
            let agents = client.agent_list().await?;
            let snapshot = client.session_snapshot().await?;
            Ok::<_, herdr_client::Error>((agents, snapshot))
        });
        match fetched {
            Ok((agents, snapshot)) => live_panes(&agents, &snapshot),
            // A refresh without the live overlay is still a useful refresh.
            Err(error) => {
                tracing::warn!(%error, "could not read the herdr snapshot");
                Vec::new()
            }
        }
    }
}

/// `agent.list` is the authority on what is running; the snapshot only adds
/// panes it did not mention.
fn live_panes(agents: &[AgentInfo], snapshot: &Snapshot) -> Vec<LivePane> {
    let mut out: Vec<LivePane> = agents.iter().filter_map(from_agent).collect();
    for pane in &snapshot.panes {
        // A pane that carries a session ref but runs no agent is a *remembered*
        // session, not a live one. herdr's session.json pass already indexes
        // those, and calling them live would show a running agent that exited.
        if pane.agent.is_none() || out.iter().any(|l| l.pane_id == pane.pane_id) {
            continue;
        }
        if let Some(live) = from_pane(pane) {
            out.push(live);
        }
    }
    out
}

fn from_agent(a: &AgentInfo) -> Option<LivePane> {
    Some(LivePane {
        pane_id: non_empty(&a.pane_id)?,
        workspace_id: non_empty(&a.workspace_id),
        tab_id: non_empty(&a.tab_id),
        agent: a.agent.clone().unwrap_or_default(),
        session: session_ref(a.agent_session.as_ref()?)?,
        cwd: a.cwd.clone().unwrap_or_default().into(),
        title: a
            .terminal_title_stripped
            .clone()
            .or_else(|| a.title.clone())
            .or_else(|| a.name.clone()),
        status: a.agent_status.to_string(),
    })
}

fn from_pane(p: &PaneInfo) -> Option<LivePane> {
    Some(LivePane {
        pane_id: non_empty(&p.pane_id)?,
        workspace_id: non_empty(&p.workspace_id),
        tab_id: non_empty(&p.tab_id),
        agent: p.agent.clone().unwrap_or_default(),
        session: session_ref(p.agent_session.as_ref()?)?,
        cwd: p.cwd.clone().unwrap_or_default().into(),
        title: p
            .terminal_title_stripped
            .clone()
            .or_else(|| p.title.clone())
            .or_else(|| p.label.clone()),
        status: p.agent_status.to_string(),
    })
}

fn session_ref(info: &AgentSessionInfo) -> Option<SessionRef> {
    if info.value.is_empty() {
        return None;
    }
    // `agent` is the herdr kind; `source` looks like "herdr:claude".
    let name = if info.agent.is_empty() {
        info.source.rsplit(':').next().unwrap_or_default()
    } else {
        info.agent.as_str()
    };
    if name.is_empty() {
        return None;
    }
    Some(SessionRef {
        harness: HarnessKind::from_name(name),
        kind: match info.kind {
            AgentSessionRefKind::Path => RefKind::Path,
            AgentSessionRefKind::Id => RefKind::Id,
        },
        value: info.value.clone(),
    })
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// `<harness>:<session id>` for whatever agent herdr has on that pane, which is
/// who a message from this popup is really from. `None` when the pane runs no
/// agent or herdr never learned its session ref.
pub fn pane_address(snapshot: &Snapshot, pane_id: &str) -> Option<String> {
    let info = snapshot
        .pane(pane_id)
        .and_then(|p| p.agent_session.as_ref())
        .or_else(|| {
            snapshot
                .agents
                .iter()
                .find(|a| a.pane_id == pane_id)?
                .agent_session
                .as_ref()
        })?;
    let session = session_ref(info)?;
    Some(format!("{}:{}", session.harness, session.value))
}

/// Drives panes for `OpenService`.
pub struct HerdrPaneOps {
    client: HerdrClient,
}

impl HerdrPaneOps {
    pub fn new(client: HerdrClient) -> HerdrPaneOps {
        HerdrPaneOps { client }
    }
}

#[async_trait]
impl PaneOps for HerdrPaneOps {
    async fn send_input(&self, pane_id: &str, text: &str, press_enter: bool) -> CoreResult<()> {
        let keys: &[&str] = if press_enter { &["enter"] } else { &[] };
        self.client
            .pane_send_input(pane_id, Some(text), keys)
            .await
            .map_err(backend)
    }

    async fn split(
        &self,
        direction: SplitDirection,
        target_pane_id: Option<&str>,
        cwd: Option<&Path>,
        focus: bool,
    ) -> CoreResult<String> {
        let mut params = PaneSplit::new(match direction {
            SplitDirection::Horizontal => HerdrSplit::Right,
            SplitDirection::Vertical => HerdrSplit::Down,
        })
        .focus(focus);
        if let Some(target) = target_pane_id {
            params = params.target_pane_id(target);
        }
        if let Some(cwd) = cwd {
            params = params.cwd(cwd.to_string_lossy().into_owned());
        }
        let pane = self.client.pane_split(&params).await.map_err(backend)?;
        Ok(pane.pane_id)
    }

    async fn create_tab(
        &self,
        cwd: Option<&Path>,
        label: Option<&str>,
        focus: bool,
    ) -> CoreResult<String> {
        let mut params = TabCreate::new().focus(focus);
        if let Some(cwd) = cwd {
            params = params.cwd(cwd.to_string_lossy().into_owned());
        }
        if let Some(label) = label {
            params = params.label(label);
        }
        let created = self.client.tab_create(&params).await.map_err(backend)?;
        Ok(created.root_pane.pane_id)
    }

    async fn agent_start(
        &self,
        name: &str,
        kind: &HarnessKind,
        pane_id: &str,
        args: &[String],
        timeout_ms: Option<u64>,
    ) -> CoreResult<()> {
        let mut params = AgentStart::new(name, kind.as_str(), pane_id).args(args.to_vec());
        if let Some(ms) = timeout_ms {
            params = params.timeout_ms(ms);
        }
        self.client.agent_start(&params).await.map_err(backend)?;
        Ok(())
    }
}

fn backend(e: herdr_client::Error) -> CoreError {
    CoreError::Backend(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agents(v: serde_json::Value) -> Vec<AgentInfo> {
        serde_json::from_value(v).expect("agents")
    }

    fn snapshot(v: serde_json::Value) -> Snapshot {
        serde_json::from_value(v).expect("snapshot")
    }

    #[test]
    fn maps_an_agent_row_to_a_live_pane() {
        let live = live_panes(
            &agents(json!([{
                "pane_id": "w6:p1",
                "workspace_id": "w6",
                "tab_id": "w6:t1",
                "agent": "claude",
                "agent_status": "working",
                "cwd": "/Users/x/Projects/trade-help",
                "terminal_title_stripped": "API authentication",
                "agent_session": {"source":"herdr:claude","agent":"claude","kind":"id","value":"8890a685"}
            }])),
            &snapshot(json!({})),
        );
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].pane_id, "w6:p1");
        assert_eq!(live[0].session.harness, HarnessKind::Claude);
        assert_eq!(live[0].session.value, "8890a685");
        assert_eq!(live[0].status, "working");
        assert_eq!(live[0].title.as_deref(), Some("API authentication"));
        assert_eq!(
            live[0].cwd.to_string_lossy(),
            "/Users/x/Projects/trade-help"
        );
    }

    #[test]
    fn a_pane_without_a_running_agent_is_not_live() {
        let live = live_panes(
            &agents(json!([])),
            &snapshot(json!({"panes":[{
                "pane_id": "w1:p9",
                "agent_status": "unknown",
                "agent_session": {"source":"herdr:claude","agent":"claude","kind":"id","value":"7713f9f6"}
            }]})),
        );
        assert!(live.is_empty());
    }

    #[test]
    fn the_snapshot_only_adds_panes_agent_list_missed() {
        let live = live_panes(
            &agents(json!([{
                "pane_id": "w6:p1",
                "agent": "claude",
                "agent_status": "idle",
                "agent_session": {"source":"herdr:claude","agent":"claude","kind":"id","value":"aaaa"}
            }])),
            &snapshot(json!({"panes":[
                {
                    "pane_id": "w6:p1",
                    "agent": "claude",
                    "agent_status": "idle",
                    "agent_session": {"source":"herdr:claude","agent":"claude","kind":"id","value":"aaaa"}
                },
                {
                    "pane_id": "w2:p14",
                    "agent": "codex",
                    "agent_status": "idle",
                    "cwd": "/Users/x",
                    "agent_session": {"source":"herdr:codex","agent":"codex","kind":"id","value":"bbbb"}
                }
            ]})),
        );
        assert_eq!(live.len(), 2);
        assert_eq!(live[1].pane_id, "w2:p14");
        assert_eq!(live[1].session.harness, HarnessKind::Codex);
    }

    #[test]
    fn a_pane_without_a_session_ref_is_skipped() {
        let live = live_panes(
            &agents(json!([{"pane_id": "w1:p1", "agent": "claude", "agent_status": "idle"}])),
            &snapshot(json!({})),
        );
        assert!(live.is_empty());
    }

    #[test]
    fn pane_address_reads_the_session_on_a_pane() {
        let snap = snapshot(json!({
            "panes": [
                {
                    "pane_id": "w6:p1",
                    "agent": "claude",
                    "agent_status": "idle",
                    "agent_session": {"source":"herdr:claude","agent":"claude","kind":"id","value":"8890a685-a0f1"}
                },
                {"pane_id": "w6:p2", "agent_status": "unknown"}
            ],
            "agents": [
                {
                    "pane_id": "w2:p14",
                    "agent": "codex",
                    "agent_status": "idle",
                    "agent_session": {"source":"herdr:codex","agent":"codex","kind":"id","value":"01a08ad8"}
                }
            ]
        }));
        assert_eq!(
            pane_address(&snap, "w6:p1").as_deref(),
            Some("claude:8890a685-a0f1")
        );
        // Known only through the agent list.
        assert_eq!(
            pane_address(&snap, "w2:p14").as_deref(),
            Some("codex:01a08ad8")
        );
        // A plain shell pane, and a pane herdr has never heard of.
        assert_eq!(pane_address(&snap, "w6:p2"), None);
        assert_eq!(pane_address(&snap, "w9:p9"), None);
    }

    #[test]
    fn an_unknown_harness_survives_as_other() {
        let live = live_panes(
            &agents(json!([{
                "pane_id": "w1:p1",
                "agent": "brand-new",
                "agent_status": "idle",
                "agent_session": {"source":"herdr:brand-new","agent":"","kind":"path","value":"/tmp/s.json"}
            }])),
            &snapshot(json!({})),
        );
        assert_eq!(
            live[0].session.harness,
            HarnessKind::Other("brand-new".into())
        );
        assert_eq!(live[0].session.kind, RefKind::Path);
    }
}

/// `PaneOps` drives mutating herdr calls, so it is only ever exercised against
/// an in-process fake: a live server would really split panes and start agents.
#[cfg(all(test, unix))]
mod socket_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use hsm_core::{OpenContext, OpenService, OpenTarget, Session, Tier};
    use serde_json::{json, Value};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    struct Fake {
        path: std::path::PathBuf,
        requests: Arc<Mutex<Vec<Value>>>,
        _dir: tempfile::TempDir,
    }

    impl Fake {
        fn spawn<F>(responder: F) -> Fake
        where
            F: Fn(&Value) -> Value + Send + Sync + 'static,
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("herdr.sock");
            let listener = UnixListener::bind(&path).expect("bind");
            let requests = Arc::new(Mutex::new(Vec::new()));
            let seen = Arc::clone(&requests);

            tokio::spawn(async move {
                let responder = Arc::new(responder);
                while let Ok((stream, _)) = listener.accept().await {
                    let responder = Arc::clone(&responder);
                    let seen = Arc::clone(&seen);
                    tokio::spawn(async move {
                        let (read, mut write) = tokio::io::split(stream);
                        let mut lines = BufReader::new(read).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            let Ok(request) = serde_json::from_str::<Value>(&line) else {
                                continue;
                            };
                            if let Ok(mut seen) = seen.lock() {
                                seen.push(request.clone());
                            }
                            let mut bytes =
                                serde_json::to_vec(&responder(&request)).expect("encode");
                            bytes.push(b'\n');
                            if write.write_all(&bytes).await.is_err() {
                                return;
                            }
                        }
                    });
                }
            });

            Fake {
                path,
                requests,
                _dir: dir,
            }
        }

        async fn ops(&self) -> HerdrPaneOps {
            HerdrPaneOps::new(
                HerdrClient::connect_path(&self.path)
                    .await
                    .expect("connect to fake"),
            )
        }

        fn params(&self, method: &str) -> Value {
            let requests = self.requests.lock().expect("lock");
            requests
                .iter()
                .find(|r| r.get("method").and_then(Value::as_str) == Some(method))
                .unwrap_or_else(|| panic!("no {method} request in {requests:?}"))
                .get("params")
                .cloned()
                .unwrap_or(Value::Null)
        }

        fn methods(&self) -> Vec<String> {
            self.requests
                .lock()
                .expect("lock")
                .iter()
                .filter_map(|r| r.get("method").and_then(Value::as_str).map(str::to_string))
                .collect()
        }
    }

    fn ok(request: &Value, result: Value) -> Value {
        json!({ "id": request.get("id"), "result": result })
    }

    fn reply(request: &Value) -> Value {
        match request.get("method").and_then(Value::as_str) {
            Some("pane.split") => ok(request, json!({ "pane": { "pane_id": "w1:p9" } })),
            Some("tab.create") => ok(
                request,
                json!({ "tab": { "tab_id": "w1:t4" }, "root_pane": { "pane_id": "w1:pA" } }),
            ),
            Some("agent.start") => ok(
                request,
                json!({ "agent": { "pane_id": "w1:p9" }, "argv": ["claude", "--resume", "x"] }),
            ),
            _ => ok(request, json!({ "type": "ok" })),
        }
    }

    fn session() -> Session {
        let mut s = Session::new(
            HarnessKind::Claude,
            "8890a685-a0f1-4a9e-949d-f7f386bc4cb6",
            "/Users/x/Projects/demo",
        );
        s.transcript_present = true;
        s.tier = Tier::Warm;
        s
    }

    #[tokio::test]
    async fn split_maps_horizontal_to_right_and_vertical_to_down() {
        let fake = Fake::spawn(reply);
        let ops = fake.ops().await;

        let pane = ops
            .split(
                SplitDirection::Horizontal,
                Some("w1:p1"),
                Some(Path::new("/repo")),
                true,
            )
            .await
            .expect("split");
        assert_eq!(pane, "w1:p9");
        assert_eq!(
            fake.params("pane.split"),
            json!({"direction": "right", "target_pane_id": "w1:p1", "cwd": "/repo", "focus": true})
        );

        let fake = Fake::spawn(reply);
        let ops = fake.ops().await;
        ops.split(SplitDirection::Vertical, None, None, false)
            .await
            .expect("split");
        assert_eq!(fake.params("pane.split"), json!({"direction": "down"}));
    }

    #[tokio::test]
    async fn create_tab_returns_the_root_pane() {
        let fake = Fake::spawn(reply);
        let pane = fake
            .ops()
            .await
            .create_tab(Some(Path::new("/repo")), Some("claude-8890a685"), true)
            .await
            .expect("tab");
        assert_eq!(pane, "w1:pA");
        assert_eq!(
            fake.params("tab.create"),
            json!({"cwd": "/repo", "label": "claude-8890a685", "focus": true})
        );
    }

    #[tokio::test]
    async fn agent_start_passes_kind_args_and_timeout() {
        let fake = Fake::spawn(reply);
        fake.ops()
            .await
            .agent_start(
                "claude-8890a685",
                &HarnessKind::Claude,
                "w1:p9",
                &["--resume".to_string(), "8890a685".to_string()],
                Some(30_000),
            )
            .await
            .expect("start");
        assert_eq!(
            fake.params("agent.start"),
            json!({
                "name": "claude-8890a685",
                "kind": "claude",
                "pane_id": "w1:p9",
                "args": ["--resume", "8890a685"],
                "timeout_ms": 30_000
            })
        );
    }

    #[tokio::test]
    async fn send_input_submits_with_the_enter_key() {
        let fake = Fake::spawn(reply);
        fake.ops()
            .await
            .send_input("w1:p1", "claude --resume x", true)
            .await
            .expect("send");
        assert_eq!(
            fake.params("pane.send_input"),
            json!({"pane_id": "w1:p1", "text": "claude --resume x", "keys": ["enter"]})
        );
    }

    #[tokio::test]
    async fn a_herdr_error_becomes_a_backend_error() {
        let fake = Fake::spawn(|request| {
            json!({
                "id": request.get("id"),
                "error": {"code": "pane_not_found", "message": "no such pane"}
            })
        });
        let err = fake
            .ops()
            .await
            .send_input("w9:p9", "hi", true)
            .await
            .expect_err("should fail");
        assert!(matches!(err, CoreError::Backend(_)), "{err:?}");
        assert!(err.to_string().contains("pane_not_found"));
    }

    #[tokio::test]
    async fn open_in_a_split_starts_the_agent_through_herdr() {
        let fake = Fake::spawn(reply);
        let service = OpenService::new(fake.ops().await);
        let report = service
            .open(
                &session(),
                OpenTarget::Split(SplitDirection::Horizontal),
                &OpenContext {
                    context_pane_id: Some("w1:p1".into()),
                    cwd_override: None,
                },
            )
            .await
            .expect("open");

        assert_eq!(report.pane_id, "w1:p9");
        assert_eq!(fake.methods(), vec!["pane.split", "agent.start"]);
        assert_eq!(
            fake.params("agent.start")["args"],
            json!(["--resume", "8890a685-a0f1-4a9e-949d-f7f386bc4cb6"])
        );
    }

    #[tokio::test]
    async fn a_refused_agent_start_falls_back_to_typing() {
        let fake = Fake::spawn(
            |request| match request.get("method").and_then(Value::as_str) {
                Some("agent.start") => json!({
                    "id": request.get("id"),
                    "error": {"code": "pane_busy", "message": "not at a shell prompt"}
                }),
                _ => reply(request),
            },
        );
        let service = OpenService::new(fake.ops().await);
        let report = service
            .open(&session(), OpenTarget::Tab, &OpenContext::default())
            .await
            .expect("open");

        assert_eq!(report.pane_id, "w1:pA");
        assert_eq!(
            fake.methods(),
            vec!["tab.create", "agent.start", "pane.send_input"]
        );
        assert_eq!(
            fake.params("pane.send_input")["text"],
            json!("claude --resume 8890a685-a0f1-4a9e-949d-f7f386bc4cb6")
        );
    }
}
