# hsm

The herdr session manager binary. `herdr-plugin.toml` runs it three ways: the
`browse` popup, the `setup-chat` popup, and the `startup` hook.

## Commands

| Command | What it does |
| --- | --- |
| `hsm browse [--pane-mode]` | The session browser (hsm-tui). Refreshes the index first, then draws. |
| `hsm open <address-or-prefix> [--target current\|split\|split-down\|tab] [--cwd DIR]` | Restores one session into a pane; prints `{address, target, pane_id, method, resumed}`. |
| `hsm index [--full]` | Rescans the harness stores. `--full` re-reads every transcript. |
| `hsm sessions [--json] [--query Q] [--limit N] [--harness H] [--project P]` | `--json` is the agentmail provider contract (docs/protocol.md); without it, a plain table. A query that is an address or an id prefix (8+ chars) is looked up by id as well, and that session is listed first. Refreshes the index first, but at most once every `PROVIDER_MAX_AGE` (10 s) so a burst of card reads costs one scan. |
| `hsm startup` | The manifest hook after herdr restores a session. Logs one line to stderr and always exits 0. |
| `hsm setup-chat` | Execs `agentmail setup`, or explains how to build it and waits for Enter. |

`--target` also accepts herdr's own spellings (`split-down`, `split-right`).

## Wiring

```
clap ──> commands ──> hsm_core::Index          (search, recent, get, pin, refresh)
                 └──> hsm_core::OpenService<HerdrPaneOps>
                 └──> hsm_tui::run(Box<dyn Actions>, BrowseContext)
```

Two adapters in `herdr_adapter` implement the traits hsm-core defines:

- `HerdrLive: LiveSessions` — `agent.list` plus `session.snapshot`, mapped to
  `LivePane`. `agent.list` decides what is running; the snapshot only adds panes
  it did not mention, and panes with a session ref but no running agent are
  skipped (those are remembered sessions, not live ones).
- `HerdrPaneOps: PaneOps` — `pane.send_input`, `pane.split`
  (`Horizontal → right`, `Vertical → down`), `tab.create`, `agent.start`.

`actions::TuiActions` joins them for the screen. The TUI is sync and the client
is async, so it blocks on a shared multi-threaded tokio runtime.

The `m` key runs `agentmail send <address> <text> [--from <address>]` with a
15 s deadline (the child is killed on expiry, since it runs on the UI thread).
`--from` is the session herdr has on the invoking pane — the plugin context says
whether that pane holds an agent, `herdr_adapter::pane_address` reads the ref out
of the snapshot. Unknown means the flag is omitted and agentmail picks the
sender itself.

The invoking pane comes from `HERDR_PLUGIN_CONTEXT_JSON`: a popup has no
`HERDR_PANE_ID`. With `--pane-mode` (the Windows launcher runs the browser in a
split instead of a popup) `HERDR_PANE_ID` is our own pane, so only the context's
focused pane counts.

## Config and state

`Config` (hsm-core) at
`HSM_CONFIG_DIR`, else `~/.config/hsm`. The index lives in
`HSM_STATE_DIR`, else `~/.local/state/hsm` (one index for the plugin and for agentmail's shell-out).

`HSM_LOG` (or `RUST_LOG`) sets the tracing filter; `browse` stays silent unless
one is set, because a popup's stderr is the popup itself.

## Tests

`cargo test -p hsm`. Mutating herdr calls are only ever exercised against an
in-process fake socket server (`herdr_adapter::socket_tests`), never a live
server.
