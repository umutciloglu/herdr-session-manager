# herdr-session-manager — implementation plan

Two products, one Cargo workspace, zero compile-time coupling between them.

| Product | Binary | Purpose | Depends on herdr |
| --- | --- | --- | --- |
| Session manager | `hsm` | Browse, search, restore, open agent sessions in herdr panes | Yes, it is a herdr plugin |
| Agent mail | `agentmail` | Cross-harness agent-to-agent messaging (MCP server + hooks) | No. Optional `herdr` feature adds a transport adapter |

Shared: one small library crate `herdr-client` (a generic herdr socket client). `agentmail` uses it only behind the `herdr` cargo feature. Nothing else is shared. Cross-product calls are shell-outs to the other binary, always optional.

## Principles

- Layered. Domain types have no deps. Services depend on domain. Adapters (herdr, harness, sqlite) implement traits the services define. Binaries wire it up.
- Loose coupling between products. `hsm` never imports an `agentmail` crate. `agentmail` never imports an `hsm` crate.
- Every functionality is available right after `herdr plugin install`. Harness-side setup (MCP registration, hooks) is a separate interactive action the user opts into.
- Rust 2021, tokio, anyhow for binaries, thiserror for libraries. `cargo clippy -- -D warnings` clean. Unit tests per crate, no network in tests.
- Match herdr plugin conventions (see the installed file-viewer plugin at `~/.config/herdr/plugins/github/herdr-file-viewer-*/` for a working manifest, launcher scripts, and fetch-or-build script).
- Comments explain why, not what. No scratch files in the repo.

## Workspace layout

```
herdr-session-manager/
├── Cargo.toml                    workspace, shared [workspace.dependencies]
├── herdr-plugin.toml             herdr plugin manifest (hsm only)
├── crates/
│   ├── herdr-client/             shared: JSONL socket client for the herdr API
│   ├── hsm-core/                 domain + harness registry + session index + open/restore service
│   ├── hsm-tui/                  ratatui popup: browse, search, pick action
│   ├── hsm/                      binary: browse | open | index | sessions | startup
│   ├── agentmail-core/           domain + sqlite store + registry + mailbox + resolver + delivery traits + poke socket
│   ├── agentmail-delivery/       adapters: claude channel, hooks, spawn, herdr (feature)
│   ├── agentmail-mcp/            MCP stdio server: tools, resources, channel notifications
│   └── agentmail/                binary: mcp | hook | send | inbox | wait | setup | sessions
├── scripts/                      launchers + fetch-or-build (sh + ps1)
├── docs/
│   ├── PLAN.md                   this file
│   ├── protocol.md               address format, message envelope, tool contracts
│   └── harnesses.md              resume args + transcript locations per harness
└── README.md
```

Dependency graph (arrows = depends on):

```
hsm ──> hsm-tui ──> hsm-core ──> herdr-client
agentmail ──> agentmail-mcp ──> agentmail-core
          └─> agentmail-delivery ──> agentmail-core
                                └─(feature herdr)─> herdr-client
```

## herdr facts the code relies on

- Transport: newline-delimited JSON over a local socket. Unix: Unix domain socket at `$HERDR_SOCKET_PATH` (default `~/.config/herdr/herdr.sock`, also on macOS). Windows: named pipe at the same env var. **The server closes the connection after one response**, so clients dial per request. Only `events.subscribe` connections stay open.
- Request: `{"id":"<string>","method":"agent.list","params":{}}`. Success: `{"id":..,"result":{"type":"agent_list", ...}}`. Error: `{"id":..,"error":{"code":"...","message":"..."}}`. Subscriptions keep the connection open; after a `subscription_started` ack, later lines are pushed events shaped `{"event": "pane_updated", "data": {...}}` (subscription names are dotted, pushed names use underscores). `session.snapshot` nests under `result.snapshot`.
- Full schema: `herdr api schema --json` (protocol 22). Snapshot: `herdr api snapshot`.
- Plugin runtime env: `HERDR_SOCKET_PATH`, `HERDR_BIN_PATH`, `HERDR_ENV=1`, `HERDR_PLUGIN_ID`, `HERDR_PLUGIN_ROOT`, `HERDR_PLUGIN_CONFIG_DIR`, `HERDR_PLUGIN_STATE_DIR`, `HERDR_PLUGIN_CONTEXT_JSON`, optional `HERDR_WORKSPACE_ID`, `HERDR_TAB_ID`, `HERDR_PANE_ID`. Popup panes do not get `HERDR_PANE_ID`; the focused tiled pane is inside `HERDR_PLUGIN_CONTEXT_JSON`.
- Agent panes launched by herdr integrations have `HERDR_ENV=1`, `HERDR_SOCKET_PATH`, `HERDR_PANE_ID` in their environment; MCP servers spawned by the agent inherit them.
- Methods used (params):
  - `session.snapshot {}` → workspaces, tabs, panes (each pane may carry `agent_session {agent, kind, source, value}`), agents.
  - `agent.list {}` → `{agents:[{agent, agent_session, agent_status: idle|working|blocked|done|unknown, cwd, pane_id, tab_id, workspace_id, terminal_title, terminal_title_stripped, focused, ...}]}`. Agent names are addressable via `agent.get {target}`.
  - `agent.start {name, kind, pane_id, args?: [..], timeout_ms?}` pane must be at a shell prompt; `timeout_ms` must be > 3000 and ≤ 300000.
  - `agent.prompt {target, text, wait?: {until?, timeout_ms?}}` returns `agent_blocked` error if the agent sits in a dialog.
  - `agent.wait {target, until?: [status..], timeout_ms?}`.
  - `agent.rename {target, name}`.
  - `pane.split {direction: right|down, target_pane_id?, cwd?, focus?, ratio?, env?, workspace_id?}` → new pane info.
  - `tab.create {cwd?, label?, focus?, workspace_id?}` → new tab + pane.
  - `pane.send_text {pane_id, text}`; `pane.send_input {pane_id, text?, keys?: ["enter"]}`.
  - `pane.current {caller_pane_id?}`; `pane.read {pane_id, source: visible|recent|recent_unwrapped|detection, lines?, strip_ansi?}`.
  - `plugin.pane.open {plugin_id, entrypoint, placement?: overlay|popup|split|tab|zoomed, direction?, width?, height?, focus?, target_pane_id?, cwd?, env?, workspace_id?}` returns plain `ok` for popups (no pane id); `popup.close {}`.
  - `events.subscribe {subscriptions:[...]}` streamed events include `pane.*`, `tab.*`, `workspace.*`, `layout.updated`; agent state changes surface as `pane.updated`.
- Herdr persists pane→session refs in `~/.config/herdr/session.json` (`workspaces[].tabs[].panes{id: {cwd, label?, agent_session?}}`). Only the current ref per pane.
- Native restore: on server restart herdr relaunches panes with a session ref using the harness resume command (see harnesses.md). We do not touch that.
- Plugin manifest: `[[build]]` (non-interactive, no socket), `[[startup]]` (one-shot after restore, has socket), `[[actions]]`, `[[panes]]` with `placement = "popup"`, `width`, `height`. Keybindings are user config `[[keys.command]] type = "plugin_action" command = "<plugin.id>.<action>"`.
- Rules: never `server.stop`; never close panes or workspaces we did not create; read-only calls against the live server are fine in tests; mutations only in unit tests with a fake server.

## Harness facts

See docs/harnesses.md for the table. Key items:

- Claude Code: transcripts `~/.claude/projects/<cwd-slug>/<session-uuid>.jsonl`. Lines are JSON objects with `type` (`user`, `assistant`, `summary`, `file-history-snapshot`, ...), `sessionId`, `cwd`, `timestamp`, `message{role, content: string | [{type:text,...}]}`. Resume: `claude --resume <id>`. Headless: `claude -p "<prompt>" [--resume <id>] [--fork-session] --output-format json`. Background: `claude --bg`, `claude attach <id>`. Hooks in `~/.claude/settings.json` under `hooks.<Event>[].hooks[] {type:"command", command, timeout}`. Stop hook input JSON on stdin: `{session_id, transcript_path, cwd, hook_event_name:"Stop", stop_hook_active, is_idle_stop?}`. Output JSON on stdout: `{"decision":"block","reason":"<text fed back to the model>"}` to continue; empty output to allow. MCP: `claude mcp add --scope user <name> -- <cmd> [args]`. Channels: server declares `capabilities.experimental["claude/channel"] = {}`, pushes `{"jsonrpc":"2.0","method":"notifications/claude/channel","params":{"content":"...","meta":{"k":"v"}}}`. Launch flag: `claude --dangerously-load-development-channels server:<name>`. Claude deletes transcripts after `cleanupPeriodDays` (default 30).
- Codex: rollouts `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`; first line `{type:"session_meta", payload:{id, cwd, timestamp, cli_version, source, thread_source, ...}}`. Codex keeps its own index in `~/.codex/state_5.sqlite` table `threads` (id, rollout_path, cwd, title, first_user_message, preview, name, is_pinned, updated_at, recency_at_ms, archived, git_branch, thread_source). Read only. Resume: `codex resume <id>`. Headless: `codex exec "<prompt>"`, `codex exec resume <id> "<prompt>"` (verify). Hooks: `~/.codex/hooks.json` same shape as Claude (`{"hooks":{"Stop":[{"hooks":[{"type":"command","command":..,"timeout":..}]}]}}`), needs `features.hooks = true` in config.toml (already on here). Stop output `{"decision":"block","reason":"..."}` makes Codex run a continuation turn with reason as the prompt. MCP: `codex mcp add <name> -- <cmd> [args]` writes `[mcp_servers.<name>]` in `~/.codex/config.toml`.
- Both harness hook scripts installed by herdr live at `~/.claude/hooks/herdr-agent-state.sh` and `~/.codex/herdr-agent-state.sh`. Never edit those. We add our own hook entries alongside.

## Product 1: hsm (session manager)

### hsm-core

Modules:

- `domain`: `HarnessKind` (enum of all 23 herdr kinds + Unknown(String)), `SessionRef {harness, kind: Id|Path, value}`, `Session {harness, id, cwd, project, title, first_prompt, started_at, last_active_at, size_bytes, transcript_path, transcript_present, tier: Hot|Warm|Gone, last_pane: Option<PaneRef>, pinned}`, `OpenTarget { Current, Split(Direction), Tab }`.
- `harness::registry`: `fn resume_args(kind, &SessionRef) -> Option<Vec<String>>` from harnesses.md table; `fn transcript_roots(kind) -> Vec<PathBuf>`; `fn executable(kind) -> &str`.
- `harness::claude`: parse a transcript head (first N lines) into `Session`; scan store; incremental by byte offset for message text.
- `harness::codex`: read `state_5.sqlite` `threads` table (read only, open with `?mode=ro`), fall back to scanning rollout `session_meta` lines.
- `harness::herdr_refs`: read `~/.config/herdr/session.json` and a live `session.snapshot` (via `herdr-client`) into `(PaneRef, SessionRef, cwd, label)` rows. This is tier 1 for every harness.
- `index`: sqlite (rusqlite, bundled) at `<state>/index.sqlite`. Tables: `sessions` (pk harness,id), `session_fts` (FTS5 over title, first_prompt, project), `messages_fts` (FTS5 over user/assistant text, hot tier only, external content), `scan_state` (path, byte_offset, mtime). `Index::refresh(opts)` incremental; `Index::search(query, filters, limit) -> Vec<Session>` ranked BM25 + boosts (live, same project, same workspace, recency). `Index::recent(limit)`.
- `open`: `OpenService` uses `herdr-client`: `open(session, target, ctx)`: Current → `pane.send_input {text: "<exe> <args>", keys:["enter"]}` to the context pane; Split → `pane.split` then `agent.start {kind, pane_id, name, args}`; Tab → `tab.create` then `agent.start`. If `agent.start` fails with not-ready/timeout, fall back to `pane.send_input`. Gone sessions open a fresh agent in the session cwd.
- `config`: TOML in `~/.config/hsm/config.toml` (override `HSM_CONFIG_DIR`; the herdr plugin dirs are ignored on purpose so agentmail's shell-out and the plugin share one index): `hot_days = 30`, `disabled_harnesses = []`, `extra_transcript_roots`, `default_open = "split"`, `agentmail_bin = "agentmail"`.

### hsm-tui

ratatui + crossterm. Single screen: search box on top, result list, preview pane (title, project, harness, dates, first prompt, last pane). Keys: type to search; ↑/↓; Enter = open with default target; `s` split, `t` tab, `c` current; `i` insert address into the calling pane (`pane.send_text` of `<harness>:<id8> `); `m` send a message (only if `agentmail` binary found: opens a one-line input, runs `agentmail send <addr> <text>`); `p` pin; `Esc` quit. Harness filter with Tab. Debounced search on the index (no re-scan while typing). Runs inside a herdr popup.

### hsm binary

Subcommands: `browse` (popup entry), `open <addr> [--target current|split|tab]`, `index [--full]`, `sessions --json [--limit N] [--query Q]` (provider output for agentmail, see protocol.md), `startup` (called by manifest startup hook: snapshot herdr refs, incremental index, exit), `setup-chat` (opens a popup running `agentmail setup` if the binary exists, else prints install hint). Reads `HERDR_*` env.

### herdr-plugin.toml

id `herdr-session-manager`, name, version, `min_herdr_version = "0.9.0"`, platforms linux/macos/windows. `[[build]]` fetch-or-build (sh / ps1). `[[startup]]` `hsm startup`. `[[actions]]`: `browse` (popup), `setup-chat`. `[[panes]]`: id `browser`, placement popup, width "85%", height "80%", command `["./target/release/hsm","browse"]` (Windows: absolute path via launcher). Suggested keybinding in README: `prefix+s` → `herdr-session-manager.browse`.

## Product 2: agentmail (cross-harness chat)

### agentmail-core

Modules:

- `domain`: `Harness` enum (Claude, Codex, Other(String)), `Address {harness, id}` with parse/display (`claude:8890a685`), `AddressTarget { Address, New(Harness), Alias(String) }`, `Message {id (ulid), from, to, text, reply_to, expects_reply, status: Pending|Delivered|Read|Failed, created_at, delivered_at}`, `Registration {harness, session_id, alias, pid, cwd, poke_path, herdr_pane, started_at, last_seen}`, `SessionCard {address, harness, project, title, started, last_active, state, first_prompt, last_user_message}`.
- `store`: sqlite at `<state>/agentmail.sqlite` (WAL). Tables `messages`, `registry`, `cursors`. API: `enqueue`, `pending_for(addr)`, `mark_delivered(ids)`, `mark_read`, `register/deregister/touch`, `live()` (pid-checked), `find_by_prefix(harness, prefix)`.
- `poke`: per-registration local socket (`interprocess` local socket; Unix path `<state>/poke/<harness>-<id>.sock`, Windows named pipe `\\.\pipe\agentmail-<harness>-<id>`). `Poker::poke(reg) -> Result<bool>` (false = nobody home). `PokeListener::bind(reg)` yields wakeups.
- `resolver`: `resolve(target, ctx) -> Resolved::{Live(reg), Offline(Address), Spawn(harness), Ambiguous(Vec<SessionCard>)}` using registry aliases, address prefixes, optional `Directory` trait (herdr adapter) and optional `SessionProvider` trait (shell-out to `hsm sessions --json`).
- `delivery` traits: `Deliverer { fn deliver(&self, msg, resolved) -> DeliveryOutcome::{Pushed, Queued, Spawned(Address), Failed(String)} }`, `Directory { fn live_agents() -> Vec<DirectoryEntry{address?, alias, state: Idle|Working|Blocked|Unknown, cwd, title}> }`, `SessionProvider { fn search(q, limit) -> Vec<SessionCard>; fn recent(limit) }`.
- `mailbox`: `Mailbox::send(target, text, opts) -> SendResult`, `reply`, `wait(for_addr, timeout, filter)`, `inbox(addr)`. Send = enqueue, resolve, deliver, record outcome. Never loses a row.
- `config`: `<state>/config.toml`: `session_provider = ["hsm","sessions","--json"]` optional, `codex_idle = "herdr" | "none"`, `spawn.claude_extra_args`, `claude_channel = true`.
- State dir: `AGENTMAIL_STATE_DIR` else `$XDG_STATE_HOME/agentmail` else `~/.local/state/agentmail` (Windows `%LOCALAPPDATA%\agentmail`).

### agentmail-delivery

- `claude_channel`: not a deliverer itself. The recipient's own MCP process owns the channel. Deliverer for a live Claude registration = poke. The MCP process, on poke or on startup, drains `pending_for(me)` and pushes channel notifications (see agentmail-mcp).
- `hook_drain`: used by `agentmail hook <harness>-stop`: reads hook input JSON from stdin, finds pending for `(harness, session_id)`, prints `{"decision":"block","reason":"<envelope(s)>"}` and marks delivered; prints nothing when empty. Also `session-start` hook: registers (harness, session_id, pid unknown, cwd) so a session is addressable even before its MCP process registers.
- `spawn`: `Spawner::ask(harness, prompt, resume?) -> reply text` via `claude -p` / `codex exec`; `Spawner::background(harness, resume?)` via `claude --bg`; `Spawner::pane(...)` only with herdr feature.
- `herdr` (feature): `HerdrDirectory` (agent.list → entries; maps herdr `agent_session.value` to `Address`), `HerdrPromptDeliverer` (idle check → `agent.prompt` with a one-line envelope; `agent_blocked` → Queued; subscribes `pane.updated` for retry once), `HerdrPaneSpawner`.
- `router`: picks deliverer chain for a resolved target: Live Claude → poke, else Queued. Live Codex → if herdr feature and directory says idle → prompt, else Queued (Stop hook drains). Offline → Queued. Spawn → Spawner by `mode`.

### agentmail-mcp

MCP over stdio. Prefer `rmcp` (server + macros). Requirements that may force a hand-rolled minimal JSON-RPC server: custom capability `experimental: {"claude/channel": {}}` in `initialize` result, custom outgoing notification `notifications/claude/channel`, resources with `resources/list_changed`. If rmcp cannot express these, implement the small stdio JSON-RPC loop directly (serde_json, tokio stdin/stdout, ~300 lines); keep tool/resource handlers transport-agnostic.

Tools (short descriptions, exact names):

- `agentmail_send {to: string, text: string, expects_reply?: bool, mode?: "auto"|"ask"|"background"|"pane"}` → `{message_id, to, outcome: "pushed"|"queued"|"spawned"|"ambiguous"|"failed", candidates?: SessionCard[], reply?: string}` (ask mode returns the reply inline).
- `agentmail_reply {message_id: string, text: string}` → same outcome shape.
- `agentmail_wait {timeout_s?: number (default 300, max 3600), reply_to?: string}` → `{message?: Message, timed_out: bool}`. Blocking; returns the next message addressed to me.
- `agentmail_find_session {query: string, harness?: string, project?: string, limit?: number (max 10)}` → `SessionCard[]` (delegates to SessionProvider; without one, searches registry + directory only).

Server `instructions` (~120 words): what an address looks like, use native Claude session messaging for a live Claude peer, use these tools for Codex/other harnesses/old sessions/spawning, keep messages short, reply with `agentmail_reply` when a message carries `expects_reply`.

Resources: list = live registrations + directory entries + provider `recent(40)`; URI `agentmail://session/<harness>:<id>`; name `<harness> · <project> · <title> · <age>`; `resources/read` returns a ~100-token card (text/markdown). Template `agentmail://session/{address}/transcript` returns transcript text via provider if available. Emit `notifications/resources/list_changed` when registry changes (poll registry mtime every 5 s or on poke).

Channel: on startup and on every poke, drain pending for self and send one `notifications/claude/channel` per message with `content` = envelope text and `meta = {from, message_id, expects_reply}`. Only when running under Claude (detect `CLAUDE_*` env or config); under Codex, pending mail is left for the Stop hook.

Identity: on startup determine `(harness, session_id)`: env `AGENTMAIL_HARNESS`/`AGENTMAIL_SESSION_ID` if set; else Claude: `CLAUDE_SESSION_ID`-style env if present, else parent process cmdline/cwd heuristics documented in code; else registry row from the session-start hook matching cwd + most recent. Register with pid, cwd, `HERDR_PANE_ID` if present.

### agentmail binary

`agentmail mcp` (stdio server), `agentmail hook claude-stop|codex-stop|claude-session-start|codex-session-start` (stdin JSON → stdout JSON), `agentmail send <to> <text> [--wait]`, `agentmail inbox [--addr]`, `agentmail wait`, `agentmail sessions [--json]`, `agentmail setup` (interactive checklist: Claude MCP, Claude Stop hook, Claude SessionStart hook, Claude channel flag hint, Codex MCP, Codex hooks; each item shows current state, installs/removes on toggle; writes JSON/TOML carefully, never touching herdr's entries; `--check` prints state and exits 0/1), `agentmail doctor`.

## Protocol (docs/protocol.md)

- Address: `<harness>:<id>`; `<id>` may be a unique prefix (≥ 8 chars) when resolving; full id when stored. `<harness>:new` spawns.
- Envelope (what a recipient model sees), single line header then body:
  `[agentmail] from claude:8890a685 (API authentication) · id 01J… · reply expected` newline body. Reply instruction appended when expects_reply: `Reply with agentmail_reply message_id=01J…`.
- Provider JSON (`hsm sessions --json`): `{"sessions":[SessionCard...]}` with `address`, `harness`, `project`, `title`, `started`, `last_active`, `first_prompt`, `transcript_path`, `resumable`.

## Phases and agents

Phase 1 (parallel, three agents): `herdr-client` (done; see crates/herdr-client/README.md for the API), `hsm-core`, `agentmail-core`.
Phase 2 (parallel, two agents): `hsm-tui + hsm + manifest + scripts`, `agentmail-delivery + agentmail-mcp + agentmail`.
Phase 3: review, fixes, `cargo build --release`, `cargo test`, `cargo clippy -D warnings`, link plugin locally with `herdr plugin link`, smoke test read-only paths.

Verified 2026-09-12: Claude `@` resource mention attaches the card on send (picker works); the Codex app-server daemon can drive a headless turn but does NOT attach to the interactive TUI, so it cannot wake a visible idle Codex — idle-wake stays on herdr and the `app-server` config option was removed. Still deferred: org channel enablement; Windows named pipe.
