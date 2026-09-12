# agentmail-delivery

The adapters that actually move a message. `agentmail-core` defines the seams
(`Deliverer`, `Directory`, the mailbox); this crate implements them against the world
outside the process.

## Modules

| Module | What it is |
| --- | --- |
| `hooks` | `drain_stop` / `session_start`: pure functions over a `&Store` that turn a harness hook payload into a `{"decision":"block","reason":...}` answer or a registry row |
| `poke_deliverer` | Rings a live session's wake-up socket |
| `spawn` | `claude -p` / `codex exec` / `claude --bg` / a herdr pane, all behind one `CommandRunner` trait |
| `provider` | `resolve_argv`: a bare provider name that `PATH` cannot find is looked for next to this binary, where the plugin build puts `hsm` |
| `herdr` (feature) | `HerdrDirectory`, `HerdrPromptDeliverer`, `HerdrPaneSpawner` |
| `router` | `default_deliverers`: poke → herdr prompt → spawn |

## Why the outcomes look the way they do

- A successful poke reports **`Queued`**, not `Pushed`. A poke carries no payload; the
  recipient reads the store itself and marks the row delivered once its model has seen
  the message. Reporting `Pushed` would mark it delivered while still unread, and the
  recipient's own drain would then skip it.
- A Stop hook claims and marks its batch in one `BEGIN IMMEDIATE` transaction, so two
  installed hooks cannot both hand over the same mail, and it skips rows whose id it
  finds in the transcript after a channel push.
- A Stop hook only ever blocks on rows that are still `Pending`, which is what makes
  `stop_hook_active` safe: the batch it already handed over is `Delivered`, so the next
  Stop has nothing to say and the loop ends.
- `Auto` never resurrects a sleeping session. The row is addressed correctly and that
  session's own hook drains it the next time a human runs it; only an explicit
  `--mode ask|background|pane` spends a harness run.
- A background spawn re-points the message row at the session it just started and
  leaves it `Pending`: that new session's MCP process or Stop hook is what finally
  shows it to the model.
- `codex exec` is run with `--json --output-last-message <file>`, so an ask-mode reply
  is the model's final message rather than the whole run transcript, and a *fresh*
  codex session can still report the thread it created back as its address.
- `session_start` records `HERDR_PANE_ID` on the registration: the hook runs inside the
  agent's own pane, which is the one moment that is knowable without the MCP process.

## Features

`herdr` adds the `herdr-client` adapters. Without it, `pane` mode fails over to the
next deliverer and the directory is simply absent.

## Tests

`cargo test -p agentmail-delivery --features herdr`. Hook drains run against real
harness JSON, the spawner against a fake `CommandRunner`, and the herdr deliverer
against an in-process fake socket — never a live herdr.
