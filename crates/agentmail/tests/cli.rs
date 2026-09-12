//! End-to-end checks of the binary itself. Everything runs against a temp home and a
//! temp state dir: nothing here may touch the real ~/.claude, ~/.codex or ~/.config.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const CODEX_ID: &str = "01999b0e-2222-4444-8888-cccccccccccc";

fn agentmail(home: &Path, state: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agentmail"));
    cmd.env("AGENTMAIL_STATE_DIR", state)
        .env("HOME", home)
        // A real herdr socket must never be dialled from a test.
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CLAUDECODE")
        .env_remove("CODEX_THREAD_ID");
    cmd
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn setup_installs_into_a_temp_home_and_check_agrees() {
    let home = tempfile::tempdir().expect("home");
    let state = tempfile::tempdir().expect("state");

    let missing = agentmail(home.path(), state.path())
        .args(["setup", "--check", "--home"])
        .arg(home.path())
        .output()
        .expect("run setup --check");
    assert_eq!(missing.status.code(), Some(1), "nothing is installed yet");

    let installed = agentmail(home.path(), state.path())
        .args(["setup", "--yes", "--home"])
        .arg(home.path())
        .output()
        .expect("run setup --yes");
    assert!(installed.status.success());
    let stdout = String::from_utf8_lossy(&installed.stdout);
    assert!(
        stdout.contains("--dangerously-load-development-channels server:agentmail"),
        "the channel flag is printed, never installed: {stdout}"
    );

    let settings = read(&home.path().join(".claude/settings.json"));
    assert!(settings.contains("hook claude-stop"), "{settings}");
    assert!(settings.contains("hook claude-session-start"), "{settings}");

    let claude_json = read(&home.path().join(".claude.json"));
    assert!(claude_json.contains("\"mcpServers\""), "{claude_json}");
    assert!(claude_json.contains("\"agentmail\""), "{claude_json}");

    let codex_hooks = read(&home.path().join(".codex/hooks.json"));
    assert!(codex_hooks.contains("hook codex-stop"), "{codex_hooks}");

    let codex_config = read(&home.path().join(".codex/config.toml"));
    assert!(
        codex_config.contains("[mcp_servers.agentmail]"),
        "{codex_config}"
    );

    let again = agentmail(home.path(), state.path())
        .args(["setup", "--check", "--home"])
        .arg(home.path())
        .output()
        .expect("run setup --check");
    assert_eq!(again.status.code(), Some(0), "everything is installed now");
}

#[test]
fn a_message_queued_by_send_comes_back_out_of_the_stop_hook() {
    let home = tempfile::tempdir().expect("home");
    let state = tempfile::tempdir().expect("state");

    let sent = agentmail(home.path(), state.path())
        .args([
            "send",
            &format!("codex:{CODEX_ID}"),
            "check the parser please",
            "--from",
            "human:tester",
            "--expects-reply",
        ])
        .output()
        .expect("run send");
    assert!(
        sent.status.success(),
        "{}",
        String::from_utf8_lossy(&sent.stderr)
    );
    let stdout = String::from_utf8_lossy(&sent.stdout);
    assert!(stdout.starts_with("queued "), "{stdout}");

    let hook_input = format!(
        r#"{{"session_id":"{CODEX_ID}","cwd":"/repo","hook_event_name":"Stop","transcript_path":"/tmp/x.jsonl"}}"#
    );
    let mut hook = agentmail(home.path(), state.path())
        .args(["hook", "codex-stop"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn hook");
    hook.stdin
        .as_mut()
        .expect("stdin")
        .write_all(hook_input.as_bytes())
        .expect("write hook input");
    let out = hook.wait_with_output().expect("hook output");
    assert!(out.status.success(), "a hook always exits 0");

    let answer: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("the hook prints one JSON object");
    assert_eq!(answer["decision"], "block");
    let reason = answer["reason"].as_str().expect("reason");
    assert!(reason.contains("[agentmail] from human:tester"), "{reason}");
    assert!(reason.contains("check the parser please"), "{reason}");
    assert!(reason.contains("Reply with agentmail_reply"), "{reason}");

    // A second Stop has nothing left to say, so the session is not blocked again.
    let mut hook = agentmail(home.path(), state.path())
        .args(["hook", "codex-stop"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn hook");
    hook.stdin
        .as_mut()
        .expect("stdin")
        .write_all(hook_input.as_bytes())
        .expect("write hook input");
    let out = hook.wait_with_output().expect("hook output");
    assert!(out.stdout.is_empty(), "empty stdout means carry on");
}

/// hsm's ask popup polls this, so the shape is a contract: every field present, oldest
/// first.
#[test]
fn inbox_json_is_a_stable_shape() {
    let home = tempfile::tempdir().expect("home");
    let state = tempfile::tempdir().expect("state");
    let to = format!("codex:{CODEX_ID}");

    for text in ["first", "second"] {
        let sent = agentmail(home.path(), state.path())
            .args([
                "send",
                &to,
                text,
                "--from",
                "human:tester",
                "--expects-reply",
            ])
            .output()
            .expect("run send");
        assert!(sent.status.success());
    }

    let out = agentmail(home.path(), state.path())
        .args(["inbox", "--addr", &to, "--json"])
        .output()
        .expect("run inbox");
    let listed: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("inbox --json is json");

    assert_eq!(listed["address"], to);
    let rows = listed["messages"].as_array().expect("messages");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["text"], "first", "oldest first");
    assert_eq!(rows[1]["text"], "second");
    for row in rows {
        for field in [
            "id",
            "from",
            "to",
            "text",
            "reply_to",
            "expects_reply",
            "status",
            "created_at",
            "delivered_at",
            "pushed_at",
        ] {
            assert!(row.get(field).is_some(), "{field} missing from {row}");
        }
        assert_eq!(row["from"], "human:tester");
        assert_eq!(row["expects_reply"], true);
        assert_eq!(row["status"], "pending");
        assert_eq!(row["reply_to"], serde_json::Value::Null);
    }
}

#[test]
fn the_session_start_hook_makes_a_session_addressable() {
    let home = tempfile::tempdir().expect("home");
    let state = tempfile::tempdir().expect("state");

    let mut hook = agentmail(home.path(), state.path())
        .args(["hook", "claude-session-start"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn hook");
    hook.stdin
        .as_mut()
        .expect("stdin")
        .write_all(
            br#"{"session_id":"8890a685-1111-2222-3333-444444444444","cwd":"/repo","hook_event_name":"SessionStart","source":"startup"}"#,
        )
        .expect("write hook input");
    let out = hook.wait_with_output().expect("hook output");
    assert!(
        out.stdout.is_empty(),
        "SessionStart says nothing to the model"
    );

    let sessions = agentmail(home.path(), state.path())
        .args(["sessions", "--json"])
        .output()
        .expect("run sessions");
    let listed: serde_json::Value =
        serde_json::from_slice(&sessions.stdout).expect("sessions --json");
    let addresses: Vec<&str> = listed["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .filter_map(|s| s["address"].as_str())
        .collect();
    assert!(
        addresses.contains(&"claude:8890a685-1111-2222-3333-444444444444"),
        "{addresses:?}"
    );
}
