mod common;

use std::path::PathBuf;
use std::sync::Arc;

use agentmail_core::domain::{Address, AddressTarget, Harness, Registration};
use agentmail_core::resolver::{ResolveCtx, Resolved, Resolver};
use agentmail_core::traits::AgentState;
use agentmail_core::{Paths, Store};
use common::{card, entry, FakeDirectory, FakeProvider};

const CLAUDE_A: &str = "8890a685-aaaa-bbbb-cccc-dddddddddddd";
const CLAUDE_B: &str = "8890a685-eeee-ffff-0000-111111111111";

fn setup() -> (tempfile::TempDir, Arc<Store>, Resolver) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(Store::open_at(&Paths::new(dir.path())).expect("open"));
    let resolver = Resolver::new(Arc::clone(&store));
    (dir, store, resolver)
}

fn me() -> Address {
    Address::new(Harness::Codex, "caller-session")
}

fn target(s: &str) -> AddressTarget {
    s.parse().expect("target")
}

#[test]
fn new_is_always_a_spawn() {
    let (_d, _s, r) = setup();
    let from = me();
    let ctx = ResolveCtx::new(&from);
    assert_eq!(
        r.resolve(&target("claude:new"), &ctx).expect("resolve"),
        Resolved::Spawn(Harness::Claude)
    );
}

#[test]
fn exact_registry_hit_wins() {
    let (_d, store, r) = setup();
    store
        .register(&Registration::new(
            Harness::Claude,
            CLAUDE_A,
            PathBuf::from("/a"),
        ))
        .expect("register");

    let from = me();
    let ctx = ResolveCtx::new(&from);
    match r
        .resolve(&target(&format!("claude:{CLAUDE_A}")), &ctx)
        .expect("resolve")
    {
        Resolved::Live(reg, dir) => {
            assert_eq!(reg.session_id, CLAUDE_A);
            assert!(dir.is_none());
        }
        other => panic!("expected Live, got {other:?}"),
    }
}

#[test]
fn a_live_claude_peer_of_a_claude_caller_is_still_live() {
    let (_d, store, r) = setup();
    store
        .register(&Registration::new(
            Harness::Claude,
            CLAUDE_A,
            PathBuf::from("/a"),
        ))
        .expect("register");

    let from = Address::new(Harness::Claude, "some-other-claude");
    let ctx = ResolveCtx::new(&from);
    assert!(matches!(
        r.resolve(&target(&format!("claude:{CLAUDE_A}")), &ctx)
            .expect("resolve"),
        Resolved::Live(..)
    ));
}

#[test]
fn unique_prefix_resolves_and_a_shared_one_is_ambiguous() {
    let (_d, store, r) = setup();
    for id in [CLAUDE_A, CLAUDE_B] {
        store
            .register(&Registration::new(Harness::Claude, id, PathBuf::from("/a")))
            .expect("register");
    }

    let from = me();
    let ctx = ResolveCtx::new(&from);

    match r
        .resolve(&target("claude:8890a685-aaaa"), &ctx)
        .expect("resolve")
    {
        Resolved::Live(reg, _) => assert_eq!(reg.session_id, CLAUDE_A),
        other => panic!("expected Live, got {other:?}"),
    }

    match r
        .resolve(&target("claude:8890a685"), &ctx)
        .expect("resolve")
    {
        Resolved::Ambiguous(cards) => assert_eq!(cards.len(), 2),
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

#[test]
fn short_prefixes_are_not_found() {
    let (_d, store, r) = setup();
    store
        .register(&Registration::new(
            Harness::Claude,
            CLAUDE_A,
            PathBuf::from("/a"),
        ))
        .expect("register");

    let from = me();
    let ctx = ResolveCtx::new(&from);
    assert_eq!(
        r.resolve(&target("claude:8890"), &ctx).expect("resolve"),
        Resolved::NotFound
    );
}

#[test]
fn registry_alias_beats_the_directory() {
    let (_d, store, r) = setup();
    let mut reg = Registration::new(Harness::Claude, CLAUDE_A, PathBuf::from("/a"));
    reg.alias = Some("reviewer".into());
    store.register(&reg).expect("register");

    let dir = FakeDirectory(vec![entry(
        Some(Address::new(Harness::Codex, "codex-1")),
        Some("reviewer"),
        AgentState::Idle,
    )]);
    let from = me();
    let ctx = ResolveCtx::new(&from).with_directory(&dir);

    match r.resolve(&target("reviewer"), &ctx).expect("resolve") {
        Resolved::Live(reg, _) => assert_eq!(reg.harness, Harness::Claude),
        other => panic!("expected Live, got {other:?}"),
    }
}

#[test]
fn directory_alias_stands_in_for_an_unregistered_agent() {
    let (_d, _s, r) = setup();
    let dir = FakeDirectory(vec![entry(
        Some(Address::new(Harness::Codex, "codex-1")),
        Some("reviewer"),
        AgentState::Idle,
    )]);
    let from = me();
    let ctx = ResolveCtx::new(&from).with_directory(&dir);

    match r.resolve(&target("reviewer"), &ctx).expect("resolve") {
        Resolved::Live(reg, entry) => {
            assert_eq!(reg.session_id, "codex-1");
            // Synthesised: no pid, no poke socket.
            assert!(reg.pid.is_none());
            assert!(reg.poke_path.is_none());
            assert_eq!(entry.expect("entry").state, AgentState::Idle);
        }
        other => panic!("expected Live, got {other:?}"),
    }
}

#[test]
fn directory_prefix_match() {
    let (_d, _s, r) = setup();
    let dir = FakeDirectory(vec![entry(
        Some(Address::new(Harness::Claude, CLAUDE_A)),
        None,
        AgentState::Working,
    )]);
    let from = me();
    let ctx = ResolveCtx::new(&from).with_directory(&dir);

    match r
        .resolve(&target("claude:8890a685"), &ctx)
        .expect("resolve")
    {
        Resolved::Live(reg, _) => assert_eq!(reg.session_id, CLAUDE_A),
        other => panic!("expected Live, got {other:?}"),
    }
}

#[test]
fn provider_prefix_hit_is_offline_with_a_card() {
    let (_d, _s, r) = setup();
    let provider = FakeProvider::new(vec![card(
        &format!("claude:{CLAUDE_A}"),
        "claude",
        "API authentication",
    )]);
    let from = me();
    let ctx = ResolveCtx::new(&from).with_provider(&provider);

    match r
        .resolve(&target("claude:8890a685"), &ctx)
        .expect("resolve")
    {
        Resolved::Offline(addr, card) => {
            assert_eq!(addr.id, CLAUDE_A);
            assert_eq!(
                card.expect("card").title.as_deref(),
                Some("API authentication")
            );
        }
        other => panic!("expected Offline, got {other:?}"),
    }
}

#[test]
fn several_provider_hits_are_ambiguous() {
    let (_d, _s, r) = setup();
    let provider = FakeProvider::new(vec![
        card(&format!("claude:{CLAUDE_A}"), "claude", "auth"),
        card(&format!("claude:{CLAUDE_B}"), "claude", "auth again"),
    ]);
    let from = me();
    let ctx = ResolveCtx::new(&from).with_provider(&provider);

    match r
        .resolve(&target("claude:8890a685"), &ctx)
        .expect("resolve")
    {
        Resolved::Ambiguous(cards) => assert_eq!(cards.len(), 2),
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

#[test]
fn alias_falls_through_to_a_provider_title_search() {
    let (_d, _s, r) = setup();
    let provider = FakeProvider::new(vec![card(
        &format!("codex:{CLAUDE_B}"),
        "codex",
        "the reviewer thread",
    )]);
    let from = me();
    let ctx = ResolveCtx::new(&from).with_provider(&provider);

    match r.resolve(&target("reviewer"), &ctx).expect("resolve") {
        Resolved::Offline(addr, _) => assert_eq!(addr.harness, Harness::Codex),
        other => panic!("expected Offline, got {other:?}"),
    }
}

#[test]
fn an_unknown_full_address_is_queued_not_lost() {
    let (_d, _s, r) = setup();
    let from = me();
    let ctx = ResolveCtx::new(&from);
    match r
        .resolve(&target("claude:some-session-we-never-saw"), &ctx)
        .expect("resolve")
    {
        Resolved::Offline(addr, card) => {
            assert_eq!(addr.id, "some-session-we-never-saw");
            assert!(card.is_none());
        }
        other => panic!("expected Offline, got {other:?}"),
    }
}

#[test]
fn an_unknown_alias_is_not_found() {
    let (_d, _s, r) = setup();
    let from = me();
    let ctx = ResolveCtx::new(&from);
    assert_eq!(
        r.resolve(&target("nobody"), &ctx).expect("resolve"),
        Resolved::NotFound
    );
}

#[test]
fn a_broken_provider_degrades_instead_of_failing() {
    let (_d, _s, r) = setup();
    let provider = FakeProvider::unavailable();
    let from = me();
    let ctx = ResolveCtx::new(&from).with_provider(&provider);

    assert_eq!(
        r.resolve(&target("reviewer"), &ctx).expect("resolve"),
        Resolved::NotFound
    );
    assert!(matches!(
        r.resolve(&target("claude:8890a685"), &ctx)
            .expect("resolve"),
        Resolved::Offline(_, None)
    ));
}
