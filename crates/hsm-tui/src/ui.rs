//! Drawing. Pure function of `App`, apart from the list scroll offset which
//! only the renderer knows (it depends on the window height).

use chrono::{DateTime, Local, Utc};
use hsm_core::{Session, Tier};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, Mode};

const HARNESS_W: usize = 7;
const PROJECT_W: usize = 12;
const AGE_W: usize = 5;
/// glyph + pin + harness + project + age, each with a trailing space.
const FIXED_W: usize = 2 + 2 + HARNESS_W + 1 + PROJECT_W + 1 + AGE_W + 1;

pub fn render(frame: &mut Frame, app: &mut App) {
    let [top, body, status, hints] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(body);

    render_search(frame, top, app);
    render_list(frame, left, app);
    render_preview(frame, right, app);
    render_status(frame, status, app);
    render_hints(frame, hints, app);
}

fn render_search(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::bordered()
        .title_top(Line::from(" search ").style(Style::new().fg(Color::Cyan)))
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
            Style::new().fg(Color::DarkGray),
        ),
    ])
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

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            s.address().to_string(),
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )),
        field("harness", s.harness.as_str()),
        field("project", &s.project),
        field("cwd", s.cwd.to_string_lossy().into_owned()),
        field("started", stamp(s.started_at)),
        field("active", stamp(s.last_active_at)),
    ];
    if let Some(pane) = &s.last_pane {
        let live = if pane.live { "live" } else { "last seen" };
        let status = pane.status.clone().unwrap_or_default();
        lines.push(field(
            "pane",
            format!("{} {live} {status}", pane.pane_id).trim_end(),
        ));
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

fn field(label: &str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(fit(label, 11), Style::new().fg(Color::DarkGray)),
        Span::raw(value.into()),
    ])
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

    let status = app.status();
    let style = if status.error {
        Style::new().fg(Color::Red)
    } else {
        Style::new().fg(Color::Green)
    };
    frame.render_widget(Paragraph::new(status.text.clone()).style(style), area);
}

fn render_hints(frame: &mut Frame, area: Rect, app: &App) {
    let hints = match app.mode() {
        Mode::Message => "enter send · esc cancel".to_string(),
        Mode::Search => {
            "type to filter · ↑↓ move · enter open · tab harness · esc key mode".to_string()
        }
        Mode::Normal => {
            let mail = if app.can_message() {
                " · m message"
            } else {
                ""
            };
            format!(
                "enter open · s split · d down · t tab · c current · i insert{mail} · p pin · / search · q quit"
            )
        }
    };
    frame.render_widget(
        Paragraph::new(hints).style(Style::new().fg(Color::DarkGray)),
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
    use crate::testing::{fake_app, Fake};
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
        let frame = draw(&mut app, 100, 20).join("\n");
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
        assert!(frame.contains("s split"), "{frame}");
        assert!(frame.contains("q quit"));
    }

    #[test]
    fn an_error_is_visible_in_the_status_line() {
        let mut fake = Fake::with_sessions();
        fake.open_fails = true;
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
    fn age_reads_in_the_largest_unit_that_fits() {
        let now = Utc::now();
        assert_eq!(age(Some(now), now), "now");
        assert_eq!(age(Some(now - chrono::Duration::minutes(5)), now), "5m");
        assert_eq!(age(Some(now - chrono::Duration::hours(3)), now), "3h");
        assert_eq!(age(Some(now - chrono::Duration::days(40)), now), "40d");
        assert_eq!(age(None, now), "-");
    }
}
