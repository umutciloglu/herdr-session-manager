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
})
```

`Actions` is sync (`search`, `recent`, `get`, `pin`, `open`, `insert_address`,
`message`, `can_message`); the binary blocks on its async herdr client behind
it. That also makes the whole screen testable with a fake — see `src/testing.rs`.

## Keys

The search box is focused at startup, so typing filters immediately (debounced
120 ms; an empty query lists `Index::recent`). Bare letters cannot both type and
act, so there are two modes:

| Key | Search mode | Normal mode (`Esc`) |
| --- | --- | --- |
| letters | type into the query | act (below) |
| `↑` `↓` `PgUp` `PgDn` | move the selection | same |
| `Enter` | open with `default_open` | same |
| `Tab` | cycle harness: all → claude → codex → … | same |
| `Esc` | leave the box | quit |
| `Backspace` | delete, back to the box | same |

Actions, bare in normal mode and with `Alt` in either mode: `s` split right,
`d` split down, `t` new tab, `c` the invoking pane, `i` insert `<harness>:<id8> `
into it, `m` message via agentmail, `p` pin, `/` back to the box, `q` quit.
`Ctrl+C` always quits.

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
