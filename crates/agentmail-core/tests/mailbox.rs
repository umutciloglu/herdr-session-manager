mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agentmail_core::domain::{
    Address, AddressTarget, Harness, Message, MessageStatus, Registration,
};
use agentmail_core::mailbox::{Mailbox, SendMode, SendOpts, SendOutcome};
use agentmail_core::resolver::{ResolveCtx, Resolver};
use agentmail_core::traits::{Deliverer, DeliveryOutcome};
use agentmail_core::{Paths, Store};
use common::{FakeDeliverer, FakeProvider};

const PEER: &str = "8890a685-aaaa-bbbb-cccc-dddddddddddd";

fn setup(deliverers: Vec<Box<dyn Deliverer>>) -> (tempfile::TempDir, Arc<Store>, Mailbox) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(Store::open_at(&Paths::new(dir.path())).expect("open"));
    let resolver = Resolver::new(Arc::clone(&store));
    let mailbox = Mailbox::new(Arc::clone(&store), resolver, deliverers);
    (dir, store, mailbox)
}

fn me() -> Address {
    Address::new(Harness::Codex, "caller-session")
}

fn peer() -> Address {
    Address::new(Harness::Claude, PEER)
}

fn register_peer(store: &Store) {
    let mut reg = Registration::new(Harness::Claude, PEER, PathBuf::from("/work"));
    reg.alias = Some("reviewer".into());
    store.register(&reg).expect("register");
}

fn target(s: &str) -> AddressTarget {
    s.parse().expect("target")
}

#[tokio::test]
async fn pushed_marks_the_row_delivered() {
    let (_d, store, mailbox) = setup(vec![Box::new(FakeDeliverer::new(DeliveryOutcome::Pushed))]);
    register_peer(&store);

    let from = me();
    let ctx = ResolveCtx::new(&from);
    let res = mailbox
        .send(
            &from,
            &target("reviewer"),
            "ping",
            SendOpts::default(),
            &ctx,
        )
        .await
        .expect("send");

    assert_eq!(res.outcome, SendOutcome::Pushed);
    assert_eq!(res.to, Some(peer()));
    let row = store.get(&res.message_id).expect("get").expect("present");
    assert_eq!(row.status, MessageStatus::Delivered);
    assert!(row.delivered_at.is_some());
    // The alias placeholder was replaced by the resolved address.
    assert_eq!(row.to, peer());
}

#[tokio::test]
async fn queued_leaves_the_row_pending() {
    let (_d, store, mailbox) = setup(vec![Box::new(FakeDeliverer::new(DeliveryOutcome::Queued))]);
    register_peer(&store);

    let from = me();
    let ctx = ResolveCtx::new(&from);
    let res = mailbox
        .send(
            &from,
            &target("reviewer"),
            "ping",
            SendOpts::default(),
            &ctx,
        )
        .await
        .expect("send");

    assert_eq!(res.outcome, SendOutcome::Queued);
    let row = store.get(&res.message_id).expect("get").expect("present");
    assert_eq!(row.status, MessageStatus::Pending);
    assert!(row.delivered_at.is_none());
    assert_eq!(store.pending_for(&peer()).expect("pending").len(), 1);
}

#[tokio::test]
async fn the_first_non_failing_deliverer_wins() {
    let refusing = FakeDeliverer::new(DeliveryOutcome::Failed("not my transport".into()));
    let refused = Arc::clone(&refusing.seen);
    let accepting = FakeDeliverer::new(DeliveryOutcome::Pushed);
    let accepted = Arc::clone(&accepting.seen);

    let (_d, store, mailbox) = setup(vec![Box::new(refusing), Box::new(accepting)]);
    register_peer(&store);

    let from = me();
    let ctx = ResolveCtx::new(&from);
    let res = mailbox
        .send(
            &from,
            &target("reviewer"),
            "ping",
            SendOpts::default(),
            &ctx,
        )
        .await
        .expect("send");

    assert_eq!(res.outcome, SendOutcome::Pushed);
    assert_eq!(refused.lock().expect("lock").len(), 1);
    assert_eq!(accepted.lock().expect("lock").len(), 1);
}

#[tokio::test]
async fn every_deliverer_failing_fails_the_send_but_keeps_the_row() {
    let (_d, store, mailbox) = setup(vec![Box::new(FakeDeliverer::new(DeliveryOutcome::Failed(
        "pane is gone".into(),
    )))]);
    register_peer(&store);

    let from = me();
    let ctx = ResolveCtx::new(&from);
    let res = mailbox
        .send(
            &from,
            &target("reviewer"),
            "ping",
            SendOpts::default(),
            &ctx,
        )
        .await
        .expect("send");

    assert_eq!(res.outcome, SendOutcome::Failed("pane is gone".into()));
    let row = store.get(&res.message_id).expect("get").expect("present");
    assert_eq!(row.status, MessageStatus::Failed);
    assert_eq!(row.error.as_deref(), Some("pane is gone"));
}

#[tokio::test]
async fn no_deliverers_means_queued() {
    let (_d, store, mailbox) = setup(vec![]);
    register_peer(&store);

    let from = me();
    let ctx = ResolveCtx::new(&from);
    let res = mailbox
        .send(
            &from,
            &target("reviewer"),
            "ping",
            SendOpts::default(),
            &ctx,
        )
        .await
        .expect("send");

    assert_eq!(res.outcome, SendOutcome::Queued);
    assert_eq!(
        store
            .get(&res.message_id)
            .expect("get")
            .expect("present")
            .status,
        MessageStatus::Pending
    );
}

#[tokio::test]
async fn an_ambiguous_target_keeps_the_row_and_returns_candidates() {
    let (_d, store, mailbox) = setup(vec![Box::new(FakeDeliverer::new(DeliveryOutcome::Pushed))]);
    for id in [PEER, "8890a685-9999-8888-7777-666666666666"] {
        store
            .register(&Registration::new(
                Harness::Claude,
                id,
                PathBuf::from("/work"),
            ))
            .expect("register");
    }

    let from = me();
    let ctx = ResolveCtx::new(&from);
    let res = mailbox
        .send(
            &from,
            &target("claude:8890a685"),
            "ping",
            SendOpts::default(),
            &ctx,
        )
        .await
        .expect("send");

    match &res.outcome {
        SendOutcome::Ambiguous(cards) => assert_eq!(cards.len(), 2),
        other => panic!("expected Ambiguous, got {other:?}"),
    }
    let row = store.get(&res.message_id).expect("get").expect("present");
    assert_eq!(row.status, MessageStatus::Failed);
    assert_eq!(row.text, "ping");
}

#[tokio::test]
async fn an_unknown_alias_is_not_found() {
    let (_d, store, mailbox) = setup(vec![Box::new(FakeDeliverer::new(DeliveryOutcome::Pushed))]);

    let from = me();
    let ctx = ResolveCtx::new(&from);
    let res = mailbox
        .send(&from, &target("nobody"), "ping", SendOpts::default(), &ctx)
        .await
        .expect("send");

    assert_eq!(res.outcome, SendOutcome::NotFound);
    assert_eq!(
        store
            .get(&res.message_id)
            .expect("get")
            .expect("present")
            .status,
        MessageStatus::Failed
    );
}

#[tokio::test]
async fn spawn_targets_retarget_to_the_spawned_address() {
    let spawned = Address::new(Harness::Claude, "fresh-session");
    let (_d, store, mailbox) = setup(vec![Box::new(FakeDeliverer::new(
        DeliveryOutcome::Spawned(spawned.clone()),
    ))]);

    let from = me();
    let ctx = ResolveCtx::new(&from);
    let res = mailbox
        .send(
            &from,
            &target("claude:new"),
            "start here",
            SendOpts::default(),
            &ctx,
        )
        .await
        .expect("send");

    assert_eq!(res.outcome, SendOutcome::Spawned);
    assert_eq!(res.to, Some(spawned.clone()));
    let row = store.get(&res.message_id).expect("get").expect("present");
    assert_eq!(row.to, spawned);
    assert_eq!(row.status, MessageStatus::Delivered);
}

#[tokio::test]
async fn reply_goes_back_to_the_sender_and_marks_the_original_read() {
    let (_d, store, mailbox) = setup(vec![Box::new(FakeDeliverer::new(DeliveryOutcome::Pushed))]);
    let from = me();
    // A registration for the caller so the reply target resolves.
    store
        .register(&Registration::new(
            Harness::Codex,
            "caller-session",
            PathBuf::from("/work"),
        ))
        .expect("register");

    let incoming =
        Message::new(peer(), from.clone(), "what is the auth flow?").expecting_reply(true);
    store.enqueue(&incoming).expect("enqueue");

    let ctx = ResolveCtx::new(&from);
    let res = mailbox
        .reply(&from, &incoming.id, "it is JWT", &ctx)
        .await
        .expect("reply");

    assert_eq!(res.outcome, SendOutcome::Pushed);
    let sent = store.get(&res.message_id).expect("get").expect("present");
    assert_eq!(sent.to, peer());
    assert_eq!(sent.reply_to.as_deref(), Some(incoming.id.as_str()));
    assert_eq!(
        store
            .get(&incoming.id)
            .expect("get")
            .expect("present")
            .status,
        MessageStatus::Read
    );
}

#[tokio::test]
async fn reply_to_an_unknown_message_errors() {
    let (_d, _store, mailbox) = setup(vec![]);
    let from = me();
    let ctx = ResolveCtx::new(&from);
    assert!(mailbox.reply(&from, "01NOPE", "hi", &ctx).await.is_err());
}

#[tokio::test]
async fn wait_returns_the_next_message_and_marks_it_read() {
    let (_d, store, mailbox) = setup(vec![]);
    let from = me();

    let incoming = Message::new(peer(), from.clone(), "hello");
    let id = incoming.id.clone();
    let store2 = Arc::clone(&store);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        store2.enqueue(&incoming).expect("enqueue");
    });

    let got = mailbox
        .wait(&from, Duration::from_secs(5), None, None)
        .await
        .expect("wait")
        .expect("message");

    assert_eq!(got.id, id);
    assert_eq!(
        store.get(&id).expect("get").expect("present").status,
        MessageStatus::Read
    );
}

#[tokio::test]
async fn wait_filters_by_reply_to_and_times_out() {
    let (_d, store, mailbox) = setup(vec![]);
    let from = me();

    store
        .enqueue(&Message::new(peer(), from.clone(), "unrelated"))
        .expect("enqueue");

    let got = mailbox
        .wait(&from, Duration::from_millis(300), Some("01ABC"), None)
        .await
        .expect("wait");
    assert!(got.is_none());

    // The unrelated message was not consumed by the filtered wait.
    assert_eq!(store.inbox(&from, false, 10).expect("inbox").len(), 1);
}

#[tokio::test]
async fn inbox_hides_what_has_been_read() {
    let (_d, store, mailbox) = setup(vec![]);
    let from = me();
    let first = Message::new(peer(), from.clone(), "one");
    let second = Message::new(peer(), from.clone(), "two");
    store.enqueue(&first).expect("enqueue");
    store.enqueue(&second).expect("enqueue");

    assert_eq!(mailbox.inbox(&from).expect("inbox").len(), 2);
    store
        .mark_read(std::slice::from_ref(&first.id))
        .expect("read");
    assert_eq!(mailbox.inbox(&from).expect("inbox").len(), 1);
}

#[tokio::test]
async fn a_resolve_error_never_loses_the_message() {
    // A provider that is down must not turn into a failed send when the registry
    // already knows the peer.
    let (_d, store, mailbox) = setup(vec![Box::new(FakeDeliverer::new(DeliveryOutcome::Queued))]);
    register_peer(&store);
    let provider = FakeProvider::unavailable();

    let from = me();
    let ctx = ResolveCtx::new(&from).with_provider(&provider);
    let res = mailbox
        .send(
            &from,
            &target("reviewer"),
            "ping",
            SendOpts::default(),
            &ctx,
        )
        .await
        .expect("send");

    assert_eq!(res.outcome, SendOutcome::Queued);
    assert_eq!(store.pending_for(&peer()).expect("pending").len(), 1);
}

#[tokio::test]
async fn a_poke_wakes_a_waiter_before_the_poll_would() {
    use agentmail_core::poke::{PokeListener, Poker};

    let dir = tempfile::tempdir().expect("tempdir");
    let paths = Paths::new(dir.path());
    let store = Arc::new(Store::open_at(&paths).expect("open"));
    let resolver = Resolver::new(Arc::clone(&store));
    let mailbox = Mailbox::new(Arc::clone(&store), resolver, vec![]);

    let from = me();
    let mut reg = Registration::new(Harness::Codex, "caller-session", PathBuf::from("/work"));
    reg.poke_path = Some(
        paths
            .poke_socket(&Harness::Codex, "caller-session")
            .to_string_lossy()
            .into_owned(),
    );
    let mut listener = PokeListener::bind(&reg).expect("bind");

    let incoming = Message::new(peer(), from.clone(), "wake up");
    let id = incoming.id.clone();
    let store2 = Arc::clone(&store);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        store2.enqueue(&incoming).expect("enqueue");
        Poker::poke(&reg).await.expect("poke");
    });

    let got = mailbox
        .wait(&from, Duration::from_secs(5), None, Some(&mut listener))
        .await
        .expect("wait")
        .expect("message");
    assert_eq!(got.id, id);
}

#[tokio::test]
async fn deliverers_are_handed_the_send_mode() {
    let deliverer = FakeDeliverer::new(DeliveryOutcome::Pushed);
    let seen = Arc::clone(&deliverer.seen);
    let (_d, store, mailbox) = setup(vec![Box::new(deliverer)]);
    register_peer(&store);

    let from = me();
    let ctx = ResolveCtx::new(&from);
    mailbox
        .send(
            &from,
            &target("reviewer"),
            "ping",
            SendOpts {
                mode: SendMode::Pane,
                ..SendOpts::default()
            },
            &ctx,
        )
        .await
        .expect("send");

    let modes: Vec<_> = seen.lock().expect("lock").iter().map(|(_, m)| *m).collect();
    assert_eq!(modes, vec![SendMode::Pane]);
}

#[tokio::test]
async fn replied_returns_the_answer_inline_and_records_the_new_session() {
    let spawned = Address::new(Harness::Claude, "fresh-session");
    let (_d, store, mailbox) = setup(vec![Box::new(FakeDeliverer::new(
        DeliveryOutcome::Replied {
            reply: "it is JWT".into(),
            from: Some(spawned.clone()),
        },
    ))]);

    let from = me();
    let ctx = ResolveCtx::new(&from);
    let res = mailbox
        .send(
            &from,
            &target("claude:new"),
            "what is the auth flow?",
            SendOpts {
                mode: SendMode::Ask,
                ..SendOpts::default()
            },
            &ctx,
        )
        .await
        .expect("send");

    assert_eq!(res.outcome, SendOutcome::Spawned);
    assert_eq!(res.reply.as_deref(), Some("it is JWT"));
    assert_eq!(res.to, Some(spawned.clone()));

    let row = store.get(&res.message_id).expect("get").expect("present");
    assert_eq!(row.to, spawned);
    assert_eq!(row.status, MessageStatus::Delivered);
}

#[tokio::test]
async fn replied_without_an_address_keeps_the_resolved_target() {
    // A headless run that leaves no resumable session: the row must not gain a
    // fabricated address.
    let (_d, store, mailbox) = setup(vec![Box::new(FakeDeliverer::new(
        DeliveryOutcome::Replied {
            reply: "done".into(),
            from: None,
        },
    ))]);

    let from = me();
    let ctx = ResolveCtx::new(&from);
    let res = mailbox
        .send(
            &from,
            &target("codex:new"),
            "run the tests",
            SendOpts {
                mode: SendMode::Ask,
                ..SendOpts::default()
            },
            &ctx,
        )
        .await
        .expect("send");

    assert_eq!(res.outcome, SendOutcome::Spawned);
    assert_eq!(res.reply.as_deref(), Some("done"));
    assert_eq!(res.to, Some(Address::new(Harness::Codex, "new")));
    assert_eq!(
        store
            .get(&res.message_id)
            .expect("get")
            .expect("present")
            .status,
        MessageStatus::Delivered
    );
}

#[tokio::test]
async fn an_ask_mode_spawner_may_answer_by_enqueuing_instead() {
    let spawned = Address::new(Harness::Claude, "fresh-session");
    let (_d, store, mailbox) = setup(vec![Box::new(FakeDeliverer::new(
        DeliveryOutcome::Spawned(spawned.clone()),
    ))]);

    let from = me();
    let ctx = ResolveCtx::new(&from);
    let res = mailbox
        .send(
            &from,
            &target("claude:new"),
            "question",
            SendOpts {
                mode: SendMode::Ask,
                ..SendOpts::default()
            },
            &ctx,
        )
        .await
        .expect("send");
    assert!(res.reply.is_none());

    // Stand in for the spawner: it answers by enqueuing a reply tied to the send.
    store
        .enqueue(
            &Message::new(spawned, from.clone(), "the answer").in_reply_to(Some(res.message_id)),
        )
        .expect("enqueue");
    let follow_up = mailbox
        .wait(&from, Duration::from_millis(100), None, None)
        .await
        .expect("wait")
        .expect("message");
    assert_eq!(follow_up.text, "the answer");
}
