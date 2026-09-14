//! Stop / SessionStart hook drains, driven by the JSON the harnesses really send.

use agentmail_core::{Address, Harness, Message, MessageStatus, Store};
use agentmail_delivery::{drain_stop, session_start, session_start_in};

const CLAUDE_ID: &str = "8890a685-1111-2222-3333-444444444444";
const CODEX_ID: &str = "01999b0e-2222-4444-8888-cccccccccccc";

fn claude_stop(active: bool) -> String {
    format!(
        r#"{{"session_id":"{CLAUDE_ID}",
            "transcript_path":"/Users/x/.claude/projects/-Users-x-proj/{CLAUDE_ID}.jsonl",
            "cwd":"/Users/x/proj","hook_event_name":"Stop",
            "stop_hook_active":{active},"is_idle_stop":false}}"#
    )
}

fn codex_stop() -> String {
    format!(
        r#"{{"session_id":"{CODEX_ID}","cwd":"/Users/x/proj","hook_event_name":"Stop",
            "transcript_path":"/Users/x/.codex/sessions/2026/09/12/rollout-{CODEX_ID}.jsonl"}}"#
    )
}

fn hook_with_transcript(path: &std::path::Path) -> String {
    // Encoded, not pasted: a Windows temp path is full of backslashes, which are JSON
    // escapes.
    let path = serde_json::to_string(&path.to_string_lossy()).expect("json string");
    format!(
        r#"{{"session_id":"{CLAUDE_ID}","cwd":"/Users/x/proj","hook_event_name":"Stop",
            "transcript_path":{path},"stop_hook_active":false}}"#
    )
}

fn store_with_mail(to: &Address, texts: &[&str]) -> Store {
    let store = Store::open_in_memory().expect("store");
    for text in texts {
        let msg = Message::new(
            Address::new(Harness::Claude, "sender-0001"),
            to.clone(),
            *text,
        );
        store.enqueue(&msg).expect("enqueue");
    }
    store
}

#[test]
fn claude_stop_blocks_with_the_envelope_batch() {
    let me = Address::new(Harness::Claude, CLAUDE_ID);
    let store = store_with_mail(&me, &["look at the auth middleware", "and the tests"]);

    let out = drain_stop(&store, &Harness::Claude, &claude_stop(false))
        .expect("drain")
        .expect("some output");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["decision"], "block");
    let reason = v["reason"].as_str().expect("reason");
    assert!(
        reason.contains("[agentmail] from claude:sender-0"),
        "{reason}"
    );
    assert!(reason.contains("look at the auth middleware"), "{reason}");
    assert!(reason.contains("and the tests"), "{reason}");
    assert!(reason.contains("\n\n---\n\n"), "batch separator missing");

    for msg in store.inbox(&me, true, 10).expect("inbox") {
        assert_eq!(msg.status, MessageStatus::Delivered);
    }
}

#[test]
fn codex_stop_uses_the_same_shape() {
    let me = Address::new(Harness::Codex, CODEX_ID);
    let store = store_with_mail(&me, &["ping"]);

    let out = drain_stop(&store, &Harness::Codex, &codex_stop())
        .expect("drain")
        .expect("some output");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["decision"], "block");
    assert!(v["reason"].as_str().expect("reason").contains("ping"));
}

#[test]
fn an_empty_mailbox_prints_nothing() {
    let store = Store::open_in_memory().expect("store");
    assert_eq!(
        drain_stop(&store, &Harness::Claude, &claude_stop(false)).expect("drain"),
        None
    );
}

#[test]
fn stop_hook_active_cannot_loop_on_the_same_mail() {
    let me = Address::new(Harness::Claude, CLAUDE_ID);
    let store = store_with_mail(&me, &["first"]);

    // The turn we caused ends with stop_hook_active set; the mail it carried is
    // already Delivered, so there is nothing to block on a second time.
    assert!(drain_stop(&store, &Harness::Claude, &claude_stop(false))
        .expect("drain")
        .is_some());
    assert_eq!(
        drain_stop(&store, &Harness::Claude, &claude_stop(true)).expect("drain"),
        None
    );

    // Mail that arrived during that turn is new and still gets through.
    let msg = Message::new(
        Address::new(Harness::Codex, "peer-0001"),
        me.clone(),
        "second",
    );
    store.enqueue(&msg).expect("enqueue");
    let out = drain_stop(&store, &Harness::Claude, &claude_stop(true))
        .expect("drain")
        .expect("second batch");
    assert!(out.contains("second"));
}

#[test]
fn session_start_registers_the_session() {
    let store = Store::open_in_memory().expect("store");
    let addr = session_start(&store, &Harness::Claude, &claude_stop(false)).expect("session start");
    assert_eq!(addr, Address::new(Harness::Claude, CLAUDE_ID));

    let reg = store.find(&addr).expect("find").expect("registered");
    assert_eq!(reg.cwd, std::path::PathBuf::from("/Users/x/proj"));
    assert_eq!(reg.pid, None);
}

#[test]
fn session_start_records_the_herdr_pane() {
    let store = Store::open_in_memory().expect("store");
    let addr = session_start_in(
        &store,
        &Harness::Claude,
        &claude_stop(false),
        Some("w1:p3".into()),
    )
    .expect("session start");

    let reg = store.find(&addr).expect("find").expect("registered");
    assert_eq!(reg.herdr_pane.as_deref(), Some("w1:p3"));
}

#[test]
fn session_start_hands_pane_parked_mail_to_the_real_session() {
    let store = Store::open_in_memory().expect("store");
    let parked = agentmail_delivery::pane_address(&Harness::Codex, "w1:p9");
    let msg = Message::new(
        Address::new(Harness::Claude, "sender-0001"),
        parked.clone(),
        "look at the parser",
    );
    store.enqueue(&msg).expect("enqueue");

    let addr = session_start_in(&store, &Harness::Codex, &codex_stop(), Some("w1:p9".into()))
        .expect("session start");

    assert!(store.pending_for(&parked).expect("pending").is_empty());
    let landed = store.pending_for(&addr).expect("pending");
    assert_eq!(landed.len(), 1);
    assert_eq!(landed[0].id, msg.id);
}

#[test]
fn session_start_leaves_another_panes_mail_alone() {
    let store = Store::open_in_memory().expect("store");
    let other = agentmail_delivery::pane_address(&Harness::Codex, "w1:p4");
    let msg = Message::new(
        Address::new(Harness::Claude, "sender-0001"),
        other.clone(),
        "not for this pane",
    );
    store.enqueue(&msg).expect("enqueue");

    session_start_in(&store, &Harness::Codex, &codex_stop(), Some("w1:p9".into()))
        .expect("session start");
    assert_eq!(store.pending_for(&other).expect("pending").len(), 1);
}

#[test]
fn session_start_delivers_nothing() {
    let me = Address::new(Harness::Codex, CODEX_ID);
    let store = store_with_mail(&me, &["wait for me"]);
    session_start(&store, &Harness::Codex, &codex_stop()).expect("session start");
    assert_eq!(store.pending_for(&me).expect("pending").len(), 1);
}

/// A channel push that the model really read is in the session transcript. Repeating it
/// at turn end would be the same words twice.
#[test]
fn a_pushed_row_the_transcript_mentions_is_not_delivered_again() {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript = dir.path().join("session.jsonl");
    let me = Address::new(Harness::Claude, CLAUDE_ID);
    let store = store_with_mail(&me, &["already seen"]);

    let msg = store.pending_for(&me).expect("pending").remove(0);
    store
        .mark_pushed(std::slice::from_ref(&msg.id))
        .expect("mark pushed");
    std::fs::write(
        &transcript,
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":"[agentmail] from codex:x · id {} · reply expected"}}}}"#,
            msg.id
        ),
    )
    .expect("write transcript");

    let hook = hook_with_transcript(&transcript);
    assert_eq!(
        drain_stop(&store, &Harness::Claude, &hook).expect("drain"),
        None,
        "the model has it already"
    );
    assert_eq!(
        store.get(&msg.id).expect("get").expect("row").status,
        MessageStatus::Read
    );
}

#[test]
fn a_pushed_row_the_transcript_never_saw_is_delivered() {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript = dir.path().join("session.jsonl");
    let me = Address::new(Harness::Claude, CLAUDE_ID);
    let store = store_with_mail(&me, &["never arrived"]);

    let msg = store.pending_for(&me).expect("pending").remove(0);
    store
        .mark_pushed(std::slice::from_ref(&msg.id))
        .expect("mark pushed");
    // A session that was not launched with channels: the event is nowhere in its turn.
    std::fs::write(
        &transcript,
        r#"{"type":"assistant","message":{"role":"assistant","content":"working on the parser"}}"#,
    )
    .expect("write transcript");

    let out = drain_stop(&store, &Harness::Claude, &hook_with_transcript(&transcript))
        .expect("drain")
        .expect("a batch");
    assert!(out.contains("never arrived"), "{out}");
    assert_eq!(
        store.get(&msg.id).expect("get").expect("row").status,
        MessageStatus::Delivered
    );
}

#[test]
fn a_row_that_was_never_pushed_ignores_the_transcript() {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript = dir.path().join("session.jsonl");
    let me = Address::new(Harness::Claude, CLAUDE_ID);
    let store = store_with_mail(&me, &["plain mail"]);
    let msg = store.pending_for(&me).expect("pending").remove(0);
    // The id is in the transcript, but nothing ever pushed it: the mention is somebody
    // else's business (a tool result, a quote), not proof of delivery.
    std::fs::write(&transcript, format!("mentions {} in passing", msg.id)).expect("write");

    let out = drain_stop(&store, &Harness::Claude, &hook_with_transcript(&transcript))
        .expect("drain")
        .expect("a batch");
    assert!(out.contains("plain mail"), "{out}");
}

#[test]
fn malformed_input_is_an_error_not_a_panic() {
    let store = Store::open_in_memory().expect("store");
    assert!(drain_stop(&store, &Harness::Claude, "not json").is_err());
}
