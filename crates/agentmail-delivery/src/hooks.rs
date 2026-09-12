//! Harness hook handlers. Pure functions over a `&Store` so they can be unit-tested
//! with sample JSON — the binary only supplies stdin and prints what comes back.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use agentmail_core::{Address, Envelope, Harness, Message, Registration, Store};
use serde::Deserialize;
use serde_json::json;

use crate::error::Result;

/// The hook payload both harnesses write to stdin. Claude and Codex agree on the
/// field names we care about; everything optional so a newer harness adding or
/// dropping fields cannot break the drain.
#[derive(Debug, Clone, Deserialize)]
pub struct HookInput {
    pub session_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub hook_event_name: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<String>,
    /// True when the harness is already running a turn our previous `block` caused.
    #[serde(default)]
    pub stop_hook_active: bool,
    /// Claude sets this when the Stop came from idling rather than from finishing.
    #[serde(default)]
    pub is_idle_stop: bool,
}

impl HookInput {
    pub fn parse(input: &str) -> Result<HookInput> {
        Ok(serde_json::from_str(input.trim())?)
    }

    pub fn address(&self, harness: &Harness) -> Address {
        Address::new(harness.clone(), self.session_id.clone())
    }

    fn working_dir(&self) -> PathBuf {
        self.cwd
            .as_deref()
            .filter(|c| !c.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
    }
}

/// Stop hook: hand the session everything addressed to it that nobody delivered yet.
///
/// Returns the JSON to print on stdout, or `None` when there is nothing to say —
/// an empty stdout is how both harnesses are told "carry on".
///
/// Loop guard: `stop_hook_active` means this Stop is the tail of a turn our own
/// `block` started. Blocking again with the same mail would spin the harness forever,
/// so we only ever block on rows that are still `Pending`; the previous drain marked
/// its own batch `Delivered`, which empties the query and ends the loop. New mail that
/// arrived during that turn is genuinely new and still gets through.
pub fn drain_stop(store: &Store, harness: &Harness, input: &str) -> Result<Option<String>> {
    let hook = HookInput::parse(input)?;
    let addr = hook.address(harness);

    // Claim and mark in one transaction: a session can have two Stop hooks installed
    // (an old path and a new one), and both firing must not hand the model the same
    // mail twice.
    let claimed = store.claim_pending(&addr)?;
    if claimed.is_empty() {
        return Ok(None);
    }

    // A row that went out over a channel may already be in the model's context. Claude
    // records channel events in the transcript, so the transcript is the only honest
    // answer to "did it arrive?" — a hit means read, a miss means push again here.
    let (seen, fresh): (Vec<Message>, Vec<Message>) = claimed
        .into_iter()
        .partition(|msg| already_seen(msg, hook.transcript_path.as_deref()));
    if !seen.is_empty() {
        let ids: Vec<String> = seen.iter().map(|m| m.id.clone()).collect();
        store.mark_read(&ids)?;
    }
    if fresh.is_empty() {
        return Ok(None);
    }

    let reason = Envelope::render_batch(&fresh);

    Ok(Some(
        json!({ "decision": "block", "reason": reason }).to_string(),
    ))
}

/// Only the tail is read: a long session's transcript is large, and a channel event
/// this turn is always near the end.
const TRANSCRIPT_TAIL: u64 = 2 * 1024 * 1024;

/// Was this message already put in front of the model by a channel push?
fn already_seen(msg: &Message, transcript: Option<&str>) -> bool {
    if msg.pushed_at.is_none() {
        return false;
    }
    transcript.is_some_and(|path| transcript_mentions(Path::new(path), &msg.id))
}

fn transcript_mentions(path: &Path, needle: &str) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len > TRANSCRIPT_TAIL && file.seek(SeekFrom::End(-(TRANSCRIPT_TAIL as i64))).is_err() {
        return false;
    }
    let mut buf = Vec::with_capacity(TRANSCRIPT_TAIL.min(len) as usize + 1);
    if file.read_to_end(&mut buf).is_err() {
        return false;
    }
    String::from_utf8_lossy(&buf).contains(needle)
}

/// SessionStart hook: make the session addressable before its MCP process is up.
///
/// Deliberately drains nothing. A session that just started has no model turn to
/// interrupt; the mail waits for the MCP process (Claude channel) or the next Stop.
pub fn session_start(store: &Store, harness: &Harness, input: &str) -> Result<Address> {
    session_start_in(store, harness, input, herdr_pane_from_env())
}

/// The hook runs inside the agent's own pane, which is the one moment `HERDR_PANE_ID`
/// is knowable without the MCP process. Recording it here is what later lets that
/// process identify itself exactly instead of guessing by directory.
pub fn session_start_in(
    store: &Store,
    harness: &Harness,
    input: &str,
    herdr_pane: Option<String>,
) -> Result<Address> {
    let hook = HookInput::parse(input)?;
    let mut reg = Registration::new(harness.clone(), hook.session_id.clone(), hook.working_dir());
    reg.herdr_pane = herdr_pane.clone();
    store.register(&reg)?;

    // Mail that was parked on this pane while the agent was still starting up (trust
    // prompts, channel confirmation) now has a real address to go to. This is the first
    // moment anything knows both halves.
    if let Some(pane) = herdr_pane {
        let placeholder = crate::pane::pane_address(harness, &pane);
        for msg in store.pending_for(&placeholder)? {
            store.retarget(&msg.id, &reg.address())?;
        }
    }
    Ok(reg.address())
}

pub fn herdr_pane_from_env() -> Option<String> {
    std::env::var("HERDR_PANE_ID")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_harness_payloads() {
        let claude = r#"{"session_id":"8890a685-1111-2222-3333-444444444444",
          "transcript_path":"/Users/x/.claude/projects/-Users-x/8890a685.jsonl",
          "cwd":"/Users/x/proj","hook_event_name":"Stop","stop_hook_active":false,"is_idle_stop":true}"#;
        let got = HookInput::parse(claude).expect("parse");
        assert_eq!(got.session_id, "8890a685-1111-2222-3333-444444444444");
        assert!(got.is_idle_stop);
        assert!(!got.stop_hook_active);

        // Codex omits the flags entirely.
        let codex = r#"{"session_id":"01999b0e","cwd":"/Users/x/proj","hook_event_name":"Stop"}"#;
        let got = HookInput::parse(codex).expect("parse");
        assert!(!got.stop_hook_active);
        assert_eq!(got.cwd.as_deref(), Some("/Users/x/proj"));
    }
}
