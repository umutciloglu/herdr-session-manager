# agentmail protocol

## Address

`<harness>:<id>` — harness is `claude`, `codex`, or another herdr kind. `<id>` is the harness session id. When resolving, a unique prefix of at least 8 characters is accepted. `<harness>:new` means spawn a new session.

Aliases: a live registration may carry an alias (herdr agent name, or `agentmail alias <name>`). Aliases resolve before prefixes.

## Envelope

What a recipient model receives, whether via channel notification, Stop hook reason, or herdr prompt:

```
[agentmail] from claude:8890a685 (API authentication) · id 01JXYZ… · reply expected
<body>

Reply with agentmail_reply message_id=01JXYZ…
```

The last line is present only when `expects_reply` is true.

## Tool contracts

See docs/PLAN.md "agentmail-mcp". Tool names: `agentmail_send`, `agentmail_reply`, `agentmail_wait`, `agentmail_find_session`.

## Session provider

`agentmail` can call an external command to search sessions it does not know about. Default `["hsm", "sessions", "--json"]`. Contract: stdin unused; args appended: `--query <q> --limit <n> [--harness <h>] [--project <p>]`; stdout JSON:

```json
{"sessions":[{"address":"claude:8890a685-...","harness":"claude","project":"trade-help","cwd":"/abs/path","title":"API authentication","started":"2026-09-10T08:00:00Z","last_active":"2026-09-12T07:55:00Z","first_prompt":"...","transcript_path":"/abs/path.jsonl","resumable":true,"pane":{"pane_id":"w6:p1","workspace_id":"w6","tab_id":"w6:t1","live":true,"status":"idle"}}]}
```

Exit code non-zero or malformed output = provider unavailable; agentmail degrades to registry + directory.

## Hook I/O

Stdin: the harness hook JSON (`session_id`, `cwd`, `hook_event_name`, `transcript_path`, ...). Stdout: for Stop hooks, `{"decision":"block","reason":"<envelopes>"}` when pending mail exists, otherwise nothing. Exit code always 0.
