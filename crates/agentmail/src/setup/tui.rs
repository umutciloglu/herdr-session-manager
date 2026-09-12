//! The setup checklist as one screen.
//!
//! Nothing here knows what a Claude hook or a Codex MCP table is: it drives the
//! [`Installer`] the `setup` module implements, so the whole screen can be tested with a
//! fake that only records what it was asked to do.

use std::io::IsTerminal;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};

const NAME_W: usize = 34;
const FILE_W: usize = 26;
const HELP: &str = "↑↓ move · space toggle · a all · n none · enter apply · ? hint · q quit";
/// Codex verifies a hash of every hook handler and asks the user to trust it again
/// whenever hooks.json changes. We cannot pre-approve that for them.
const CODEX_TRUST: &str = "Codex asks you to trust its hooks once after each change to hooks.json.";

/// One checklist row as the installer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemState {
    pub label: String,
    pub file: String,
    pub installed: bool,
    /// A row that cannot be installed at all — the launch flag is advice, not a file.
    pub informational: bool,
}

/// What the screen is allowed to do. `setup::Setup` is the real one.
pub trait Installer {
    /// Binary path and home directory, for the title.
    fn title(&self) -> (String, String);
    fn items(&self) -> Vec<ItemState>;
    fn install(&self, index: usize) -> anyhow::Result<()>;
    fn uninstall(&self, index: usize) -> anyhow::Result<()>;
    /// Lines to show when the informational row is opened.
    fn hint(&self) -> Vec<String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub item: ItemState,
    /// What the user wants; `installed` is what is true right now.
    pub checked: bool,
}

pub struct App {
    installer: Box<dyn Installer>,
    rows: Vec<Row>,
    cursor: usize,
    results: Vec<String>,
    hint: Vec<String>,
    exit: bool,
}

impl App {
    pub fn new(installer: Box<dyn Installer>) -> App {
        let rows = rows_of(installer.items());
        App {
            installer,
            rows,
            cursor: 0,
            results: Vec::new(),
            hint: Vec::new(),
            exit: false,
        }
    }

    pub fn should_exit(&self) -> bool {
        self.exit
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.exit = true,
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Char('a') => self.set_all(true),
            KeyCode::Char('n') => self.set_all(false),
            KeyCode::Char('?') => self.show_hint(),
            KeyCode::Enter => match self.rows.get(self.cursor).map(|r| r.item.informational) {
                // Enter on the advice row opens the advice instead of installing
                // everything: it is the one row where Enter means something else.
                Some(true) => self.show_hint(),
                _ => self.apply(),
            },
            _ => {}
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() - 1;
        self.cursor = match delta {
            d if d < 0 => self.cursor.checked_sub(1).unwrap_or(last),
            _ => (self.cursor + 1) % self.rows.len(),
        };
    }

    fn toggle(&mut self) {
        if let Some(row) = self.rows.get_mut(self.cursor) {
            if row.item.informational {
                self.hint = self.installer.hint();
                return;
            }
            row.checked = !row.checked;
        }
    }

    fn set_all(&mut self, checked: bool) {
        for row in self.rows.iter_mut().filter(|r| !r.item.informational) {
            row.checked = checked;
        }
    }

    fn show_hint(&mut self) {
        self.hint = self.installer.hint();
    }

    /// Applies only the difference between what is checked and what is installed, then
    /// re-reads the world so the boxes cannot drift from the files.
    pub fn apply(&mut self) {
        self.results.clear();
        for (index, row) in self.rows.iter().enumerate() {
            if row.item.informational {
                continue;
            }
            let outcome = match (row.checked, row.item.installed) {
                (true, false) => Some(("installed", self.installer.install(index))),
                (false, true) => Some(("removed", self.installer.uninstall(index))),
                _ => None,
            };
            if let Some((verb, result)) = outcome {
                self.results.push(match result {
                    Ok(()) => format!("{verb} {}", row.item.label),
                    Err(e) => format!("{}: {e}", row.item.label),
                });
            }
        }
        if self.results.is_empty() {
            self.results.push("nothing to change".to_string());
        }
        self.refresh();
    }

    fn refresh(&mut self) {
        let cursor = self.cursor;
        self.rows = rows_of(self.installer.items());
        self.cursor = cursor.min(self.rows.len().saturating_sub(1));
    }
}

fn rows_of(items: Vec<ItemState>) -> Vec<Row> {
    items
        .into_iter()
        .map(|item| Row {
            checked: item.installed,
            item,
        })
        .collect()
}

pub fn render(frame: &mut Frame, app: &App) {
    let footer = (app.results.len() + app.hint.len()).min(6) as u16;
    let [head, body, notes, trust, help] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(footer),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    render_head(frame, head, app);
    render_rows(frame, body, app);
    render_notes(frame, notes, app);
    frame.render_widget(
        Paragraph::new(Line::from(CODEX_TRUST).style(Style::new().fg(Color::DarkGray))),
        trust,
    );
    frame.render_widget(
        Paragraph::new(Line::from(HELP).style(Style::new().fg(Color::DarkGray))),
        help,
    );
}

fn render_head(frame: &mut Frame, area: Rect, app: &App) {
    let (exe, home) = app.installer.title();
    let lines = vec![
        Line::from(vec![
            Span::styled("agentmail setup", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(exe, Style::new().fg(Color::DarkGray)),
        ]),
        Line::from(Span::styled(
            format!("home {home}"),
            Style::new().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_rows(frame: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .rows
        .iter()
        .map(|row| {
            let box_ = match (row.item.informational, row.checked) {
                (true, _) => "[-]",
                (false, true) => "[x]",
                (false, false) => "[ ]",
            };
            let state = match (row.item.informational, row.item.installed) {
                (true, _) => "n/a",
                (false, true) => "installed",
                (false, false) => "missing",
            };
            let style = match (row.item.informational, row.item.installed) {
                (true, _) => Style::new().fg(Color::DarkGray),
                (false, true) => Style::new().fg(Color::Green),
                (false, false) => Style::new(),
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{box_} "), style),
                Span::raw(pad(&row.item.label, NAME_W)),
                Span::styled(
                    pad(&row.item.file, FILE_W),
                    Style::new().fg(Color::DarkGray),
                ),
                Span::styled(state.to_string(), style),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default())
        .highlight_symbol("▸ ")
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default().with_selected(Some(app.cursor));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_notes(frame: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }
    let mut lines: Vec<Line> = app
        .results
        .iter()
        .map(|r| Line::from(Span::styled(r.clone(), Style::new().fg(Color::Cyan))))
        .collect();
    lines.extend(
        app.hint
            .iter()
            .map(|h| Line::from(Span::styled(h.clone(), Style::new().fg(Color::Yellow)))),
    );
    frame.render_widget(Paragraph::new(lines), area);
}

/// Columns, with an ellipsis rather than a silent cut: a truncated path that still
/// looks like a path is worse than no path.
fn pad(text: &str, width: usize) -> String {
    let room = width.saturating_sub(1);
    let mut out: String = if text.chars().count() > room {
        text.chars()
            .take(room.saturating_sub(1))
            .chain(['…'])
            .collect()
    } else {
        text.to_string()
    };
    while out.chars().count() < width {
        out.push(' ');
    }
    out
}

/// Takes over the terminal until the user quits. Refuses politely when there is no
/// terminal to take over, so the caller can print the plain list instead.
pub fn run(installer: Box<dyn Installer>) -> anyhow::Result<()> {
    if !std::io::stdout().is_terminal() {
        anyhow::bail!("stdout is not a terminal");
    }
    let mut app = App::new(installer);
    let mut terminal = ratatui::try_init()?;
    let outcome = event_loop(&mut terminal, &mut app);
    if let Err(e) = ratatui::try_restore() {
        eprintln!("could not restore the terminal: {e}");
    }
    outcome
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> anyhow::Result<()> {
    while !app.should_exit() {
        terminal.draw(|frame| render(frame, app))?;
        if let Event::Key(key) = event::read()? {
            app.on_key(key);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use crossterm::event::KeyModifiers;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;

    /// Records what the screen asked for, and pretends the files changed.
    #[derive(Default)]
    struct Fake {
        items: RefCell<Vec<ItemState>>,
        calls: RefCell<Vec<String>>,
    }

    impl Fake {
        fn new() -> Rc<Fake> {
            let item = |label: &str, file: &str, installed: bool, informational: bool| ItemState {
                label: label.into(),
                file: file.into(),
                installed,
                informational,
            };
            Rc::new(Fake {
                items: RefCell::new(vec![
                    item("Claude MCP server", "~/.claude.json", true, false),
                    item("Claude Stop hook", "~/.claude/settings.json", false, false),
                    item(
                        "Claude SessionStart hook",
                        "~/.claude/settings.json",
                        false,
                        false,
                    ),
                    item(
                        "Claude channel flag (launch hint)",
                        "~/.claude/settings.json",
                        false,
                        true,
                    ),
                    item("Codex MCP server", "~/.codex/config.toml", false, false),
                    item("Codex Stop hook", "~/.codex/hooks.json", false, false),
                    item(
                        "Codex SessionStart hook",
                        "~/.codex/hooks.json",
                        false,
                        false,
                    ),
                ]),
                calls: RefCell::new(Vec::new()),
            })
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl Installer for Rc<Fake> {
        fn title(&self) -> (String, String) {
            ("/opt/agentmail".into(), "/Users/x".into())
        }

        fn items(&self) -> Vec<ItemState> {
            self.items.borrow().clone()
        }

        fn install(&self, index: usize) -> anyhow::Result<()> {
            self.calls.borrow_mut().push(format!("install {index}"));
            self.items.borrow_mut()[index].installed = true;
            Ok(())
        }

        fn uninstall(&self, index: usize) -> anyhow::Result<()> {
            self.calls.borrow_mut().push(format!("uninstall {index}"));
            self.items.borrow_mut()[index].installed = false;
            Ok(())
        }

        fn hint(&self) -> Vec<String> {
            vec![
                "claude --dangerously-load-development-channels server:agentmail".into(),
                "alias claude-mail='claude --dangerously-load-development-channels server:agentmail'"
                    .into(),
            ]
        }
    }

    fn app(fake: &Rc<Fake>) -> App {
        App::new(Box::new(Rc::clone(fake)))
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn draw(app: &App, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        terminal.draw(|frame| render(frame, app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| {
                        buffer
                            .cell((x, y))
                            .map(|c| c.symbol().to_string())
                            .unwrap_or_default()
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn the_screen_shows_every_row_with_its_state() {
        let fake = Fake::new();
        let lines = draw(&app(&fake), 84, 12);

        assert_eq!(lines[0], "agentmail setup  /opt/agentmail");
        assert_eq!(lines[1], "home /Users/x");
        assert_eq!(
            lines[2],
            "▸ [x] Claude MCP server                 ~/.claude.json            installed"
        );
        assert_eq!(
            lines[3],
            "  [ ] Claude Stop hook                  ~/.claude/settings.json   missing"
        );
        assert_eq!(
            lines[5],
            "  [-] Claude channel flag (launch hint) ~/.claude/settings.json   n/a"
        );
        assert_eq!(
            lines[10],
            "Codex asks you to trust its hooks once after each change to hooks.json."
        );
        assert_eq!(
            lines[11],
            "↑↓ move · space toggle · a all · n none · enter apply · ? hint · q quit"
        );
    }

    #[test]
    fn space_toggles_only_the_row_under_the_cursor() {
        let fake = Fake::new();
        let mut app = app(&fake);
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Char(' ')));

        assert!(app.rows[0].checked, "row 0 was installed and is untouched");
        assert!(app.rows[1].checked, "row 1 is now wanted");
        assert!(!app.rows[2].checked);
        assert!(
            fake.calls().is_empty(),
            "a checkbox writes nothing by itself"
        );
    }

    #[test]
    fn all_and_none_skip_the_informational_row() {
        let fake = Fake::new();
        let mut app = app(&fake);

        app.on_key(key(KeyCode::Char('a')));
        assert!(app
            .rows
            .iter()
            .filter(|r| !r.item.informational)
            .all(|r| r.checked));
        assert!(!app.rows[3].checked, "the hint row is not installable");

        app.on_key(key(KeyCode::Char('n')));
        assert!(app.rows.iter().all(|r| !r.checked));
    }

    #[test]
    fn enter_applies_only_the_difference() {
        let fake = Fake::new();
        let mut app = app(&fake);

        // Uncheck the one installed row, check one missing row, leave the rest alone.
        app.on_key(key(KeyCode::Char(' ')));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Char(' ')));
        app.on_key(key(KeyCode::Enter));

        assert_eq!(fake.calls(), ["uninstall 0", "install 1"]);
        assert_eq!(
            app.results,
            ["removed Claude MCP server", "installed Claude Stop hook"]
        );
        // The boxes are re-read from the installer, not assumed.
        assert!(!app.rows[0].checked && !app.rows[0].item.installed);
        assert!(app.rows[1].checked && app.rows[1].item.installed);

        // A second apply has nothing left to do.
        app.on_key(key(KeyCode::Enter));
        assert_eq!(fake.calls().len(), 2);
        assert_eq!(app.results, ["nothing to change"]);
    }

    #[test]
    fn the_hint_row_shows_the_launch_line_instead_of_installing() {
        let fake = Fake::new();
        let mut app = app(&fake);
        for _ in 0..3 {
            app.on_key(key(KeyCode::Down));
        }
        app.on_key(key(KeyCode::Enter));

        assert!(fake.calls().is_empty());
        let lines = draw(&app, 84, 14);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("--dangerously-load-development-channels")),
            "{lines:#?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("alias claude-mail=")),
            "{lines:#?}"
        );
    }

    #[test]
    fn q_leaves() {
        let fake = Fake::new();
        let mut app = app(&fake);
        assert!(!app.should_exit());
        app.on_key(key(KeyCode::Char('q')));
        assert!(app.should_exit());
    }
}
