mod common;

use std::path::PathBuf;

use agentmail_core::domain::{Address, Harness, Message, MessageStatus, Registration};
use agentmail_core::store::{Liveness, HOOK_ROW_TTL, PRUNE_AFTER};
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
    assert_eq!(store.schema_version().expect("version"), 2);
    assert!(paths.db().exists());

    // Re-opening an existing db must be a no-op, not a failed migration.
    drop(store);
    let store = Store::open_at(&paths).expect("reopen");
    assert_eq!(store.schema_version().expect("version"), 2);
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

fn hook_row(harness: Harness, id: &str, age: chrono::Duration) -> Registration {
    let mut reg = Registration::new(harness, id, PathBuf::from("/h"));
    reg.last_seen -= age;
    reg
}

#[test]
fn live_drops_dead_pids_and_keeps_fresh_pidless_rows() {
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

#[test]
fn liveness_classifies_every_kind_of_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_at(&Paths::new(dir.path()))
        .expect("open")
        .with_probe(Box::new(FakeProbe(vec![100])));

    let mut proven = Registration::new(Harness::Claude, "proven", PathBuf::from("/a"));
    proven.pid = Some(100);
    let mut dead = Registration::new(Harness::Claude, "dead", PathBuf::from("/a"));
    dead.pid = Some(999);

    assert_eq!(store.liveness(&proven), Liveness::Proven);
    assert_eq!(store.liveness(&dead), Liveness::Gone);
    assert_eq!(
        store.liveness(&hook_row(
            Harness::Codex,
            "fresh",
            chrono::Duration::minutes(1)
        )),
        Liveness::Unproven
    );
    assert_eq!(
        store.liveness(&hook_row(
            Harness::Codex,
            "stale",
            HOOK_ROW_TTL + chrono::Duration::seconds(1)
        )),
        Liveness::Gone
    );
    // A person is never a process.
    assert_eq!(
        store.liveness(&hook_row(
            Harness::Other(Harness::HUMAN.into()),
            "umut",
            chrono::Duration::zero()
        )),
        Liveness::Gone
    );
}

#[test]
fn live_ignores_a_stale_hook_row_but_keeps_it_addressable() {
    let (_dir, store) = temp_store();
    let stale = hook_row(
        Harness::Codex,
        "closed-pane",
        HOOK_ROW_TTL + chrono::Duration::minutes(1),
    );
    store.register(&stale).expect("register");

    assert!(store.live().expect("live").is_empty());
    // Not live is not gone: the session is still a valid offline target.
    assert!(store
        .get_registration(&Harness::Codex, "closed-pane")
        .expect("get")
        .is_some());
    assert_eq!(store.registrations().expect("all").len(), 1);
}

#[test]
fn live_never_includes_a_human_row() {
    let (_dir, store) = temp_store();
    store
        .register(&hook_row(
            Harness::Other(Harness::HUMAN.into()),
            "umut",
            chrono::Duration::zero(),
        ))
        .expect("register");

    assert!(store.live().expect("live").is_empty());
    assert_eq!(store.registrations().expect("all").len(), 1);
}

#[test]
fn prune_drops_dead_pids_and_expired_hook_rows_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_at(&Paths::new(dir.path()))
        .expect("open")
        .with_probe(Box::new(FakeProbe(vec![100])));

    let mut alive = Registration::new(Harness::Claude, "alive", PathBuf::from("/a"));
    alive.pid = Some(100);
    let mut dead = Registration::new(Harness::Claude, "dead", PathBuf::from("/a"));
    dead.pid = Some(999);
    let recent = hook_row(Harness::Codex, "recent", chrono::Duration::hours(1));
    let expired = hook_row(
        Harness::Codex,
        "expired",
        PRUNE_AFTER + chrono::Duration::hours(1),
    );
    // Old enough to prune on age alone, but a reply sink must survive.
    let human = hook_row(
        Harness::Other(Harness::HUMAN.into()),
        "umut",
        chrono::Duration::days(90),
    );

    for r in [&alive, &dead, &recent, &expired, &human] {
        store.register(r).expect("register");
    }

    assert_eq!(store.prune(chrono::Utc::now()).expect("prune"), 2);

    let mut left: Vec<_> = store
        .registrations()
        .expect("all")
        .into_iter()
        .map(|r| r.session_id)
        .collect();
    left.sort();
    assert_eq!(left, vec!["alive", "recent", "umut"]);

    // Idempotent: a second sweep finds nothing new.
    assert_eq!(store.prune(chrono::Utc::now()).expect("prune"), 0);
}

// ---- migrations ------------------------------------------------------------------
//
// Two processes open the store within the same second all the time: a CLI send and an
// MCP server starting up. Both must come out with a current schema.

/// A genuine v1 database: the schema as it shipped, with no `pushed_at` column.
fn write_v1_fixture(path: &std::path::Path) {
    let conn = rusqlite::Connection::open(path).expect("open fixture");
    conn.execute_batch(
        "CREATE TABLE messages (
           id            TEXT PRIMARY KEY,
           from_addr     TEXT NOT NULL,
           to_addr       TEXT NOT NULL,
           text          TEXT NOT NULL,
           reply_to      TEXT,
           expects_reply INTEGER NOT NULL DEFAULT 0,
           status        TEXT NOT NULL,
           created_at    TEXT NOT NULL,
           delivered_at  TEXT,
           error         TEXT
         );
         CREATE TABLE registry (
           harness    TEXT NOT NULL,
           session_id TEXT NOT NULL,
           alias      TEXT,
           pid        INTEGER,
           cwd        TEXT NOT NULL,
           poke_path  TEXT,
           herdr_pane TEXT,
           started_at TEXT NOT NULL,
           last_seen  TEXT NOT NULL,
           PRIMARY KEY (harness, session_id)
         );
         CREATE TABLE cursors (address TEXT PRIMARY KEY, last_read_id TEXT);",
    )
    .expect("v1 schema");
    conn.pragma_update(None, "user_version", 1i64)
        .expect("user_version");
}

#[test]
fn migrating_a_v1_db_twice_is_a_no_op_the_second_time() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("agentmail.sqlite");
    write_v1_fixture(&path);

    let store = Store::open(&path).expect("first open");
    assert_eq!(store.schema_version().expect("version"), 2);
    drop(store);

    let store = Store::open(&path).expect("second open");
    assert_eq!(store.schema_version().expect("version"), 2);

    // The migrated schema is usable, not just versioned.
    let me = addr(Harness::Claude, "me");
    let msg = Message::new(addr(Harness::Codex, "peer"), me.clone(), "hi");
    store.enqueue(&msg).expect("enqueue");
    assert_eq!(store.pending_for(&me).expect("pending").len(), 1);
}

#[test]
fn a_column_already_added_at_v1_migrates_cleanly() {
    // What a pre-transactional partial run left behind: the column is there but the
    // version bump never landed.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("agentmail.sqlite");
    write_v1_fixture(&path);
    {
        let conn = rusqlite::Connection::open(&path).expect("open");
        conn.execute_batch("ALTER TABLE messages ADD COLUMN pushed_at TEXT;")
            .expect("add column");
        conn.pragma_update(None, "user_version", 1i64)
            .expect("stay at v1");
    }

    let store = Store::open(&path).expect("open over a half-applied migration");
    assert_eq!(store.schema_version().expect("version"), 2);
}

#[test]
fn concurrent_opens_of_a_v1_db_all_succeed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("agentmail.sqlite");
    write_v1_fixture(&path);

    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| scope.spawn(|| Store::open(&path).map(|s| s.schema_version())))
            .collect();
        for h in handles {
            let version = h
                .join()
                .expect("thread panicked")
                .expect("open")
                .expect("version");
            assert_eq!(version, 2);
        }
    });
}

#[test]
fn opening_waits_out_a_held_write_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("agentmail.sqlite");
    write_v1_fixture(&path);

    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut conn = rusqlite::Connection::open(&path).expect("open");
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .expect("begin immediate");
            std::thread::sleep(std::time::Duration::from_millis(300));
            tx.commit().expect("commit");
        });

        // Well inside the 5 s busy timeout, so this waits rather than failing.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let store = Store::open(&path).expect("open against a held write lock");
        assert_eq!(store.schema_version().expect("version"), 2);
    });
}
