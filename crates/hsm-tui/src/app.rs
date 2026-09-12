//! Screen state and key handling. No rendering, no IO beyond `Actions`.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use hsm_core::{HarnessKind, OpenMethod, OpenTarget, Session, SplitDirection};

use crate::actions::{Actions, BrowseContext, Panel, ReplyRow, Sent};

/// Long enough to swallow a fast typist's keystrokes, short enough that the
/// list feels attached to the keyboard.
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(120);
/// Idle poll interval: nothing changes on its own, this only keeps the loop
/// responsive to resizes.
pub const IDLE_TICK: Duration = Duration::from_millis(250);

const RESULT_LIMIT: usize = 200;
const PAGE: usize = 10;

/// Gap between two looks at the mailbox. Each one is a subprocess, so this is
/// as often as is polite.
pub const ASK_POLL: Duration = Duration::from_secs(2);
/// A question is a prompt, not a document.
pub const COMPOSE_MAX: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Typing filters. The popup starts here.
    Search,
    /// Bare letters are actions (`s`, `d`, `t`, `c`, `i`, `m`, `p`, `q`).
    Normal,
    /// One-line agentmail message for the selected session.
    Message,
    /// Writing the question for the selected session (ask mode).
    Compose,
    /// The question is out; watching for the answer.
    Waiting,
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
    compose: String,
    wait: Option<Wait>,
    reply: Option<Reply>,
    /// The human's mailbox, loaded once when the popup opens.
    replies: Vec<ReplyRow>,
    reply_cursor: usize,
    /// Reading one reply rather than the list.
    reply_detail: bool,
    panel: Panel,
    exit: bool,
}

/// A question in flight.
struct Wait {
    sent: Sent,
    deadline: Instant,
    next_poll: Instant,
}

/// An answer, kept against the session it answers so moving the cursor cannot
/// show it under the wrong one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// Address the question went to.
    pub address: String,
    pub row: ReplyRow,
}

/// Anything that shells out: queued so its notice reaches the screen first.
enum Pending {
    Message { session: Box<Session>, text: String },
    Ask { session: Box<Session>, text: String },
}

impl App {
    pub fn new(actions: Box<dyn Actions>, ctx: BrowseContext) -> App {
        let panel = ctx.start_panel;
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
            compose: String::new(),
            wait: None,
            reply: None,
            replies: Vec::new(),
            reply_cursor: 0,
            reply_detail: false,
            panel,
            exit: false,
        };
        app.run_search();
        app.filters = filters_for(&app.results);
        // Mail the human has not seen is worth knowing about before anything
        // else; a failure here must not cost them the popup.
        match app.actions.replies() {
            Ok(rows) => app.replies = rows,
            Err(error) => tracing::debug!(%error, "cannot read the reply mailbox"),
        }
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

    pub fn panel(&self) -> Panel {
        self.panel
    }

    /// Mail the human has not been shown yet.
    pub fn unseen(&self) -> usize {
        self.replies.iter().filter(|r| !r.seen).count()
    }

    /// Who the question goes out as, for the compose box to state plainly.
    pub fn sender_note(&self) -> Option<&str> {
        self.ctx.sender_note.as_deref()
    }

    /// The question being written, or the one we are waiting on an answer to.
    pub fn compose_input(&self) -> &str {
        &self.compose
    }

    /// Address the pending question went to, while one is in flight.
    pub fn waiting_on(&self) -> Option<&str> {
        self.wait.as_ref().map(|w| w.sent.address.as_str())
    }

    pub fn reply(&self) -> Option<&Reply> {
        self.reply.as_ref()
    }

    pub fn replies(&self) -> &[ReplyRow] {
        &self.replies
    }

    pub fn reply_cursor(&self) -> usize {
        self.reply_cursor
    }

    pub fn selected_reply(&self) -> Option<&ReplyRow> {
        self.replies.get(self.reply_cursor)
    }

    pub fn reply_detail(&self) -> bool {
        self.reply_detail
    }

    /// Seconds left on the current wait, for the countdown.
    pub fn wait_remaining(&self, now: Instant) -> Option<Duration> {
        self.wait
            .as_ref()
            .map(|w| w.deadline.saturating_duration_since(now))
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
        let due = [
            self.search_due,
            self.wait.as_ref().map(|w| w.next_poll.min(w.deadline)),
        ]
        .into_iter()
        .flatten()
        .min();
        match due {
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
        self.poll_wait(now);
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
            Mode::Compose => self.compose_key(key),
            // The list is frozen while an answer is outstanding; the only way
            // out is to stop waiting for it.
            Mode::Waiting => {
                if key.code == KeyCode::Esc {
                    self.stop_waiting("stopped waiting · it will show under replies (r) next time");
                }
            }
            Mode::Search | Mode::Normal => self.browse_key(key),
        }
    }

    fn browse_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => return self.move_by(-1),
            KeyCode::Down => return self.move_by(1),
            KeyCode::PageUp => return self.move_by(-(PAGE as isize)),
            KeyCode::PageDown => return self.move_by(PAGE as isize),
            KeyCode::Enter => return self.activate(),
            KeyCode::Tab => return self.cycle_filter(),
            KeyCode::Esc => {
                // A reply reader closes back to its list, the search box closes
                // to key mode, and only then does Esc quit.
                if self.reply_detail {
                    self.reply_detail = false;
                } else if self.mode == Mode::Search {
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
            // Panels first: these three are the same keys everywhere.
            's' => self.show(Panel::Search),
            'a' => self.answer_or_ask(),
            'r' => self.show(Panel::Replies),
            // `s` is the search panel now, so a split to the right is `v`.
            'v' => self.open(OpenTarget::Split(SplitDirection::Horizontal)),
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
        if self.panel != Panel::Search {
            return self.warn("opening lives on the search panel (s)");
        }
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
        self.run_pending_at(Instant::now())
    }

    pub fn run_pending_at(&mut self, now: Instant) -> bool {
        match self.pending.take() {
            Some(Pending::Message { session, text }) => match self.actions.message(&session, &text)
            {
                Ok(line) => self.info(line),
                Err(e) => self.warn(e),
            },
            Some(Pending::Ask { session, text }) => match self.actions.ask(&session, &text) {
                Ok(sent) => self.question_sent(sent, now),
                Err(e) => {
                    // Stay in the box so the text is not lost on a failure.
                    self.warn(e);
                    return true;
                }
            },
            None => return false,
        }
        true
    }

    fn start_compose(&mut self) {
        self.panel = Panel::Ask;
        if self.selected_session().is_none() {
            return self.warn("nothing selected");
        }
        // The previous answer stays in the preview: a follow-up is usually
        // about what came back.
        self.compose.clear();
        self.mode = Mode::Compose;
    }

    fn compose_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.compose.clear();
            }
            KeyCode::Backspace => {
                self.compose.pop();
            }
            KeyCode::Enter => self.send_question(),
            // A question is a prompt, not a document, so it stops at the cap.
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && self.compose.chars().count() < COMPOSE_MAX =>
            {
                self.compose.push(c);
            }
            _ => {}
        }
    }

    /// Queued like a message: `agentmail` is a subprocess, so the notice has to
    /// reach the screen before the loop blocks on it.
    fn send_question(&mut self) {
        let Some(session) = self.selected_session().cloned() else {
            return self.warn("nothing selected");
        };
        let text = self.compose.trim().to_string();
        if text.is_empty() {
            return self.warn("nothing to ask");
        }
        self.info(format!("asking {}…", session.address().short()));
        self.pending = Some(Pending::Ask {
            session: Box::new(session),
            text,
        });
    }

    fn question_sent(&mut self, sent: Sent, now: Instant) {
        let short = sent
            .address
            .split_once(':')
            .map(|(h, id)| format!("{h}:{}", id.chars().take(8).collect::<String>()))
            .unwrap_or_else(|| sent.address.clone());
        if !sent.awaiting_reply {
            self.mode = Mode::Normal;
            self.compose.clear();
            return self.info(format!(
                "sent {} → {short} · reply will arrive in your session",
                sent.message_id
            ));
        }
        self.info(format!("sent {} → {short}", sent.message_id));
        self.mode = Mode::Waiting;
        self.wait = Some(Wait {
            sent,
            deadline: now + self.ctx.ask_wait,
            next_poll: now + ASK_POLL,
        });
    }

    /// One look for the answer, at most every `ASK_POLL`, until `ASK_TIMEOUT`.
    fn poll_wait(&mut self, now: Instant) {
        let Some(wait) = self.wait.as_ref() else {
            return;
        };
        if now < wait.next_poll {
            return;
        }
        let sent = wait.sent.clone();
        let deadline = wait.deadline;
        if let Some(wait) = self.wait.as_mut() {
            wait.next_poll = now + ASK_POLL;
        }

        match self.actions.poll_reply(&sent) {
            Ok(Some(row)) => {
                // It has been put in front of the human, so it is not new mail
                // any more.
                let _ = self.actions.mark_seen(std::slice::from_ref(&row.id));
                self.replies.retain(|r| r.id != row.id);
                let mut seen_row = row.clone();
                seen_row.seen = true;
                self.replies.insert(0, seen_row);
                self.reply = Some(Reply {
                    address: sent.address.clone(),
                    row,
                });
                self.stop_waiting(format!("reply from {} · enter to ask again", sent.address));
            }
            Ok(None) => {
                if now >= deadline {
                    self.stop_waiting("no reply yet · it will show under replies (r) next time");
                }
            }
            Err(e) => {
                let text = e.to_string();
                self.stop_waiting("");
                self.warn(text);
            }
        }
    }

    /// Ends the wait without closing the popup: the question is delivered
    /// either way, and the answer will keep until the replies panel.
    fn stop_waiting(&mut self, note: impl std::fmt::Display) {
        self.wait = None;
        self.mode = Mode::Normal;
        self.compose.clear();
        let note = note.to_string();
        if !note.is_empty() {
            self.info(note);
        }
    }

    /// Panel switching keeps the query, the selection and the harness filter:
    /// they are one app's state, not three screens'.
    fn show(&mut self, panel: Panel) {
        if self.panel == panel {
            return;
        }
        self.panel = panel;
        self.reply_detail = false;
        self.status = Status::default();
    }

    /// `a` on the replies panel answers whoever wrote the selected reply; on
    /// the other panels it is just the panel key.
    fn answer_or_ask(&mut self) {
        if self.panel == Panel::Replies {
            if let Some(from) = self.selected_reply().map(|r| r.from.clone()) {
                self.select_address(&from);
            }
        }
        self.show(Panel::Ask);
    }

    /// Puts the session at `address` under the cursor, fetching it into the
    /// list when the current query does not hold it.
    fn select_address(&mut self, address: &str) {
        if let Some(at) = self
            .results
            .iter()
            .position(|s| s.address().to_string() == address)
        {
            self.selected = at;
            return;
        }
        let Some((harness, id)) = address.split_once(':') else {
            return;
        };
        let harness = HarnessKind::from_name(harness);
        match self.actions.get(&harness, id) {
            Ok(Some(session)) => {
                // Not a search hit, but it is what the user is answering, so it
                // goes to the top where they are looking.
                self.results.insert(0, session);
                self.selected = 0;
            }
            _ => self.warn(format!("{address} is not in the index")),
        }
    }

    /// Enter: open, ask, or read the selected reply, by panel.
    fn activate(&mut self) {
        match self.panel {
            Panel::Search => self.open(self.ctx.default_open),
            Panel::Ask => self.start_compose(),
            Panel::Replies => self.read_reply(),
        }
    }

    /// Reading a reply is what marks it seen; the count is about what the human
    /// has actually been shown.
    fn read_reply(&mut self) {
        let Some(row) = self.replies.get_mut(self.reply_cursor) else {
            return self.warn("no replies yet");
        };
        self.reply_detail = true;
        if !row.seen {
            row.seen = true;
            let id = row.id.clone();
            if let Err(error) = self.actions.mark_seen(&[id]) {
                tracing::debug!(%error, "cannot record which replies were shown");
            }
        }
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
        // On the replies panel the cursor belongs to the mailbox; a reader has
        // nothing to move through at all.
        let (cursor, len) = match self.panel {
            Panel::Replies if self.reply_detail => return,
            Panel::Replies => (&mut self.reply_cursor, self.replies.len()),
            _ => (&mut self.selected, self.results.len()),
        };
        if len == 0 {
            *cursor = 0;
            return;
        }
        let next = *cursor as isize + delta;
        *cursor = next.clamp(0, len as isize - 1) as usize;
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
    use crate::testing::{ask_app, fake_app, key, log, session, Fake};

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
        app.on_key(key('v'));
        assert_eq!(app.query(), "v");
        assert!(log().is_empty());

        app.on_key(key_code(KeyCode::Esc));
        app.on_key(key('v'));
        assert_eq!(log(), vec!["open(claude:8890a685, split-horizontal)"]);
        assert!(app.should_exit());
    }

    #[test]
    fn v_splits_right_and_s_only_switches_panel() {
        let mut app = fake_app(Fake::with_sessions());
        app.on_key(key_code(KeyCode::Esc));

        // `s` is the search panel key: it must never open anything.
        app.on_key(key('s'));
        assert_eq!(app.panel(), Panel::Search);
        assert!(log().is_empty(), "s did not open a pane");
        assert!(!app.should_exit());

        app.on_key(key('v'));
        assert_eq!(log(), vec!["open(claude:8890a685, split-horizontal)"]);
    }

    #[test]
    fn panels_switch_without_losing_the_query_the_filter_or_the_cursor() {
        let mut app = fake_app(Fake::with_sessions());
        for c in "prompt".chars() {
            app.on_key(key(c));
        }
        app.flush_search();
        app.on_key(key_code(KeyCode::Tab));
        app.on_key(key_code(KeyCode::Down));
        let query = app.query().to_string();
        let filter = app.filter_label();
        let picked = app.selected_session().map(|s| s.id.clone());
        assert_eq!(filter, "claude");
        assert_eq!(app.selected(), 1);

        app.on_key(key_code(KeyCode::Esc));
        for panel in ['a', 'r', 's'] {
            app.on_key(key(panel));
        }
        assert_eq!(app.panel(), Panel::Search);
        assert_eq!(app.query(), query);
        assert_eq!(app.filter_label(), filter);
        assert_eq!(app.selected(), 1);
        assert_eq!(app.selected_session().map(|s| s.id.clone()), picked);
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
                ..BrowseContext::default()
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
                ..BrowseContext::default()
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

    fn asking() -> Fake {
        let mut fake = Fake::with_sessions();
        fake.agentmail = true;
        fake.awaiting = true;
        fake
    }

    #[test]
    fn ask_mode_turns_the_open_keys_off() {
        let mut app = ask_app(asking());
        app.on_key(key_code(KeyCode::Esc));
        for k in ['v', 'd', 't', 'c'] {
            app.on_key(key(k));
            assert!(app.status().error, "{k} should be refused");
            assert!(app.status().text.contains("search panel"));
        }
        assert!(log().is_empty(), "nothing was opened");
        assert!(!app.should_exit());
    }

    #[test]
    fn enter_composes_a_question_and_sends_it() {
        let mut app = ask_app(asking());
        app.on_key(key_code(KeyCode::Enter));
        assert_eq!(app.mode(), Mode::Compose);

        for c in "does the token refresh?".chars() {
            app.on_key(key(c));
        }
        assert_eq!(app.compose_input(), "does the token refresh?");

        // Enter only queues it, so "asking…" gets a frame of its own.
        app.on_key(key_code(KeyCode::Enter));
        assert!(log().is_empty());
        assert!(app.status().text.starts_with("asking claude:8890a685"));

        assert!(app.run_pending());
        assert_eq!(log(), vec!["ask(claude:8890a685, does the token refresh?)"]);
        assert_eq!(
            app.status().text,
            "sent 01JASKTESTID0000000000000 → claude:8890a685"
        );
        assert_eq!(app.mode(), Mode::Waiting, "the peer can answer now");
        assert!(!app.should_exit(), "the popup stays for the reply");
    }

    #[test]
    fn esc_cancels_the_compose_box() {
        let mut app = ask_app(asking());
        app.on_key(key_code(KeyCode::Enter));
        for c in "wait no".chars() {
            app.on_key(key(c));
        }
        app.on_key(key_code(KeyCode::Esc));
        assert_eq!(app.mode(), Mode::Normal);
        assert_eq!(app.compose_input(), "");
        assert!(log().is_empty());
        assert!(!app.should_exit());
    }

    #[test]
    fn a_reply_arrives_on_the_third_poll() {
        let mut fake = asking();
        fake.reply_on_poll = Some(3);
        let mut app = ask_app(fake);
        let start = Instant::now();

        app.on_key(key_code(KeyCode::Enter));
        app.on_key(key('?'));
        app.on_key(key_code(KeyCode::Enter));
        assert!(app.run_pending_at(start));
        assert_eq!(app.mode(), Mode::Waiting);
        assert_eq!(
            app.waiting_on(),
            Some("claude:8890a685-a0f1-4a9e-949d-f7f386bc4cb6")
        );

        // Nothing is asked of the inbox before the first interval is up.
        app.tick_at(start + Duration::from_millis(500));
        assert_eq!(app.reply(), None);

        for n in 1..=2 {
            app.tick_at(start + ASK_POLL * n);
            assert_eq!(app.reply(), None, "poll {n} is still empty");
            assert_eq!(app.mode(), Mode::Waiting);
        }

        app.tick_at(start + ASK_POLL * 3);
        let reply = app.reply().expect("the third poll answers");
        assert_eq!(reply.row.text, "yes, the token refreshes hourly");
        assert_eq!(reply.address, "claude:8890a685-a0f1-4a9e-949d-f7f386bc4cb6");
        assert_eq!(app.mode(), Mode::Normal);
        assert!(app.status().text.starts_with("reply from claude:8890a685"));
        assert_eq!(log().iter().filter(|l| l.starts_with("poll(")).count(), 3);
    }

    #[test]
    fn the_wait_gives_up_after_the_timeout() {
        let mut app = ask_app(asking());
        let start = Instant::now();
        app.on_key(key_code(KeyCode::Enter));
        app.on_key(key('?'));
        app.on_key(key_code(KeyCode::Enter));
        app.run_pending_at(start);

        app.tick_at(start + BrowseContext::default().ask_wait);
        assert_eq!(app.mode(), Mode::Normal);
        assert_eq!(app.reply(), None);
        assert!(app.status().text.contains("under replies (r) next time"));
    }

    #[test]
    fn esc_stops_waiting_early() {
        let mut app = ask_app(asking());
        app.on_key(key_code(KeyCode::Enter));
        app.on_key(key('?'));
        app.on_key(key_code(KeyCode::Enter));
        app.run_pending();
        assert_eq!(app.mode(), Mode::Waiting);

        app.on_key(key_code(KeyCode::Esc));
        assert_eq!(app.mode(), Mode::Normal);
        assert!(app.status().text.starts_with("stopped waiting"));
    }

    #[test]
    fn a_peer_that_cannot_answer_now_says_where_the_reply_lands() {
        let mut fake = asking();
        fake.awaiting = false;
        let mut app = ask_app(fake);
        app.on_key(key_code(KeyCode::Enter));
        app.on_key(key('?'));
        app.on_key(key_code(KeyCode::Enter));
        app.run_pending();

        assert_eq!(app.mode(), Mode::Normal, "nothing to wait for");
        assert!(app
            .status()
            .text
            .contains("reply will arrive in your session"));
    }

    #[test]
    fn a_question_stops_at_the_length_cap() {
        let mut app = ask_app(asking());
        app.on_key(key_code(KeyCode::Enter));
        for _ in 0..COMPOSE_MAX + 50 {
            app.on_key(key('x'));
        }
        assert_eq!(app.compose_input().chars().count(), COMPOSE_MAX);
    }

    fn reply_row(id: &str, from: &str, text: &str) -> ReplyRow {
        ReplyRow {
            id: id.into(),
            from: from.into(),
            title: Some("API authentication".into()),
            when: chrono::Utc::now().checked_sub_signed(chrono::Duration::minutes(3)),
            text: text.into(),
            reply_to: None,
            seen: false,
        }
    }

    #[test]
    fn an_answer_after_a_minute_is_still_caught() {
        let mut fake = asking();
        // 2 s per poll, so the answer lands on the poll just after 60 s.
        fake.reply_on_poll = Some(31);
        let mut app = ask_app(fake);
        let start = Instant::now();

        app.on_key(key_code(KeyCode::Enter));
        app.on_key(key('?'));
        app.on_key(key_code(KeyCode::Enter));
        app.run_pending_at(start);

        // Drive the loop the way the event loop does, one tick per interval.
        let mut at = start;
        for _ in 1..=30 {
            at += ASK_POLL;
            app.tick_at(at);
        }
        assert_eq!(at, start + Duration::from_secs(60));
        assert_eq!(app.mode(), Mode::Waiting, "still waiting a minute in");
        assert_eq!(
            app.wait_remaining(at).map(|d| d.as_secs()),
            Some(60),
            "two minutes of patience by default"
        );

        at += ASK_POLL;
        app.tick_at(at);
        let reply = app.reply().expect("the answer came back after a minute");
        assert_eq!(reply.row.text, "yes, the token refreshes hourly");
        assert_eq!(app.mode(), Mode::Normal);
        assert!(app.status().text.contains("enter to ask again"));
        // Shown once, so it is not new mail any more.
        assert!(log().contains(&"mark_seen(01JREPLY0000000000000000A)".to_string()));
    }

    #[test]
    fn the_countdown_runs_down_and_esc_stops_it() {
        let mut app = ask_app(asking());
        let start = Instant::now();
        app.on_key(key_code(KeyCode::Enter));
        app.on_key(key('?'));
        app.on_key(key_code(KeyCode::Enter));
        app.run_pending_at(start);

        assert_eq!(
            app.wait_remaining(start).map(|d| d.as_secs()),
            Some(120),
            "the config budget"
        );
        assert_eq!(
            app.wait_remaining(start + Duration::from_secs(33))
                .map(|d| d.as_secs()),
            Some(87)
        );

        app.on_key(key_code(KeyCode::Esc));
        assert_eq!(app.mode(), Mode::Normal);
        assert_eq!(app.wait_remaining(start), None);
        assert!(app.status().text.contains("under replies (r) next time"));
        // The popup stays: the question was delivered either way.
        assert!(!app.should_exit());
    }

    #[test]
    fn a_follow_up_question_keeps_the_answer_on_screen() {
        let mut fake = asking();
        fake.reply_on_poll = Some(1);
        let mut app = ask_app(fake);
        let start = Instant::now();
        app.on_key(key_code(KeyCode::Enter));
        app.on_key(key('?'));
        app.on_key(key_code(KeyCode::Enter));
        app.run_pending_at(start);
        app.tick_at(start + ASK_POLL);
        assert!(app.reply().is_some());

        app.on_key(key_code(KeyCode::Enter));
        assert_eq!(app.mode(), Mode::Compose, "enter reopens the box");
        assert!(
            app.reply().is_some(),
            "the answer is still there to reply to"
        );
    }

    #[test]
    fn the_replies_panel_lists_the_mailbox_newest_first() {
        let mut fake = asking();
        fake.waiting_replies = vec![
            reply_row("01A", "claude:8890a685-a0f1", "yes, hourly"),
            reply_row("01B", "codex:01a08ad8-1a08", "it is in config.toml"),
        ];
        let mut app = ask_app(fake);
        assert_eq!(app.replies().len(), 2);
        assert_eq!(app.unseen(), 2);

        app.on_key(key_code(KeyCode::Esc));
        app.on_key(key('r'));
        assert_eq!(app.panel(), Panel::Replies);
        assert_eq!(
            app.selected_reply().map(|r| r.id.clone()),
            Some("01A".into())
        );

        // The cursor belongs to the mailbox here, not to the session list.
        app.on_key(key_code(KeyCode::Down));
        assert_eq!(app.reply_cursor(), 1);
        assert_eq!(app.selected(), 0, "the session list did not move");
    }

    #[test]
    fn reading_a_reply_opens_it_and_marks_it_seen() {
        let mut fake = asking();
        fake.waiting_replies = vec![reply_row("01A", "claude:8890a685-a0f1", "yes, hourly")];
        let mut app = ask_app(fake);
        app.on_key(key_code(KeyCode::Esc));
        app.on_key(key('r'));
        assert_eq!(app.unseen(), 1);

        app.on_key(key_code(KeyCode::Enter));
        assert!(app.reply_detail());
        assert_eq!(log(), vec!["mark_seen(01A)"]);
        assert_eq!(app.unseen(), 0, "the marker is gone");
        assert!(app.selected_reply().is_some_and(|r| r.seen));

        // Esc reads out, not out of the popup.
        app.on_key(key_code(KeyCode::Esc));
        assert!(!app.reply_detail());
        assert_eq!(app.panel(), Panel::Replies);
        assert!(!app.should_exit());
    }

    #[test]
    fn a_from_a_reply_answers_its_sender() {
        let mut fake = asking();
        fake.waiting_replies = vec![reply_row(
            "01A",
            "claude:43901a13-7735-465b-9e08-86e55b01c4c5",
            "yes, hourly",
        )];
        let mut app = ask_app(fake);
        app.on_key(key_code(KeyCode::Esc));
        app.on_key(key('r'));
        app.on_key(key('a'));

        assert_eq!(app.panel(), Panel::Ask);
        assert_eq!(
            app.selected_session().map(|s| s.id.clone()),
            Some("43901a13-7735-465b-9e08-86e55b01c4c5".to_string()),
            "the sender is under the cursor, ready to answer"
        );
    }

    #[test]
    fn an_empty_mailbox_says_so() {
        let mut app = ask_app(asking());
        assert!(app.replies().is_empty());
        app.on_key(key_code(KeyCode::Esc));
        app.on_key(key('r'));
        assert_eq!(app.panel(), Panel::Replies);
        app.on_key(key_code(KeyCode::Enter));
        assert!(!app.reply_detail());
        assert_eq!(app.status().text, "no replies yet");
    }

    fn key_code(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
}
