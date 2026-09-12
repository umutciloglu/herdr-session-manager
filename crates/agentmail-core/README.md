# agentmail-core

The heart of agentmail: cross-harness agent-to-agent messaging. Domain types, a sqlite
store, address resolution, wake-up sockets, and the mailbox service.

This crate is standalone. It does not depend on `herdr-client` or any `hsm` crate, and it
knows nothing about MCP, hooks, or spawning agents — those live behind the traits in
`traits`, implemented by `agentmail-delivery` and wired up by the `agentmail` binary.

## Layers

```
domain                      no dependencies of its own
  ├─ store        sqlite: messages, registry, cursors
  ├─ resolver     target -> Resolved, over store + Directory + SessionProvider
  └─ mailbox      send / reply / wait / inbox, over store + resolver + Deliverers
traits                      Directory, SessionProvider, Deliverer, ProcessProbe
paths / config / poke / ids  process-local plumbing
```

## State

Everything lives under one directory, resolved as
`AGENTMAIL_STATE_DIR` → `$XDG_STATE_HOME/agentmail` → `~/.local/state/agentmail`
(`%LOCALAPPDATA%\agentmail` on Windows):

```
<state>/agentmail.sqlite     messages + registry + cursors (WAL)
<state>/config.toml          optional, every field has a default
<state>/poke/<harness>-<id>.sock   one wake-up socket per live session (Unix)
```

On Windows the poke socket is a named pipe, `\\.\pipe\agentmail-<harness>-<id>`.

## Invariants

- **A message is written before anything else happens.** `send` enqueues as `Pending`,
  then resolves, then delivers. A crash, a dead peer or a broken adapter can lose a
  delivery attempt, never a message. Ambiguous and not-found sends keep their row too,
  marked `Failed` with the reason.
- **`Delivered` is not `Read`.** Delivered means a transport accepted it; read means the
  recipient consumed it through `wait` or `reply`. `wait` hands over both.
- **A poke carries no payload.** It only says "check the store", so the store stays the
  single source of truth and the socket stays trivial.
- **A broken `SessionProvider` is a missing one.** Non-zero exit or malformed JSON
  degrades to registry + directory; it never fails a send.
- **Ambiguity is never guessed.** Two matches stop as `Ambiguous` with candidate cards.
- **A hook row is a claim, not a process.** A SessionStart hook records that a session
  existed; nothing tells us when its pane closed. So a registration with no pid counts as
  live only inside `HOOK_ROW_TTL` (10 min), and only if a `Directory` confirms it —
  the multiplexer is authoritative whenever one is wired up. A proven pid is live
  regardless. Not live is not gone: an unconfirmed session still resolves `Offline`, which
  is what lets ask mode resume it. `Store::prune` clears dead pids and pid-less rows older
  than 24 h; `human:<name>` rows survive forever.
- **Ask mode has two shapes.** A spawner either returns `DeliveryOutcome::Replied` with
  the one-shot answer, or enqueues a reply and returns `Spawned`; `send` picks up either
  without blocking. A headless run that leaves no resumable session reports
  `Replied { from: None }` rather than inventing an address for the row.

## Resolution order

`exact address in registry → alias in registry → alias in directory → prefix (≥ 8 chars)
in registry → prefix in directory → provider search`. Registry lookups run over every row,
not just the live ones, and each match is then judged `Live` or `Offline` by the rule
above. A full-looking address that nothing recognises resolves to `Offline` rather than
`NotFound`: the message queues, and a SessionStart hook drains it once that session
appears.

## Tests

`cargo test -p agentmail-core`. No network, no shared global state; the store and poke
tests run in temp dirs.
