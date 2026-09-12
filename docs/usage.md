# Using herdr-session-manager

Two tools. `hsm` is the herdr plugin for sessions. `agentmail` is agent-to-agent chat. Each works without the other.

## Install

```sh
herdr plugin install <owner>/herdr-session-manager
```

The build links `hsm` and `agentmail` into `~/.local/bin` and adds that directory to your shell rc if it is not on `PATH`. Open a new shell afterwards. Then bind keys in `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+s"
type = "plugin_action"
command = "herdr-session-manager.browse"

[[keys.command]]
key = "prefix+a"
type = "plugin_action"
command = "herdr-session-manager.ask"

[[keys.command]]
key = "prefix+shift+s"
type = "plugin_action"
command = "herdr-session-manager.setup-chat"
```

Reload with `herdr server reload-config`.

## Session browser

Press the browse key. Type to filter. The list shows every session the index knows: live agents in herdr, past Claude Code and Codex sessions, and herdr-only refs for other harnesses.

One popup, three panels: **search**, **ask** and **replies**. `s`, `a` and `r`
switch between them and keep your query, filter and selection.

| Key | Action |
| --- | --- |
| type | filter |
| `↑` `↓` | move |
| `Tab` | cycle harness filter |
| `Esc` | leave the search box; again to quit |
| `s` `a` `r` | search panel, ask panel, replies panel |
| `q` | quit |

Search panel:

| Key | Action |
| --- | --- |
| `Enter` | jump to the live pane, otherwise open with the default target (the `jump` key; rebindable) |
| `o` | open in split right (same as `v`; rebindable) |
| `v` `d` `t` `c` | open in split right, split down, new tab, current pane |
| `i` | insert the session address into your pane, then close |
| `m` | send a one-line message to the selected session |
| `p` | pin |

Ask panel: `Enter` opens a one-line box, `Enter` sends the question and the
popup watches for the answer (`ask_wait_secs`, 120 s by default) with a
countdown; `Esc` cancels the box, or stops waiting without closing the popup.
Answers show in the preview, and `Enter` asks a follow-up.

Replies panel: everything that has answered you, newest first, `•` marking what
you have not read. `↑` `↓` move, `Enter` reads one in full, `Esc` goes back to
the list, `a` answers its sender on the ask panel.

Letters act after `Esc`, or with `Alt` held while typing. `c` is refused when your pane already runs an agent.

Row glyphs: `@` live idle, `>` working, `!` blocked, `~` running with no pane, `+` recent, `-` older, `x` transcript gone, `*` pinned. A row whose agent is running in herdr right now also carries a `live` tag; `Enter` on one of those focuses that pane instead of starting a second copy. If the pane has closed since the popup opened, it opens the session instead.

A `~` row is alive in a process of its own: a Claude Code background job (`claude --bg`, `/jobs`) or a Claude running in a terminal outside herdr, tagged `job` and `run` respectively. Only Claude publishes such a registry (`~/.claude/sessions`); a Codex process outside herdr is invisible to hsm.

A job has no pane, but the interactive Claude watching one puts the job's name in its terminal title, so hsm matches that title against the job name and `Enter` jumps to that pane — the preview says which (`57845 job busy in pane w9:p7`). A job nobody is viewing opens instead.

A session marked gone opens a fresh agent in the same directory. Claude Code deletes transcripts after its `cleanupPeriodDays` setting, 30 days by default.

## hsm on the command line

```
hsm browse              the popup entrypoint
hsm open <addr>         restore a session; --target current|split|split-down|tab
hsm index [--full]      refresh the index
hsm sessions [--json]   list; --query accepts text or an address prefix
hsm startup             what the plugin runs after herdr restores a session
hsm setup-chat          runs agentmail setup
```

Config at `~/.config/hsm/config.toml`:

```toml
hot_days = 30                 # sessions younger than this get full-text search
disabled_harnesses = []       # e.g. ["codex"]
extra_transcript_roots = []   # extra Claude project dirs
default_open = "split"        # current | split | split-down | tab
agentmail_bin = "agentmail"   # or an absolute path

[keys]
jump = "enter"                # enter | tab | a letter | alt-<letter>
open_split = "o"
```

Letters type into the search box until you press `Esc`; `alt-<letter>` acts anywhere. A configured key wins over the built-in one with the same letter. Jumping lives on the `jump` key alone, so `Enter` jumps only while `jump = "enter"`, the default; bind it elsewhere and `Enter` goes back to plain opening.

Index and state live in `~/.local/state/hsm`.

## Agent chat

Run `agentmail setup` once. It is a checklist: arrows move, space toggles, `a` all, `n` none, enter applies. It installs the MCP server and the Stop and SessionStart hooks into Claude Code and Codex. Nothing is installed until you press enter.

Codex asks once whether to trust the new hooks. That prompt returns only when the hooks file changes.

### Addresses

`<harness>:<session id>`, for example `claude:8890a685-...`. A unique prefix of eight or more characters works. `claude:new` and `codex:new` start a fresh session.

### From inside Claude Code

Type `@agentmail` and pick the session. That attaches a small card with the address. Then say what you want sent. Claude calls `agentmail_send`. With `expects_reply` the peer is told to answer with `agentmail_reply`, and the answer comes back into your session.

To receive replies while idle, start Claude with channels:

```sh
claude --dangerously-load-development-channels server:agentmail
```

Without the flag, replies arrive at the end of your next turn through the Stop hook.

### From inside Codex

Codex has no session picker. Use the herdr popup with `i` to paste an address, or name the session in words and Codex will call `agentmail_find_session`. Mail reaches Codex when it is idle in herdr, or at the end of its current turn.

### From the shell

```
agentmail send <addr> "<text>" [--expects-reply] [--mode ask|background|pane] [--wait <s>]
agentmail inbox [--addr <addr>]
agentmail wait
agentmail sessions
agentmail doctor
```

`--mode ask` runs the target headless and prints the answer. `background` starts a background Claude session with the message as its first prompt. `pane` opens the agent in a herdr pane.

### What a peer sees

```
[agentmail] from claude:8890a685 (API authentication) · id 01J... · reply expected
<your text>

Reply with agentmail_reply message_id=01J...
```

Config at `~/.local/state/agentmail/config.toml`:

```toml
session_provider = ["hsm", "sessions", "--json"]   # how agentmail searches past sessions
codex_idle = "herdr"                                # or "none"
claude_channel = true

[spawn]
claude_extra_args = []
codex_extra_args = []
```

## When something looks wrong

- **A card in Claude's `@` list is thin or stale.** The MCP process for that session predates a rebuild. Run `/mcp` and reconnect, or restart the session.
- **A message says queued and nothing happens.** The target is busy or in a dialog. It gets the mail when its turn ends. `agentmail inbox --addr <addr>` shows the row.
- **Two copies of a message.** Two hook entries pointed at different copies of the binary. Run `agentmail setup` once; it collapses duplicates.
- **`agentmail doctor`** prints the state dir, identity, live registrations, provider status, and herdr reachability.
