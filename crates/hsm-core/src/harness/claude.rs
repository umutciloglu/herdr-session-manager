//! Claude Code transcripts: `~/.claude/projects/<cwd-slug>/<session-uuid>.jsonl`.
//!
//! Observed line shapes (2026-09, cli 2.1.x):
//! - `{"type":"user","message":{"role":"user","content":"…"|[{"type":"text","text":"…"}]},
//!    "timestamp":"…Z","sessionId":"…","cwd":"/abs","isSidechain":false,"isMeta":false}`
//! - `{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking"|"text"|"tool_use",…}]}}`
//! - title carriers, written near the end of the file and without timestamps:
//!   `custom-title{customTitle}`, `ai-title{aiTitle}`, `agent-name{agentName}`,
//!   and older `summary{summary}`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::domain::{HarnessKind, Session};
use crate::error::Result;
use crate::harness::jsonl::{self, TAIL_WINDOW};
use crate::harness::message::{is_meta_text, ExtractedMessage, Role};

/// Enough to cover the preamble lines plus the first real exchange.
const HEAD_LINES: usize = 80;
const FIRST_PROMPT_MAX: usize = 2000;
const MESSAGE_MAX: usize = 20_000;

pub fn transcripts(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .flat_map(|r| jsonl::files_in_subdirs(r, "jsonl"))
        .collect()
}

/// Head + tail parse. `None` for a file with no usable content.
pub fn parse_transcript(path: &Path) -> Result<Option<Session>> {
    let stat = jsonl::stat(path)?;
    if stat.size == 0 {
        return Ok(None);
    }

    let mut id: Option<String> = None;
    let mut cwd: Option<PathBuf> = None;
    let mut started: Option<DateTime<Utc>> = None;
    let mut last: Option<DateTime<Utc>> = None;
    let mut first_prompt: Option<String> = None;
    let mut title: Option<(u8, String)> = None;

    for line in jsonl::head_lines(path, HEAD_LINES)? {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        absorb_common(&v, &mut id, &mut cwd, &mut started, &mut last, &mut title);
        if first_prompt.is_none() {
            if let Some(t) = human_prompt(&v) {
                first_prompt = Some(jsonl::truncate(&t, FIRST_PROMPT_MAX));
            }
        }
    }

    // Titles and the last timestamp live at the end of the file.
    if stat.size > 0 {
        for line in jsonl::tail_lines(path, TAIL_WINDOW)? {
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            absorb_common(&v, &mut id, &mut cwd, &mut started, &mut last, &mut title);
        }
    }

    let id = id.unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    if id.is_empty() {
        return Ok(None);
    }
    let cwd = cwd.unwrap_or_else(|| cwd_from_slug(path));

    let mut session = Session::new(HarnessKind::Claude, id, cwd);
    session.title = title.map(|(_, t)| t);
    session.first_prompt = first_prompt;
    session.started_at = started;
    session.last_active_at = last.or(Some(ms_to_utc(stat.mtime_ms)));
    session.size_bytes = stat.size;
    session.transcript_path = Some(path.to_path_buf());
    session.transcript_present = true;
    Ok(Some(session))
}

/// User and assistant prose appended since `from_offset`, for the FTS table.
pub fn extract_messages(path: &Path, from_offset: u64) -> Result<(Vec<ExtractedMessage>, u64)> {
    let (lines, offset) = jsonl::lines_from(path, from_offset)?;
    let mut out = Vec::new();
    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let ts = jsonl::parse_ts(v.get("timestamp").and_then(Value::as_str));
        let role = match v.get("type").and_then(Value::as_str) {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            _ => continue,
        };
        if role == Role::User && v.get("isMeta").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let Some(text) = message_text(&v) else {
            continue;
        };
        if text.is_empty() || (role == Role::User && is_meta_text(&text)) {
            continue;
        }
        out.push(ExtractedMessage {
            role,
            text: jsonl::truncate(&text, MESSAGE_MAX),
            ts,
        });
    }
    Ok((out, offset))
}

fn absorb_common(
    v: &Value,
    id: &mut Option<String>,
    cwd: &mut Option<PathBuf>,
    started: &mut Option<DateTime<Utc>>,
    last: &mut Option<DateTime<Utc>>,
    title: &mut Option<(u8, String)>,
) {
    if id.is_none() {
        if let Some(s) = v.get("sessionId").and_then(Value::as_str) {
            *id = Some(s.to_string());
        }
    }
    if cwd.is_none() {
        if let Some(s) = v.get("cwd").and_then(Value::as_str) {
            *cwd = Some(PathBuf::from(s));
        }
    }
    if let Some(ts) = jsonl::parse_ts(v.get("timestamp").and_then(Value::as_str)) {
        if started.is_none_or(|s| ts < s) {
            *started = Some(ts);
        }
        if last.is_none_or(|l| ts > l) {
            *last = Some(ts);
        }
    }
    if let Some((rank, text)) = title_of(v) {
        let better = title.as_ref().is_none_or(|(r, _)| rank >= *r);
        if better {
            *title = Some((rank, jsonl::truncate(&text, 200)));
        }
    }
}

/// A user-set title beats a generated one; `agent-name` is the weakest since
/// herdr also writes pane labels there.
fn title_of(v: &Value) -> Option<(u8, String)> {
    let (rank, key) = match v.get("type").and_then(Value::as_str)? {
        "custom-title" => (3, "customTitle"),
        "summary" => (2, "summary"),
        "ai-title" => (2, "aiTitle"),
        "agent-name" => (1, "agentName"),
        _ => return None,
    };
    let text = v.get(key).and_then(Value::as_str)?.trim();
    if text.is_empty() {
        None
    } else {
        Some((rank, text.to_string()))
    }
}

/// The first thing a human actually typed, skipping harness-injected prompts.
fn human_prompt(v: &Value) -> Option<String> {
    if v.get("type").and_then(Value::as_str) != Some("user") {
        return None;
    }
    if v.get("isSidechain")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    if v.get("isMeta").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let text = message_text(v)?;
    if text.is_empty() || is_meta_text(&text) {
        return None;
    }
    Some(text)
}

/// Concatenated `text` blocks. `tool_use`, `tool_result`, `thinking` and image
/// blocks carry no prose worth searching.
fn message_text(v: &Value) -> Option<String> {
    let content = v.get("message")?.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.trim().to_string());
    }
    let arr = content.as_array()?;
    let mut parts = Vec::new();
    for block in arr {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        if let Some(t) = block.get("text").and_then(Value::as_str) {
            let t = t.trim();
            if !t.is_empty() {
                parts.push(t.to_string());
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

/// `-Users-x-proj` -> `/Users/x/proj`. Lossy for directories containing `-`,
/// so it is only used when no line in the file carried a cwd.
fn cwd_from_slug(path: &Path) -> PathBuf {
    let Some(slug) = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy())
    else {
        return PathBuf::new();
    };
    PathBuf::from(slug.replace('-', "/"))
}

fn ms_to_utc(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shapes copied from real transcripts on 2026-09-12, values anonymised.
    const FIXTURE: &str = concat!(
        r#"{"type":"agent-setting","agentSetting":"claude","sessionId":"84fcacde-6b20-49dd-9d9f-9de98138ac24"}"#,
        "\n",
        r#"{"type":"queue-operation","operation":"enqueue","timestamp":"2026-09-08T11:27:38.153Z","sessionId":"84fcacde-6b20-49dd-9d9f-9de98138ac24","content":"queued"}"#,
        "\n",
        r#"{"parentUuid":null,"isSidechain":false,"type":"user","message":{"role":"user","content":"<local-command-caveat>Caveat: generated while running local commands.</local-command-caveat>"},"isMeta":true,"uuid":"u0","timestamp":"2026-09-08T11:27:38.200Z","cwd":"/Users/x/Projects/demo","sessionId":"84fcacde-6b20-49dd-9d9f-9de98138ac24"}"#,
        "\n",
        r#"{"parentUuid":"u0","isSidechain":false,"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>\n<command-args></command-args>"},"uuid":"u1","timestamp":"2026-09-08T11:27:38.263Z","cwd":"/Users/x/Projects/demo","sessionId":"84fcacde-6b20-49dd-9d9f-9de98138ac24"}"#,
        "\n",
        r#"{"parentUuid":"u1","isSidechain":true,"type":"user","message":{"role":"user","content":"sidechain worker prompt"},"uuid":"u2","timestamp":"2026-09-08T11:27:39.000Z","sessionId":"84fcacde-6b20-49dd-9d9f-9de98138ac24"}"#,
        "\n",
        r#"{"parentUuid":"u2","isSidechain":false,"type":"user","message":{"role":"user","content":"<local-command-stdout>Set model to opus</local-command-stdout>"},"uuid":"u2b","timestamp":"2026-09-08T11:27:39.500Z","cwd":"/Users/x/Projects/demo","sessionId":"84fcacde-6b20-49dd-9d9f-9de98138ac24"}"#,
        "\n",
        r#"{"parentUuid":"u1","isSidechain":false,"type":"user","message":{"role":"user","content":[{"type":"text","text":"fix the auth middleware"}]},"uuid":"u3","timestamp":"2026-09-08T11:27:40.000Z","cwd":"/Users/x/Projects/demo","sessionId":"84fcacde-6b20-49dd-9d9f-9de98138ac24"}"#,
        "\n",
        r#"{"parentUuid":"u3","isSidechain":false,"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hidden"},{"type":"text","text":"Looking at the middleware now."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]},"uuid":"a1","timestamp":"2026-09-08T11:27:45.000Z","sessionId":"84fcacde-6b20-49dd-9d9f-9de98138ac24"}"#,
        "\n",
        r#"{"parentUuid":"a1","isSidechain":false,"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"a b c"}]},"uuid":"u4","timestamp":"2026-09-08T11:27:46.000Z","sessionId":"84fcacde-6b20-49dd-9d9f-9de98138ac24"}"#,
        "\n",
        r#"{"type":"ai-title","aiTitle":"generated title","sessionId":"84fcacde-6b20-49dd-9d9f-9de98138ac24"}"#,
        "\n",
        r#"{"type":"custom-title","customTitle":"Auth middleware fix","sessionId":"84fcacde-6b20-49dd-9d9f-9de98138ac24"}"#,
        "\n",
    );

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tmp");
        let proj = dir.path().join("-Users-x-Projects-demo");
        std::fs::create_dir_all(&proj).expect("mkdir");
        let p = proj.join("84fcacde-6b20-49dd-9d9f-9de98138ac24.jsonl");
        std::fs::write(&p, FIXTURE).expect("write");
        (dir, p)
    }

    #[test]
    fn parses_head_and_tail() {
        let (_d, p) = fixture();
        let s = parse_transcript(&p).expect("parse").expect("session");
        assert_eq!(s.harness, HarnessKind::Claude);
        assert_eq!(s.id, "84fcacde-6b20-49dd-9d9f-9de98138ac24");
        assert_eq!(s.cwd, PathBuf::from("/Users/x/Projects/demo"));
        assert_eq!(s.project, "demo");
        // Command wrappers, injected local-command output and the sidechain turn
        // are all skipped; the first thing a human typed wins.
        assert_eq!(s.first_prompt.as_deref(), Some("fix the auth middleware"));
        // custom-title outranks ai-title even though it comes later.
        assert_eq!(s.title.as_deref(), Some("Auth middleware fix"));
        assert_eq!(
            s.started_at.map(|t| t.to_rfc3339()),
            Some("2026-09-08T11:27:38.153+00:00".to_string())
        );
        assert_eq!(
            s.last_active_at.map(|t| t.to_rfc3339()),
            Some("2026-09-08T11:27:46+00:00".to_string())
        );
        assert!(s.transcript_present);
        assert!(s.size_bytes > 0);
    }

    #[test]
    fn extracts_only_prose_turns() {
        let (_d, p) = fixture();
        let (msgs, off) = extract_messages(&p, 0).expect("extract");
        let got: Vec<_> = msgs.iter().map(|m| (m.role, m.text.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (Role::User, "fix the auth middleware"),
                (Role::Assistant, "Looking at the middleware now."),
            ]
        );
        assert_eq!(off as usize, FIXTURE.len());

        // Nothing new appended -> nothing re-emitted.
        let (again, off2) = extract_messages(&p, off).expect("extract");
        assert!(again.is_empty());
        assert_eq!(off2, off);
    }

    #[test]
    fn injected_local_command_output_is_never_a_prompt() {
        for injected in [
            "<local-command-stdout>Set model to opus</local-command-stdout>",
            "<local-command-caveat>Caveat: ...</local-command-caveat>",
            "<command-name>/clear</command-name>",
            "<command-message>clear</command-message>",
            "<command-args>--foo</command-args>",
            "<system-reminder>be brief</system-reminder>",
        ] {
            assert!(is_meta_text(injected), "{injected}");
        }
        assert!(!is_meta_text(
            "<div> is something a user might actually type"
        ));
    }

    #[test]
    fn falls_back_to_the_directory_slug_for_cwd() {
        let dir = tempfile::tempdir().expect("tmp");
        let proj = dir.path().join("-Users-x-Projects-demo");
        std::fs::create_dir_all(&proj).expect("mkdir");
        let p = proj.join("11111111-2222-3333-4444-555555555555.jsonl");
        std::fs::write(&p, "{\"type\":\"mode\",\"mode\":\"normal\"}\n").expect("write");
        let s = parse_transcript(&p).expect("parse").expect("session");
        assert_eq!(s.id, "11111111-2222-3333-4444-555555555555");
        assert_eq!(s.cwd, PathBuf::from("/Users/x/Projects/demo"));
    }

    #[test]
    fn empty_file_yields_nothing() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("empty.jsonl");
        std::fs::write(&p, "").expect("write");
        assert!(parse_transcript(&p).expect("parse").is_none());
    }
}
