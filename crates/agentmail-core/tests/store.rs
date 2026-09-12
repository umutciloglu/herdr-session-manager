mod common;

use std::path::PathBuf;

use agentmail_core::domain::{Address, Harness, Message, MessageStatus, Registration};
use agentmail_core::{Paths, Store};
use common::FakeProbe;

fn temp_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = Paths::new(dir.path());
    let store = Store::open_at(&paths).expect("open");
    (dir, store)
}

fn addr(h: Harness, id: &str) -> Address {
    Address::new(h, id)
}

#[test]
fn opens_and_migrates_on_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = Paths::new(dir.path());
    let store = Store::open_at(&paths).expect("open");
    assert_eq!(store.schema_version().expect("version"), 1);
    assert!(paths.db().exists());

    // Re-opening an existing db must be a no-op, not a failed migration.
    drop(store);
    let store = Store::open_at(&paths).expect("reopen");
    assert_eq!(store.schema_version().expect("version"), 1);
}

#[test]
fn message_round_trip_and_status_transitions() {
    let (_dir, store) = temp_store();
    let me = addr(Harness::Codex, "codex-session-1");
    let peer = addr(Harness::Claude, "claude-session-1");

    let msg = Message::new(peer.clone(), me.clone(), "ping").expecting_reply(true);
    store.enqueue(&msg).expect("enqueue");

    let got = store.get(&msg.id).expect("get").expect("present");
    assert_eq!(got, msg);
    assert_eq!(got.status, MessageStatus::Pending);
    assert!(got.expects_reply);

    assert_eq!(store.pending_for(&me).expect("pending").len(), 1);
    assert!(store.pending_for(&peer).expect("pending").is_empty());

    store
        .mark_delivered(std::slice::from_ref(&msg.id))
        .expect("deliver");
    let got = store.get(&msg.id).expect("get").expect("present");
    assert_eq!(got.status, MessageStatus::Delivered);
    assert!(got.delivered_at.is_some());
    assert!(store.pending_for(&me).expect("pending").is_empty());

    // Delivered is not read: wait must still hand it over.
    let next = store.next_for(&me, None).expect("next").expect("present");
    assert_eq!(next.id, msg.id);

    store
        .mark_read(std::slice::from_ref(&msg.id))
        .expect("read");
    assert_eq!(
        store.get(&msg.id).expect("get").expect("present").status,
        MessageStatus::Read
    );
    assert!(store.next_for(&me, None).expect("next").is_none());
}

#[test]
fn next_for_is_oldest_first_and_filters_by_reply_to() {
    let (_dir, store) = temp_store();
    let me = addr(Harness::Claude, "me");
    let peer = addr(Harness::Codex, "peer");

    let first = Message::new(peer.clone(), me.clone(), "first");
    let second = Message::new(peer.clone(), me.clone(), "second").in_reply_to(Some("01ABC".into()));
    store.enqueue(&first).expect("enqueue");
    store.enqueue(&second).expect("enqueue");

    assert_eq!(
        store
            .next_for(&me, None)
            .expect("next")
            .expect("present")
            .id,
        first.id
    );
    assert_eq!(
        store
            .next_for(&me, Some("01ABC"))
            .expect("next")
            .expect("present")
            .id,
        second.id
    );
    assert!(store.next_for(&me, Some("nope")).expect("next").is_none());
}

#[test]
fn failed_messages_keep_their_row_and_reason() {
    let (_dir, store) = temp_store();
    let me = addr(Harness::Claude, "me");
    let msg = Message::new(
        me.clone(),
        addr(Harness::Other("alias".into()), "ghost"),
        "hi",
    );
    store.enqueue(&msg).expect("enqueue");
    store
        .mark_failed(&msg.id, "ambiguous address")
        .expect("fail");

    let got = store.get(&msg.id).expect("get").expect("present");
    assert_eq!(got.status, MessageStatus::Failed);
    assert_eq!(got.error.as_deref(), Some("ambiguous address"));
}

#[test]
fn inbox_and_outbox() {
    let (_dir, store) = temp_store();
    let me = addr(Harness::Claude, "me");
    let peer = addr(Harness::Codex, "peer");

    let incoming = Message::new(peer.clone(), me.clone(), "in");
    let outgoing = Message::new(me.clone(), peer.clone(), "out");
    store.enqueue(&incoming).expect("enqueue");
    store.enqueue(&outgoing).expect("enqueue");
    store
        .mark_read(std::slice::from_ref(&incoming.id))
        .expect("read");

    assert!(store.inbox(&me, false, 10).expect("inbox").is_empty());
    assert_eq!(store.inbox(&me, true, 10).expect("inbox").len(), 1);
    assert_eq!(store.outbox(&me, 10).expect("outbox").len(), 1);
}

#[test]
fn retarget_moves_a_placeholder_to_a_real_address() {
    let (_dir, store) = temp_store();
    let me = addr(Harness::Claude, "me");
    let placeholder = addr(Harness::Other("alias".into()), "reviewer");
    let real = addr(Harness::Codex, "codex-7");

    let msg = Message::new(me, placeholder, "hi");
    store.enqueue(&msg).expect("enqueue");
    store.retarget(&msg.id, &real).expect("retarget");

    assert_eq!(store.get(&msg.id).expect("get").expect("present").to, real);
    assert_eq!(store.pending_for(&real).expect("pending").len(), 1);
}

#[test]
fn cursors_round_trip() {
    let (_dir, store) = temp_store();
    let me = addr(Harness::Claude, "me");
    assert!(store.cursor(&me).expect("cursor").is_none());
    store.set_cursor(&me, "01ABC").expect("set");
    store.set_cursor(&me, "01DEF").expect("set");
    assert_eq!(store.cursor(&me).expect("cursor").as_deref(), Some("01DEF"));
}

#[test]
fn registry_round_trip_and_upsert() {
    let (_dir, store) = temp_store();
    let mut reg = Registration::new(Harness::Claude, "claude-1", PathBuf::from("/work/proj"));
    reg.pid = Some(4242);
    reg.alias = Some("reviewer".into());
    reg.poke_path = Some("/tmp/poke.sock".into());
    reg.herdr_pane = Some("pane-9".into());
    store.register(&reg).expect("register");

    let got = store
        .get_registration(&Harness::Claude, "claude-1")
        .expect("get")
        .expect("present");
    assert_eq!(got.alias.as_deref(), Some("reviewer"));
    assert_eq!(got.pid, Some(4242));
    assert_eq!(got.cwd, PathBuf::from("/work/proj"));
    assert_eq!(got.herdr_pane.as_deref(), Some("pane-9"));

    // A hook re-registering without a pid must not erase what the MCP process knew.
    let bare = Registration::new(Harness::Claude, "claude-1", PathBuf::from("/work/proj"));
    store.register(&bare).expect("re-register");
    let got = store
        .get_registration(&Harness::Claude, "claude-1")
        .expect("get")
        .expect("present");
    assert_eq!(got.pid, Some(4242));
    assert_eq!(got.alias.as_deref(), Some("reviewer"));

    store
        .deregister(&Harness::Claude, "claude-1")
        .expect("deregister");
    assert!(store
        .get_registration(&Harness::Claude, "claude-1")
        .expect("get")
        .is_none());
}

#[test]
fn live_drops_dead_pids_and_keeps_pidless_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_at(&Paths::new(dir.path()))
        .expect("open")
        .with_probe(Box::new(FakeProbe(vec![100])));

    let mut alive = Registration::new(Harness::Claude, "alive", PathBuf::from("/a"));
    alive.pid = Some(100);
    let mut dead = Registration::new(Harness::Claude, "dead", PathBuf::from("/b"));
    dead.pid = Some(999);
    let hooked = Registration::new(Harness::Codex, "hooked", PathBuf::from("/c"));

    for r in [&alive, &dead, &hooked] {
        store.register(r).expect("register");
    }

    let mut live: Vec<_> = store
        .live()
        .expect("live")
        .into_iter()
        .map(|r| r.session_id)
        .collect();
    live.sort();
    assert_eq!(live, vec!["alive", "hooked"]);

    // The dead row is gone for good, not just filtered out.
    assert!(store
        .get_registration(&Harness::Claude, "dead")
        .expect("get")
        .is_none());
}

#[test]
fn prefix_and_alias_lookup() {
    let (_dir, store) = temp_store();
    let a = Registration::new(Harness::Claude, "8890a685-aaaa", PathBuf::from("/a"));
    let b = Registration::new(Harness::Claude, "8890a685-bbbb", PathBuf::from("/b"));
    let mut c = Registration::new(Harness::Codex, "cccccccc-cccc", PathBuf::from("/c"));
    c.alias = Some("Reviewer".into());
    for r in [&a, &b, &c] {
        store.register(r).expect("register");
    }

    assert_eq!(
        store
            .find_by_prefix(&Harness::Claude, "8890a685")
            .expect("prefix")
            .len(),
        2
    );
    assert_eq!(
        store
            .find_by_prefix(&Harness::Claude, "8890a685-a")
            .expect("prefix")
            .len(),
        1
    );
    // Shorter than the protocol minimum: refused rather than matched loosely.
    assert!(store
        .find_by_prefix(&Harness::Claude, "8890")
        .expect("prefix")
        .is_empty());
    // Prefixes never cross harnesses.
    assert!(store
        .find_by_prefix(&Harness::Codex, "8890a685")
        .expect("prefix")
        .is_empty());

    assert_eq!(
        store
            .find_alias("reviewer")
            .expect("alias")
            .expect("present")
            .session_id,
        "cccccccc-cccc"
    );
    assert!(store.find_alias("nobody").expect("alias").is_none());

    store
        .set_alias(&Harness::Claude, "8890a685-aaaa", Some("builder"))
        .expect("set alias");
    assert!(store.find_alias("builder").expect("alias").is_some());
    assert!(store
        .set_alias(&Harness::Claude, "missing", Some("x"))
        .is_err());
}
