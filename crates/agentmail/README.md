# agentmail

Cross-harness agent-to-agent messaging. One binary that is an MCP server, a pair of
harness hooks, and a CLI.

```
agentmail mcp                      serve MCP on stdio (the harness starts this)
agentmail hook claude-stop         hook JSON on stdin, answer on stdout
agentmail send <to> <text>         --from --expects-reply --mode --wait
agentmail inbox [--addr]           what is waiting
agentmail wait [--timeout]         block for the next message
agentmail sessions [--json]        what agentmail can see
agentmail alias <name>             name this session
agentmail setup [--check] [--yes]  install into Claude and Codex
agentmail doctor                   state dir, registry, provider, herdr
```

An address is `<harness>:<session-id>`; an 8+ character id prefix, an alias, or
`claude:new` also work. `--from` defaults to this session, and to `human:<user>` when
the command is run by a person — which is also registered as a real mailbox, so replies
to a CLI message land somewhere.

## setup

An in-place checklist (ratatui) over seven items: the Claude MCP server (`~/.claude.json`),
the Claude Stop and SessionStart hooks (`~/.claude/settings.json`), the channel launch
flag (printed, never installed), the Codex MCP server (`~/.codex/config.toml`) and the
Codex Stop and SessionStart hooks (`~/.codex/hooks.json`).

Hooks are always added as a **new** group; herdr's own groups live in the same files
and are never read into, reordered or rewritten. The harness CLIs (`claude mcp add`,
`codex mcp add`) are preferred where they exist, and the JSON/TOML editors are the
fallback. `--home <dir>` points the whole thing at another directory (tests use it, and
it also disables the harness CLIs, which would write to the real home anyway).

Keys: `↑↓` move, `space` toggle, `a` all, `n` none, `enter` apply, `?` launch hint,
`q` quit. Enter applies only the difference between the boxes and the files, then
re-reads them and prints one result line per change. `--check` prints the list and exits
1 if anything is missing; `--yes` installs everything without asking; a non-tty stdout
(a pipe, a herdr action, CI) falls back to a plain numbered list.

## Features

`herdr` is on by default: it adds the herdr directory, prompt delivery to idle non-
Claude agents, and `--mode pane`. `--no-default-features` builds a herdr-free binary
that still does everything else.

## Logging

`AGENTMAIL_LOG` (an `EnvFilter` string, default `warn`), always to stderr — in `mcp`
mode stdout carries JSON-RPC frames and nothing else.
