//! Screen state and key handling. No rendering, no IO beyond `Actions`.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use hsm_core::{HarnessKind, OpenMethod, OpenTarget, Session, SplitDirection};

use crate::actions::{Actions, BrowseContext};

/// Long enough to swallow a fast typist's keystrokes, short enough that the
/// list feels attached to the keyboard.
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(120);
/// Idle poll interval: nothing changes on its own, this only keeps the loop
/// responsive to resizes.
pub const IDLE_TICK: Duration = Duration::from_millis(250);

const RESULT_LIMIT: usize = 200;
const PAGE: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Typing filters. The popup starts here.
    Search,
    /// Bare letters are actions (`s`, `d`, `t`, `c`, `i`, `m`, `p`, `q`).
    Normal,
    /// One-line agentmail message for the selected session.
    Message,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub text: String,
    pub error: bool,
}

pub struct App {
    actions: Box<dyn Actions>,
    ctx: BrowseContext,
    query: String,
    results: Vec<Session>,
    selected: usize,
    /// First visible row; the renderer owns the window height so it updates
    /// this, and key handling only moves `selected`.
    offset: usize,
    status: Status,
    mode: Mode,
    /// Mode to return to when the message line is dismissed.
    mode_before_message: Mode,
    message: String,
    filters: Vec<Option<HarnessKind>>,
    filter: usize,
    /// Query the current results were produced from, so a filter change can
    /// keep the cursor where it is while a new query resets it.
    searched: Option<String>,
    search_due: Option<Instant>,
    /// Work held back until the screen has drawn once, so a slow action can
    /// show a notice before it blocks the loop.
    pending: Option<Pending>,
    exit: bool,
}

/// Only one kind so far; the shape is what matters, not the variant count.
enum Pending {
    Message { session: Box<Session>, text: String },
}

impl App {
    pub fn new(actions: Box<dyn Actions>, ctx: BrowseContext) -> App {
        let mut app = App {
            actions,
            ctx,
            query: String::new(),
            results: Vec::new(),
            selected: 0,
            offset: 0,
            status: Status::default(),
            mode: Mode::Search,
            mode_before_message: Mode::Search,
            message: String::new(),
            filters: vec![None],
            filter: 0,
            searched: None,
            search_due: None,
            pending: None,
            exit: false,
        };
        app.run_search();
        app.filters = filters_for(&app.results);
        app
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn results(&self) -> &[Session] {
        &self.results
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn selected_session(&self) -> Option<&Session> {
        self.results.get(self.selected)
    }

    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn set_offset(&mut self, offset: usize) {
        self.offset = offset;
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn message_input(&self) -> &str {
        &self.message
    }

    pub fn can_message(&self) -> bool {
        self.actions.can_message()
    }

    pub fn should_exit(&self) -> bool {
        self.exit
    }

    /// `all` or the kind the `Tab` filter currently pins to.
    pub fn filter_label(&self) -> String {
        match self.filters.get(self.filter).and_then(Option::as_ref) {
            Some(h) => h.to_string(),
            None => "all".to_string(),
        }
    }

    /// How long the event loop may block before something needs doing.
    pub fn poll_timeout(&self, now: Instant) -> Duration {
        match self.search_due {
            Some(due) => due.saturating_duration_since(now),
            None => IDLE_TICK,
        }
    }

    pub fn tick(&mut self) {
        self.tick_at(Instant::now());
    }

    pub fn tick_at(&mut self, now: Instant) {
        if self.search_due.is_some_and(|due| now >= due) {
            self.run_search();
        }
    }

    /// Run the pending search now, ignoring the debounce. Tests use it; so does
    /// anything that must not wait, like a filter change.
    pub fn flush_search(&mut self) {
        self.run_search();
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        // Windows sends press and release for every key; acting on both would
        // double every action.
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.exit = true;
            return;
        }
        match self.mode {
            Mode::Message => self.message_key(key),
            Mode::Search | Mode::Normal => self.browse_key(key),
        }
    }

    fn browse_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => return self.move_by(-1),
            KeyCode::Down => return self.move_by(1),
            KeyCode::PageUp => return self.move_by(-(PAGE as isize)),
            KeyCode::PageDown => return self.move_by(PAGE as isize),
            KeyCode::Enter => return self.open(self.ctx.default_open),
            KeyCode::Tab => return self.cycle_filter(),
            KeyCode::Esc => {
                // Esc leaves the search box first so it never quits out from
                // under someone who only wanted to stop typing.
                if self.mode == Mode::Search {
                    self.mode = Mode::Normal;
                } else {
                    self.exit = true;
                }
                return;
            }
            KeyCode::Backspace => {
                self.mode = Mode::Search;
                self.query.pop();
                self.schedule_search();
                return;
            }
            _ => {}
        }

        let KeyCode::Char(c) = key.code else { return };
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // Alt is the escape hatch that reaches the actions without leaving the
        // search box; bare letters only act once the box is left.
        if (alt || self.mode == Mode::Normal) && self.action_key(c) {
            return;
        }
        if self.mode == Mode::Search && !alt && !ctrl {
            self.query.push(c);
            self.schedule_search();
        }
    }

    fn action_key(&mut self, c: char) -> bool {
        match c {
            's' => self.open(OpenTarget::Split(SplitDirection::Horizontal)),
            'd' => self.open(OpenTarget::Split(SplitDirection::Vertical)),
            't' => self.open(OpenTarget::Tab),
            'c' => self.open(OpenTarget::Current),
            'i' => self.insert_address(),
            'm' => self.start_message(),
            'p' => self.toggle_pin(),
            'q' => self.exit = true,
            '/' => {
                self.mode = Mode::Search;
                self.status = Status::default();
            }
            'j' => self.move_by(1),
            'k' => self.move_by(-1),
            _ => return false,
        }
        true
    }

    fn message_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = self.mode_before_message;
                self.message.clear();
            }
            KeyCode::Backspace => {
                self.message.pop();
            }
            KeyCode::Enter => self.send_message(),
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.message.push(c);
            }
            _ => {}
        }
    }

    fn open(&mut self, target: OpenTarget) {
        let Some(session) = self.selected_session().cloned() else {
            return self.warn("nothing selected");
        };
        if target == OpenTarget::Current {
            if self.ctx.invoking_pane.is_none() {
                return self.warn("no invoking pane to open into; use s, d or t");
            }
            if self.ctx.invoking_pane_has_agent {
                return self.warn("that pane already runs an agent; use s, d or t");
            }
        }
        match self.actions.open(&session, target) {
            Ok(report) => {
                let how = match report.method {
                    OpenMethod::AgentStart => "agent.start",
                    OpenMethod::SendInput => "typed",
                };
                self.info(format!(
                    "{} in pane {} ({how})",
                    session.address().short(),
                    report.pane_id
                ));
                self.exit = true;
            }
            Err(e) => self.warn(e),
        }
    }

    fn insert_address(&mut self) {
        let Some(session) = self.selected_session().cloned() else {
            return self.warn("nothing selected");
        };
        if self.ctx.invoking_pane.is_none() {
            return self.warn("no invoking pane to insert into");
        }
        match self.actions.insert_address(&session) {
            Ok(()) => {
                self.info(format!("inserted {}", session.address().short()));
                self.exit = true;
            }
            Err(e) => self.warn(e),
        }
    }

    fn start_message(&mut self) {
        if !self.actions.can_message() {
            return self.warn("no agentmail binary; run the \"Set up agent chat\" action first");
        }
        if self.selected_session().is_none() {
            return self.warn("nothing selected");
        }
        self.mode_before_message = self.mode;
        self.mode = Mode::Message;
        self.message.clear();
    }

    /// Queues the send instead of running it: `agentmail` is a subprocess and
    /// blocks this thread, so the "sending" notice has to reach the screen
    /// first. [`App::run_pending`] does the work on the far side of a frame.
    fn send_message(&mut self) {
        let Some(session) = self.selected_session().cloned() else {
            return self.warn("nothing selected");
        };
        let text = self.message.trim().to_string();
        if text.is_empty() {
            return self.warn("message is empty");
        }
        self.info(format!("sending to {}…", session.address().short()));
        self.pending = Some(Pending::Message {
            session: Box::new(session),
            text,
        });
        self.mode = self.mode_before_message;
        self.message.clear();
    }

    /// Runs whatever [`App::send_message`] queued. The caller draws a frame
    /// first; returns true when something ran, so it can draw the result.
    pub fn run_pending(&mut self) -> bool {
        let Some(Pending::Message { session, text }) = self.pending.take() else {
            return false;
        };
        match self.actions.message(&session, &text) {
            Ok(line) => self.info(line),
            Err(e) => self.warn(e),
        }
        true
    }

    fn toggle_pin(&mut self) {
        let Some(session) = self.selected_session().cloned() else {
            return self.warn("nothing selected");
        };
        let want = !session.pinned;
        if let Err(e) = self.actions.pin(&session.harness, &session.id, want) {
            return self.warn(e);
        }
        // Re-read so the row shows whatever the index really stored.
        match self.actions.get(&session.harness, &session.id) {
            Ok(Some(fresh)) => self.results[self.selected] = fresh,
            _ => self.results[self.selected].pinned = want,
        }
        self.info(if want { "pinned" } else { "unpinned" });
    }

    fn cycle_filter(&mut self) {
        if self.filters.len() > 1 {
            self.filter = (self.filter + 1) % self.filters.len();
        }
        self.run_search();
    }

    fn move_by(&mut self, delta: isize) {
        if self.results.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.results.len() - 1;
        let next = self.selected as isize + delta;
        self.selected = next.clamp(0, last as isize) as usize;
    }

    fn schedule_search(&mut self) {
        self.search_due = Some(Instant::now() + SEARCH_DEBOUNCE);
    }

    fn run_search(&mut self) {
        self.search_due = None;
        let harness = self.filters.get(self.filter).cloned().flatten();
        // `recent` only for the plain case. With a filter, `search` is the
        // better route even for an empty query: it scans a wider candidate set,
        // so a rare harness still fills the list.
        let found = if self.query.trim().is_empty() && harness.is_none() {
            self.actions.recent(RESULT_LIMIT)
        } else {
            self.actions
                .search(&self.query, harness.as_ref(), RESULT_LIMIT)
        };
        let mut rows = match found {
            Ok(rows) => rows,
            Err(e) => {
                self.warn(e);
                return;
            }
        };
        if let Some(h) = &harness {
            rows.retain(|s| &s.harness == h);
        }

        // A new query means a new best match, so the cursor goes to the top.
        // A same-query re-run (filter cycle, pin refresh) keeps it where it is.
        let keep = (self.searched.as_deref() == Some(self.query.as_str()))
            .then(|| {
                self.selected_session()
                    .map(|s| (s.harness.clone(), s.id.clone()))
            })
            .flatten();
        self.selected = keep
            .and_then(|(h, id)| rows.iter().position(|s| s.harness == h && s.id == id))
            .unwrap_or(0);
        self.searched = Some(self.query.clone());
        self.results = rows;
    }

    fn info(&mut self, text: impl std::fmt::Display) {
        self.status = Status {
            text: text.to_string(),
            error: false,
        };
    }

    fn warn(&mut self, text: impl std::fmt::Display) {
        self.status = Status {
            text: text.to_string(),
            error: true,
        };
    }
}

/// all → claude → codex → every other kind the index actually holds. Claude and
/// codex stay in the cycle even when empty; they are the two we always index.
fn filters_for(sessions: &[Session]) -> Vec<Option<HarnessKind>> {
    let mut out = vec![None, Some(HarnessKind::Claude), Some(HarnessKind::Codex)];
    let mut rest: Vec<HarnessKind> = sessions
        .iter()
        .map(|s| s.harness.clone())
        .filter(|h| !matches!(h, HarnessKind::Claude | HarnessKind::Codex))
        .collect();
    rest.sort();
    rest.dedup();
    out.extend(rest.into_iter().map(Some));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{fake_app, key, log, session, Fake};

    #[test]
    fn starts_in_search_mode_with_recent_sessions() {
        let app = fake_app(Fake::with_sessions());
        assert_eq!(app.mode(), Mode::Search);
        assert_eq!(app.results().len(), 3);
        assert_eq!(
            app.results()[0].title.as_deref(),
            Some("API authentication")
        );
    }

    #[test]
    fn typing_is_debounced_then_filters() {
        let mut app = fake_app(Fake::with_sessions());
        app.on_key(key('a'));
        app.on_key(key('u'));
        assert_eq!(app.query(), "au");
        // Nothing ran yet: the debounce has not elapsed.
        assert_eq!(app.results().len(), 3);
        app.tick_at(Instant::now());
        assert_eq!(app.results().len(), 3);

        app.tick_at(Instant::now() + SEARCH_DEBOUNCE);
        assert_eq!(app.results().len(), 1);
        assert_eq!(
            app.results()[0].title.as_deref(),
            Some("API authentication")
        );
    }

    #[test]
    fn esc_leaves_the_box_before_it_quits() {
        let mut app = fake_app(Fake::with_sessions());
        app.on_key(key_code(KeyCode::Esc));
        assert_eq!(app.mode(), Mode::Normal);
        assert!(!app.should_exit());
        app.on_key(key_code(KeyCode::Esc));
        assert!(app.should_exit());
    }

    #[test]
    fn letters_type_in_search_mode_and_act_in_normal_mode() {
        let mut app = fake_app(Fake::with_sessions());
        app.on_key(key('s'));
        assert_eq!(app.query(), "s");
        assert!(log().is_empty());

        app.on_key(key_code(KeyCode::Esc));
        app.on_key(key('s'));
        assert_eq!(log(), vec!["open(claude:8890a685, split-horizontal)"]);
        assert!(app.should_exit());
    }

    #[test]
    fn alt_reaches_actions_without_leaving_the_box() {
        let mut app = fake_app(Fake::with_sessions());
        app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::ALT));
        assert_eq!(app.query(), "");
        assert_eq!(log(), vec!["open(claude:8890a685, tab)"]);
    }

    #[test]
    fn current_pane_is_refused_while_that_pane_runs_an_agent() {
        let mut app = App::new(
            Box::new(Fake::with_sessions()),
            BrowseContext {
                invoking_pane: Some("w1:p1".into()),
                invoking_pane_has_agent: true,
                default_open: OpenTarget::default(),
            },
        );
        app.on_key(key_code(KeyCode::Esc));
        app.on_key(key('c'));
        assert!(app.status().error);
        assert!(app.status().text.contains("already runs an agent"));
        assert!(!app.should_exit());
    }

    #[test]
    fn current_pane_without_an_invoking_pane_explains_itself() {
        let mut app = fake_app(Fake::with_sessions());
        app.on_key(key_code(KeyCode::Esc));
        app.on_key(key('c'));
        assert!(app.status().text.contains("no invoking pane"));
    }

    #[test]
    fn insert_exits_after_typing_the_address() {
        let mut app = App::new(
            Box::new(Fake::with_sessions()),
            BrowseContext {
                invoking_pane: Some("w1:p1".into()),
                invoking_pane_has_agent: true,
                default_open: OpenTarget::default(),
            },
        );
        app.on_key(key_code(KeyCode::Esc));
        app.on_key(key('i'));
        assert_eq!(log(), vec!["insert(claude:8890a685)"]);
        assert!(app.should_exit());
    }

    #[test]
    fn pin_toggles_and_stays_open() {
        let mut app = fake_app(Fake::with_sessions());
        app.on_key(key_code(KeyCode::Esc));
        app.on_key(key('p'));
        assert_eq!(log(), vec!["pin(8890a685, true)"]);
        assert!(app.results()[0].pinned);
        assert!(!app.should_exit());
        app.on_key(key('p'));
        assert!(!app.results()[0].pinned);
    }

    #[test]
    fn message_needs_an_agentmail_binary() {
        let mut app = fake_app(Fake::with_sessions());
        app.on_key(key_code(KeyCode::Esc));
        app.on_key(key('m'));
        assert_eq!(app.mode(), Mode::Normal);
        assert!(app.status().text.contains("no agentmail binary"));
    }

    #[test]
    fn message_prompts_then_sends() {
        let mut fake = Fake::with_sessions();
        fake.agentmail = true;
        let mut app = fake_app(fake);
        app.on_key(key_code(KeyCode::Esc));
        app.on_key(key('m'));
        assert_eq!(app.mode(), Mode::Message);
        for c in "hi".chars() {
            app.on_key(key(c));
        }
        assert_eq!(app.message_input(), "hi");

        // Enter only queues it, so the notice gets a frame of its own.
        app.on_key(key_code(KeyCode::Enter));
        assert!(log().is_empty());
        assert_eq!(app.mode(), Mode::Normal);
        assert_eq!(app.status().text, "sending to claude:8890a685…");
        assert!(!app.status().error);

        assert!(app.run_pending());
        assert_eq!(log(), vec!["message(claude:8890a685, hi)"]);
        assert_eq!(app.status().text, "queued for claude:8890a685");
        assert!(!app.status().error);
        assert!(!app.should_exit());
        // Nothing left to run.
        assert!(!app.run_pending());
    }

    #[test]
    fn tab_cycles_all_claude_codex_then_back() {
        let mut app = fake_app(Fake::with_sessions());
        assert_eq!(app.filter_label(), "all");
        app.on_key(key_code(KeyCode::Tab));
        assert_eq!(app.filter_label(), "claude");
        assert_eq!(app.results().len(), 2);
        app.on_key(key_code(KeyCode::Tab));
        assert_eq!(app.filter_label(), "codex");
        assert_eq!(app.results().len(), 1);
        app.on_key(key_code(KeyCode::Tab));
        assert_eq!(app.filter_label(), "all");
    }

    #[test]
    fn a_new_query_selects_the_best_match_but_a_filter_change_does_not_move() {
        let mut app = fake_app(Fake::with_sessions());
        app.on_key(key_code(KeyCode::Down));
        assert_eq!(app.selected(), 1);

        // Typing moves the cursor back to the top of the new result set.
        for c in "prompt".chars() {
            app.on_key(key(c));
        }
        app.flush_search();
        assert_eq!(app.selected(), 0);
        assert_eq!(app.results().len(), 3);

        // Cycling the harness filter leaves the same session selected.
        app.on_key(key_code(KeyCode::Down));
        let before = app.selected_session().map(|s| s.id.clone());
        app.on_key(key_code(KeyCode::Tab));
        assert_eq!(app.filter_label(), "claude");
        assert_eq!(app.results().len(), 2);
        assert_eq!(app.selected_session().map(|s| s.id.clone()), before);
    }

    #[test]
    fn arrows_clamp_at_both_ends() {
        let mut app = fake_app(Fake::with_sessions());
        app.on_key(key_code(KeyCode::Up));
        assert_eq!(app.selected(), 0);
        for _ in 0..10 {
            app.on_key(key_code(KeyCode::Down));
        }
        assert_eq!(app.selected(), 2);
    }

    #[test]
    fn an_open_failure_is_shown_and_the_popup_stays() {
        let mut fake = Fake::with_sessions();
        fake.open_fails = true;
        let mut app = fake_app(fake);
        app.on_key(key_code(KeyCode::Enter));
        assert!(app.status().error);
        assert!(app.status().text.contains("herdr is not reachable"));
        assert!(!app.should_exit());
    }

    #[test]
    fn filters_always_offer_claude_and_codex() {
        let mut other = session("pi", "aaaa1111", "demo", "pi thing");
        other.harness = HarnessKind::Pi;
        let f = filters_for(&[other]);
        assert_eq!(
            f,
            vec![
                None,
                Some(HarnessKind::Claude),
                Some(HarnessKind::Codex),
                Some(HarnessKind::Pi)
            ]
        );
    }

    fn key_code(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
}
