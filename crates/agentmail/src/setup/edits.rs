//! Pure content transformers for the files `setup` touches.
//!
//! Every function takes the current file content and returns the new content, so the
//! risky part — rewriting somebody's Claude or Codex configuration — is testable
//! without a filesystem, and the caller is the only thing that ever writes.
//!
//! Two rules hold everywhere here: never touch an entry we did not write (herdr's hook
//! groups live in the same files), and never reformat what we are not changing.

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

/// Seconds a harness gives our hook before giving up. A drain is a couple of sqlite
/// reads; ten seconds is generous and still bounded.
pub const HOOK_TIMEOUT: u64 = 10;

fn parse_object(content: &str, path: &str) -> Result<Map<String, Value>> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Ok(Map::new());
    }
    let value: Value =
        serde_json::from_str(trimmed).with_context(|| format!("{path} is not valid JSON"))?;
    match value {
        Value::Object(map) => Ok(map),
        _ => bail!("{path} is not a JSON object"),
    }
}

fn render(map: &Map<String, Value>) -> String {
    let mut out = serde_json::to_string_pretty(&Value::Object(map.clone()))
        .unwrap_or_else(|_| "{}".to_string());
    out.push('\n');
    out
}

// ---- hooks (Claude settings.json and Codex hooks.json share this shape) -----

fn hook_groups<'a>(root: &'a Value, event: &str) -> Option<&'a Vec<Value>> {
    root.get("hooks")?.get(event)?.as_array()
}

fn is_ours(entry: &Value, command: &str) -> bool {
    entry.get("command").and_then(Value::as_str) == Some(command)
}

pub fn hook_installed(content: &str, event: &str, command: &str) -> bool {
    let Ok(map) = parse_object(content, "hooks") else {
        return false;
    };
    let root = Value::Object(map);
    hook_groups(&root, event).is_some_and(|groups| {
        groups.iter().any(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|entries| entries.iter().any(|e| is_ours(e, command)))
        })
    })
}

/// Adds our command as a brand new matcher group. Existing groups — herdr's above all —
/// are never read into, reordered or rewritten.
pub fn add_hook(
    content: &str,
    event: &str,
    command: &str,
    matcher: Option<&str>,
    path: &str,
) -> Result<String> {
    if hook_installed(content, event, command) {
        return Ok(content.to_string());
    }
    let mut map = parse_object(content, path)?;

    let mut group = Map::new();
    if let Some(matcher) = matcher {
        group.insert("matcher".into(), json!(matcher));
    }
    group.insert(
        "hooks".into(),
        json!([{ "type": "command", "command": command, "timeout": HOOK_TIMEOUT }]),
    );

    let hooks = map
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        bail!("{path}: `hooks` is not an object");
    }
    let events = hooks.as_object_mut().expect("checked above");
    let groups = events
        .entry(event.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    match groups.as_array_mut() {
        Some(groups) => groups.push(Value::Object(group)),
        None => bail!("{path}: hooks.{event} is not an array"),
    }
    Ok(render(&map))
}

/// Removes only the entries whose command is ours, then drops any group that is left
/// with no hooks at all.
pub fn remove_hook(content: &str, event: &str, command: &str, path: &str) -> Result<String> {
    let mut map = parse_object(content, path)?;
    let Some(groups) = map
        .get_mut("hooks")
        .and_then(Value::as_object_mut)
        .and_then(|events| events.get_mut(event))
        .and_then(Value::as_array_mut)
    else {
        return Ok(content.to_string());
    };

    for group in groups.iter_mut() {
        if let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) {
            entries.retain(|e| !is_ours(e, command));
        }
    }
    groups.retain(|g| {
        g.get("hooks")
            .and_then(Value::as_array)
            .is_none_or(|entries| !entries.is_empty())
    });
    Ok(render(&map))
}

// ---- Claude MCP registration (~/.claude.json) -------------------------------

pub fn mcp_json_installed(content: &str, name: &str) -> bool {
    let Ok(map) = parse_object(content, "mcp") else {
        return false;
    };
    map.get("mcpServers")
        .and_then(Value::as_object)
        .is_some_and(|servers| servers.contains_key(name))
}

pub fn add_mcp_json(
    content: &str,
    name: &str,
    command: &str,
    args: &[&str],
    path: &str,
) -> Result<String> {
    let mut map = parse_object(content, path)?;
    let servers = map
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()));
    match servers.as_object_mut() {
        Some(servers) => {
            servers.insert(
                name.to_string(),
                json!({
                    "type": "stdio",
                    "command": command,
                    "args": args,
                    "env": {}
                }),
            );
        }
        None => bail!("{path}: `mcpServers` is not an object"),
    }
    Ok(render(&map))
}

pub fn remove_mcp_json(content: &str, name: &str, path: &str) -> Result<String> {
    let mut map = parse_object(content, path)?;
    if let Some(servers) = map.get_mut("mcpServers").and_then(Value::as_object_mut) {
        servers.remove(name);
    }
    Ok(render(&map))
}

// ---- Codex MCP registration (~/.codex/config.toml) --------------------------

/// TOML is edited as text, not through a parser: `~/.codex/config.toml` is a hand-kept
/// file full of comments and section order that a round trip through `toml` would
/// silently rewrite.
pub fn mcp_toml_installed(content: &str, name: &str) -> bool {
    let header = format!("[mcp_servers.{name}]");
    content
        .lines()
        .any(|line| line.trim_start().starts_with(&header))
}

pub fn add_mcp_toml(content: &str, name: &str, command: &str, args: &[&str]) -> Result<String> {
    if mcp_toml_installed(content, name) {
        return Ok(content.to_string());
    }
    let args = args
        .iter()
        .map(|a| toml_string(a))
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = content.trim_end().to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "[mcp_servers.{name}]\ncommand = {}\nargs = [{args}]\n",
        toml_string(command)
    ));
    validate_toml(out)
}

/// Drops the `[mcp_servers.<name>]` table and any sub-table of it, up to the next
/// unrelated table header.
pub fn remove_mcp_toml(content: &str, name: &str) -> Result<String> {
    let ours = format!("[mcp_servers.{name}]");
    let ours_sub = format!("[mcp_servers.{name}.");
    let mut out = String::new();
    let mut skipping = false;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            skipping = trimmed.starts_with(&ours) || trimmed.starts_with(&ours_sub);
        }
        if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    validate_toml(out.trim_end().to_string() + "\n")
}

/// A text edit that produced invalid TOML must never reach the user's config.
fn validate_toml(out: String) -> Result<String> {
    toml::from_str::<toml::Value>(&out).context("the edit would produce invalid TOML")?;
    Ok(out)
}

fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed copy of a real `~/.claude/settings.json`: the point is herdr's
    /// SessionStart group, which must survive everything we do.
    const CLAUDE_SETTINGS: &str = r#"{
  "permissions": { "defaultMode": "auto" },
  "model": "claude-fable-5-1[1m]",
  "hooks": {
    "SessionStart": [
      {
        "matcher": "*",
        "hooks": [
          {
            "type": "command",
            "command": "bash '/Users/x/.claude/hooks/herdr-agent-state.sh' session",
            "timeout": 10
          }
        ]
      }
    ]
  },
  "effortLevel": "xhigh"
}"#;

    /// A real `~/.codex/hooks.json`: same shape, no matcher key.
    const CODEX_HOOKS: &str = r#"{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "command": "bash '/Users/x/.codex/herdr-agent-state.sh' session",
            "timeout": 10,
            "type": "command"
          }
        ]
      }
    ]
  }
}"#;

    /// An excerpt of a real `~/.codex/config.toml`, comments and all.
    const CODEX_CONFIG: &str = r#"# managed by the user
model = "gpt-5"

[features]
hooks = true

[mcp_servers.node_repl]
args = []
command = "/Applications/ChatGPT.app/Contents/Resources/cua_node/bin/node_repl"
startup_timeout_sec = 120

[mcp_servers.node_repl.env]
CODEX_HOME = "/Users/x/.codex"

[desktop]
followUpQueueMode = "queue"
"#;

    const STOP_CMD: &str = "/usr/local/bin/agentmail hook claude-stop";

    fn json(content: &str) -> Value {
        serde_json::from_str(content).expect("valid json")
    }

    #[test]
    fn a_stop_hook_is_added_as_its_own_group() {
        assert!(!hook_installed(CLAUDE_SETTINGS, "Stop", STOP_CMD));
        let out = add_hook(CLAUDE_SETTINGS, "Stop", STOP_CMD, None, "settings").expect("add");
        assert!(hook_installed(&out, "Stop", STOP_CMD));

        let v = json(&out);
        let groups = v["hooks"]["Stop"].as_array().expect("Stop groups");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["hooks"][0]["type"], "command");
        assert_eq!(groups[0]["hooks"][0]["command"], STOP_CMD);
        assert_eq!(groups[0]["hooks"][0]["timeout"], 10);
        assert!(groups[0].get("matcher").is_none(), "Stop has no matcher");
    }

    #[test]
    fn herdrs_group_is_never_touched() {
        let out = add_hook(
            CLAUDE_SETTINGS,
            "SessionStart",
            "/usr/local/bin/agentmail hook claude-session-start",
            Some("*"),
            "settings",
        )
        .expect("add");

        let v = json(&out);
        let groups = v["hooks"]["SessionStart"].as_array().expect("groups");
        assert_eq!(groups.len(), 2, "a new group, not an edit of herdr's");
        assert_eq!(
            groups[0]["hooks"][0]["command"],
            "bash '/Users/x/.claude/hooks/herdr-agent-state.sh' session"
        );
        assert_eq!(groups[1]["matcher"], "*");
        // Unrelated settings survive.
        assert_eq!(v["model"], "claude-fable-5-1[1m]");
        assert_eq!(v["permissions"]["defaultMode"], "auto");
    }

    #[test]
    fn removing_our_hook_leaves_herdrs_behind() {
        let ours = "/usr/local/bin/agentmail hook claude-session-start";
        let added =
            add_hook(CLAUDE_SETTINGS, "SessionStart", ours, Some("*"), "settings").expect("add");
        let removed = remove_hook(&added, "SessionStart", ours, "settings").expect("remove");

        assert!(!hook_installed(&removed, "SessionStart", ours));
        let v = json(&removed);
        let groups = v["hooks"]["SessionStart"].as_array().expect("groups");
        assert_eq!(groups.len(), 1);
        assert!(groups[0]["hooks"][0]["command"]
            .as_str()
            .expect("command")
            .contains("herdr-agent-state.sh"));
    }

    #[test]
    fn adding_twice_changes_nothing() {
        let once = add_hook(CLAUDE_SETTINGS, "Stop", STOP_CMD, None, "settings").expect("add");
        let twice = add_hook(&once, "Stop", STOP_CMD, None, "settings").expect("add");
        assert_eq!(once, twice);
    }

    #[test]
    fn codex_hooks_take_the_same_shape_without_a_matcher() {
        let cmd = "/usr/local/bin/agentmail hook codex-stop";
        let out = add_hook(CODEX_HOOKS, "Stop", cmd, None, "hooks").expect("add");
        let v = json(&out);
        let group = &v["hooks"]["Stop"][0];
        assert!(group.get("matcher").is_none());
        assert_eq!(group["hooks"][0]["command"], cmd);
        // herdr's SessionStart group is still the only one there.
        assert_eq!(
            v["hooks"]["SessionStart"].as_array().expect("groups").len(),
            1
        );
    }

    #[test]
    fn an_empty_or_missing_file_is_a_fresh_object() {
        let out = add_hook("", "Stop", STOP_CMD, None, "hooks").expect("add");
        assert!(hook_installed(&out, "Stop", STOP_CMD));
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn broken_json_is_reported_not_overwritten() {
        assert!(add_hook("{not json", "Stop", STOP_CMD, None, "settings").is_err());
        assert!(!hook_installed("{not json", "Stop", STOP_CMD));
    }

    #[test]
    fn the_claude_mcp_entry_is_a_stdio_server() {
        let content = r#"{"numStartups": 12, "projects": {"/repo": {}}}"#;
        assert!(!mcp_json_installed(content, "agentmail"));

        let out = add_mcp_json(
            content,
            "agentmail",
            "/usr/local/bin/agentmail",
            &["mcp"],
            "~/.claude.json",
        )
        .expect("add");
        assert!(mcp_json_installed(&out, "agentmail"));

        let v = json(&out);
        assert_eq!(v["mcpServers"]["agentmail"]["type"], "stdio");
        assert_eq!(
            v["mcpServers"]["agentmail"]["command"],
            "/usr/local/bin/agentmail"
        );
        assert_eq!(v["mcpServers"]["agentmail"]["args"], json!(["mcp"]));
        assert_eq!(v["numStartups"], 12, "unrelated keys survive");

        let back = remove_mcp_json(&out, "agentmail", "~/.claude.json").expect("remove");
        assert!(!mcp_json_installed(&back, "agentmail"));
        assert_eq!(json(&back)["projects"]["/repo"], json!({}));
    }

    #[test]
    fn the_codex_mcp_table_is_appended_and_nothing_else_moves() {
        assert!(!mcp_toml_installed(CODEX_CONFIG, "agentmail"));
        let out = add_mcp_toml(
            CODEX_CONFIG,
            "agentmail",
            "/usr/local/bin/agentmail",
            &["mcp"],
        )
        .expect("add");
        assert!(mcp_toml_installed(&out, "agentmail"));
        assert!(
            out.starts_with("# managed by the user\n"),
            "comments survive"
        );
        assert!(
            out.contains("[mcp_servers.node_repl.env]"),
            "other servers survive"
        );
        assert!(
            out.trim_end().ends_with(
                "[mcp_servers.agentmail]\ncommand = \"/usr/local/bin/agentmail\"\nargs = [\"mcp\"]"
            ),
            "{out}"
        );

        let parsed: toml::Value = toml::from_str(&out).expect("valid toml");
        assert_eq!(
            parsed["mcp_servers"]["agentmail"]["command"].as_str(),
            Some("/usr/local/bin/agentmail")
        );
    }

    #[test]
    fn removing_the_codex_table_keeps_its_neighbours() {
        let out = add_mcp_toml(CODEX_CONFIG, "agentmail", "/opt/agentmail", &["mcp"]).expect("add");
        let back = remove_mcp_toml(&out, "agentmail").expect("remove");
        assert!(!mcp_toml_installed(&back, "agentmail"));
        let parsed: toml::Value = toml::from_str(&back).expect("valid toml");
        assert!(parsed["mcp_servers"].get("node_repl").is_some());
        assert!(parsed.get("desktop").is_some());
    }

    #[test]
    fn a_path_with_quotes_stays_valid_toml() {
        let out = add_mcp_toml("", "agentmail", "/opt/we\"ird/agentmail", &["mcp"]).expect("add");
        let parsed: toml::Value = toml::from_str(&out).expect("valid toml");
        assert_eq!(
            parsed["mcp_servers"]["agentmail"]["command"].as_str(),
            Some("/opt/we\"ird/agentmail")
        );
    }
}
