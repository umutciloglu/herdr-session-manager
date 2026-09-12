# Harness table

Source: herdr 0.9.0 docs "Session state and restore" + local inspection (2026-09-12).

| Kind (herdr) | Executable | Resume command | Session ref kind | Transcript store |
| --- | --- | --- | --- | --- |
| claude | claude | `claude --resume <id>` | id | `~/.claude/projects/<cwd-slug>/<id>.jsonl` |
| codex | codex | `codex resume <id>` | id | `~/.codex/sessions/YYYY/MM/DD/rollout-*-<id>.jsonl`, index `~/.codex/state_5.sqlite` |
| pi | pi | `pi --session <path-or-id>` | id or path | unknown |
| omp | omp | `omp --resume=<path-or-id>` | id or path | unknown |
| agy | agy | `agy --conversation <id>` | id | unknown |
| cursor | cursor-agent | `cursor-agent --resume <id>` | id | unknown |
| grok | grok | `grok --resume <id>` | id | unknown |
| copilot | copilot | `copilot --resume=<id>` | id | unknown |
| devin | devin | `devin --resume <id>` | id | unknown |
| droid | droid | `droid --resume <id>` | id | unknown |
| kimi | kimi | `kimi --session <id>` | id | unknown |
| qodercli | qodercli | `qodercli --resume <id>` | id | unknown |
| qwen | qwen | `qwen --resume <id>` | id | unknown |
| opencode | opencode | `opencode --session <id>` | id | unknown |
| kilo | kilo | `kilo --session <id>` | id | unknown |
| hermes | hermes | `hermes --resume <id>` | id | unknown |
| mastracode | mastracode | `mastracode --thread <id>` | id | unknown |

Kinds herdr detects but cannot resume (tier 1 metadata only): gemini, cline, amp, kiro, maki, muse.

Claude Code cwd slug: the absolute cwd with `/` replaced by `-` (e.g. `/Users/x/proj` → `-Users-x-proj`). Verify against `~/.claude/projects` on the machine.

Headless / spawn:

| Harness | Ask (one-shot) | Background | Notes |
| --- | --- | --- | --- |
| claude | `claude -p "<prompt>" [--resume <id> --fork-session] --output-format json` | `claude --bg [--resume <id>]`, later `claude attach <id>` | `-p` loads user MCP config |
| codex | `codex exec "<prompt>"`, `codex exec resume <id> "<prompt>"` (verify) | none known | |
