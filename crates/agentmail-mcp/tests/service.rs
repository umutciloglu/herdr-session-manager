//! The MCP service over an in-memory store: what a client sees, without a transport.

use std::path::PathBuf;
use std::sync::Arc;

use agentmail_core::{
    Address, AgentState, Config, Directory, DirectoryEntry, Harness, Mailbox, Message,
    MessageStatus, Registration, Resolver, SessionCard, SessionProvider, Store,
};
use agentmail_mcp::identity::Identity;
use agentmail_mcp::server_info;
use agentmail_mcp::service::{tools, ChannelMessage, DirectorySource, Service};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Notify;

const ME: &str = "8890a685-1111-2222-3333-444444444444";
const PEER: &str = "01999b0e-2222-4444-8888-cccccccccccc";

fn identity(harness: Harness, id: &str) -> Identity {
    Identity {
        harness,
        session_id: id.to_string(),
        cwd: PathBuf::from("/repo/trade-help"),
        pid: std::process::id(),
        herdr_pane: None,
        provisional: false,
    }
}

fn service(harness: Harness) -> (Arc<Store>, Service) {
    let store = Arc::new(Store::open_in_memory().expect("store"));
    let mailbox = Mailbox::new(
        Arc::clone(&store),
        Resolver::new(Arc::clone(&store)),
        Vec::new(),
    );
    let svc = Service::new(
        identity(harness, ME),
        Arc::clone(&store),
        mailbox,
        Config::default(),
    );
    (store, svc)
}

struct FakeProvider;

impl SessionProvider for FakeProvider {
    fn search(
        &self,
        query: &str,
        _harness: Option<&Harness>,
        _project: Option<&str>,
        _limit: usize,
    ) -> agentmail_core::Result<Vec<SessionCard>> {
        Ok(vec![SessionCard {
            address: "claude:cafe0000-1111-2222-3333-444444444444".into(),
            harness: "claude".into(),
            project: Some("trade-help".into()),
            title: Some(format!("about {query}")),
            last_active: Some("2026-09-12T07:55:00Z".into()),
            first_prompt: Some("how does the auth middleware work?".into()),
            resumable: true,
            ..SessionCard::default()
        }])
    }

    fn recent(&self, _limit: usize) -> agentmail_core::Result<Vec<SessionCard>> {
        self.search("recent", None, None, 1)
    }

    fn transcript(&self, _address: &Address) -> agentmail_core::Result<Option<String>> {
        Ok(Some("user: hello\nassistant: hi".into()))
    }
}

/// A provider that answers about one specific address, the way the session index does
/// for a session herdr can also see.
struct ProviderKnowing(String);

impl SessionProvider for ProviderKnowing {
    fn search(
        &self,
        _query: &str,
        _harness: Option<&Harness>,
        _project: Option<&str>,
        _limit: usize,
    ) -> agentmail_core::Result<Vec<SessionCard>> {
        self.recent(1)
    }

    fn recent(&self, _limit: usize) -> agentmail_core::Result<Vec<SessionCard>> {
        Ok(vec![SessionCard {
            address: self.0.clone(),
            harness: "codex".into(),
            project: Some("trade-help".into()),
            title: Some("fixing the parser".into()),
            started: Some("2026-09-10T08:00:00Z".into()),
            last_active: Some("2026-09-12T07:55:00Z".into()),
            first_prompt: Some("the parser drops trailing commas".into()),
            resumable: true,
            ..SessionCard::default()
        }])
    }

    fn transcript(&self, _address: &Address) -> agentmail_core::Result<Option<String>> {
        Ok(None)
    }
}

struct FakeDirectory;

impl Directory for FakeDirectory {
    fn live_agents(&self) -> Vec<DirectoryEntry> {
        vec![DirectoryEntry {
            address: Some(Address::new(Harness::Codex, PEER)),
            alias: Some("reviewer".into()),
            state: AgentState::Idle,
            cwd: Some(PathBuf::from("/repo/trade-help")),
            title: Some("fixing the parser".into()),
            pane: Some("w1:p3".into()),
        }]
    }
}

#[async_trait]
impl DirectorySource for FakeDirectory {
    async fn refresh(&self) {}
    fn as_directory(&self) -> &dyn Directory {
        self
    }
}

#[test]
fn initialize_result_declares_the_claude_channel() {
    let info = serde_json::to_value(server_info(&Harness::Claude)).expect("json");
    assert_eq!(
        info["capabilities"]["experimental"]["claude/channel"],
        json!({})
    );
    assert_eq!(
        info["capabilities"]["resources"]["listChanged"],
        json!(true)
    );
    assert_eq!(info["capabilities"]["tools"], json!({}));
    assert_eq!(info["serverInfo"]["name"], "agentmail");
    let instructions = info["instructions"].as_str().expect("instructions");
    assert!(instructions.contains("agentmail_reply"), "{instructions}");
    assert!(instructions.len() > 300);
}

#[test]
fn codex_gets_no_channel_capability() {
    let info = serde_json::to_value(server_info(&Harness::Codex)).expect("json");
    assert!(info["capabilities"].get("experimental").is_none());
}

#[test]
fn the_tool_list_is_the_documented_one() {
    let names: Vec<&str> = tools().iter().map(|t| t.name).collect();
    assert_eq!(
        names,
        [
            "agentmail_send",
            "agentmail_reply",
            "agentmail_wait",
            "agentmail_find_session"
        ]
    );
    for tool in tools() {
        assert_eq!(tool.schema["type"], "object", "{}", tool.name);
        assert!(!tool.description.is_empty());
    }
    assert_eq!(tools()[0].schema["required"], json!(["to", "text"]));
}

#[tokio::test]
async fn send_queues_for_a_session_that_is_not_running() {
    let (store, svc) = service(Harness::Claude);
    let out = svc
        .call_tool(
            "agentmail_send",
            &json!({"to": format!("codex:{PEER}"), "text": "have a look at the parser"}),
        )
        .await
        .expect("send");

    assert_eq!(out["outcome"], "queued");
    assert_eq!(out["to"], format!("codex:{PEER}"));
    let id = out["message_id"].as_str().expect("message id");

    let pending = store
        .pending_for(&Address::new(Harness::Codex, PEER))
        .expect("pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, id);
}

#[tokio::test]
async fn send_to_a_live_claude_peer_defers_to_native_messaging() {
    let (store, svc) = service(Harness::Claude);
    let svc = svc.serving_mcp();
    let mut peer = Registration::new(
        Harness::Claude,
        "7777aaaa-1111-2222-3333-444444444444",
        "/repo",
    );
    peer.alias = Some("planner".into());
    // Only a peer something can actually reach counts as "use native instead".
    peer.poke_path = Some("/tmp/agentmail-native-test.sock".into());
    store.register(&peer).expect("register");

    let out = svc
        .call_tool(
            "agentmail_send",
            &json!({"to": "claude:7777aaaa", "text": "ping"}),
        )
        .await
        .expect("send");

    assert_eq!(out["outcome"], "use_native");
    assert_eq!(out["peer"], "planner");
    assert_eq!(out["message_id"], Value::Null);
    // Nothing was enqueued: the model is being told to use another channel entirely.
    assert!(store
        .inbox(&peer.address(), true, 10)
        .expect("inbox")
        .is_empty());
}

/// The CLI, the hooks and every other non-MCP caller have no native messaging to fall
/// back to, so they must deliver — `use_native` is only an answer an interactive Claude
/// session can act on.
#[tokio::test]
async fn the_cli_path_delivers_to_a_live_claude_peer() {
    use agentmail_core::PokeListener;
    use agentmail_delivery::PokeDeliverer;

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(Store::open_in_memory().expect("store"));
    let mut peer = Registration::new(
        Harness::Claude,
        "7777aaaa-1111-2222-3333-444444444444",
        "/repo",
    );
    peer.poke_path = Some(dir.path().join("peer.sock").to_string_lossy().into_owned());
    store.register(&peer).expect("register");
    let mut listener = PokeListener::bind(&peer).expect("bind");

    let mailbox = Mailbox::new(
        Arc::clone(&store),
        Resolver::new(Arc::clone(&store)),
        vec![Box::new(PokeDeliverer::new())],
    );
    // No `serving_mcp()`: this is the shape the CLI builds.
    let svc = Service::new(
        identity(Harness::Claude, ME),
        Arc::clone(&store),
        mailbox,
        Config::default(),
    );

    let woke = tokio::spawn(async move { listener.next().await });
    let out = svc
        .call_tool(
            "agentmail_send",
            &json!({"to": "claude:7777aaaa", "text": "ping"}),
        )
        .await
        .expect("send");

    assert_eq!(out["outcome"], "queued", "delivered, not deferred: {out}");
    tokio::time::timeout(std::time::Duration::from_secs(5), woke)
        .await
        .expect("the peer should have been poked")
        .expect("join");
    assert_eq!(
        store.pending_for(&peer.address()).expect("pending").len(),
        1,
        "the row waits for the peer's own drain"
    );
}

#[tokio::test]
async fn a_codex_caller_never_gets_the_native_hint() {
    let (store, svc) = service(Harness::Codex);
    let svc = svc.serving_mcp();
    let peer = Registration::new(
        Harness::Claude,
        "7777aaaa-1111-2222-3333-444444444444",
        "/repo",
    );
    store.register(&peer).expect("register");

    let out = svc
        .call_tool(
            "agentmail_send",
            &json!({"to": "claude:7777aaaa", "text": "ping"}),
        )
        .await
        .expect("send");
    assert_eq!(out["outcome"], "queued");
}

#[tokio::test]
async fn reply_answers_the_sender_and_marks_the_original_read() {
    let (store, svc) = service(Harness::Claude);
    let incoming = Message::new(
        Address::new(Harness::Codex, PEER),
        Address::new(Harness::Claude, ME),
        "what does the middleware do?",
    )
    .expecting_reply(true);
    store.enqueue(&incoming).expect("enqueue");

    let out = svc
        .call_tool(
            "agentmail_reply",
            &json!({"message_id": incoming.id, "text": "it validates the bearer token"}),
        )
        .await
        .expect("reply");

    assert_eq!(out["to"], format!("codex:{PEER}"));
    assert_eq!(
        store.get(&incoming.id).expect("get").expect("row").status,
        MessageStatus::Read
    );
    let back = store
        .pending_for(&Address::new(Harness::Codex, PEER))
        .expect("pending");
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].reply_to.as_deref(), Some(incoming.id.as_str()));
}

#[tokio::test]
async fn wait_hands_over_the_next_message() {
    let (store, svc) = service(Harness::Claude);
    let msg = Message::new(
        Address::new(Harness::Codex, PEER),
        Address::new(Harness::Claude, ME),
        "the parser is fixed",
    );
    store.enqueue(&msg).expect("enqueue");

    let out = svc
        .call_tool("agentmail_wait", &json!({"timeout_s": 5}))
        .await
        .expect("wait");
    assert_eq!(out["timed_out"], false);
    assert_eq!(out["message"]["text"], "the parser is fixed");
    assert_eq!(out["message"]["from"], format!("codex:{PEER}"));
}

#[tokio::test]
async fn a_poke_wakes_a_blocked_wait_before_the_next_poll() {
    let (store, svc) = service(Harness::Claude);
    let wake = Arc::new(Notify::new());
    let svc = svc.with_signals(Arc::clone(&wake), Arc::new(Notify::new()));

    // The socket owner rings continuously; mail lands well after the first ring, so
    // only the wake path can pick it up before the mailbox's own 250 ms poll would.
    let ringer = tokio::spawn({
        let wake = Arc::clone(&wake);
        async move {
            for _ in 0..200 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                wake.notify_waiters();
            }
        }
    });
    let poster = tokio::spawn({
        let store = Arc::clone(&store);
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            let msg = Message::new(
                Address::new(Harness::Codex, PEER),
                Address::new(Harness::Claude, ME),
                "late but awaited",
            );
            store.enqueue(&msg).expect("enqueue");
        }
    });

    let started = std::time::Instant::now();
    let out = svc
        .call_tool("agentmail_wait", &json!({"timeout_s": 10}))
        .await
        .expect("wait");
    let elapsed = started.elapsed();
    ringer.abort();
    poster.await.expect("poster");

    assert_eq!(out["message"]["text"], "late but awaited");
    assert!(
        elapsed < std::time::Duration::from_millis(200),
        "a poke should beat the poll, took {elapsed:?}"
    );
}

#[tokio::test]
async fn a_provisional_identity_is_adopted_once_the_hook_row_lands() {
    let store = Arc::new(Store::open_in_memory().expect("store"));
    let mailbox = Mailbox::new(
        Arc::clone(&store),
        Resolver::new(Arc::clone(&store)),
        Vec::new(),
    );
    let mut provisional = identity(Harness::Claude, "unk4242");
    provisional.provisional = true;
    provisional.herdr_pane = Some("w1:p3".into());
    let svc = Service::new(provisional, Arc::clone(&store), mailbox, Config::default());
    assert_eq!(svc.resolved_identity(), None);

    let mut hook_row = Registration::new(Harness::Claude, "real-session", "/repo/trade-help");
    hook_row.herdr_pane = Some("w1:p3".into());
    store.register(&hook_row).expect("register");

    let found = svc.resolved_identity().expect("resolved");
    assert_eq!(found.session_id, "real-session");
    svc.adopt_identity(found);
    assert_eq!(svc.me(), Address::new(Harness::Claude, "real-session"));
    assert_eq!(svc.resolved_identity(), None, "only ever adopted once");
}

#[tokio::test]
async fn wait_times_out_without_mail() {
    let (_store, svc) = service(Harness::Claude);
    let out = svc
        .call_tool("agentmail_wait", &json!({"timeout_s": 0}))
        .await
        .expect("wait");
    assert_eq!(out["timed_out"], true);
    assert!(out.get("message").is_none());
}

#[tokio::test]
async fn find_session_merges_registry_directory_and_provider() {
    let (store, svc) = service(Harness::Claude);
    store
        .register(&Registration::new(
            Harness::Claude,
            "beef0000-1111-2222-3333-444444444444",
            "/repo/trade-help",
        ))
        .expect("register");
    let svc = svc
        .with_provider(Arc::new(FakeProvider))
        .with_directory(Arc::new(FakeDirectory));

    let out = svc
        .call_tool(
            "agentmail_find_session",
            &json!({"query": "trade-help", "limit": 10}),
        )
        .await
        .expect("find");
    let sessions = out["sessions"].as_array().expect("sessions");
    let addresses: Vec<&str> = sessions
        .iter()
        .filter_map(|s| s["address"].as_str())
        .collect();
    assert!(
        addresses.contains(&"claude:beef0000-1111-2222-3333-444444444444"),
        "{addresses:?}"
    );
    assert!(
        addresses.contains(&format!("codex:{PEER}").as_str()),
        "{addresses:?}"
    );
    assert!(
        addresses.iter().any(|a| a.starts_with("claude:cafe0000")),
        "{addresses:?}"
    );
}

#[tokio::test]
async fn resources_list_live_sessions_and_read_a_card() {
    let (store, svc) = service(Harness::Claude);
    store
        .register(&Registration::new(
            Harness::Claude,
            "beef0000-1111-2222-3333-444444444444",
            "/repo/trade-help",
        ))
        .expect("register");
    let svc = svc.with_directory(Arc::new(FakeDirectory));

    let resources = svc.resources().await;
    let uris: Vec<&str> = resources.iter().map(|r| r.uri.as_str()).collect();
    assert!(uris.contains(&"agentmail://session/claude:beef0000-1111-2222-3333-444444444444"));
    assert!(uris.contains(&format!("agentmail://session/codex:{PEER}").as_str()));

    let codex = resources
        .iter()
        .find(|r| r.uri.contains("codex:"))
        .expect("the codex resource");
    assert!(
        codex
            .name
            .starts_with("codex · trade-help · fixing the parser · "),
        "{}",
        codex.name
    );
    assert_eq!(codex.mime, "text/markdown");

    let card = svc
        .read_resource(&format!("agentmail://session/codex:{PEER}"))
        .await
        .expect("card");
    assert!(card.starts_with(&format!("# codex:{PEER}")), "{card}");
    assert!(card.contains("- harness: codex"), "{card}");
    assert!(card.contains("agentmail_send"), "{card}");
}

#[tokio::test]
async fn a_live_session_borrows_its_title_and_age_from_the_index() {
    let (_store, svc) = service(Harness::Claude);
    let svc = svc
        .with_directory(Arc::new(FakeDirectory))
        .with_provider(Arc::new(ProviderKnowing(format!("codex:{PEER}"))));

    let cards = svc.sessions().await;
    let codex = cards
        .iter()
        .find(|c| c.address == format!("codex:{PEER}"))
        .expect("the codex session");
    assert_eq!(
        codex.state.as_deref(),
        Some("idle"),
        "liveness comes from herdr"
    );
    assert_eq!(
        codex.last_active.as_deref(),
        Some("2026-09-12T07:55:00Z"),
        "the timestamp comes from the index"
    );

    let entry = svc
        .resources()
        .await
        .into_iter()
        .find(|r| r.uri.contains(PEER))
        .expect("the codex resource");
    assert!(
        !entry.name.ends_with("· ?"),
        "a real age, not a shrug: {}",
        entry.name
    );
}

#[tokio::test]
async fn a_session_is_never_offered_its_own_address() {
    let (store, svc) = service(Harness::Claude);
    // The MCP process registers itself, exactly as `run` does at startup.
    store
        .register(&Registration::new(Harness::Claude, ME, "/repo/trade-help"))
        .expect("register");
    let svc = svc.with_directory(Arc::new(FakeDirectory));

    let uris: Vec<String> = svc.resources().await.into_iter().map(|r| r.uri).collect();
    assert!(
        !uris.iter().any(|u| u.contains(ME)),
        "resources/list must not offer the caller itself: {uris:?}"
    );
    assert!(
        uris.iter().any(|u| u.contains(PEER)),
        "peers are still listed"
    );

    let found = svc
        .call_tool("agentmail_find_session", &json!({"query": "trade-help"}))
        .await
        .expect("find");
    let addresses: Vec<String> = found["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .filter_map(|s| s["address"].as_str().map(str::to_string))
        .collect();
    assert!(!addresses.iter().any(|a| a.contains(ME)), "{addresses:?}");
}

#[tokio::test]
async fn reading_a_card_asks_the_index_about_that_exact_address() {
    let (_store, svc) = service(Harness::Claude);
    let svc = svc
        .with_directory(Arc::new(FakeDirectory))
        .with_provider(Arc::new(ProviderKnowing(format!("codex:{PEER}"))));

    let card = svc
        .read_resource(&format!("agentmail://session/codex:{PEER}"))
        .await
        .expect("card");
    // Liveness from herdr, everything else from the index.
    assert!(card.contains("- state: idle"), "{card}");
    assert!(card.contains("- project: trade-help"), "{card}");
    assert!(
        card.contains("- last active: 2026-09-12T07:55:00Z"),
        "{card}"
    );
    assert!(card.contains("the parser drops trailing commas"), "{card}");
}

#[tokio::test]
async fn the_transcript_template_reads_through_the_provider() {
    let (_store, svc) = service(Harness::Claude);
    let svc = svc.with_provider(Arc::new(FakeProvider));

    let templates = svc.resource_templates();
    assert_eq!(templates[0].uri, "agentmail://session/{address}/transcript");

    let text = svc
        .read_resource(&format!("agentmail://session/codex:{PEER}/transcript"))
        .await
        .expect("transcript");
    assert!(text.contains("assistant: hi"));
}

#[tokio::test]
async fn the_channel_drain_renders_envelopes_and_leaves_the_row_pending() {
    let (store, svc) = service(Harness::Claude);
    let msg = Message::new(
        Address::new(Harness::Codex, PEER),
        Address::new(Harness::Claude, ME),
        "the parser is fixed",
    )
    .expecting_reply(true);
    store.enqueue(&msg).expect("enqueue");

    let drained = svc.drain_channel().expect("drain");
    assert_eq!(drained.len(), 1);
    // Claude only routes channel events to a server the session was launched with as a
    // channel, and the server cannot tell. Marking it delivered here would lose it.
    let row = store.get(&msg.id).expect("get").expect("row");
    assert_eq!(
        row.status,
        MessageStatus::Pending,
        "the Stop hook must still be able to hand this over"
    );
    assert!(
        row.pushed_at.is_some(),
        "the attempt is recorded so the Stop hook can check the transcript for it"
    );
    // A second drain is empty: this process does not push the same row twice.
    assert!(svc.drain_channel().expect("drain").is_empty());

    let wire = drained[0].notification();
    assert_eq!(wire["jsonrpc"], "2.0");
    assert_eq!(wire["method"], "notifications/claude/channel");
    assert_eq!(wire["params"]["meta"]["from"], format!("codex:{PEER}"));
    assert_eq!(wire["params"]["meta"]["message_id"], msg.id);
    assert_eq!(wire["params"]["meta"]["expects_reply"], "true");
    let content = wire["params"]["content"].as_str().expect("content");
    assert!(
        content.starts_with("[agentmail] from codex:01999b0e"),
        "{content}"
    );
    assert!(content.contains("the parser is fixed"), "{content}");
    assert!(
        content.contains(&format!("agentmail_reply message_id={}", msg.id)),
        "{content}"
    );
}

/// A tool call is proof the model is awake with its context in front of it, which is the
/// only acknowledgement a channel push ever gets.
#[tokio::test]
async fn a_tool_call_acknowledges_what_the_channel_pushed() {
    let (store, svc) = service(Harness::Claude);
    let msg = Message::new(
        Address::new(Harness::Codex, PEER),
        Address::new(Harness::Claude, ME),
        "the parser is fixed",
    );
    store.enqueue(&msg).expect("enqueue");
    assert_eq!(svc.drain_channel().expect("drain").len(), 1);

    svc.call_tool("agentmail_find_session", &json!({"query": "anything"}))
        .await
        .expect("find");

    assert_eq!(
        store.get(&msg.id).expect("get").expect("row").status,
        MessageStatus::Read
    );
}

#[tokio::test]
async fn codex_sessions_do_not_use_the_channel() {
    let (store, svc) = service(Harness::Codex);
    let msg = Message::new(
        Address::new(Harness::Claude, "x-0000-0000"),
        Address::new(Harness::Codex, ME),
        "hello",
    );
    store.enqueue(&msg).expect("enqueue");

    assert!(svc.drain_channel().expect("drain").is_empty());
    // The row is untouched, so the Stop hook can still hand it over.
    assert_eq!(
        store.get(&msg.id).expect("get").expect("row").status,
        MessageStatus::Pending
    );
}

#[tokio::test]
async fn unknown_tools_and_bad_arguments_are_rejected() {
    let (_store, svc) = service(Harness::Claude);
    assert!(svc.call_tool("agentmail_nope", &json!({})).await.is_err());
    assert!(svc
        .call_tool("agentmail_send", &json!({"to": "claude:x"}))
        .await
        .is_err());
    assert!(svc
        .call_tool(
            "agentmail_send",
            &json!({"to": "claude:x", "text": "hi", "mode": "telepathy"})
        )
        .await
        .is_err());
}

#[test]
fn channel_params_are_a_flat_string_map() {
    let msg = ChannelMessage {
        content: "[agentmail] from codex:01999b0e · id 01JX\nbody".into(),
        from: "codex:01999b0e".into(),
        message_id: "01JX".into(),
        expects_reply: false,
    };
    assert_eq!(
        msg.params(),
        json!({
            "content": "[agentmail] from codex:01999b0e · id 01JX\nbody",
            "meta": {"from": "codex:01999b0e", "message_id": "01JX", "expects_reply": "false"}
        })
    );
}
