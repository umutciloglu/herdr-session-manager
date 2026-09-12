# hsm-core

Session index and restore service behind the `hsm` binary. No herdr, no
network, no tokio runtime of its own — the binary supplies the adapters.

## Layers

| Module | Responsibility |
| --- | --- |
| `domain` | `HarnessKind`, `SessionRef`, `Session`, `Address`, `OpenTarget`, `SessionCard`. Plain types. |
| `harness::registry` | Resume args, executable and transcript roots per kind (docs/harnesses.md). |
| `harness::claude` | `~/.claude/projects/<cwd-slug>/<uuid>.jsonl` head/tail parse + incremental message extraction. |
| `harness::codex` | `~/.codex/state_5.sqlite` `threads` (read-only), falling back to `~/.codex/sessions/**/rollout-*.jsonl`. |
| `harness::herdr_refs` | `~/.config/herdr/session.json` pane → session refs (tier 1, every harness). |
| `index` | sqlite + FTS5 at `<state>/index.sqlite`: `refresh`, `search`, `recent`, `get`, `pin`, `last_refresh`. |
| `open` | `OpenService` drives the `PaneOps` trait to restore a session into a pane. |
| `config`, `paths` | TOML config and the state/config directory rules. |

## Traits the binary implements

- `LiveSessions::live() -> Vec<LivePane>` — the running herdr snapshot. `NoLive`
  is the offline stand-in.
- `PaneOps` — `send_input`, `split`, `create_tab`, `agent_start`. `async_trait`.
  `SplitDirection::Horizontal` maps to herdr's `right`, `Vertical` to `down`.

## Ranking

BM25 over `sessions_fts` (title, first prompt, project) and `messages_fts`
(user/assistant prose of hot sessions), normalised to 0..1, then boosted:
live pane +5, same project +2, pinned +1.5, recency +3·e^(−days/30). An empty
query is `recent`.

## Tiers

`Hot` = active within `hot_days` (messages indexed). `Warm` = older. `Gone` =
we scan this harness's store (Claude, Codex) and the transcript is missing
(Claude prunes after `cleanupPeriodDays`); those sessions open a fresh agent in
the old cwd instead of resuming. A harness with no scanned store is never Gone:
the herdr session ref is all there ever was, and it still resumes.

## Config (`<config dir>/config.toml`)

```toml
hot_days = 30
disabled_harnesses = []
extra_transcript_roots = []
default_open = "split"        # current | split | split-vertical | tab
agentmail_bin = "agentmail"
```

State dir: `HSM_STATE_DIR`, else `~/.local/state/hsm` (the herdr plugin state
dir is ignored so every entrypoint shares one index). Config dir:
`HSM_CONFIG_DIR`, else `~/.config/hsm`.

## Timing a real refresh

```sh
cargo run -p hsm-core --release --example refresh -- --state-dir /tmp/hsm
```

Read-only against the harness stores.
