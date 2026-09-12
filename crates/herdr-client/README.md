# herdr-client

Async client for the [herdr](https://herdr.dev) local socket API: newline-delimited
JSON over a Unix domain socket, or a named pipe on Windows.

Product-agnostic on purpose. It knows herdr's protocol and nothing about the
crates that use it.

## Usage

```rust
use herdr_client::{
    AgentPrompt, AgentStatus, HerdrClient, PaneSplit, PromptWait, Result, SplitDirection,
};

async fn run_tests_in_a_new_pane() -> Result<()> {
    let herdr = HerdrClient::connect().await?;

    for agent in herdr.agent_list().await? {
        println!("{} {}", agent.pane_id, agent.agent_status);
    }

    let pane = herdr
        .pane_split(&PaneSplit::new(SplitDirection::Right).cwd("/repo").focus(true))
        .await?;

    let agent = herdr
        .agent_prompt(
            &AgentPrompt::new(&pane.pane_id, "run the tests")
                .wait(PromptWait::until([AgentStatus::Idle]).timeout_ms(120_000)),
        )
        .await?;

    println!("{} is {}", agent.pane_id, agent.agent_status);
    Ok(())
}
```

## Notes

- Socket resolution: `HERDR_SOCKET_PATH`, else `HERDR_SESSION=<name>` →
  `~/.config/herdr/sessions/<name>/herdr.sock`, else `~/.config/herdr/herdr.sock`.
- herdr 0.9 answers one request per connection and then closes it, so each call
  dials a fresh socket. Subscriptions are the exception and hold their own.
- `PluginEnv::from_env()` reads the `HERDR_*` variables herdr injects into plugin
  processes, including the focused-pane context a popup needs.
- `herdr_bin()` gives the herdr executable path for callers that shell out.
- Live-server tests are read-only and ignored by default:
  `cargo test -p herdr-client -- --ignored`.
