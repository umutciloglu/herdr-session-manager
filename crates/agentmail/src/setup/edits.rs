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

/// What a hook edit did, so the caller can tell the user about a cleanup it did not ask
/// for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEdit {
    pub content: String,
    /// Duplicate entries of ours that were folded into one.
    pub collapsed: usize,
}

fn hook_groups<'a>(root: &'a Value, event: &str) -> Option<&'a Vec<Value>> {
    root.get("hooks")?.get(event)?.as_array()
}

/// Splits a command line the way a shell would for our purposes: whitespace, with
/// single and double quotes holding a word together. Good enough to recognise our own
/// entries, which is all it is for.
pub fn split_command(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    for c in command.chars() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some(_), _) => word.push(c),
            (None, '\'') | (None, '"') => quote = Some(c),
            (None, c) if c.is_whitespace() => {
                if !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
            }
            (None, c) => word.push(c),
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// Is this our command, wherever the binary happens to live?
///
/// Matching on the whole string was the bug: `setup` run from a build directory and
/// again from `~/.local/bin` installed two entries that both fired, and the model got
/// every message twice.
pub fn command_matches(command: &str, basename: &str, args: &[&str]) -> bool {
    let words = split_command(command);
    let Some(program) = words.first() else {
        return false;
    };
    let stem = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .trim_end_matches(".exe");
    stem == basename && words[1..] == *args
}

fn entry_matches(entry: &Value, basename: &str, args: &[&str]) -> bool {
    entry
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| command_matches(c, basename, args))
}

pub fn hook_installed(content: &str, event: &str, basename: &str, args: &[&str]) -> bool {
    let Ok(map) = parse_object(content, "hooks") else {
        return false;
    };
    let root = Value::Object(map);
    hook_groups(&root, event).is_some_and(|groups| {
        groups.iter().any(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|entries| entries.iter().any(|e| entry_matches(e, basename, args)))
        })
    })
}

/// Points the one entry of ours at `command`, collapsing any duplicates of it, and adds
/// a new matcher group only when there is nothing of ours to update.
///
/// Groups we did not write — herdr's above all — are never read into or reordered.
pub struct HookSpec<'a> {
    pub event: &'a str,
    /// The command line to write.
    pub command: &'a str,
    /// How our own entries are recognised, whatever directory they point at.
    pub basename: &'a str,
    pub args: &'a [&'a str],
    pub matcher: Option<&'a str>,
    /// False turns this into a collapse-only pass: it never installs.
    pub add_if_missing: bool,
    /// Only for error messages.
    pub path: &'a str,
}

pub fn set_hook(content: &str, spec: &HookSpec<'_>) -> Result<HookEdit> {
    let HookSpec {
        event,
        command,
        basename,
        args,
        matcher,
        add_if_missing,
        path,
    } = *spec;
    let mut map = parse_object(content, path)?;
    let mut seen = false;
    let mut collapsed = 0;

    if let Some(groups) = map
        .get_mut("hooks")
        .and_then(Value::as_object_mut)
        .and_then(|events| events.get_mut(event))
        .and_then(Value::as_array_mut)
    {
        for group in groups.iter_mut() {
            let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            entries.retain_mut(|entry| {
                if !entry_matches(entry, basename, args) {
                    return true;
                }
                if seen {
                    collapsed += 1;
                    return false;
                }
                seen = true;
                if let Some(obj) = entry.as_object_mut() {
                    obj.insert("command".into(), json!(command));
                }
                true
            });
        }
        groups.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|entries| !entries.is_empty())
        });
    }

    if !seen {
        if !add_if_missing {
            return Ok(HookEdit {
                content: content.to_string(),
                collapsed,
            });
        }
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
    }

    Ok(HookEdit {
        content: render(&map),
        collapsed,
    })
}

/// Removes every entry of ours for this event, then drops any group left empty.
pub fn remove_hook(
    content: &str,
    event: &str,
    basename: &str,
    args: &[&str],
    path: &str,
) -> Result<String> {
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
            entries.retain(|e| !entry_matches(e, basename, args));
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

/// Installed *and* pointing at this binary. An entry left behind by a copy that has
/// since moved is not an installation, it is a stale path to replace.
pub fn mcp_json_installed(content: &str, name: &str, basename: &str) -> bool {
    let Ok(map) = parse_object(content, "mcp") else {
        return false;
    };
    let Some(entry) = map
        .get("mcpServers")
        .and_then(Value::as_object)
        .and_then(|servers| servers.get(name))
    else {
        return false;
    };
    let command = entry.get("command").and_then(Value::as_str).unwrap_or("");
    let args: Vec<&str> = entry
        .get("args")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let same_binary = command
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(command)
        .trim_end_matches(".exe")
        == basename;
    same_binary && args == ["mcp"]
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
pub fn mcp_toml_installed(content: &str, name: &str, basename: &str) -> bool {
    let Ok(parsed) = toml::from_str::<toml::Value>(content) else {
        return false;
    };
    let Some(entry) = parsed.get("mcp_servers").and_then(|m| m.get(name)) else {
        return false;
    };
    let command = entry.get("command").and_then(|c| c.as_str()).unwrap_or("");
    let args: Vec<&str> = entry
        .get("args")
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let same_binary = command
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(command)
        .trim_end_matches(".exe")
        == basename;
    same_binary && args == ["mcp"]
}

/// The table exists, whoever it points at.
fn mcp_toml_present(content: &str, name: &str) -> bool {
    let header = format!("[mcp_servers.{name}]");
    content
        .lines()
        .any(|line| line.trim_start().starts_with(&header))
}

pub fn add_mcp_toml(content: &str, name: &str, command: &str, args: &[&str]) -> Result<String> {
    // A table pointing at an old copy of the binary is replaced, not duplicated.
    let content = &match mcp_toml_present(content, name) {
        true => remove_mcp_toml(content, name)?,
        false => content.to_string(),
    };
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

    const EXE: &str = "/usr/local/bin/agentmail";
    const OTHER_EXE: &str = "/Users/x/proj/target/release/agentmail";
    const STOP: [&str; 2] = ["hook", "claude-stop"];
    const SESSION: [&str; 2] = ["hook", "claude-session-start"];

    fn json(content: &str) -> Value {
        serde_json::from_str(content).expect("valid json")
    }

    fn install(content: &str, exe: &str, event: &str, args: &[&str; 2]) -> HookEdit {
        set_hook(
            content,
            &HookSpec {
                event,
                command: &format!("{exe} {} {}", args[0], args[1]),
                basename: "agentmail",
                args,
                matcher: None,
                add_if_missing: true,
                path: "settings",
            },
        )
        .expect("set hook")
    }

    #[test]
    fn a_stop_hook_is_added_as_its_own_group() {
        assert!(!hook_installed(CLAUDE_SETTINGS, "Stop", "agentmail", &STOP));
        let out = install(CLAUDE_SETTINGS, EXE, "Stop", &STOP);
        assert_eq!(out.collapsed, 0);
        assert!(hook_installed(&out.content, "Stop", "agentmail", &STOP));

        let v = json(&out.content);
        let groups = v["hooks"]["Stop"].as_array().expect("Stop groups");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["hooks"][0]["type"], "command");
        assert_eq!(
            groups[0]["hooks"][0]["command"],
            format!("{EXE} hook claude-stop")
        );
        assert_eq!(groups[0]["hooks"][0]["timeout"], 10);
        assert!(groups[0].get("matcher").is_none(), "Stop has no matcher");
    }

    #[test]
    fn herdrs_group_is_never_touched() {
        let out = set_hook(
            CLAUDE_SETTINGS,
            &HookSpec {
                event: "SessionStart",
                command: &format!("{EXE} hook claude-session-start"),
                basename: "agentmail",
                args: &SESSION,
                matcher: Some("*"),
                add_if_missing: true,
                path: "settings",
            },
        )
        .expect("set hook");

        let v = json(&out.content);
        let groups = v["hooks"]["SessionStart"].as_array().expect("groups");
        assert_eq!(groups.len(), 2, "a new group, not an edit of herdr's");
        assert_eq!(
            groups[0]["hooks"][0]["command"],
            "bash '/Users/x/.claude/hooks/herdr-agent-state.sh' session"
        );
        assert_eq!(groups[1]["matcher"], "*");
        assert_eq!(v["model"], "claude-fable-5-1[1m]");
        assert_eq!(v["permissions"]["defaultMode"], "auto");
    }

    #[test]
    fn removing_our_hook_leaves_herdrs_behind() {
        let added = set_hook(
            CLAUDE_SETTINGS,
            &HookSpec {
                event: "SessionStart",
                command: &format!("{EXE} hook claude-session-start"),
                basename: "agentmail",
                args: &SESSION,
                matcher: Some("*"),
                add_if_missing: true,
                path: "settings",
            },
        )
        .expect("set hook");
        let removed = remove_hook(
            &added.content,
            "SessionStart",
            "agentmail",
            &SESSION,
            "settings",
        )
        .expect("remove");

        assert!(!hook_installed(
            &removed,
            "SessionStart",
            "agentmail",
            &SESSION
        ));
        let v = json(&removed);
        let groups = v["hooks"]["SessionStart"].as_array().expect("groups");
        assert_eq!(groups.len(), 1);
        assert!(groups[0]["hooks"][0]["command"]
            .as_str()
            .expect("command")
            .contains("herdr-agent-state.sh"));
    }

    #[test]
    fn installing_twice_from_the_same_path_changes_nothing() {
        let once = install(CLAUDE_SETTINGS, EXE, "Stop", &STOP);
        let twice = install(&once.content, EXE, "Stop", &STOP);
        assert_eq!(once.content, twice.content);
        assert_eq!(twice.collapsed, 0);
    }

    /// The bug: `setup` run from a build directory and again from `~/.local/bin` used to
    /// leave two entries, both firing, so every message arrived twice.
    #[test]
    fn installing_from_another_path_updates_the_entry_in_place() {
        let first = install(CLAUDE_SETTINGS, OTHER_EXE, "Stop", &STOP);
        let second = install(&first.content, EXE, "Stop", &STOP);

        let groups = json(&second.content)["hooks"]["Stop"]
            .as_array()
            .expect("groups")
            .clone();
        assert_eq!(groups.len(), 1, "one entry, not two");
        assert_eq!(
            groups[0]["hooks"][0]["command"],
            format!("{EXE} hook claude-stop")
        );
    }

    #[test]
    fn duplicates_left_by_an_older_setup_are_folded_together() {
        // Two groups, two paths, exactly what the old code wrote.
        let first = install(CLAUDE_SETTINGS, OTHER_EXE, "Stop", &STOP);
        let mut v = json(&first.content);
        let groups = v["hooks"]["Stop"].as_array_mut().expect("groups");
        groups.push(json!({"hooks": [{"type": "command", "command": format!("{EXE} hook claude-stop"), "timeout": 10}]}));
        let doubled = serde_json::to_string_pretty(&v).expect("json");

        // Collapse without installing anything new.
        let edit = set_hook(
            &doubled,
            &HookSpec {
                event: "Stop",
                command: &format!("{EXE} hook claude-stop"),
                basename: "agentmail",
                args: &STOP,
                matcher: None,
                add_if_missing: false,
                path: "settings",
            },
        )
        .expect("set hook");
        assert_eq!(edit.collapsed, 1);
        let groups = json(&edit.content)["hooks"]["Stop"]
            .as_array()
            .expect("groups")
            .clone();
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0]["hooks"][0]["command"],
            format!("{EXE} hook claude-stop")
        );
    }

    #[test]
    fn collapsing_does_not_install_what_was_never_there() {
        let edit = set_hook(
            CLAUDE_SETTINGS,
            &HookSpec {
                event: "Stop",
                command: &format!("{EXE} hook claude-stop"),
                basename: "agentmail",
                args: &STOP,
                matcher: None,
                add_if_missing: false,
                path: "settings",
            },
        )
        .expect("set hook");
        assert_eq!(edit.content, CLAUDE_SETTINGS);
        assert!(!hook_installed(&edit.content, "Stop", "agentmail", &STOP));
    }

    #[test]
    fn codex_hooks_take_the_same_shape_without_a_matcher() {
        let args = ["hook", "codex-stop"];
        let out = set_hook(
            CODEX_HOOKS,
            &HookSpec {
                event: "Stop",
                command: &format!("{EXE} hook codex-stop"),
                basename: "agentmail",
                args: &args,
                matcher: None,
                add_if_missing: true,
                path: "hooks",
            },
        )
        .expect("set hook");
        let v = json(&out.content);
        let group = &v["hooks"]["Stop"][0];
        assert!(group.get("matcher").is_none());
        assert_eq!(
            group["hooks"][0]["command"],
            format!("{EXE} hook codex-stop")
        );
        assert_eq!(
            v["hooks"]["SessionStart"].as_array().expect("groups").len(),
            1
        );
    }

    #[test]
    fn an_empty_or_missing_file_is_a_fresh_object() {
        let out = install("", EXE, "Stop", &STOP);
        assert!(hook_installed(&out.content, "Stop", "agentmail", &STOP));
        assert!(out.content.ends_with('\n'));
    }

    #[test]
    fn broken_json_is_reported_not_overwritten() {
        assert!(set_hook(
            "{not json",
            &HookSpec {
                event: "Stop",
                command: "x",
                basename: "agentmail",
                args: &STOP,
                matcher: None,
                add_if_missing: true,
                path: "settings",
            },
        )
        .is_err());
        assert!(!hook_installed("{not json", "Stop", "agentmail", &STOP));
    }

    #[test]
    fn a_command_is_ours_wherever_the_binary_lives() {
        assert!(command_matches(
            "/opt/agentmail hook claude-stop",
            "agentmail",
            &STOP
        ));
        assert!(command_matches(
            "'/Users/x/my tools/agentmail' hook claude-stop",
            "agentmail",
            &STOP
        ));
        assert!(command_matches(
            "C:\\tools\\agentmail.exe hook claude-stop",
            "agentmail",
            &STOP
        ));
        // Somebody else's hook, and our own with different arguments.
        assert!(!command_matches(
            "bash '/Users/x/.claude/hooks/herdr-agent-state.sh' session",
            "agentmail",
            &STOP
        ));
        assert!(!command_matches(
            "/opt/agentmail hook codex-stop",
            "agentmail",
            &STOP
        ));
    }

    #[test]
    fn the_claude_mcp_entry_is_a_stdio_server() {
        let content = r#"{"numStartups": 12, "projects": {"/repo": {}}}"#;
        assert!(!mcp_json_installed(content, "agentmail", "agentmail"));

        let out = add_mcp_json(content, "agentmail", EXE, &["mcp"], "~/.claude.json").expect("add");
        assert!(mcp_json_installed(&out, "agentmail", "agentmail"));

        let v = json(&out);
        assert_eq!(v["mcpServers"]["agentmail"]["type"], "stdio");
        assert_eq!(v["mcpServers"]["agentmail"]["command"], EXE);
        assert_eq!(v["mcpServers"]["agentmail"]["args"], json!(["mcp"]));
        assert_eq!(v["numStartups"], 12, "unrelated keys survive");

        let back = remove_mcp_json(&out, "agentmail", "~/.claude.json").expect("remove");
        assert!(!mcp_json_installed(&back, "agentmail", "agentmail"));
        assert_eq!(json(&back)["projects"]["/repo"], json!({}));
    }

    #[test]
    fn an_entry_pointing_at_another_binary_counts_as_missing() {
        let content = json!({"mcpServers": {"agentmail": {"command": "/opt/something-else", "args": ["mcp"]}}})
            .to_string();
        assert!(!mcp_json_installed(&content, "agentmail", "agentmail"));

        let toml_content =
            "[mcp_servers.agentmail]\ncommand = \"/opt/something-else\"\nargs = [\"mcp\"]\n";
        assert!(!mcp_toml_installed(toml_content, "agentmail", "agentmail"));
        // Installing replaces the stale table instead of adding a second one.
        let out = add_mcp_toml(toml_content, "agentmail", EXE, &["mcp"]).expect("add");
        assert!(mcp_toml_installed(&out, "agentmail", "agentmail"));
        assert_eq!(out.matches("[mcp_servers.agentmail]").count(), 1);
    }

    #[test]
    fn the_codex_mcp_table_is_appended_and_nothing_else_moves() {
        assert!(!mcp_toml_installed(CODEX_CONFIG, "agentmail", "agentmail"));
        let out = add_mcp_toml(CODEX_CONFIG, "agentmail", EXE, &["mcp"]).expect("add");
        assert!(mcp_toml_installed(&out, "agentmail", "agentmail"));
        assert!(
            out.starts_with("# managed by the user\n"),
            "comments survive"
        );
        assert!(
            out.contains("[mcp_servers.node_repl.env]"),
            "other servers survive"
        );

        let parsed: toml::Value = toml::from_str(&out).expect("valid toml");
        assert_eq!(
            parsed["mcp_servers"]["agentmail"]["command"].as_str(),
            Some(EXE)
        );
    }

    #[test]
    fn removing_the_codex_table_keeps_its_neighbours() {
        let out = add_mcp_toml(CODEX_CONFIG, "agentmail", "/opt/agentmail", &["mcp"]).expect("add");
        let back = remove_mcp_toml(&out, "agentmail").expect("remove");
        assert!(!mcp_toml_installed(&back, "agentmail", "agentmail"));
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
