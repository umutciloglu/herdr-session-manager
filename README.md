# herdr-session-manager

Two tools for people who run many AI coding agents inside [herdr](https://herdr.dev).

- **hsm** — a herdr plugin. Browse, search, restore, and open past agent sessions in the pane of your choice. Works for every harness herdr can resume.
- **agentmail** — cross-harness agent-to-agent messaging. An MCP server plus hooks for Claude Code and Codex, with a durable mailbox. Works without herdr; herdr adds a live directory and idle-wake fallback.

See `docs/PLAN.md` for the architecture, `docs/protocol.md` for the message and tool contracts, and `docs/harnesses.md` for the per-harness table.

## Install

```sh
herdr plugin install <owner>/herdr-session-manager
```

Then bind a key in `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+s"
type = "plugin_action"
command = "herdr-session-manager.browse"
description = "browse agent sessions"
```

The build step links `hsm` and `agentmail` into `~/.local/bin` and adds that directory to your shell rc when it is not on `PATH` yet, so both commands work by name in a new shell. On Windows the release directory goes on the user `PATH` instead.

Agent chat is opt-in. Run the `Set up agent chat` plugin action, or `agentmail setup` directly. It is an in-place checklist: arrows move, space toggles, `a` selects all, enter applies.
