//! Mutating methods are exercised against an in-process fake server: never
//! against a live herdr, which would really split panes and start agents.
#![cfg(unix)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use herdr_client::{
    kind, AgentPrompt, AgentStart, AgentStatus, AgentWait, Error, EventStream, HerdrClient,
    PaneRead, PaneSplit, PluginPaneOpen, PluginPanePlacement, PopupSize, PromptWait, ReadSource,
    SplitDirection, Subscription, TabCreate,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

/// A fake herdr: one connection at a time, canned replies from `responder`.
/// The responder returns every line to write, so it can also push events.
struct Fake {
    path: std::path::PathBuf,
    requests: Arc<Mutex<Vec<Value>>>,
    _dir: tempfile::TempDir,
}

impl Fake {
    fn spawn<F>(responder: F) -> Fake
    where
        F: Fn(&Value) -> Vec<Value> + Send + Sync + 'static,
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
                        if let Ok(mut seen) = seen.lock() {
                            seen.push(request.clone());
                        }
                        for reply in responder(&request) {
                            let mut bytes = serde_json::to_vec(&reply).expect("encode");
                            bytes.push(b'\n');
                            if write.write_all(&bytes).await.is_err() {
                                return;
                            }
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
            .expect("connect to fake")
    }

    fn last_params(&self) -> Value {
        let requests = self.requests.lock().expect("lock");
        requests
            .last()
            .expect("a request")
            .get("params")
            .cloned()
            .unwrap_or(Value::Null)
    }

    fn last_method(&self) -> String {
        let requests = self.requests.lock().expect("lock");
        requests
            .last()
            .and_then(|r| r.get("method"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// Replies with `result`, echoing the request id back like herdr does.
fn ok(request: &Value, result: Value) -> Vec<Value> {
    vec![json!({ "id": request.get("id"), "result": result })]
}

fn pane(pane_id: &str) -> Value {
    json!({
        "pane_id": pane_id,
        "terminal_id": "term_1",
        "workspace_id": "w1",
        "tab_id": "w1:t1",
        "focused": true,
        "agent_status": "idle",
        "revision": 1
    })
}

fn agent(pane_id: &str, status: &str) -> Value {
    json!({
        "terminal_id": "term_1",
        "workspace_id": "w1",
        "tab_id": "w1:t1",
        "pane_id": pane_id,
        "agent": "claude",
        "agent_status": status,
        "cwd": "/repo",
        "agent_session": {
            "source": "herdr:claude",
            "agent": "claude",
            "kind": "id",
            "value": "abc-123"
        },
        "focused": false,
        "revision": 2
    })
}

#[tokio::test]
async fn ping_round_trip() {
    let fake = Fake::spawn(|req| ok(req, json!({ "type": "pong", "version": "0.9.0" })));
    let client = fake.client().await;
    assert_eq!(client.ping().await.expect("ping"), "0.9.0");
}

#[tokio::test]
async fn snapshot_unwraps_the_snapshot_field() {
    let fake = Fake::spawn(|req| {
        ok(
            req,
            json!({
                "type": "session_snapshot",
                "snapshot": {
                    "version": "0.9.0",
                    "protocol": 22,
                    "focused_pane_id": "w1:p1",
                    "workspaces": [{
                        "workspace_id": "w1", "number": 1, "label": "repo", "focused": true,
                        "pane_count": 1, "tab_count": 1, "active_tab_id": "w1:t1", "agent_status": "idle"
                    }],
                    "tabs": [{
                        "tab_id": "w1:t1", "workspace_id": "w1", "number": 1, "label": "1",
                        "focused": true, "pane_count": 1, "agent_status": "unknown"
                    }],
                    "panes": [pane("w1:p1")],
                    "agents": [agent("w1:p1", "working")],
                    "layouts": []
                }
            }),
        )
    });
    let snapshot = fake
        .client()
        .await
        .session_snapshot()
        .await
        .expect("snapshot");
    assert_eq!(snapshot.protocol, 22);
    assert_eq!(snapshot.focused_pane_id.as_deref(), Some("w1:p1"));
    assert_eq!(snapshot.pane("w1:p1").expect("pane").tab_id, "w1:t1");
    assert_eq!(snapshot.agents[0].agent_status, AgentStatus::Working);
    assert_eq!(
        snapshot.agents[0]
            .agent_session
            .as_ref()
            .expect("session")
            .value,
        "abc-123"
    );
}

#[tokio::test]
async fn unknown_status_and_unknown_fields_do_not_break_parsing() {
    let fake = Fake::spawn(|req| {
        ok(
            req,
            json!({
                "type": "agent_list",
                "agents": [{
                    "pane_id": "w1:p1",
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "terminal_id": "t",
                    "focused": false,
                    "revision": 1,
                    "agent_status": "reticulating",
                    "brand_new_field": {"a": 1}
                }]
            }),
        )
    });
    let agents = fake.client().await.agent_list().await.expect("agents");
    assert_eq!(agents[0].agent_status, AgentStatus::Unknown);
}

#[tokio::test]
async fn agent_start_sends_schema_params_and_parses_argv() {
    let fake = Fake::spawn(|req| {
        ok(
            req,
            json!({ "type": "agent_started", "agent": agent("w1:p2", "working"), "argv": ["claude", "--resume", "abc"] }),
        )
    });
    let started = fake
        .client()
        .await
        .agent_start(
            &AgentStart::new("reviewer", "claude", "w1:p2")
                .args(["--resume", "abc"])
                .timeout_ms(15_000),
        )
        .await
        .expect("start");

    assert_eq!(fake.last_method(), "agent.start");
    assert_eq!(
        fake.last_params(),
        json!({
            "name": "reviewer",
            "kind": "claude",
            "pane_id": "w1:p2",
            "args": ["--resume", "abc"],
            "timeout_ms": 15000
        })
    );
    assert_eq!(started.argv, vec!["claude", "--resume", "abc"]);
    assert_eq!(started.agent.pane_id, "w1:p2");
}

#[tokio::test]
async fn agent_prompt_nests_wait_options() {
    let fake = Fake::spawn(|req| {
        ok(
            req,
            json!({ "type": "agent_prompted", "agent": agent("w1:p1", "idle") }),
        )
    });
    let info = fake
        .client()
        .await
        .agent_prompt(
            &AgentPrompt::new("reviewer", "ship it")
                .wait(PromptWait::until([AgentStatus::Idle, AgentStatus::Done]).timeout_ms(60_000)),
        )
        .await
        .expect("prompt");

    assert_eq!(
        fake.last_params(),
        json!({
            "target": "reviewer",
            "text": "ship it",
            "wait": { "until": ["idle", "done"], "timeout_ms": 60000 }
        })
    );
    assert_eq!(info.agent_status, AgentStatus::Idle);
}

#[tokio::test]
async fn agent_wait_and_rename() {
    let fake = Fake::spawn(|req| {
        ok(
            req,
            json!({ "type": "agent_info", "agent": agent("w1:p1", "done") }),
        )
    });
    let client = fake.client().await;

    let info = client
        .agent_wait(
            &AgentWait::new("w1:p1")
                .until([AgentStatus::Done])
                .timeout_ms(5_000),
        )
        .await
        .expect("wait");
    assert_eq!(info.agent_status, AgentStatus::Done);
    assert_eq!(
        fake.last_params(),
        json!({ "target": "w1:p1", "until": ["done"], "timeout_ms": 5000 })
    );

    client
        .agent_rename("w1:p1", Some("reviewer"))
        .await
        .expect("rename");
    assert_eq!(
        fake.last_params(),
        json!({ "target": "w1:p1", "name": "reviewer" })
    );

    client
        .agent_rename("w1:p1", None)
        .await
        .expect("clear name");
    assert_eq!(
        fake.last_params(),
        json!({ "target": "w1:p1", "name": null })
    );
}

#[tokio::test]
async fn pane_split_omits_untouched_options() {
    let fake = Fake::spawn(|req| ok(req, json!({ "type": "pane_info", "pane": pane("w1:p2") })));
    let client = fake.client().await;

    let created = client
        .pane_split(&PaneSplit::new(SplitDirection::Down))
        .await
        .expect("split");
    assert_eq!(created.pane_id, "w1:p2");
    assert_eq!(fake.last_params(), json!({ "direction": "down" }));

    client
        .pane_split(
            &PaneSplit::new(SplitDirection::Right)
                .target_pane_id("w1:p1")
                .cwd("/repo")
                .ratio(0.5)
                .focus(true)
                .env("HERDR_ROLE", "tests"),
        )
        .await
        .expect("split");
    assert_eq!(
        fake.last_params(),
        json!({
            "direction": "right",
            "target_pane_id": "w1:p1",
            "cwd": "/repo",
            "ratio": 0.5,
            "focus": true,
            "env": { "HERDR_ROLE": "tests" }
        })
    );
}

#[tokio::test]
async fn tab_create_returns_tab_and_root_pane() {
    let fake = Fake::spawn(|req| {
        ok(
            req,
            json!({
                "type": "tab_created",
                "tab": { "tab_id": "w1:t2", "workspace_id": "w1", "number": 2, "label": "logs",
                         "focused": true, "pane_count": 1, "agent_status": "unknown" },
                "root_pane": pane("w1:p9")
            }),
        )
    });
    let created = fake
        .client()
        .await
        .tab_create(&TabCreate::new().label("logs").cwd("/repo").focus(true))
        .await
        .expect("tab");

    assert_eq!(created.tab.tab_id, "w1:t2");
    assert_eq!(created.root_pane.pane_id, "w1:p9");
    assert_eq!(
        fake.last_params(),
        json!({ "cwd": "/repo", "label": "logs", "focus": true })
    );
}

#[tokio::test]
async fn pane_input_helpers() {
    let fake = Fake::spawn(|req| ok(req, json!({ "type": "ok" })));
    let client = fake.client().await;

    client
        .pane_send_text("w1:p1", "claude --resume x")
        .await
        .expect("text");
    assert_eq!(
        fake.last_params(),
        json!({ "pane_id": "w1:p1", "text": "claude --resume x" })
    );

    client
        .pane_send_input("w1:p1", Some("claude"), &["enter"])
        .await
        .expect("input");
    assert_eq!(
        fake.last_params(),
        json!({ "pane_id": "w1:p1", "text": "claude", "keys": ["enter"] })
    );

    client
        .pane_send_input("w1:p1", None, &["ctrl+c"])
        .await
        .expect("keys");
    assert_eq!(
        fake.last_params(),
        json!({ "pane_id": "w1:p1", "keys": ["ctrl+c"] })
    );
}

#[tokio::test]
async fn pane_current_and_read() {
    let fake = Fake::spawn(|req| {
        let method = req
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match method {
            "pane.current" => ok(
                req,
                json!({ "type": "pane_current", "pane": pane("w1:p1") }),
            ),
            _ => ok(
                req,
                json!({
                    "type": "pane_read",
                    "read": {
                        "pane_id": "w1:p1", "workspace_id": "w1", "tab_id": "w1:t1",
                        "source": "recent", "format": "text", "text": "hello\n",
                        "revision": 3, "truncated": false
                    }
                }),
            ),
        }
    });
    let client = fake.client().await;

    let current = client.pane_current(Some("w1:p1")).await.expect("current");
    assert_eq!(current.pane_id, "w1:p1");
    assert_eq!(fake.last_params(), json!({ "caller_pane_id": "w1:p1" }));

    let read = client
        .pane_read(&PaneRead::new("w1:p1", ReadSource::RecentUnwrapped).lines(50))
        .await
        .expect("read");
    assert_eq!(read.text, "hello\n");
    assert_eq!(read.source, Some(ReadSource::Recent));
    assert_eq!(
        fake.last_params(),
        json!({ "pane_id": "w1:p1", "source": "recent_unwrapped", "lines": 50 })
    );
}

#[tokio::test]
async fn plugin_pane_open_popup_has_no_pane() {
    let fake = Fake::spawn(|req| {
        let placement = req.pointer("/params/placement").and_then(Value::as_str);
        // Popup launches answer `ok`; a popup has no pane id at all.
        if placement == Some("popup") {
            ok(req, json!({ "type": "ok" }))
        } else {
            ok(
                req,
                json!({
                    "type": "plugin_pane_opened",
                    "plugin_pane": { "plugin_id": "p", "entrypoint": "browser", "pane": pane("w1:p4") }
                }),
            )
        }
    });
    let client = fake.client().await;

    let popup = client
        .plugin_pane_open(
            &PluginPaneOpen::new("herdr-session-manager", "browser")
                .placement(PluginPanePlacement::Popup)
                .width(PopupSize::percent(85))
                .height(PopupSize::Cells(40)),
        )
        .await
        .expect("open popup");
    assert!(popup.is_none());
    assert_eq!(
        fake.last_params(),
        json!({
            "plugin_id": "herdr-session-manager",
            "entrypoint": "browser",
            "placement": "popup",
            "width": "85%",
            "height": 40
        })
    );

    let split = client
        .plugin_pane_open(
            &PluginPaneOpen::new("herdr-session-manager", "browser")
                .placement(PluginPanePlacement::Split)
                .direction(SplitDirection::Right)
                .focus(true),
        )
        .await
        .expect("open split");
    assert_eq!(split.expect("pane").pane.pane_id, "w1:p4");
}

#[tokio::test]
async fn popup_close_maps_error_code() {
    let fake = Fake::spawn(|req| {
        vec![json!({
            "id": req.get("id"),
            "error": { "code": "popup_not_open", "message": "no popup" }
        })]
    });
    let err = fake
        .client()
        .await
        .popup_close()
        .await
        .expect_err("should fail");
    assert!(err.is_code("popup_not_open"), "{err}");
    assert!(matches!(err, Error::Api(_)));
}

#[tokio::test]
async fn unmatched_line_is_skipped() {
    let fake = Fake::spawn(|req| {
        vec![
            json!({ "id": "hc0", "result": { "type": "pong", "version": "stale" } }),
            json!({ "id": req.get("id"), "result": { "type": "pong", "version": "0.9.0" } }),
        ]
    });
    assert_eq!(fake.client().await.ping().await.expect("ping"), "0.9.0");
}

#[tokio::test]
async fn missing_result_field_is_reported_with_the_method() {
    let fake = Fake::spawn(|req| ok(req, json!({ "type": "agent_list" })));
    let err = fake
        .client()
        .await
        .agent_list()
        .await
        .expect_err("should fail");
    assert!(matches!(
        err,
        Error::UnexpectedResult { ref method, ref field } if method == "agent.list" && field == "agents"
    ));
}

#[tokio::test]
async fn request_times_out_without_a_reply() {
    let fake = Fake::spawn(|_| Vec::new());
    let client = fake.client().await.with_timeout(Duration::from_millis(100));
    let err = client.ping().await.expect_err("should time out");
    assert!(matches!(err, Error::Timeout { .. }), "{err}");
}

#[tokio::test]
async fn subscribe_acks_then_streams_events() {
    let fake = Fake::spawn(|req| {
        vec![
            json!({ "id": req.get("id"), "result": { "type": "subscription_started" } }),
            json!({ "event": "pane_updated", "data": { "pane_id": "w1:p1", "agent_status": "working" } }),
            json!({ "event": "layout_updated", "data": { "tab_id": "w1:t1" } }),
        ]
    });
    let mut events = EventStream::connect(
        &fake.path,
        &[
            Subscription::new(kind::PANE_UPDATED),
            Subscription::new(kind::LAYOUT_UPDATED),
        ],
    )
    .await
    .expect("subscribe");

    assert_eq!(
        fake.last_params(),
        json!({ "subscriptions": [{ "type": "pane.updated" }, { "type": "layout.updated" }] })
    );

    let first = events
        .next_event_timeout(Duration::from_secs(2))
        .await
        .expect("read")
        .expect("event");
    assert!(first.is(kind::PANE_UPDATED));
    assert_eq!(first.pane_id(), Some("w1:p1"));

    let second = events
        .next_event_timeout(Duration::from_secs(2))
        .await
        .expect("read")
        .expect("event");
    assert!(second.is(kind::LAYOUT_UPDATED));
    assert_eq!(second.tab_id(), Some("w1:t1"));
}

#[tokio::test]
async fn subscribe_surfaces_a_rejected_subscription() {
    let fake = Fake::spawn(|req| {
        vec![json!({
            "id": req.get("id"),
            "error": { "code": "invalid_params", "message": "unknown subscription" }
        })]
    });
    let err = EventStream::connect(&fake.path, &[Subscription::new("pane.nope")])
        .await
        .expect_err("should fail");
    assert!(err.is_code("invalid_params"), "{err}");
}
