//! The herdr deliverer against an in-process fake herdr — never a live server.
#![cfg(all(unix, feature = "herdr"))]

use std::sync::{Arc, Mutex};

use agentmail_core::{
    Address, AgentState, Deliverer, DeliveryOutcome, DeliveryRequest, DirectoryEntry, Harness,
    Message, Registration, Resolved, SendMode,
};
use agentmail_core::{Message as CoreMessage, Store};
use agentmail_delivery::{
    pane_address, HerdrDirectory, HerdrPaneSpawner, HerdrPromptDeliverer, PaneRequest, PaneSpawner,
};
use herdr_client::HerdrClient;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

const CODEX_ID: &str = "01999b0e-2222-4444-8888-cccccccccccc";

/// One canned reply per request, plus a log of everything that came in.
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
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let responder = Arc::clone(&responder);
                let seen = Arc::clone(&seen);
                tokio::spawn(async move {
                    let (read, mut write) = tokio::io::split(stream);
                    let mut lines = BufReader::new(read).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let Ok(request) = serde_json::from_str::<Value>(&line) else {
                            continue;
                        };
                        seen.lock().expect("lock").push(request.clone());
                        let mut bytes = serde_json::to_vec(&responder(&request)).expect("encode");
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

    async fn client(&self) -> HerdrClient {
        HerdrClient::connect_path(&self.path)
            .await
            .expect("connect")
    }

    fn request(&self, method: &str) -> Option<Value> {
        self.requests
            .lock()
            .expect("lock")
            .iter()
            .find(|r| r.get("method").and_then(Value::as_str) == Some(method))
            .cloned()
    }
}

fn agent(status: &str) -> Value {
    json!({
        "pane_id": "w1:p3",
        "agent": "codex",
        "name": "reviewer",
        "agent_status": status,
        "cwd": "/repo",
        "terminal_title_stripped": "fixing the parser",
        "agent_session": {"source": "herdr:codex", "agent": "codex", "kind": "id", "value": CODEX_ID}
    })
}

fn live(state: AgentState) -> Resolved {
    let mut reg = Registration::new(Harness::Codex, CODEX_ID, "/repo");
    reg.alias = Some("reviewer".into());
    Resolved::Live(
        reg,
        Some(DirectoryEntry {
            address: Some(Address::new(Harness::Codex, CODEX_ID)),
            alias: Some("reviewer".into()),
            state,
            cwd: None,
            title: None,
            pane: Some("w1:p3".into()),
        }),
    )
}

fn message() -> Message {
    Message::new(
        Address::new(Harness::Claude, "8890a685-1111-2222-3333-444444444444"),
        Address::new(Harness::Codex, CODEX_ID),
        "the parser drops trailing commas",
    )
    .expecting_reply(true)
}

#[tokio::test]
async fn an_idle_agent_is_prompted_with_the_envelope() {
    let fake = Fake::spawn(|req| {
        let id = req.get("id").cloned();
        match req.get("method").and_then(Value::as_str) {
            Some("agent.list") => json!({"id": id, "result": {"agents": [agent("idle")]}}),
            Some("agent.prompt") => json!({"id": id, "result": {"agent": agent("working")}}),
            _ => json!({"id": id, "error": {"code": "unknown_method", "message": "no"}}),
        }
    });

    let deliverer = HerdrPromptDeliverer::new(fake.client().await);
    let msg = message();
    let outcome = deliverer
        .deliver(&DeliveryRequest {
            message: &msg,
            resolved: &live(AgentState::Idle),
            mode: SendMode::Auto,
        })
        .await;
    assert_eq!(outcome, DeliveryOutcome::Pushed);

    let prompt = fake.request("agent.prompt").expect("a prompt request");
    let params = &prompt["params"];
    assert_eq!(params["target"], "w1:p3");
    let text = params["text"].as_str().expect("text");
    assert!(
        text.starts_with("[agentmail] from claude:8890a685"),
        "{text}"
    );
    assert!(text.contains(&format!("id {}", msg.id)), "{text}");
    assert!(text.contains("reply expected"), "{text}");
    assert!(text.contains("the parser drops trailing commas"), "{text}");
    assert!(
        !text.contains('\n'),
        "a prompt must stay on one line: {text}"
    );
}

#[tokio::test]
async fn a_blocked_agent_leaves_the_mail_queued() {
    let fake = Fake::spawn(|req| {
        let id = req.get("id").cloned();
        match req.get("method").and_then(Value::as_str) {
            Some("agent.list") => json!({"id": id, "result": {"agents": [agent("idle")]}}),
            _ => json!({"id": id, "error": {"code": "agent_blocked", "message": "in a dialog"}}),
        }
    });

    let deliverer = HerdrPromptDeliverer::new(fake.client().await);
    let msg = message();
    let outcome = deliverer
        .deliver(&DeliveryRequest {
            message: &msg,
            resolved: &live(AgentState::Idle),
            mode: SendMode::Auto,
        })
        .await;
    assert_eq!(outcome, DeliveryOutcome::Queued);
}

#[tokio::test]
async fn a_working_agent_is_never_interrupted() {
    let fake = Fake::spawn(|req| {
        let id = req.get("id").cloned();
        json!({"id": id, "result": {"agents": [agent("working")]}})
    });

    let deliverer = HerdrPromptDeliverer::new(fake.client().await);
    let msg = message();
    let outcome = deliverer
        .deliver(&DeliveryRequest {
            message: &msg,
            resolved: &live(AgentState::Working),
            mode: SendMode::Auto,
        })
        .await;
    assert_eq!(outcome, DeliveryOutcome::Queued);
    assert!(fake.request("agent.prompt").is_none());
}

#[tokio::test]
async fn claude_peers_are_left_to_their_channel() {
    let fake = Fake::spawn(|req| json!({"id": req.get("id"), "result": {"agents": []}}));
    let deliverer = HerdrPromptDeliverer::new(fake.client().await);

    let reg = Registration::new(
        Harness::Claude,
        "8890a685-1111-2222-3333-444444444444",
        "/repo",
    );
    let msg = message();
    let outcome = deliverer
        .deliver(&DeliveryRequest {
            message: &msg,
            resolved: &Resolved::Live(reg, None),
            mode: SendMode::Auto,
        })
        .await;
    assert!(matches!(outcome, DeliveryOutcome::Failed(e) if e.contains("channel")));
}

/// A brand new agent may sit in a trust or channel dialog for a while, and `agent.start`
/// says so. The pane exists, so the mail waits on the pane rather than on an address
/// nothing can ever drain.
#[tokio::test]
async fn a_pane_that_will_not_start_yet_keeps_the_mail_on_the_pane() {
    let fake = Fake::spawn(|req| {
        let id = req.get("id").cloned();
        match req.get("method").and_then(Value::as_str) {
            Some("pane.split") => json!({"id": id, "result": {"pane": {"pane_id": "w1:p9"}}}),
            _ => {
                json!({"id": id, "error": {"code": "agent_not_ready", "message": "still booting"}})
            }
        }
    });

    let store = Store::open_in_memory().expect("store");
    let msg = CoreMessage::new(
        Address::new(Harness::Claude, "8890a685-1111-2222-3333-444444444444"),
        Address::new(Harness::Codex, "new"),
        "look at the parser",
    );
    store.enqueue(&msg).expect("enqueue");

    let spawner = HerdrPaneSpawner::new(fake.client().await);
    let outcome = spawner
        .spawn_pane(PaneRequest {
            harness: &Harness::Codex,
            message: &msg,
            envelope: "[agentmail] from claude:8890a685 · id x\nlook at the parser",
            resume: None,
            extra_args: &[],
            store: &store,
        })
        .await;

    assert_eq!(
        outcome,
        DeliveryOutcome::Queued,
        "a dialog is a wait, not a failure"
    );
    let parked = pane_address(&Harness::Codex, "w1:p9");
    assert_eq!(
        store.pending_for(&parked).expect("pending").len(),
        1,
        "the row waits on the pane"
    );
    assert!(store
        .pending_for(&Address::new(Harness::Codex, "new"))
        .expect("pending")
        .is_empty());
}

#[tokio::test]
async fn the_directory_maps_agent_list_into_entries() {
    let fake =
        Fake::spawn(|req| json!({"id": req.get("id"), "result": {"agents": [agent("done")]}}));
    let dir = HerdrDirectory::new(fake.client().await);
    assert!(dir.entries().is_empty(), "the cache starts empty");

    dir.refresh().await.expect("refresh");
    let entries = dir.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].address,
        Some(Address::new(Harness::Codex, CODEX_ID))
    );
    assert_eq!(entries[0].state, AgentState::Idle, "done means free");
}
