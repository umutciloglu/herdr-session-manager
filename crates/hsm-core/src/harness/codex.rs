//! Codex threads.
//!
//! Primary source is Codex's own index, `~/.codex/state_5.sqlite`, table
//! `threads` (observed 2026-09-12):
//! `id, rollout_path, created_at, updated_at, source, cwd, title, archived,
//!  first_user_message, preview, name, is_pinned, recency_at, recency_at_ms,
//!  thread_source, git_branch, …`. `created_at`/`updated_at` are unix seconds,
//! `recency_at_ms` milliseconds. `thread_source` is `user`, `subagent` or
//! empty; `source` is `cli`, `vscode` or a JSON blob like
//! `{"subagent":{"thread_spawn":{…}}}` for spawned children.
//!
//! Fallback is the rollout itself,
//! `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`, whose first line is
//! `{"timestamp":…,"type":"session_meta","payload":{"session_id","id","cwd",
//!  "timestamp","source","thread_source",…}}`. Conversation turns appear twice:
//! as `{"type":"response_item","payload":{"type":"message","role":…,
//!  "content":[{"type":"input_text"|"output_text","text":…}]}}` and as the
//! cleaner UI item `{"type":"event_msg","payload":{"type":"item_completed",
//!  "item":{"type":"UserMessage"|"AgentMessage","content":[{"text":…}]}}}`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use crate::domain::{HarnessKind, Session};
use crate::error::Result;
use crate::harness::jsonl::{self, TAIL_WINDOW};
use crate::harness::message::{ExtractedMessage, Role};

const HEAD_LINES: usize = 120;
const FIRST_PROMPT_MAX: usize = 2000;
const TITLE_MAX: usize = 200;
const MESSAGE_MAX: usize = 20_000;

/// Read Codex's thread index. Opened `SQLITE_OPEN_READ_ONLY` (never `?mode=ro`
/// with `immutable=1`, which is unsafe while Codex holds a WAL).
pub fn from_state_db(db: &Path) -> Result<Vec<Session>> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare(
        "SELECT id, rollout_path, cwd, \
                COALESCE(name, ''), COALESCE(title, ''), \
                COALESCE(first_user_message, ''), COALESCE(preview, ''), \
                is_pinned, created_at, updated_at, recency_at_ms, \
                COALESCE(thread_source, ''), COALESCE(source, '') \
         FROM threads WHERE archived = 0",
    )?;

    let rows = stmt.query_map([], |r| {
        Ok(ThreadRow {
            id: r.get(0)?,
            rollout_path: r.get(1)?,
            cwd: r.get(2)?,
            name: r.get(3)?,
            title: r.get(4)?,
            first_user_message: r.get(5)?,
            preview: r.get(6)?,
            is_pinned: r.get::<_, i64>(7)? != 0,
            created_at: r.get(8)?,
            updated_at: r.get(9)?,
            recency_at_ms: r.get(10)?,
            thread_source: r.get(11)?,
            source: r.get(12)?,
        })
    })?;

    let mut out = Vec::new();
    for row in rows {
        let row = row?;
        if row.is_subagent() {
            continue;
        }
        out.push(row.into_session());
    }
    Ok(out)
}

struct ThreadRow {
    id: String,
    rollout_path: String,
    cwd: String,
    name: String,
    title: String,
    first_user_message: String,
    preview: String,
    is_pinned: bool,
    created_at: i64,
    updated_at: i64,
    recency_at_ms: i64,
    thread_source: String,
    source: String,
}

impl ThreadRow {
    /// Threads spawned by another thread are implementation detail, not
    /// something a user resumes.
    fn is_subagent(&self) -> bool {
        self.thread_source == "subagent" || self.source.contains("subagent")
    }

    fn into_session(self) -> Session {
        let path = crate::paths::without_verbatim_prefix(PathBuf::from(&self.rollout_path));
        let stat = jsonl::stat(&path).ok();

        let mut s = Session::new(HarnessKind::Codex, self.id, PathBuf::from(&self.cwd));
        s.title = pick(&[&self.name, &self.title]).map(|t| jsonl::truncate(t, TITLE_MAX));
        s.first_prompt = pick(&[&self.first_user_message, &self.preview])
            .map(|t| jsonl::truncate(t, FIRST_PROMPT_MAX));
        s.started_at = DateTime::from_timestamp(self.created_at, 0);
        s.last_active_at =
            DateTime::from_timestamp_millis(self.recency_at_ms.max(self.updated_at * 1000));
        s.size_bytes = stat.as_ref().map(|st| st.size).unwrap_or(0);
        s.transcript_present = stat.is_some();
        s.transcript_path = Some(path);
        s.pinned = self.is_pinned;
        s
    }
}

fn pick<'a>(candidates: &[&'a str]) -> Option<&'a str> {
    candidates.iter().map(|s| s.trim()).find(|s| !s.is_empty())
}

pub fn rollouts(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .flat_map(|r| jsonl::files_with_ext(r, "jsonl"))
        .collect()
}

/// Used when `state_5.sqlite` is absent (fresh install, or an older Codex).
pub fn parse_rollout(path: &Path) -> Result<Option<Session>> {
    let stat = jsonl::stat(path)?;
    if stat.size == 0 {
        return Ok(None);
    }

    let head = jsonl::head_lines(path, HEAD_LINES)?;
    let mut meta: Option<Value> = None;
    let mut started: Option<DateTime<Utc>> = None;
    let mut first_prompt: Option<String> = None;
    let mut fallback_prompt: Option<String> = None;

    for line in &head {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if started.is_none() {
            started = jsonl::parse_ts(v.get("timestamp").and_then(Value::as_str));
        }
        if meta.is_none() && v.get("type").and_then(Value::as_str) == Some("session_meta") {
            meta = v.get("payload").cloned();
        }
        if first_prompt.is_none() {
            if let Some(m) = item_message(&v) {
                if m.role == Role::User {
                    first_prompt = Some(jsonl::truncate(&m.text, FIRST_PROMPT_MAX));
                }
            }
        }
        if fallback_prompt.is_none() {
            if let Some(m) = response_message(&v) {
                if m.role == Role::User {
                    fallback_prompt = Some(jsonl::truncate(&m.text, FIRST_PROMPT_MAX));
                }
            }
        }
    }

    let meta = meta.unwrap_or(Value::Null);
    if meta.get("thread_source").and_then(Value::as_str) == Some("subagent") {
        return Ok(None);
    }

    let id = meta
        .get("session_id")
        .or_else(|| meta.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| id_from_filename(path))
        .unwrap_or_default();
    if id.is_empty() {
        return Ok(None);
    }
    let cwd = meta
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_default();

    let mut last = None;
    for line in jsonl::tail_lines(path, TAIL_WINDOW)? {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(ts) = jsonl::parse_ts(v.get("timestamp").and_then(Value::as_str)) {
            if last.is_none_or(|l| ts > l) {
                last = Some(ts);
            }
        }
    }

    let mut s = Session::new(HarnessKind::Codex, id, cwd);
    s.first_prompt = first_prompt.or(fallback_prompt);
    s.started_at =
        started.or_else(|| jsonl::parse_ts(meta.get("timestamp").and_then(Value::as_str)));
    s.last_active_at = last.or_else(|| DateTime::from_timestamp_millis(stat.mtime_ms));
    s.size_bytes = stat.size;
    s.transcript_path = Some(path.to_path_buf());
    s.transcript_present = true;
    Ok(Some(s))
}

/// User and agent prose appended since `from_offset`.
pub fn extract_messages(path: &Path, from_offset: u64) -> Result<(Vec<ExtractedMessage>, u64)> {
    let (lines, offset) = jsonl::lines_from(path, from_offset)?;
    let mut items = Vec::new();
    let mut responses = Vec::new();
    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let ts = jsonl::parse_ts(v.get("timestamp").and_then(Value::as_str));
        if let Some(mut m) = item_message(&v) {
            m.ts = ts;
            m.text = jsonl::truncate(&m.text, MESSAGE_MAX);
            items.push(m);
        } else if let Some(mut m) = response_message(&v) {
            m.ts = ts;
            m.text = jsonl::truncate(&m.text, MESSAGE_MAX);
            responses.push(m);
        }
    }
    // The two encodings duplicate each other; prefer the UI items, which carry
    // only what the user and the model actually exchanged.
    let out = if items.is_empty() { responses } else { items };
    Ok((out, offset))
}

/// `event_msg` / `item_completed` -> `UserMessage` | `AgentMessage`.
fn item_message(v: &Value) -> Option<ExtractedMessage> {
    let payload = v.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("item_completed") {
        return None;
    }
    let item = payload.get("item")?;
    let role = match item.get("type").and_then(Value::as_str)? {
        "UserMessage" => Role::User,
        "AgentMessage" => Role::Assistant,
        _ => return None,
    };
    let text = join_text(item.get("content")?)?;
    Some(ExtractedMessage {
        role,
        text,
        ts: None,
    })
}

/// `response_item` / `message` with a user or assistant role. `developer`
/// messages are instruction injection, not conversation.
fn response_message(v: &Value) -> Option<ExtractedMessage> {
    if v.get("type").and_then(Value::as_str) != Some("response_item") {
        return None;
    }
    let payload = v.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let role = Role::from_name(payload.get("role").and_then(Value::as_str)?)?;
    let text = join_text(payload.get("content")?)?;
    Some(ExtractedMessage {
        role,
        text,
        ts: None,
    })
}

fn join_text(content: &Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        let s = s.trim();
        return (!s.is_empty()).then(|| s.to_string());
    }
    let parts: Vec<String> = content
        .as_array()?
        .iter()
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// `rollout-2026-09-10T13-23-33-<uuid>` -> `<uuid>`.
fn id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy();
    let parts: Vec<&str> = stem.split('-').collect();
    (parts.len() >= 5).then(|| parts[parts.len() - 5..].join("-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shapes copied from a real rollout on 2026-09-12, values anonymised.
    const ROLLOUT: &str = concat!(
        r#"{"timestamp":"2026-08-13T09:04:38.664Z","ordinal":0,"type":"session_meta","payload":{"session_id":"019ffa5d-8515-71f0-bb83-0c02d2b9ceb6","id":"019ffa5d-8515-71f0-bb83-0c02d2b9ceb6","timestamp":"2026-08-13T09:04:20.784Z","cwd":"/Users/x/Projects/demo","originator":"codex-tui","cli_version":"0.147.0","source":"cli","thread_source":"user"}}"#,
        "\n",
        r#"{"timestamp":"2026-08-13T09:04:39.526Z","ordinal":2,"type":"response_item","payload":{"type":"message","id":"m0","role":"developer","content":[{"type":"input_text","text":"<skills_instructions>ignore me</skills_instructions>"}]}}"#,
        "\n",
        r##"{"timestamp":"2026-08-13T09:04:39.526Z","ordinal":3,"type":"response_item","payload":{"type":"message","id":"m1","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions\nimplement the gap recovery plan"}]}}"##,
        "\n",
        r#"{"timestamp":"2026-08-13T09:04:39.933Z","ordinal":7,"type":"event_msg","payload":{"type":"item_completed","thread_id":"019ffa5d-8515-71f0-bb83-0c02d2b9ceb6","item":{"type":"UserMessage","id":"i1","content":[{"type":"text","text":"implement the gap recovery plan","text_elements":[]}]}}}"#,
        "\n",
        r#"{"timestamp":"2026-08-13T09:04:47.308Z","ordinal":9,"type":"response_item","payload":{"type":"reasoning","id":"r1","summary":[],"encrypted_content":"AAAA"}}"#,
        "\n",
        r#"{"timestamp":"2026-08-13T09:04:48.471Z","ordinal":11,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","id":"i2","content":[{"type":"Text","text":"Reading the plan first."}]}}}"#,
        "\n",
        r#"{"timestamp":"2026-08-13T09:04:50.010Z","ordinal":12,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","id":"i3","command":["/bin/zsh","-lc","ls"]}}}"#,
        "\n",
        r#"{"timestamp":"2026-08-13T09:05:00.000Z","ordinal":20,"type":"event_msg","payload":{"type":"task_complete","turn_id":"t1"}}"#,
        "\n",
    );

    fn rollout_file() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tmp");
        let day = dir.path().join("2026/08/13");
        std::fs::create_dir_all(&day).expect("mkdir");
        let p = day.join("rollout-2026-08-13T12-04-20-019ffa5d-8515-71f0-bb83-0c02d2b9ceb6.jsonl");
        std::fs::write(&p, ROLLOUT).expect("write");
        (dir, p)
    }

    #[test]
    fn parses_a_rollout_head() {
        let (_d, p) = rollout_file();
        let s = parse_rollout(&p).expect("parse").expect("session");
        assert_eq!(s.harness, HarnessKind::Codex);
        assert_eq!(s.id, "019ffa5d-8515-71f0-bb83-0c02d2b9ceb6");
        assert_eq!(s.project, "demo");
        // The clean UI item wins over the AGENTS.md-prefixed response_item.
        assert_eq!(
            s.first_prompt.as_deref(),
            Some("implement the gap recovery plan")
        );
        assert_eq!(
            s.last_active_at.map(|t| t.to_rfc3339()),
            Some("2026-08-13T09:05:00+00:00".to_string())
        );
    }

    #[test]
    fn extracts_prose_turns_without_duplicates() {
        let (_d, p) = rollout_file();
        let (msgs, off) = extract_messages(&p, 0).expect("extract");
        let got: Vec<_> = msgs.iter().map(|m| (m.role, m.text.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (Role::User, "implement the gap recovery plan"),
                (Role::Assistant, "Reading the plan first."),
            ]
        );
        assert_eq!(off as usize, ROLLOUT.len());
    }

    #[test]
    fn id_recovered_from_the_filename() {
        let p = Path::new("rollout-2026-09-10T13-23-33-01a08ad8-1a08-7013-97e7-1053ecf353fe.jsonl");
        assert_eq!(
            id_from_filename(p).as_deref(),
            Some("01a08ad8-1a08-7013-97e7-1053ecf353fe")
        );
    }

    #[test]
    fn reads_a_thread_index() {
        let dir = tempfile::tempdir().expect("tmp");
        let db = dir.path().join("state_5.sqlite");
        let conn = Connection::open(&db).expect("open");
        conn.execute_batch(
            "CREATE TABLE threads (
                 id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL, created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL, source TEXT NOT NULL, cwd TEXT NOT NULL,
                 title TEXT NOT NULL, archived INTEGER NOT NULL DEFAULT 0,
                 first_user_message TEXT NOT NULL DEFAULT '', preview TEXT NOT NULL DEFAULT '',
                 name TEXT, is_pinned INTEGER NOT NULL DEFAULT 0,
                 recency_at_ms INTEGER NOT NULL DEFAULT 0, thread_source TEXT);
             INSERT INTO threads VALUES
               ('t-user','/tmp/r1.jsonl',1786942725,1786962427,'cli','/Users/x/Projects/demo',
                'a very long generated title',0,'how do I set up ssh','how do I set up',
                'Restricted dev SSH',1,1786962419682,'user'),
               ('t-sub','/tmp/r2.jsonl',1,2,'{\"subagent\":\"review\"}','/Users/x/Projects/demo',
                'child',0,'','',NULL,0,2000,'subagent'),
               ('t-arch','/tmp/r3.jsonl',1,2,'cli','/Users/x/Projects/demo',
                'old',1,'','',NULL,0,2000,'user');",
        )
        .expect("seed");
        drop(conn);

        let sessions = from_state_db(&db).expect("read");
        assert_eq!(
            sessions.len(),
            1,
            "archived and subagent threads are skipped"
        );
        let s = &sessions[0];
        assert_eq!(s.id, "t-user");
        // The short human name beats the long generated title.
        assert_eq!(s.title.as_deref(), Some("Restricted dev SSH"));
        assert_eq!(s.first_prompt.as_deref(), Some("how do I set up ssh"));
        assert!(s.pinned);
        assert!(!s.transcript_present, "rollout file does not exist");
        assert_eq!(
            s.last_active_at.map(|t| t.timestamp_millis()),
            Some(1_786_962_427_000)
        );
    }
}
