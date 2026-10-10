# agentmail for Claude Code

Agent-to-agent mail between Claude Code, Codex and other sessions, as a Claude Code plugin. The project README explains what agentmail does. This file covers only the plugin.

```
/plugin install agentmail --marketplace umutciloglu/herdr-session-manager
```

## What it bundles

| Piece | What runs |
| --- | --- |
| MCP server `agentmail` | `scripts/agentmail mcp` |
| Stop hook | `sh scripts/agentmail hook claude-stop` |
| SessionStart hook | `sh scripts/agentmail hook claude-session-start` |

These are the same entries `agentmail setup` writes into `~/.claude.json` and `~/.claude/settings.json`. If you ran setup before, run it again and let it remove its copies. Otherwise every message arrives twice. Setup only recognises the plugin when it is installed at user scope. With a project or local install, remove the Claude rows in setup yourself.

The hooks start the launcher through `sh`, so they do not depend on its executable bit. Their timeout is 120 seconds. A normal run takes milliseconds. The first run may wait up to a minute for another session's download and then download itself.

Channels need the plugin's name:

```sh
claude --dangerously-load-development-channels plugin:agentmail@herdr-session-manager
```

## The binary

The plugin ships launchers, not a binary. The first run for a plugin version does this:

1. Read the version from `.claude-plugin/plugin.json`.
2. Download `agentmail-<platform>` and `SHA256SUMS` from the GitHub release of that version.
3. Check the SHA-256. On a mismatch nothing is installed.
4. Rename it into `~/.claude/plugins/data/<plugin id>/bin/<version>/`.

Later runs only check that the file is there. Each version gets its own folder, so an update never overwrites a binary that a running session still uses. Old version folders are not deleted. Uninstalling the plugin removes them all.

Two sessions that start at once share one download. One takes a lock file, the others wait for it.

Prebuilt binaries: macOS arm64 and x86_64, Linux x86_64, Windows x86_64. Anything else stops with a message. You can build agentmail yourself and point the launcher at the result:

```sh
cargo install --git https://github.com/umutciloglu/herdr-session-manager agentmail
export AGENTMAIL_BIN="$HOME/.cargo/bin/agentmail"   # in the environment Claude Code starts from
```

`AGENTMAIL_PLUGIN_VERSION` fetches another release than the plugin's own version. It is meant for testing.

## Windows

- **MCP server.** Claude Code starts `scripts/agentmail` without a shell. On Windows that name resolves through `PATHEXT` to `scripts/agentmail.cmd`. That file runs `agentmail.ps1` to fetch the binary, then starts the binary itself. cmd.exe passes the MCP pipes through untouched. PowerShell would re-encode the output line by line.
- **Hooks.** Claude Code runs hook commands through Git Bash, which runs the same POSIX `scripts/agentmail` as on macOS and Linux. Without Git for Windows, Claude Code falls back to PowerShell, and the hooks fail. Adding the marketplace from GitHub needs git anyway.

## Files

- `.claude-plugin/plugin.json`: name and version. The release workflow refuses a tag that this version disagrees with.
- `.mcp.json`, `hooks/hooks.json`: the server and the two hooks.
- `scripts/agentmail`: POSIX launcher.
- `scripts/agentmail.cmd`, `scripts/agentmail.ps1`: Windows launcher for the MCP server.
