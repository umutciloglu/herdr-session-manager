# herdr-session-manager

Session browsing and agent-to-agent chat for people who run many AI coding agents inside [herdr](https://herdr.dev).

> **Early work in progress.** Version 0.1 was built and tested on one machine, macOS with Claude Code and Codex. It works there end to end, but other transcript layouts, harness versions, and workflows have not been exercised yet. Expect rough edges, keep an eye on the [issues](https://github.com/umutciloglu/herdr-session-manager/issues), and please report what breaks.

herdr already restores your agent panes after a restart. This plugin adds the parts around that:

- **Find any session again.** Every Claude Code and Codex session you ever ran, plus the sessions herdr knows for every other harness, in one searchable popup. Open one in the current pane, a split, or a new tab.
- **Jump to what is running.** A session that runs in a herdr pane right now is marked `live`. Press Enter and the popup drops you into that pane instead of starting a second copy. Claude Code background jobs are marked `job`, and Enter jumps to the pane that is showing them. Working, idle, and blocked agents are told apart at a glance.
- **Let agents talk to each other.** A Claude session can ask a Codex session a question and get the answer back, and the other way round. Works across harnesses, works when the other session is idle, and works when it is not running at all.
- **Ask a session yourself.** Pick a session, type a question, get the reply in the popup.

Two binaries ship together. `hsm` is the herdr plugin. `agentmail` is the chat side, an MCP server plus hooks for Claude Code and Codex, and it also works without herdr.

## Requirements

- herdr 0.9.0 or newer, on macOS, Linux or Windows. herdr calls plugins on Windows a preview, and so does this one: CI installs and drives it there on every push, but it has had less real use than the other two.
- For chat: Claude Code and/or Codex CLI installed. On Windows, reaching a session that is *not* running needs the native `claude.exe` or `codex.exe`; an npm install leaves only a `.cmd` script, which cannot be started that way.
- A Rust toolchain only if no prebuilt binary matches your platform, plus the Visual Studio C++ Build Tools on Windows. Installs try a checksum-verified download first and fall back to `cargo build`.

## Install

```sh
herdr plugin install umutciloglu/herdr-session-manager
```

The build links `hsm` and `agentmail` into `~/.local/bin` so both work by name — on Windows, `hsm.cmd` and `agentmail.cmd` in `%USERPROFILE%\.local\bin`. If that directory is not on your `PATH`, one marked `export PATH` line is appended to your shell rc, or the directory is added to your user `PATH`. Set `HSM_NO_PATH=1` in herdr's environment to skip both.

Then add keybindings to `~/.config/herdr/config.toml`, or `%APPDATA%\herdr\config.toml` on Windows. All three actions are optional, bind the ones you want:

```toml
# browse, search, open sessions
[[keys.command]]
key = "prefix+s"
type = "plugin_action"
command = "herdr-session-manager.browse"
description = "browse agent sessions"

# ask a session a question and wait for the reply
[[keys.command]]
key = "prefix+a"
type = "plugin_action"
command = "herdr-session-manager.ask"
description = "ask an agent session"

# install or remove the chat hooks in Claude Code and Codex
[[keys.command]]
key = "prefix+shift+s"
type = "plugin_action"
command = "herdr-session-manager.setup-chat"
description = "set up agent chat"
```

Reload with `herdr server reload-config`. On Windows the action ids end in `-windows` (`herdr-session-manager.browse-windows`, and so on), because herdr refuses the same action id twice even across platforms.

## First five minutes

**Sessions.** Press `prefix+s`. Type to filter. Rows marked `live` run in a herdr pane right now; rows marked `job` are Claude Code background jobs. Enter on either jumps to the pane where it runs or is shown. Enter on anything else opens the session in a split. `Esc` switches from typing to keys: `o` or `v` split right, `d` split down, `t` new tab, `c` this pane, `i` paste the session's address into your pane. `s`, `a`, `r` jump between the search, ask, and replies panels. The jump and split keys are rebindable under `[keys]` in `~/.config/hsm/config.toml`.

**Chat, one-time setup.** Run `agentmail setup` in a terminal, or press `prefix+shift+s`. It is a checklist: arrows move, space toggles, `a` selects everything, Enter applies. It registers the MCP server and installs Stop and SessionStart hooks in Claude Code and Codex. Codex asks once whether to trust the new hooks.

**Chat, from Claude Code.** Type `@agentmail` and pick the session from the list. Then say what you want sent. Claude calls `agentmail_send`. To receive replies while Claude sits idle, start it with channels:

```sh
claude --dangerously-load-development-channels server:agentmail
```

Without that flag, replies arrive at the end of Claude's next turn instead of immediately. The flag asks for confirmation on every launch. That is a Claude Code research-preview rule, not something this plugin can remove.

**Chat, from Codex.** Codex has no session picker. Use `prefix+s`, select a session, press `i` to paste its address, then tell Codex what to send. Or name the session in words and Codex will search for it. Mail reaches Codex the moment it is idle, or at the end of its current turn.

**Ask a session yourself.** Press `prefix+a`, pick a session, type the question, Enter. The popup waits up to two minutes and shows the reply. If you close it first, the reply lands under `r` next time.

## How delivery works

A message is written to a local mailbox first and never lost. Delivery then depends on who receives it:

| Receiver | How it arrives |
| --- | --- |
| Claude, idle, started with channels | pushed into the session immediately |
| Claude, working, or started without channels | at the end of its current turn, through the Stop hook |
| Codex, idle in herdr | herdr types it into the pane |
| Codex, working | at the end of its current turn, through the Stop hook |
| A session that is not running | queued until it resumes, or run headless with `--mode ask` |

An address is `<harness>:<session id>`, for example `codex:01a09638`. An eight-character prefix is enough.

## Commands

```
hsm browse | ask | open <addr> | index | sessions [--json] | startup | setup-chat | doctor
agentmail setup | send <addr> "<text>" | inbox | wait | sessions | doctor
```

`docs/usage.md` has every key, every config option, and a troubleshooting list. `docs/PLAN.md` describes the architecture.

## State on disk

- Session index: `~/.local/state/hsm/index.sqlite`, config in `~/.config/hsm/config.toml`. On Windows: `%LOCALAPPDATA%\hsm\index.sqlite` and `%APPDATA%\hsm\config.toml`.
- Mailbox and registry: `~/.local/state/agentmail/`, config alongside. On Windows: `%LOCALAPPDATA%\agentmail\`.
- Hooks are added as separate entries beside herdr's own hook files. herdr's files are never edited.

## Uninstall

`herdr plugin uninstall herdr-session-manager`, then `agentmail setup` and untick everything before removing the binaries, so no hook entry points at a missing file.

## License

MIT.
