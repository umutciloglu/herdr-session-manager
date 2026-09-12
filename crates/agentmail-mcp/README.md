# agentmail-mcp

One MCP server per agent session: the process that lets a model send mail, and the
thing that hands it mail it has received.

## Layers

```
service   every decision, no MCP types: tools, resources, the channel drain
server    rmcp over stdio; shape conversion only
identity  which session this process belongs to
lib       run(): registry row, poke socket, deliverer chain, background tasks
```

## rmcp, not a hand-rolled loop

The server needs three things that are not in the plain MCP spec, and rmcp 3 expresses
all of them: `capabilities.experimental = {"claude/channel": {}}` via
`ServerCapabilities::experimental`, an arbitrary outgoing notification via
`ServerNotification::CustomNotification`, and resources with `list_changed`.

## Identity

`AGENTMAIL_HARNESS`/`AGENTMAIL_SESSION_ID` → the harness's own variables (`CLAUDECODE` +
`CLAUDE_CODE_SESSION_ID`, `CODEX_THREAD_ID`/`CODEX_*`, all inherited by child processes)
→ the SessionStart-hook row for this very pane (`HERDR_PANE_ID`, exact under herdr) →
the newest unclaimed hook row for this directory started in the last 120 s → a
provisional `unk<pid>` row with a warning.

A provisional identity is not permanent: the hook row may simply not have been written
yet. Every poke, every tool call and every 30 s touch asks the session loop to retry,
and when it succeeds the loop re-binds the wake-up socket, rewrites the registry row and
logs `adopted the real session id` once.

## Channel

Under Claude, pending mail is drained on startup and on every poke, one
`notifications/claude/channel` per message. A push is an *attempt*: the row keeps its
`Pending` status and gets a `pushed_at` stamp, because only a session launched with the
channel flag ever receives the event. At turn end the Stop hook looks each pushed id up
in the session transcript — Claude records channel events there — and marks it read
instead of repeating it when it finds one. A later tool call acknowledges the rest. A poke also wakes any blocked
`agentmail_wait` — the session loop owns the listener and rings a `Notify`, which
restarts the mailbox wait instead of leaving it to the 250 ms poll. Under Codex there is
no listener, so `agentmail_wait` polls. Under Codex there is no channel and no poke
socket is bound at all — a socket that answered but did nothing would make senders
believe the message landed, instead of letting them prompt or queue it for the Stop
hook.

Claude only opens the channel when it is launched with
`claude --dangerously-load-development-channels server:agentmail`.

## Sessions

`resources/list`, `agentmail_find_session` and `agentmail sessions` all come from one
merge: the registry, the herdr directory and the session index, in that order, with each
address filled in from the sources behind it. Liveness and pane names come from the
first two; titles, first prompts and timestamps only exist in the index, so dropping the
duplicates would cost one half of every live session's card.

## Tests

`cargo test -p agentmail-mcp` runs every handler against an in-memory store: the
initialize result shape, the tool list, each tool, resources and the channel
notification bytes.
