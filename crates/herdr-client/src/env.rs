//! Plugin runtime context herdr injects into processes it launches.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::transport;
use crate::types::AgentStatus;

fn var(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn path_var(key: &str) -> Option<PathBuf> {
    var(key).map(PathBuf::from)
}

/// `HERDR_PLUGIN_CONTEXT_JSON`: what herdr focused when it invoked us.
///
/// Every field is optional by design, and the shape grows between releases, so
/// unknown keys land in `extra` instead of failing the parse.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PluginContext {
    #[serde(default)]
    pub invocation_source: Option<String>,
    #[serde(default)]
    pub correlation_id: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub workspace_label: Option<String>,
    #[serde(default)]
    pub workspace_cwd: Option<String>,
    #[serde(default)]
    pub tab_id: Option<String>,
    #[serde(default)]
    pub tab_label: Option<String>,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
    #[serde(default)]
    pub focused_pane_cwd: Option<String>,
    #[serde(default)]
    pub focused_pane_agent: Option<String>,
    #[serde(default)]
    pub focused_pane_status: Option<AgentStatus>,
    #[serde(default)]
    pub selected_text: Option<String>,
    #[serde(default)]
    pub clicked_url: Option<String>,
    #[serde(default)]
    pub link_handler_id: Option<String>,
    #[serde(default)]
    pub worktree: Option<Value>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// Everything herdr tells a plugin process about where it is running.
#[derive(Debug, Clone, Default)]
pub struct PluginEnv {
    /// `HERDR_ENV=1`: we were launched by herdr at all.
    pub in_herdr: bool,
    pub socket_path: Option<PathBuf>,
    /// `HERDR_BIN_PATH`, falling back to `herdr` on `PATH`.
    pub bin_path: PathBuf,
    pub plugin_id: Option<String>,
    pub plugin_root: Option<PathBuf>,
    pub config_dir: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub workspace_id: Option<String>,
    pub tab_id: Option<String>,
    /// Absent for popup panes; those have no pane id at all.
    pub pane_id: Option<String>,
    pub context: Option<PluginContext>,
}

impl PluginEnv {
    pub fn from_env() -> Self {
        let context = var("HERDR_PLUGIN_CONTEXT_JSON").and_then(|raw| {
            match serde_json::from_str::<PluginContext>(&raw) {
                Ok(ctx) => Some(ctx),
                Err(error) => {
                    tracing::warn!(%error, "HERDR_PLUGIN_CONTEXT_JSON is not parseable");
                    None
                }
            }
        });

        PluginEnv {
            in_herdr: var("HERDR_ENV").as_deref() == Some("1"),
            socket_path: path_var("HERDR_SOCKET_PATH"),
            bin_path: transport::herdr_bin(),
            plugin_id: var("HERDR_PLUGIN_ID"),
            plugin_root: path_var("HERDR_PLUGIN_ROOT"),
            config_dir: path_var("HERDR_PLUGIN_CONFIG_DIR"),
            state_dir: path_var("HERDR_PLUGIN_STATE_DIR"),
            workspace_id: var("HERDR_WORKSPACE_ID"),
            tab_id: var("HERDR_TAB_ID"),
            pane_id: var("HERDR_PANE_ID"),
            context,
        }
    }

    /// The tiled pane the user was on. A popup has no `HERDR_PANE_ID`, so the
    /// context's focused pane is the only way to know what to act on.
    pub fn focused_pane_id(&self) -> Option<&str> {
        self.pane_id
            .as_deref()
            .or_else(|| self.context.as_ref()?.focused_pane_id.as_deref())
    }

    pub fn workspace(&self) -> Option<&str> {
        self.workspace_id
            .as_deref()
            .or_else(|| self.context.as_ref()?.workspace_id.as_deref())
    }

    pub fn tab(&self) -> Option<&str> {
        self.tab_id
            .as_deref()
            .or_else(|| self.context.as_ref()?.tab_id.as_deref())
    }

    /// Focused pane cwd, falling back to the workspace cwd.
    pub fn cwd(&self) -> Option<&str> {
        let context = self.context.as_ref()?;
        context
            .focused_pane_cwd
            .as_deref()
            .or(context.workspace_cwd.as_deref())
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.context.as_ref()?.selected_text.as_deref()
    }

    /// Agent kind on the focused pane, when herdr detected one.
    pub fn agent(&self) -> Option<&str> {
        self.context.as_ref()?.focused_pane_agent.as_deref()
    }

    pub fn agent_status(&self) -> Option<AgentStatus> {
        self.context.as_ref()?.focused_pane_status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_tolerates_unknown_fields() {
        let raw = r#"{"focused_pane_id":"w1:p3","selected_text":"hi","future_field":7}"#;
        let ctx: PluginContext = serde_json::from_str(raw).expect("parse");
        assert_eq!(ctx.focused_pane_id.as_deref(), Some("w1:p3"));
        assert_eq!(ctx.selected_text.as_deref(), Some("hi"));
        assert!(ctx.extra.contains_key("future_field"));
    }

    #[test]
    fn popup_falls_back_to_context_pane() {
        let env = PluginEnv {
            pane_id: None,
            context: Some(PluginContext {
                focused_pane_id: Some("w1:p3".to_string()),
                workspace_cwd: Some("/repo".to_string()),
                ..PluginContext::default()
            }),
            ..PluginEnv::default()
        };
        assert_eq!(env.focused_pane_id(), Some("w1:p3"));
        assert_eq!(env.cwd(), Some("/repo"));
    }
}
