//! Drawing. Pure function of `App`, apart from the list scroll offset which
//! only the renderer knows (it depends on the window height).

use chrono::{DateTime, Local, Utc};
use hsm_core::{KeyBinding, Keys, ProcessKind, Session, Tier};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::actions::{Panel, ReplyRow};
use crate::app::{App, Mode, COMPOSE_MAX};

const HARNESS_W: usize = 7;
const PROJECT_W: usize = 12;
const AGE_W: usize = 5;
/// Wide enough for the word itself; blank on rows that are not running.
const LIVE_W: usize = 4;
/// glyph + pin + live + harness + project + age, each with a trailing space.
const FIXED_W: usize = 2 + 2 + LIVE_W + 1 + HARNESS_W + 1 + PROJECT_W + 1 + AGE_W + 1;
/// The row glyphs, spelled out under the panel row.
const LEGEND: &str =
    "@ live idle · > live working · ! live blocked · ~ no pane · + hot · - warm · x gone · * pinned";

pub fn render(frame: &mut Frame, app: &mut App) {
    if app.panel() == Panel::Replies {
        let [body, status, hints] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .areas(frame.area());
        render_replies(frame, body, app);
        render_status(frame, status, app);
        render_hints(frame, hints, app);
        return;
    }

    // The compose box only exists while there is a question on the go, and it
    // sits under the list rather than over it so the pick stays visible.
    let composing = matches!(app.mode(), Mode::Compose | Mode::Waiting);
    let [top, body, compose, status, hints] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(if composing { 3 } else { 0 }),
        Constraint::Length(1),
        Constraint::Length(hints_height(app)),
    ])
    .areas(frame.area());
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(body);

    render_search(frame, top, app);
    render_list(frame, left, app);
    render_preview(frame, right, app);
    if composing {
        render_compose(frame, compose, app);
    }
    render_status(frame, status, app);
    render_hints(frame, hints, app);
}

fn render_search(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::bordered()
        .title_top(Line::from(panel_title(app)).style(Style::new().fg(Color::Cyan)))
        .title_top(Line::from(format!(" harness: {} ", app.filter_label())).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(app.query()), inner);

    if app.mode() == Mode::Search {
        let x =
            inner.x + display_width(app.query()).min(inner.width.saturating_sub(1) as usize) as u16;
        frame.set_cursor_position((x, inner.y));
    }
}

fn render_list(frame: &mut Frame, area: Rect, app: &mut App) {
    let block =
        Block::bordered().title_top(Line::from(format!(" sessions ({}) ", app.results().len())));
    if app.results().is_empty() {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new("no sessions").style(Style::new().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    let title_w = (block.inner(area).width as usize)
        .saturating_sub(FIXED_W)
        .max(8);
    let now = Utc::now();
    let items: Vec<ListItem> = app
        .results()
        .iter()
        .map(|s| ListItem::new(row(s, title_w, now)))
        .collect();

    let list = List::new(items).block(block).highlight_style(
        Style::new()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    let mut state = ListState::default()
        .with_offset(app.offset())
        .with_selected(Some(app.selected()));
    frame.render_stateful_widget(list, area, &mut state);
    app.set_offset(state.offset());
}

fn row(s: &Session, title_w: usize, now: DateTime<Utc>) -> Line<'static> {
    let (glyph, color) = state_glyph(s);
    Line::from(vec![
        Span::styled(glyph.to_string(), Style::new().fg(color)),
        Span::raw(" "),
        Span::styled(
            if s.pinned { "*" } else { " " }.to_string(),
            Style::new().fg(Color::Yellow),
        ),
        Span::raw(" "),
        // The glyph alone is a puzzle the first few times; the word is not.
        Span::styled(fit(state_tag(s), LIVE_W), Style::new().fg(color)),
        Span::raw(" "),
        Span::styled(
            fit(s.harness.as_str(), HARNESS_W),
            Style::new().fg(Color::Magenta),
        ),
        Span::raw(" "),
        Span::styled(fit(&s.project, PROJECT_W), Style::new().fg(Color::Blue)),
        Span::raw(" "),
        Span::raw(fit(&headline(s), title_w)),
        Span::raw(" "),
        Span::styled(
            pad_left(&age(s.last_active_at, now), AGE_W),
            // Not DarkGray: that is also the highlight background, and the age
            // of the selected row would disappear into it.
            Style::new().fg(Color::Gray),
        ),
    ])
}

/// The word next to the glyph: where the session is running, if it is.
fn state_tag(s: &Session) -> &'static str {
    if s.is_live() {
        return "live";
    }
    match s.process.as_ref().map(|p| p.kind) {
        Some(ProcessKind::Job) => "job",
        Some(ProcessKind::Interactive) => "run",
        None => "",
    }
}

/// Live state first (that is what the user is looking for), then how reachable
/// the transcript still is.
pub fn state_glyph(s: &Session) -> (&'static str, Color) {
    if let Some(pane) = &s.last_pane {
        if pane.live {
            return match pane.status.as_deref() {
                Some("working") => (">", Color::Yellow),
                Some("blocked") => ("!", Color::Red),
                _ => ("@", Color::Green),
            };
        }
    }
    // Running, but in a process of its own: there is no pane to jump to.
    if let Some(process) = &s.process {
        return match process.status.as_deref() {
            Some("busy") => ("~", Color::Yellow),
            _ => ("~", Color::Green),
        };
    }
    match s.tier {
        Tier::Hot => ("+", Color::Cyan),
        Tier::Warm => ("-", Color::Gray),
        Tier::Gone => ("x", Color::DarkGray),
    }
}

fn headline(s: &Session) -> String {
    s.title
        .clone()
        .or_else(|| s.first_prompt.clone())
        .unwrap_or_else(|| s.short_id())
}

fn render_preview(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::bordered().title_top(Line::from(" preview "));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(s) = app.selected_session() else {
        return;
    };

    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        s.address().to_string(),
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    ))];

    // Unread mail is the first thing worth saying, whatever row is selected.
    if app.unseen() > 0 {
        lines.push(Line::from(Span::styled(
            format!("replies ({}) · press r", app.unseen()),
            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        )));
    }

    // An answer belongs to the session that sent it, so it only shows while
    // that session is the one selected.
    if let Some(reply) = app.reply().filter(|r| r.address == s.address().to_string()) {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "reply",
            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        )));
        for line in reply.row.text.lines() {
            lines.push(Line::from(line.to_string()));
        }
        lines.push(Line::default());
    }

    lines.extend([
        field("harness", s.harness.as_str()),
        field("project", &s.project),
        field("cwd", s.cwd.to_string_lossy().into_owned()),
        field("started", stamp(s.started_at)),
        field("active", stamp(s.last_active_at)),
    ]);
    if let Some(pane) = &s.last_pane {
        let live = if pane.live { "live" } else { "last seen" };
        let status = pane.status.clone().unwrap_or_default();
        lines.push(field(
            "pane",
            format!("{} {live} {status}", pane.pane_id).trim_end(),
        ));
    }
    if let Some(process) = &s.process {
        let status = process.status.clone().unwrap_or_default();
        let head = format!("{} {} {status}", process.pid, process.kind);
        // Where Enter will land: the pane of whoever is watching the job.
        let watched = match &process.pane_id {
            Some(pane) => format!(" in pane {pane}"),
            None => String::new(),
        };
        lines.push(field("process", format!("{}{watched}", head.trim_end())));
    }
    lines.push(field(
        "transcript",
        s.transcript_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "-".into()),
    ));
    lines.push(field("pinned", if s.pinned { "yes" } else { "no" }));

    if s.tier == Tier::Gone {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            format!(
                "not resumable, opens a fresh agent in {}",
                s.cwd.to_string_lossy()
            ),
            Style::new().fg(Color::Yellow),
        )));
    }

    if let Some(prompt) = &s.first_prompt {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "first prompt",
            Style::new().fg(Color::DarkGray),
        )));
        for line in prompt.lines() {
            lines.push(Line::from(line.to_string()));
        }
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// The whole popup while the replies panel is up: the list, or one reply.
fn render_replies(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.reply_detail() {
        return render_reply_detail(frame, area, app);
    }
    let unseen = app.unseen();
    let block = Block::bordered().title_top(
        Line::from(format!(" replies ({}, {unseen} new) ", app.replies().len()))
            .style(Style::new().fg(Color::Cyan)),
    );
    if app.replies().is_empty() {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new("no replies yet · questions you send from the ask panel land here")
                .style(Style::new().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    let width = block.inner(area).width as usize;
    let now = Utc::now();
    let items: Vec<ListItem> = app
        .replies()
        .iter()
        .map(|row| ListItem::new(reply_row(row, width, now)))
        .collect();
    let list = List::new(items).block(block).highlight_style(
        Style::new()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    let mut state = ListState::default().with_selected(Some(app.reply_cursor()));
    frame.render_stateful_widget(list, area, &mut state);
}

/// `• 3m  claude:8890a685  API authentication  yes, the token refreshes hourly`
fn reply_row(row: &ReplyRow, width: usize, now: DateTime<Utc>) -> Line<'static> {
    const FROM_W: usize = 16;
    const TITLE_W: usize = 22;
    let fixed = 2 + AGE_W + 1 + FROM_W + 1 + TITLE_W + 1;
    let text_w = width.saturating_sub(fixed).max(10);
    Line::from(vec![
        Span::styled(
            if row.seen { "  " } else { "• " }.to_string(),
            Style::new().fg(Color::Green),
        ),
        Span::styled(
            pad_left(&age(row.when, now), AGE_W),
            Style::new().fg(Color::DarkGray),
        ),
        Span::raw(" "),
        Span::styled(
            fit(&short_address(&row.from), FROM_W),
            Style::new().fg(Color::Cyan),
        ),
        Span::raw(" "),
        Span::styled(
            fit(row.title.as_deref().unwrap_or("-"), TITLE_W),
            Style::new().fg(Color::Blue),
        ),
        Span::raw(" "),
        Span::raw(fit(row.first_line(), text_w)),
    ])
}

fn render_reply_detail(frame: &mut Frame, area: Rect, app: &App) {
    let block =
        Block::bordered().title_top(Line::from(" reply ").style(Style::new().fg(Color::Cyan)));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(row) = app.selected_reply() else {
        return;
    };
    let mut lines = vec![
        field("from", row.from.clone()),
        field("session", row.title.clone().unwrap_or_else(|| "-".into())),
        field("age", age(row.when, Utc::now())),
        field("id", row.id.clone()),
    ];
    if let Some(to) = &row.reply_to {
        // agentmail lists inbound mail only, so the question is an id here, not
        // its text.
        lines.push(field("in reply to", to.clone()));
    }
    lines.push(Line::default());
    for line in row.text.lines() {
        lines.push(Line::from(line.to_string()));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// `<harness>:<id8>`, which is all of an address worth a column.
fn short_address(address: &str) -> String {
    match address.split_once(':') {
        Some((h, id)) => format!("{h}:{}", id.chars().take(8).collect::<String>()),
        None => address.to_string(),
    }
}

fn field(label: &str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        // 12 wide so the longest label ("in reply to") keeps its gap.
        Span::styled(fit(label, 12), Style::new().fg(Color::DarkGray)),
        Span::raw(value.into()),
    ])
}

fn render_compose(frame: &mut Frame, area: Rect, app: &App) {
    let to = app
        .waiting_on()
        .map(str::to_string)
        .or_else(|| app.selected_session().map(|s| s.address().to_string()))
        .unwrap_or_default();
    let waiting = app.mode() == Mode::Waiting;
    let left = if waiting {
        " waiting for a reply ".to_string()
    } else {
        format!(" ask {to} ")
    };
    let block = Block::bordered()
        .title_top(Line::from(left).style(Style::new().fg(if waiting {
            Color::Yellow
        } else {
            Color::Green
        })))
        .title_top(
            Line::from(format!(
                " {}/{COMPOSE_MAX} ",
                app.compose_input().chars().count()
            ))
            .right_aligned(),
        );
    let block = match app.sender_note() {
        Some(note) => block
            .title_bottom(Line::from(format!(" {note} ")).style(Style::new().fg(Color::DarkGray))),
        None => block,
    };
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(app.compose_input()), inner);

    if !waiting {
        let x = inner.x
            + display_width(app.compose_input()).min(inner.width.saturating_sub(1) as usize) as u16;
        frame.set_cursor_position((x, inner.y));
    }
}

/// What the top-left corner says the popup is showing.
fn panel_title(app: &App) -> String {
    match app.panel() {
        Panel::Search => " search ".to_string(),
        Panel::Ask => " ask ".to_string(),
        Panel::Replies => format!(" replies ({} new) ", app.unseen()),
    }
}

fn render_status(frame: &mut Frame, area: Rect, app: &App) {
    if app.mode() == Mode::Message {
        let to = app
            .selected_session()
            .map(|s| s.address().short())
            .unwrap_or_default();
        let prefix = format!("message {to}: ");
        let text = format!("{prefix}{}", app.message_input());
        frame.render_widget(
            Paragraph::new(text).style(Style::new().fg(Color::Yellow)),
            area,
        );
        let x = area.x
            + display_width(&format!("{prefix}{}", app.message_input()))
                .min(area.width.saturating_sub(1) as usize) as u16;
        frame.set_cursor_position((x, area.y));
        return;
    }

    if app.mode() == Mode::Waiting {
        let left = app
            .wait_remaining(std::time::Instant::now())
            .map(|d| d.as_secs())
            .unwrap_or_default();
        frame.render_widget(
            Paragraph::new(format!("waiting for reply · {left}s · esc to stop waiting"))
                .style(Style::new().fg(Color::Yellow)),
            area,
        );
        return;
    }

    let status = app.status();
    let style = if status.error {
        Style::new().fg(Color::Red)
    } else {
        Style::new().fg(Color::Green)
    };
    frame.render_widget(Paragraph::new(status.text.clone()).style(style), area);
}

/// Two rows, three on the search panel: what this panel's keys do, how to reach
/// the other panels, and what the row glyphs mean. One row cannot hold them
/// without truncating on a normal popup width.
fn hints_height(app: &App) -> u16 {
    if legend_shown(app) {
        3
    } else {
        2
    }
}

fn legend_shown(app: &App) -> bool {
    app.panel() == Panel::Search && matches!(app.mode(), Mode::Search | Mode::Normal)
}

/// What the footer can honestly promise about jumping. A bare letter types
/// while the search box is open, so it is only offered in key mode, and `Enter`
/// falls back to opening whenever the jump key is something else.
fn jump_hint(keys: &Keys, mode: Mode) -> String {
    match keys.jump {
        KeyBinding::Enter => "enter jump/open".to_string(),
        KeyBinding::Char(_) if mode == Mode::Search => "enter open".to_string(),
        _ => format!("enter open · {} jump", keys.jump),
    }
}

/// Both keys for the same split, unless the configured one is `v` itself.
fn split_hint(keys: &Keys) -> String {
    if keys.open_split == KeyBinding::Char('v') {
        "v split".to_string()
    } else {
        format!("{}/v split", keys.open_split)
    }
}

fn render_hints(frame: &mut Frame, area: Rect, app: &App) {
    let keys = app.keys();
    let actions = match app.mode() {
        Mode::Message => "enter send · esc cancel".to_string(),
        Mode::Compose => "enter ask · esc cancel".to_string(),
        Mode::Waiting => "esc stop waiting".to_string(),
        Mode::Search => {
            let enter = match app.panel() {
                Panel::Search => jump_hint(keys, app.mode()),
                Panel::Ask => "enter ask".to_string(),
                Panel::Replies => "enter read".to_string(),
            };
            format!("type to filter · ↑↓ move · {enter} · tab harness · esc key mode")
        }
        Mode::Normal => match app.panel() {
            Panel::Search => format!(
                "{} · {} · d down · t tab · c current · i insert{} · p pin",
                jump_hint(keys, app.mode()),
                split_hint(keys),
                if app.can_message() {
                    " · m message"
                } else {
                    ""
                }
            ),
            Panel::Ask => "enter ask · i insert · p pin · esc quit".to_string(),
            Panel::Replies if app.reply_detail() => "esc back · a answer".to_string(),
            Panel::Replies => "↑↓ move · enter read · a answer".to_string(),
        },
    };
    let panels = format!("s search · a ask · r replies ({}) · q quit", app.unseen());

    let mut lines = vec![Line::from(actions)];
    // While a box is open the letters type, so the panel row would be a lie.
    if matches!(app.mode(), Mode::Search | Mode::Normal) {
        lines.push(Line::from(panels));
    }
    if legend_shown(app) {
        lines.push(Line::from(LEGEND));
    }
    frame.render_widget(
        Paragraph::new(lines).style(Style::new().fg(Color::DarkGray)),
        area,
    );
}

fn stamp(t: Option<DateTime<Utc>>) -> String {
    match t {
        Some(t) => t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string(),
        None => "-".to_string(),
    }
}

fn age(t: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(t) = t else {
        return "-".to_string();
    };
    let secs = (now - t).num_seconds().max(0);
    match secs {
        s if s < 60 => "now".to_string(),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s if s < 31_536_000 => format!("{}d", s / 86_400),
        s => format!("{}y", s / 31_536_000),
    }
}

/// Truncate or pad to exactly `width` characters. Char counting, not display
/// width: close enough for the latin text these fields hold, and it never
/// panics on a multi-byte boundary.
fn fit(s: &str, width: usize) -> String {
    let mut out: String = s.chars().take(width).collect();
    let len = out.chars().count();
    if len < width {
        out.push_str(&" ".repeat(width - len));
    }
    out
}

fn pad_left(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.chars().take(width).collect()
    } else {
        format!("{}{s}", " ".repeat(width - len))
    }
}

fn display_width(s: &str) -> usize {
    s.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ask_app, fake_app, keys_app, session, Fake};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn draw(app: &mut App, w: u16, h: u16) -> Vec<String> {
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
    fn draws_search_list_preview_and_hints() {
        let mut app = fake_app(Fake::with_sessions());
        let frame = draw(&mut app, 100, 20).join("\n");
        println!("{frame}");

        assert!(frame.contains("search"));
        assert!(frame.contains("sessions (3)"));
        assert!(frame.contains("API authentication"));
        assert!(frame.contains("trade-help"));
        // The live pane shows up both as a row glyph and in the preview.
        assert!(frame.contains("w6:p1 live idle"));
        assert!(frame.contains("claude:8890a685"));
        assert!(frame.contains("type to filter"));
    }

    #[test]
    fn gone_sessions_say_they_start_fresh() {
        let mut app = fake_app(Fake::with_sessions());
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Down,
        ));
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Down,
        ));
        let frame = draw(&mut app, 100, 24).join("\n");
        assert!(
            frame.contains("not resumable, opens a fresh agent in"),
            "{frame}"
        );
    }

    #[test]
    fn normal_mode_lists_the_action_keys() {
        let mut app = fake_app(Fake::with_sessions());
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Esc,
        ));
        let frame = draw(&mut app, 100, 20).join("\n");
        assert!(
            frame.contains("enter jump/open · o/v split · d down"),
            "{frame}"
        );
        assert!(frame.contains("s search · a ask · r replies"), "{frame}");
        assert!(frame.contains("q quit"));
        assert!(frame.contains("@ live idle · > live working"), "{frame}");
        assert!(frame.contains("~ no pane"), "{frame}");
        assert!(frame.contains("x gone · * pinned"), "{frame}");
    }

    #[test]
    fn a_row_running_without_a_pane_names_its_process_in_the_preview() {
        let mut app = fake_app(Fake::with_sessions());
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Down,
        ));
        let frame = draw(&mut app, 100, 24).join("\n");
        assert!(frame.contains("~   job "), "{frame}");
        assert!(frame.contains("57845 job busy in pane w9:p7"), "{frame}");
    }

    #[test]
    fn the_hints_offer_the_jump_key_only_where_it_works() {
        let jump_bound_to = |jump| Keys {
            jump,
            ..Keys::default()
        };

        // Alt reaches the action from either mode, and Enter now just opens.
        let mut app = keys_app(
            Fake::with_sessions(),
            jump_bound_to(KeyBinding::AltChar('g')),
        );
        let frame = draw(&mut app, 100, 20).join("\n");
        assert!(frame.contains("enter open · alt-g jump"), "{frame}");

        // A bare letter types into the box, so it is only promised in key mode.
        let mut app = keys_app(Fake::with_sessions(), jump_bound_to(KeyBinding::Char('g')));
        let frame = draw(&mut app, 100, 20).join("\n");
        assert!(frame.contains("enter open · tab harness"), "{frame}");
        assert!(!frame.contains("g jump"), "{frame}");

        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Esc,
        ));
        let frame = draw(&mut app, 100, 20).join("\n");
        assert!(frame.contains("enter open · g jump"), "{frame}");
    }

    #[test]
    fn a_running_session_says_live_and_a_quiet_one_leaves_the_column_blank() {
        let now = Utc::now();
        let mut s = session(
            "claude",
            "8890a685-a0f1-4a9e-949d-f7f386bc4cb6",
            "trade-help",
            "API authentication",
        );
        let quiet = text_of(&row(&s, 20, now));
        s.last_pane = Some(hsm_core::PaneRef {
            pane_id: "w6:p1".into(),
            live: true,
            status: Some("idle".into()),
            ..hsm_core::PaneRef::default()
        });
        let live = text_of(&row(&s, 20, now));

        assert!(live.starts_with("@   live "), "{live:?}");
        assert!(!quiet.contains("live"), "{quiet:?}");
        // The columns after it still line up.
        assert_eq!(live.len(), quiet.len());
    }

    /// A process without a pane is running too, and says which kind it is.
    #[test]
    fn a_session_running_outside_herdr_gets_the_tilde_and_its_own_tag() {
        let now = Utc::now();
        let mut s = session(
            "claude",
            "8890a685-a0f1-4a9e-949d-f7f386bc4cb6",
            "trade-help",
            "API authentication",
        );
        let quiet = text_of(&row(&s, 20, now));

        s.process = Some(hsm_core::ProcessRef {
            pid: 57845,
            kind: hsm_core::ProcessKind::Job,
            status: Some("busy".into()),
            name: None,
            pane_id: None,
        });
        let job = text_of(&row(&s, 20, now));
        assert!(job.starts_with("~   job  "), "{job:?}");
        assert_eq!(state_glyph(&s), ("~", Color::Yellow), "busy");

        s.process = Some(hsm_core::ProcessRef {
            kind: hsm_core::ProcessKind::Interactive,
            status: Some("idle".into()),
            ..s.process.clone().expect("process")
        });
        let interactive = text_of(&row(&s, 20, now));
        assert!(interactive.starts_with("~   run  "), "{interactive:?}");
        assert_eq!(state_glyph(&s), ("~", Color::Green), "idle");

        // The columns after the tag still line up.
        assert_eq!(job.len(), quiet.len());
        assert_eq!(interactive.len(), quiet.len());
    }

    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.to_string()).collect()
    }

    #[test]
    fn an_error_is_visible_in_the_status_line() {
        let mut fake = Fake::with_sessions();
        fake.open_fails = true;
        // The first row is live, so Enter tries the pane before it opens.
        fake.jump_fails = true;
        let mut app = fake_app(fake);
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Enter,
        ));
        let frame = draw(&mut app, 100, 20).join("\n");
        assert!(frame.contains("herdr is not reachable"), "{frame}");
    }

    #[test]
    fn narrow_frames_still_render() {
        let mut app = fake_app(Fake::with_sessions());
        let lines = draw(&mut app, 40, 12);
        assert_eq!(lines.len(), 12);
    }

    #[test]
    fn ask_mode_draws_the_compose_box_under_the_list() {
        let mut fake = Fake::with_sessions();
        fake.agentmail = true;
        fake.awaiting = true;
        let mut app = ask_app(fake);

        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Enter,
        ));
        for c in "does the token refresh hourly?".chars() {
            app.on_key(crossterm::event::KeyEvent::from(
                crossterm::event::KeyCode::Char(c),
            ));
        }
        let frame = draw(&mut app, 100, 20).join("\n");
        println!("{frame}");

        assert!(frame.contains("┌ ask "), "{frame}");
        assert!(frame.contains("ask claude:8890a685-a0f1-4a9e-949d-f7f386bc4cb6"));
        assert!(frame.contains("does the token refresh hourly?"));
        assert!(frame.contains("30/500"));
        assert!(frame.contains("enter ask · esc cancel"));
        // The list is still there to pick from.
        assert!(frame.contains("API authentication"));
    }

    #[test]
    fn a_reply_shows_up_in_the_preview() {
        let mut fake = Fake::with_sessions();
        fake.agentmail = true;
        fake.awaiting = true;
        fake.reply_on_poll = Some(1);
        let mut app = ask_app(fake);
        let start = std::time::Instant::now();

        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Enter,
        ));
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Char('?'),
        ));
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Enter,
        ));
        app.run_pending_at(start);
        app.tick_at(start + crate::app::ASK_POLL);

        let frame = draw(&mut app, 100, 20).join("\n");
        assert!(frame.contains("reply"), "{frame}");
        assert!(frame.contains("yes, the token refreshes hourly"), "{frame}");
    }

    fn mailbox() -> Vec<crate::actions::ReplyRow> {
        vec![
            crate::actions::ReplyRow {
                id: "01JREPLYA".into(),
                from: "claude:8890a685-a0f1-4a9e-949d-f7f386bc4cb6".into(),
                title: Some("API authentication".into()),
                when: Utc::now().checked_sub_signed(chrono::Duration::minutes(3)),
                text: "yes, the token refreshes hourly\nit is the same window everywhere".into(),
                reply_to: Some("01JQUESTION".into()),
                seen: false,
            },
            crate::actions::ReplyRow {
                id: "01JREPLYB".into(),
                from: "codex:01a08ad8-1a08-7013-97e7-1053ecf353fe".into(),
                title: Some("Transcribe screenshot".into()),
                when: Utc::now().checked_sub_signed(chrono::Duration::hours(2)),
                text: "the worker count lives in config.toml".into(),
                reply_to: None,
                seen: true,
            },
        ]
    }

    fn replies_app(rows: Vec<crate::actions::ReplyRow>) -> App {
        let mut fake = Fake::with_sessions();
        fake.agentmail = true;
        fake.waiting_replies = rows;
        let mut app = ask_app(fake);
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Esc,
        ));
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Char('r'),
        ));
        app
    }

    #[test]
    fn the_replies_panel_is_a_full_screen_list() {
        let mut app = replies_app(mailbox());
        let frame = draw(&mut app, 100, 14).join("\n");
        println!("{frame}");

        assert!(frame.contains("replies (2, 1 new)"), "{frame}");
        assert!(frame.contains("• "), "the unseen marker");
        assert!(frame.contains("claude:8890a685"));
        assert!(frame.contains("API authentication"));
        assert!(frame.contains("yes, the token refreshes hourly"));
        assert!(frame.contains("codex:01a08ad8"));
        assert!(frame.contains("a answer"), "{frame}");
        // Full screen: no session list beside it.
        assert!(!frame.contains("sessions ("), "{frame}");
    }

    #[test]
    fn reading_a_reply_shows_the_whole_message() {
        let mut app = replies_app(mailbox());
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Enter,
        ));
        let frame = draw(&mut app, 100, 14).join("\n");
        println!("{frame}");

        assert!(frame.contains("┌ reply"), "{frame}");
        assert!(frame.contains("from        claude:8890a685-a0f1-4a9e-949d-f7f386bc4cb6"));
        assert!(frame.contains("session     API authentication"));
        assert!(frame.contains("id          01JREPLYA"));
        assert!(frame.contains("in reply to 01JQUESTION"), "{frame}");
        assert!(frame.contains("it is the same window everywhere"));
        assert!(frame.contains("esc back"));
    }

    #[test]
    fn the_unseen_marker_goes_away_once_read() {
        let mut app = replies_app(mailbox());
        assert!(draw(&mut app, 100, 14).join("\n").contains("1 new"));

        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Enter,
        ));
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Esc,
        ));
        let frame = draw(&mut app, 100, 14).join("\n");
        assert!(frame.contains("replies (2, 0 new)"), "{frame}");
        assert!(!frame.contains("• "), "no markers left: {frame}");
    }

    #[test]
    fn the_countdown_is_in_the_status_line() {
        let mut fake = Fake::with_sessions();
        fake.agentmail = true;
        fake.awaiting = true;
        let mut app = ask_app(fake);
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Enter,
        ));
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Char('?'),
        ));
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Enter,
        ));
        app.run_pending();

        let frame = draw(&mut app, 100, 20).join("\n");
        assert!(frame.contains("waiting for reply ·"), "{frame}");
        assert!(frame.contains("s · esc to stop waiting"));
        assert!(frame.contains("waiting for a reply"), "the box says so too");
    }

    #[test]
    fn age_reads_in_the_largest_unit_that_fits() {
        let now = Utc::now();
        assert_eq!(age(Some(now), now), "now");
        assert_eq!(age(Some(now - chrono::Duration::minutes(5)), now), "5m");
        assert_eq!(age(Some(now - chrono::Duration::hours(3)), now), "3h");
        assert_eq!(age(Some(now - chrono::Duration::days(40)), now), "40d");
        assert_eq!(age(None, now), "-");
    }
}
