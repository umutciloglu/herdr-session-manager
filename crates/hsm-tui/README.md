# hsm-tui

The session browser screen: ratatui 0.30 over crossterm 0.29. One layout —
search box, result list, preview, status, key hints.

Knows nothing about herdr or sqlite. Everything it can do goes through two
inputs the `hsm` binary supplies:

```rust
hsm_tui::run(Box::new(actions), BrowseContext {
    invoking_pane: Some("w1:p3".into()),   // the tiled pane behind the popup
    invoking_pane_has_agent: false,        // is it already running an agent?
    default_open: OpenTarget::Split(SplitDirection::Horizontal),
    purpose: Purpose::Browse,              // or Ask
})
```

`Purpose::Ask` is the same screen with opening turned off: Enter opens a
one-line compose box under the list (500 characters), Enter there sends the
question through `Actions::ask`, and if the peer can answer now the screen polls
`Actions::poll_reply` every 2 s for `ask_wait` (120 s by default), counting down
in the status line. The answer lands in the preview and Enter asks a follow-up.
Esc cancels the box; Esc while waiting stops waiting without closing the popup.

`r` shows the replies panel: mail `Actions::replies` reports as unseen, listed
with sender, age, id and text. Opening it calls `Actions::mark_seen`, so the
count only ever includes what is new.

`Actions` is sync (`search`, `recent`, `get`, `pin`, `open`, `insert_address`,
`message`, `can_message`); the binary blocks on its async herdr client behind
it. That also makes the whole screen testable with a fake — see `src/testing.rs`.

## Panels

One app, three panels, switched with `s` (search), `a` (ask) and `r` (replies).
Switching keeps the query, the harness filter and the selection.
`BrowseContext.start_panel` only says which one the popup opens on: `hsm browse`
starts on search, `hsm ask` on ask.

- **search** — the session list, preview, and the open keys.
- **ask** — the same list with opening off. Enter opens a one-line compose box
  under it (500 characters); Enter there sends through `Actions::ask`, and if the
  peer can answer now the screen polls `Actions::poll_reply` every 2 s for
  `ask_wait` (120 s by default) with a countdown in the status line. The answer
  lands in the preview and Enter asks a follow-up. Esc cancels the box; Esc while
  waiting stops waiting without closing the popup.
- **replies** — full screen. Everything `Actions::replies` reports, newest first:
  age, sender, that session's title, the first line, and `•` while unseen. Enter
  reads one in full (and marks it seen through `Actions::mark_seen`), Esc goes
  back to the list, `a` answers its sender on the ask panel.

## Keys

The search box is focused at startup, so typing filters immediately (debounced
120 ms; an empty query lists `Index::recent`). Bare letters cannot both type and
act, so there are two modes:

| Key | Search mode | Normal mode (`Esc`) |
| --- | --- | --- |
| letters | type into the query | act (below) |
| `↑` `↓` `PgUp` `PgDn` | move the selection | same |
| `Enter` | open / ask / read, by panel | same |
| `Tab` | cycle harness: all → claude → codex → … | same |
| `Esc` | leave the box | leave the reply reader, else quit |
| `Backspace` | delete, back to the box | same |

Actions, bare in normal mode and with `Alt` in either mode: `s` `a` `r` switch
panel, `v` split right, `d` split down, `t` new tab, `c` the invoking pane,
`i` insert `<harness>:<id8> ` into it, `m` message via agentmail, `p` pin,
`/` back to the box, `q` quit. `Ctrl+C` always quits.

`c` is refused with a status message when the invoking pane already runs an
agent: typing a resume command there would feed the agent a prompt instead.
`m` is refused when the binary found no `agentmail`. A send is queued rather
than run inline: the loop draws the "sending to …" notice first and only then
blocks on `Actions::message`, which is what `App::run_pending` is for.

After an open or an insert the loop ends — a herdr popup closes when its
process exits. Errors land in the status line and the popup stays.

## Row glyphs

`@` live idle · `>` live working · `!` live blocked · `+` hot · `-` warm ·
`x` gone · `*` (second column) pinned. A gone session says
"not resumable, opens a fresh agent in \<cwd\>" in the preview; opening it still
works.

## Tests

`cargo test -p hsm-tui` — key handling against a fake `Actions`, and frames
rendered through ratatui's `TestBackend` asserted as text. No terminal needed.
