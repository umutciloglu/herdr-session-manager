# hsm

The herdr session manager binary. `herdr-plugin.toml` runs it three ways: the
`browse` popup, the `setup-chat` popup, and the `startup` hook.

## Commands

| Command | What it does |
| --- | --- |
| `hsm browse [--pane-mode]` | The popup (hsm-tui), opened on the search panel. Refreshes the index first, then draws. |
| `hsm ask [--pane-mode]` | The same popup opened on the ask panel. `s`, `a` and `r` switch panels once it is up. |
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

Everything the popups send goes out as `--from human:<login user>`, never as the
agent in the invoking pane: a reply addressed to that agent is injected into it
by its Stop hook instead of reaching the person who asked. The pane's agent is
still resolved, but only so the compose box can say
`asking as human:me from claude:8890a685`.

The popup is one app with three panels — search, ask and replies — switched with
`s`, `a` and `r`. Because `s` is a panel key, splitting right is `v`; the search
panel's open keys are Enter, `v`, `d`, `t`, `c`, plus `i`, `m` and `p`.

`ask` then watches `human:<user>`'s mailbox for `ask_wait_secs` (default 120),
polling `agentmail inbox --addr <me> --json` every 2 s, with a countdown in the
status line; Esc stops waiting without closing the popup. An answer lands in the
preview and Enter asks a follow-up. Anything that arrives later shows up on the replies
panel (`r`), which lists the whole mailbox newest first with the sender's session
title, marks what you have not read with `•`, reads one in full on Enter, and
answers its sender with `a`. **`agentmail inbox` has no `--json` flag yet** —
until it does, the listing is diffed against how it looked when the question
went out and the new tail is treated as the answer, and the replies panel stays
empty because prose has no rows to read.

Read state is hsm's own: agentmail has no `--mark-read`, so the ids already put
in front of the human live in `<state>/seen_replies` and only unseen mail is
counted.

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

## Actions and keys

`herdr-plugin.toml` exposes `browse`, `ask` and `setup-chat` (each with a
`-windows` twin that splits a pane instead of opening a popup). Suggested user
keybindings:

```toml
[[keys.command]]
type = "plugin_action"
command = "herdr-session-manager.browse"   # prefix+s

[[keys.command]]
type = "plugin_action"
command = "herdr-session-manager.ask"      # prefix+a
```

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
