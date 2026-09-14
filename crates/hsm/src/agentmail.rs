//! Finding and shelling out to the other product's binary.
//!
//! Optional by design: `hsm` works fully without it, and only the `m` key and
//! the `setup-chat` action need it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;

/// A send is a subprocess on the UI thread, so it gets a deadline. Long enough
/// for agentmail to resolve a peer and hand the message over, short enough that
/// a wedged child cannot hold the popup.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(15);
/// How often the wait loop looks at the child.
const POLL: Duration = Duration::from_millis(25);
/// Reading our own mailbox is a local sqlite query behind a process spawn; if
/// it takes longer than this something is wrong and the popup should not care.
pub const INBOX_TIMEOUT: Duration = Duration::from_secs(3);

/// `human:<login user>` — agentmail's permanent sink for a person rather than
/// a session. Everything this popup sends is from the human: a reply addressed
/// to the agent in the invoking pane would be injected into that agent by its
/// Stop hook instead of reaching whoever asked.
pub fn human_address() -> String {
    let user = ["USER", "LOGNAME", "USERNAME"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "user".to_string());
    format!("human:{user}")
}

/// Next to our own executable first — one `cargo build` produces both binaries
/// into the same directory — then the configured name on `PATH`.
pub fn find(configured: &str) -> Option<PathBuf> {
    sibling().or_else(|| which(configured))
}

fn sibling() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe
        .parent()?
        .join(format!("agentmail{}", std::env::consts::EXE_SUFFIX));
    candidate.is_file().then_some(candidate)
}

fn which(name: &str) -> Option<PathBuf> {
    let file = with_exe_suffix(name);
    let named = Path::new(name);
    // More than one component is a path the user spelled out, `bin/agentmail` included,
    // whichever separator it uses.
    if named.is_absolute() || named.components().count() > 1 {
        return [named.to_path_buf(), PathBuf::from(&file)]
            .into_iter()
            .find(|candidate| is_executable(candidate));
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(&file))
        .find(|candidate| is_executable(candidate))
}

/// `agentmail` → `agentmail.exe` on Windows, and a name that already says `.exe` stays
/// as it is.
fn with_exe_suffix(name: &str) -> String {
    let suffix = std::env::consts::EXE_SUFFIX;
    let has_suffix = name
        .len()
        .checked_sub(suffix.len())
        .is_some_and(|cut| name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(suffix));
    if has_suffix {
        name.to_string()
    } else {
        format!("{name}{suffix}")
    }
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// `agentmail send <address> <text> [--from <address>]`, reduced to the one
/// line the popup shows. `from` is the session in the invoking pane; without
/// one agentmail decides for itself who the sender is.
pub fn send(bin: &Path, address: &str, text: &str, from: Option<&str>) -> Result<String> {
    send_with_timeout(bin, address, text, from, SEND_TIMEOUT)
}

/// Same, plus `--expects-reply`: the peer is being asked, not told.
pub fn ask(bin: &Path, address: &str, text: &str, from: Option<&str>) -> Result<String> {
    let mut args = vec!["send", address, text, "--expects-reply"];
    if let Some(from) = from {
        args.extend(["--from", from]);
    }
    outcome(run(bin, &args, SEND_TIMEOUT)?, address)
}

pub fn send_with_timeout(
    bin: &Path,
    address: &str,
    text: &str,
    from: Option<&str>,
    timeout: Duration,
) -> Result<String> {
    let mut args = vec!["send", address, text];
    if let Some(from) = from {
        args.extend(["--from", from]);
    }
    outcome(run(bin, &args, timeout)?, address)
}

/// A send either happened or it did not; either way the last line is the part
/// worth putting in the status bar.
fn outcome(run: Run, address: &str) -> Result<String> {
    if run.ok {
        Ok(run.line().unwrap_or_else(|| format!("sent to {address}")))
    } else {
        Err(anyhow!(run
            .line()
            .unwrap_or_else(|| "agentmail send failed".to_string())))
    }
}

/// Our own mailbox as agentmail prints it. `--json` is the contract we want;
/// an agentmail that does not have it yet falls back to the plain listing,
/// which the caller diffs by line instead.
pub fn inbox(bin: &Path, address: &str) -> Result<String> {
    let json = run(bin, &["inbox", "--addr", address, "--json"], INBOX_TIMEOUT)?;
    if json.ok {
        return Ok(json.stdout);
    }
    let plain = run(bin, &["inbox", "--addr", address], INBOX_TIMEOUT)?;
    if plain.ok {
        Ok(plain.stdout)
    } else {
        Err(anyhow!(plain
            .line()
            .unwrap_or_else(|| "agentmail inbox failed".to_string())))
    }
}

/// What one subprocess said.
struct Run {
    ok: bool,
    stdout: String,
    stderr: String,
}

impl Run {
    /// The last thing it said, which is the part worth showing.
    fn line(&self) -> Option<String> {
        last_line(&self.stdout).or_else(|| last_line(&self.stderr))
    }
}

/// Runs `bin` with a deadline: these all sit on the UI thread, so a wedged
/// child must never hold the popup.
fn run(bin: &Path, args: &[&str], timeout: Duration) -> Result<Run> {
    let mut child = Command::new(bin)
        .args(args)
        // No stdin: a child that decides to prompt must fail, not hang us.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running {}", bin.display()))?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(anyhow!(
                    "agentmail {} did not answer within {}s",
                    args.first().copied().unwrap_or_default(),
                    timeout.as_secs_f32().round()
                ));
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(e) => return Err(anyhow!(e).context("waiting for agentmail")),
        }
    }

    // The child is already gone, so this only drains the pipes.
    let out = child
        .wait_with_output()
        .context("reading agentmail output")?;
    Ok(Run {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// The reply to `message_id` in a mailbox listing, given how that listing
/// looked before the question was asked.
///
/// `agentmail inbox --json` is the shape we want: rows with `id`, `text` and
/// `reply_to`. Until that exists the plain listing is diffed instead, and
/// whatever the baseline did not already contain counts as the answer.
pub fn reply_from(baseline: &str, current: &str, message_id: &str) -> Option<String> {
    if current.trim().is_empty() || current == baseline {
        return None;
    }
    if let Some(text) = json_reply(baseline, current, message_id) {
        return Some(text);
    }
    // Plain listing: the tail the baseline did not have.
    let tail = current.strip_prefix(baseline).unwrap_or(current);
    let tail = tail.trim();
    (!tail.is_empty() && tail != baseline.trim()).then(|| tail.to_string())
}

fn json_reply(baseline: &str, current: &str, message_id: &str) -> Option<String> {
    let now = messages(&serde_json::from_str(current).ok()?)?;
    let before: Vec<String> = serde_json::from_str(baseline)
        .ok()
        .as_ref()
        .and_then(messages)
        .map(|rows| rows.iter().filter_map(id_of).collect())
        .unwrap_or_default();

    // An explicit answer to this question wins; otherwise anything that was not
    // there when we asked.
    let answer = now
        .iter()
        .find(|m| m.get("reply_to").and_then(Value::as_str) == Some(message_id))
        .or_else(|| {
            now.iter()
                .find(|m| id_of(m).is_some_and(|id| !before.contains(&id)))
        })?;
    text_of(answer)
}

fn messages(root: &Value) -> Option<Vec<Value>> {
    match root {
        Value::Array(rows) => Some(rows.clone()),
        Value::Object(_) => ["messages", "inbox", "unread"]
            .iter()
            .find_map(|k| root.get(*k).and_then(Value::as_array).cloned()),
        _ => None,
    }
}

fn id_of(m: &Value) -> Option<String> {
    ["id", "message_id"]
        .iter()
        .find_map(|k| m.get(*k).and_then(Value::as_str))
        .map(str::to_string)
}

fn text_of(m: &Value) -> Option<String> {
    ["text", "body", "content"]
        .iter()
        .find_map(|k| m.get(*k).and_then(Value::as_str))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Every message an inbox listing holds, newest last, as far as the JSON says.
/// An `agentmail` without `inbox --json` prints prose instead; there is nothing
/// structured to read there, so the panel stays empty until the flag lands.
pub fn inbox_rows(listing: &str) -> Vec<InboxRow> {
    let Some(rows) = serde_json::from_str::<Value>(listing)
        .ok()
        .as_ref()
        .and_then(messages)
    else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|m| {
            Some(InboxRow {
                id: id_of(m)?,
                from: ["from", "sender"]
                    .iter()
                    .find_map(|k| m.get(*k).and_then(Value::as_str))
                    .unwrap_or_default()
                    .to_string(),
                when: ["created_at", "created", "at", "timestamp", "delivered_at"]
                    .iter()
                    .find_map(|k| m.get(*k).and_then(Value::as_str))
                    .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                    .map(|t| t.with_timezone(&Utc)),
                text: text_of(m).unwrap_or_default(),
                reply_to: m
                    .get("reply_to")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect()
}

/// One row of `agentmail inbox --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxRow {
    pub id: String,
    pub from: String,
    pub when: Option<DateTime<Utc>>,
    pub text: String,
    pub reply_to: Option<String>,
}

/// The id `agentmail send` reported, out of JSON if it printed JSON and out of
/// the prose otherwise. Falls back to the whole line so the status bar still
/// says something true.
pub fn message_id_of(line: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(line) {
        if let Some(id) = id_of(&v) {
            return id;
        }
    }
    line.split_whitespace()
        .map(|t| t.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
        .find(|t| is_ulid(t))
        .map(str::to_string)
        .unwrap_or_else(|| line.trim().to_string())
}

/// ULID: 26 characters of Crockford base32.
fn is_ulid(t: &str) -> bool {
    t.len() == 26
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() && !"ILOUilou".contains(c))
}

/// Hands the terminal to `agentmail setup`. On unix this replaces the process,
/// so the popup keeps one pid and closes when setup ends.
#[cfg(unix)]
pub fn run_setup(bin: &Path) -> Result<std::convert::Infallible> {
    use std::os::unix::process::CommandExt;
    // `exec` only returns when it failed.
    Err(anyhow!(Command::new(bin).arg("setup").exec())
        .context(format!("running {} setup", bin.display())))
}

#[cfg(not(unix))]
pub fn run_setup(bin: &Path) -> Result<std::process::ExitStatus> {
    Command::new(bin)
        .arg("setup")
        .status()
        .with_context(|| format!("running {} setup", bin.display()))
}

fn last_line(s: &str) -> Option<String> {
    s.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_takes_an_absolute_path_as_given() {
        let dir = tempfile::tempdir().expect("tmp");
        let bin = dir
            .path()
            .join(format!("am{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&bin, "#!/bin/sh\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        assert_eq!(
            which(&bin.to_string_lossy()).as_deref(),
            Some(bin.as_path())
        );

        let missing = dir.path().join("nope");
        assert!(which(&missing.to_string_lossy()).is_none());
    }

    #[test]
    fn a_name_that_already_says_exe_is_not_doubled() {
        let suffix = std::env::consts::EXE_SUFFIX;
        assert_eq!(
            with_exe_suffix(&format!("agentmail{suffix}")),
            format!("agentmail{suffix}")
        );
        assert_eq!(with_exe_suffix("agentmail"), format!("agentmail{suffix}"));
        #[cfg(windows)]
        assert_eq!(with_exe_suffix("AGENTMAIL.EXE"), "AGENTMAIL.EXE");
    }

    #[test]
    fn which_rejects_a_missing_name() {
        assert!(which("definitely-not-a-binary-93f1").is_none());
    }

    #[test]
    fn last_line_skips_trailing_blanks() {
        assert_eq!(last_line("a\nb\n\n").as_deref(), Some("b"));
        assert_eq!(last_line("   \n"), None);
    }

    /// A stand-in `agentmail`: a shell script is enough to see the argv and to
    /// hang on purpose.
    #[cfg(unix)]
    fn script(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("agentmail");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    #[cfg(unix)]
    #[test]
    fn send_passes_from_and_returns_the_last_line() {
        let dir = tempfile::tempdir().expect("tmp");
        let bin = script(dir.path(), r#"echo "argv: $*"; echo queued"#);

        let line = send(&bin, "codex:01a08ad8", "ping", Some("claude:8890a685")).expect("send");
        assert_eq!(line, "queued");

        let bin = script(dir.path(), r#"echo "argv: $*""#);
        let line = send(&bin, "codex:01a08ad8", "ping", Some("claude:8890a685")).expect("send");
        assert_eq!(
            line,
            "argv: send codex:01a08ad8 ping --from claude:8890a685"
        );

        // Without a sender agentmail picks one itself, so the flag is absent.
        let line = send(&bin, "codex:01a08ad8", "ping", None).expect("send");
        assert_eq!(line, "argv: send codex:01a08ad8 ping");
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_send_reports_its_last_line() {
        let dir = tempfile::tempdir().expect("tmp");
        let bin = script(dir.path(), "echo 'no such session' >&2; exit 2");
        let err = send(&bin, "codex:nope", "ping", None).expect_err("should fail");
        assert_eq!(err.to_string(), "no such session");
    }

    #[cfg(unix)]
    #[test]
    fn ask_adds_expects_reply() {
        let dir = tempfile::tempdir().expect("tmp");
        let bin = script(dir.path(), r#"echo "argv: $*""#);
        let line = ask(&bin, "codex:01a08ad8", "ping?", Some("claude:8890a685")).expect("ask");
        assert_eq!(
            line,
            "argv: send codex:01a08ad8 ping? --expects-reply --from claude:8890a685"
        );

        let line = ask(&bin, "codex:01a08ad8", "ping?", None).expect("ask");
        assert_eq!(line, "argv: send codex:01a08ad8 ping? --expects-reply");
    }

    #[cfg(unix)]
    #[test]
    fn inbox_falls_back_when_json_is_not_supported_yet() {
        let dir = tempfile::tempdir().expect("tmp");
        // An agentmail whose `inbox` has no `--json` flag yet.
        let bin = script(
            dir.path(),
            r#"case "$*" in *--json*) echo "unexpected argument --json" >&2; exit 2;; esac
echo '01J1 from codex:01a08ad8: hello'"#,
        );
        let out = inbox(&bin, "claude:8890a685").expect("inbox");
        assert_eq!(out.trim(), "01J1 from codex:01a08ad8: hello");
    }

    #[test]
    fn a_reply_is_the_row_this_question_did_not_have() {
        let baseline = r#"{"messages":[{"id":"01A","text":"older"}]}"#;
        let with_answer = r#"{"messages":[
            {"id":"01A","text":"older"},
            {"id":"01B","reply_to":"01Q","text":"yes, hourly"}
        ]}"#;

        assert_eq!(reply_from(baseline, baseline, "01Q"), None);
        assert_eq!(
            reply_from(baseline, with_answer, "01Q").as_deref(),
            Some("yes, hourly")
        );
        // A new row that answers someone else still counts as new mail.
        let other = r#"{"messages":[{"id":"01A","text":"older"},{"id":"01C","text":"unrelated"}]}"#;
        assert_eq!(
            reply_from(baseline, other, "01Q").as_deref(),
            Some("unrelated")
        );
    }

    #[test]
    fn a_plain_listing_is_diffed_by_its_tail() {
        let baseline = "01A from codex:01a08ad8: older\n";
        let current = "01A from codex:01a08ad8: older\n01B from codex:01a08ad8: yes, hourly\n";
        assert_eq!(
            reply_from(baseline, current, "01Q").as_deref(),
            Some("01B from codex:01a08ad8: yes, hourly")
        );
        assert_eq!(reply_from(baseline, baseline, "01Q"), None);
        assert_eq!(reply_from("", "", "01Q"), None);
    }

    #[test]
    fn the_sender_is_the_person_not_the_pane() {
        let who = human_address();
        assert!(who.starts_with("human:"), "{who}");
        assert!(who.len() > "human:".len(), "a name, not an empty one");
    }

    #[test]
    fn inbox_rows_read_the_json_listing() {
        let rows = inbox_rows(
            r#"{"messages":[
                {"id":"01A","from":"claude:8890a685","created_at":"2026-09-12T08:00:00Z","text":"yes, hourly"},
                {"id":"01B","from":"codex:01a08ad8","text":"it is in config.toml","reply_to":"01Q"}
            ]}"#,
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "01A");
        assert_eq!(rows[0].from, "claude:8890a685");
        assert_eq!(rows[0].text, "yes, hourly");
        assert_eq!(
            rows[0].when.map(|t| t.to_rfc3339()),
            Some("2026-09-12T08:00:00+00:00".to_string())
        );
        assert_eq!(rows[1].reply_to.as_deref(), Some("01Q"));
        assert_eq!(rows[1].when, None);
    }

    #[test]
    fn a_prose_listing_has_no_rows_to_read() {
        // Until `inbox --json` exists there is nothing structured to show.
        assert!(inbox_rows("01A from codex:01a08ad8: hello").is_empty());
        assert!(inbox_rows("").is_empty());
    }

    #[test]
    fn the_message_id_comes_out_of_json_or_out_of_the_prose() {
        assert_eq!(
            message_id_of(r#"{"message_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","outcome":"queued"}"#),
            "01ARZ3NDEKTSV4RRFFQ69G5FAV"
        );
        assert_eq!(
            message_id_of("queued 01ARZ3NDEKTSV4RRFFQ69G5FAV for codex:01a08ad8"),
            "01ARZ3NDEKTSV4RRFFQ69G5FAV"
        );
        // Nothing id-shaped: the line itself is the most honest answer.
        assert_eq!(message_id_of("  queued  "), "queued");
    }

    #[cfg(unix)]
    #[test]
    fn a_wedged_send_is_killed_at_the_deadline() {
        let dir = tempfile::tempdir().expect("tmp");
        let bin = script(dir.path(), "sleep 30");
        let started = Instant::now();
        let err = send_with_timeout(
            &bin,
            "codex:01a08ad8",
            "ping",
            None,
            Duration::from_millis(200),
        )
        .expect_err("should time out");
        assert!(err.to_string().contains("did not answer"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "it waited too long"
        );
    }
}
