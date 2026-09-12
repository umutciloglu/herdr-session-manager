//! Terminal setup and the event loop.

use std::time::Instant;

use crossterm::event::{self, Event};
use ratatui::DefaultTerminal;

use crate::actions::{Actions, BrowseContext, Result};
use crate::app::{App, Status};
use crate::ui;

/// Runs the browser until the user leaves or an action is taken, and returns
/// the last status line so a caller whose terminal survives can print it.
pub fn run(actions: Box<dyn Actions>, ctx: BrowseContext) -> Result<Status> {
    // Build the state before touching the terminal: a failing index shows up as
    // a status line instead of a half-initialised screen.
    let mut app = App::new(actions, ctx);

    // `try_init` rather than `ratatui::run`, which panics when there is no tty;
    // a plain error is what a CLI caller can act on. It installs the panic hook
    // that restores the terminal.
    let mut terminal = ratatui::try_init()?;
    let outcome = event_loop(&mut terminal, &mut app);
    if let Err(error) = ratatui::try_restore() {
        tracing::warn!(%error, "could not restore the terminal");
    }
    outcome
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<Status> {
    while !app.should_exit() {
        terminal.draw(|frame| ui::render(frame, app))?;
        // Queued work runs only now, with its notice already on screen.
        if app.run_pending() {
            continue;
        }
        if event::poll(app.poll_timeout(Instant::now()))? {
            if let Event::Key(key) = event::read()? {
                app.on_key(key);
            }
        }
        app.tick();
    }
    Ok(app.status().clone())
}
