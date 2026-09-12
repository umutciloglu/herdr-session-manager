//! Spawner behaviour, with a fake `CommandRunner` in place of a real harness.

use std::sync::{Arc, Mutex};

use agentmail_core::{
    Address, Deliverer, DeliveryOutcome, DeliveryRequest, Harness, Message, Resolved, SendMode,
    SpawnConfig, Store,
};
use agentmail_delivery::{CommandOutput, CommandRunner, CommandSpec, Spawner};
use async_trait::async_trait;

#[derive(Default)]
struct FakeRunner {
    seen: Mutex<Vec<CommandSpec>>,
    reply: Mutex<CommandOutput>,
}

impl FakeRunner {
    fn answering(stdout: &str) -> Arc<FakeRunner> {
        let fake = FakeRunner::default();
        *fake.reply.lock().expect("lock") = CommandOutput {
            code: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        };
        Arc::new(fake)
    }

    fn last(&self) -> CommandSpec {
        self.seen
            .lock()
            .expect("lock")
            .last()
            .cloned()
            .expect("a command")
    }

    fn count(&self) -> usize {
        self.seen.lock().expect("lock").len()
    }
}

#[async_trait]
impl CommandRunner for FakeRunner {
    async fn run(&self, spec: &CommandSpec) -> std::io::Result<CommandOutput> {
        self.seen.lock().expect("lock").push(spec.clone());
        Ok(self.reply.lock().expect("lock").clone())
    }
}

fn store() -> Arc<Store> {
    Arc::new(Store::open_in_memory().expect("store"))
}

fn message(to: Address, expects_reply: bool) -> Message {
    Message::new(
        Address::new(Harness::Claude, "me-00000001"),
        to,
        "review the parser",
    )
    .expecting_reply(expects_reply)
}

async fn deliver(
    spawner: &Spawner,
    msg: &Message,
    resolved: &Resolved,
    mode: SendMode,
) -> DeliveryOutcome {
    spawner
        .deliver(&DeliveryRequest {
            message: msg,
            resolved,
            mode,
        })
        .await
}

#[tokio::test]
async fn ask_claude_runs_headless_and_returns_the_reply() {
    let runner = FakeRunner::answering(
        r#"{"type":"result","subtype":"success","result":"parser looks fine","session_id":"aaaabbbb-1111-2222-3333-444444444444"}"#,
    );
    let spawner = Spawner::new(store(), SpawnConfig::default()).with_runner(runner.clone());
    let msg = message(Address::new(Harness::Claude, "new"), true);

    let outcome = deliver(
        &spawner,
        &msg,
        &Resolved::Spawn(Harness::Claude),
        SendMode::Auto,
    )
    .await;
    assert_eq!(
        outcome,
        DeliveryOutcome::Replied {
            reply: "parser looks fine".into(),
            from: Some(Address::new(
                Harness::Claude,
                "aaaabbbb-1111-2222-3333-444444444444"
            )),
        }
    );

    let spec = runner.last();
    assert_eq!(spec.program, "claude");
    assert_eq!(spec.args[0], "-p");
    assert!(spec.args[1].starts_with("[agentmail] from claude:me-00000"));
    assert_eq!(&spec.args[2..4], ["--output-format", "json"]);
    assert!(spec
        .env
        .contains(&("AGENTMAIL_HARNESS".into(), "claude".into())));
}

#[tokio::test]
async fn ask_claude_forks_when_resuming_a_known_session() {
    let runner = FakeRunner::answering(
        r#"{"result":"ok","session_id":"ffff0000-1111-2222-3333-444444444444"}"#,
    );
    let spawner = Spawner::new(store(), SpawnConfig::default()).with_runner(runner.clone());
    let peer = Address::new(Harness::Claude, "8890a685-1111-2222-3333-444444444444");
    let msg = message(peer.clone(), true);

    deliver(
        &spawner,
        &msg,
        &Resolved::Offline(peer.clone(), None),
        SendMode::Ask,
    )
    .await;

    let args = runner.last().args;
    assert!(
        args.windows(2).any(|w| w == ["--resume", peer.id.as_str()]),
        "{args:?}"
    );
    assert!(args.iter().any(|a| a == "--fork-session"), "{args:?}");
    // A fork gets a new id, so nothing may claim to be the resumed session.
    assert!(!runner
        .last()
        .env
        .iter()
        .any(|(k, _)| k == "AGENTMAIL_SESSION_ID"));
}

#[tokio::test]
async fn ask_codex_resumes_with_the_exec_subcommand() {
    let runner = FakeRunner::answering(
        r#"{"type":"item.completed","item":{"type":"agent_message","text":"looks good to me"}}"#,
    );
    let spawner = Spawner::new(store(), SpawnConfig::default()).with_runner(runner.clone());
    let peer = Address::new(Harness::Codex, "01999b0e-2222-4444-8888-cccccccccccc");
    let msg = message(peer.clone(), true);

    let outcome = deliver(
        &spawner,
        &msg,
        &Resolved::Offline(peer.clone(), None),
        SendMode::Ask,
    )
    .await;
    assert_eq!(
        outcome,
        DeliveryOutcome::Replied {
            reply: "looks good to me".into(),
            from: Some(peer.clone()),
        }
    );

    let spec = runner.last();
    assert_eq!(spec.program, "codex");
    assert_eq!(
        &spec.args[..4],
        ["exec", "resume", "--json", "--output-last-message"]
    );
    // Flags first, then codex's own positionals: <session id> <prompt>.
    let tail = &spec.args[spec.args.len() - 2..];
    assert_eq!(tail[0], peer.id);
    assert!(tail[1].starts_with("[agentmail] from"));
    assert!(spec
        .env
        .contains(&("AGENTMAIL_SESSION_ID".into(), peer.id.clone())));
}

#[tokio::test]
async fn a_fresh_codex_run_reports_the_thread_it_created() {
    // Two shapes of the same event stream, so the parser cannot be pinned to one.
    let runner = FakeRunner::answering(concat!(
        r#"{"type":"thread.started","thread_id":"7f3d9a20-1111-4222-8333-444444444444"}"#,
        "\n",
        r#"{"id":"0","msg":{"type":"agent_message","message":"first draft"}}"#,
        "\n",
        r#"{"type":"item.completed","item":{"type":"agent_message","text":"the parser is fixed"}}"#,
        "\n",
        r#"{"type":"turn.completed"}"#,
    ));
    let spawner = Spawner::new(store(), SpawnConfig::default()).with_runner(runner.clone());
    let msg = message(Address::new(Harness::Codex, "new"), true);

    let outcome = deliver(
        &spawner,
        &msg,
        &Resolved::Spawn(Harness::Codex),
        SendMode::Auto,
    )
    .await;
    assert_eq!(
        outcome,
        DeliveryOutcome::Replied {
            reply: "the parser is fixed".into(),
            from: Some(Address::new(
                Harness::Codex,
                "7f3d9a20-1111-4222-8333-444444444444"
            )),
        },
        "the last agent message wins, and the thread id becomes the peer address"
    );
    assert_eq!(
        &runner.last().args[..3],
        ["exec", "--json", "--output-last-message"]
    );
}

#[tokio::test]
async fn codex_output_that_is_not_json_is_still_an_answer() {
    let runner = FakeRunner::answering("plain text answer\n");
    let spawner = Spawner::new(store(), SpawnConfig::default()).with_runner(runner.clone());
    let msg = message(Address::new(Harness::Codex, "new"), true);

    let outcome = deliver(
        &spawner,
        &msg,
        &Resolved::Spawn(Harness::Codex),
        SendMode::Ask,
    )
    .await;
    assert_eq!(
        outcome,
        DeliveryOutcome::Replied {
            reply: "plain text answer".into(),
            from: None,
        }
    );
}

#[tokio::test]
async fn background_claude_retargets_the_row_and_queues_it() {
    let runner = FakeRunner::answering("Started session 7c7c7c7c-1111-2222-3333-444444444444\n");
    let store = store();
    let spawner =
        Spawner::new(Arc::clone(&store), SpawnConfig::default()).with_runner(runner.clone());
    let msg = message(Address::new(Harness::Claude, "new"), false);
    store.enqueue(&msg).expect("enqueue");

    let outcome = deliver(
        &spawner,
        &msg,
        &Resolved::Spawn(Harness::Claude),
        SendMode::Auto,
    )
    .await;
    assert_eq!(outcome, DeliveryOutcome::Queued);
    assert_eq!(runner.last().args[0], "--bg");

    let spawned = Address::new(Harness::Claude, "7c7c7c7c-1111-2222-3333-444444444444");
    assert_eq!(store.pending_for(&spawned).expect("pending").len(), 1);
}

#[tokio::test]
async fn codex_has_no_background_mode() {
    let runner = FakeRunner::answering("");
    let spawner = Spawner::new(store(), SpawnConfig::default()).with_runner(runner.clone());
    let msg = message(Address::new(Harness::Codex, "new"), false);

    let outcome = deliver(
        &spawner,
        &msg,
        &Resolved::Spawn(Harness::Codex),
        SendMode::Background,
    )
    .await;
    assert_eq!(
        outcome,
        DeliveryOutcome::Failed("codex has no background mode".into())
    );
    assert_eq!(runner.count(), 0);
}

#[tokio::test]
async fn an_offline_peer_is_left_for_its_own_hook_in_auto_mode() {
    let runner = FakeRunner::answering("");
    let spawner = Spawner::new(store(), SpawnConfig::default()).with_runner(runner.clone());
    let peer = Address::new(Harness::Codex, "01999b0e-2222-4444-8888-cccccccccccc");
    let msg = message(peer.clone(), false);

    let outcome = deliver(
        &spawner,
        &msg,
        &Resolved::Offline(peer, None),
        SendMode::Auto,
    )
    .await;
    assert_eq!(outcome, DeliveryOutcome::Queued);
    assert_eq!(runner.count(), 0, "auto mode must not spend a harness run");
}

#[tokio::test]
async fn extra_args_from_config_reach_the_command_line() {
    let runner = FakeRunner::answering(r#"{"result":"ok"}"#);
    let cfg = SpawnConfig {
        claude_extra_args: vec!["--model".into(), "opus".into()],
        codex_extra_args: vec![],
    };
    let spawner = Spawner::new(store(), cfg).with_runner(runner.clone());
    let msg = message(Address::new(Harness::Claude, "new"), true);

    deliver(
        &spawner,
        &msg,
        &Resolved::Spawn(Harness::Claude),
        SendMode::Ask,
    )
    .await;
    let args = runner.last().args;
    assert!(
        args.windows(2).any(|w| w == ["--model", "opus"]),
        "{args:?}"
    );
}

#[tokio::test]
async fn pane_mode_without_herdr_fails_over_to_the_next_deliverer() {
    let runner = FakeRunner::answering("");
    let spawner = Spawner::new(store(), SpawnConfig::default()).with_runner(runner.clone());
    let msg = message(Address::new(Harness::Claude, "new"), false);

    let outcome = deliver(
        &spawner,
        &msg,
        &Resolved::Spawn(Harness::Claude),
        SendMode::Pane,
    )
    .await;
    assert!(matches!(outcome, DeliveryOutcome::Failed(e) if e.contains("herdr feature")));
}

#[tokio::test]
async fn a_live_peer_is_not_the_spawners_business() {
    let runner = FakeRunner::answering("");
    let spawner = Spawner::new(store(), SpawnConfig::default()).with_runner(runner.clone());
    let peer = Address::new(Harness::Claude, "8890a685-1111-2222-3333-444444444444");
    let reg = agentmail_core::Registration::new(Harness::Claude, peer.id.clone(), "/repo");
    let msg = message(peer, false);

    let outcome = deliver(&spawner, &msg, &Resolved::Live(reg, None), SendMode::Auto).await;
    assert_eq!(outcome, DeliveryOutcome::Queued);
    assert_eq!(runner.count(), 0);
}
