//! `hsm doctor` — what is wrong, and nothing else.
//!
//! Quiet on purpose: a healthy machine prints one line. Every other line is a
//! problem with the thing to do about it, because the questions this answers are
//! always "why is the list empty" and "why is nothing marked live".

use std::path::Path;

use anyhow::Result;
use herdr_client::AgentInfo;
use hsm_core::{paths, HarnessKind};

use crate::commands::index::refresh;
use crate::runtime::Ctx;

/// How many refresh errors are worth printing before they are just noise.
const MAX_ERRORS: usize = 5;

pub fn run(ctx: &Ctx) -> Result<bool> {
    let mut problems = Vec::new();

    for kind in [HarnessKind::Claude, HarnessKind::Codex] {
        for root in hsm_core::harness::registry::transcript_roots(&kind) {
            if let Some(p) = missing_store(&kind, &root) {
                problems.push(p);
            }
        }
    }

    let mut index = ctx.index()?;
    let client = ctx.herdr();
    if client.is_none() {
        problems.push(format!(
            "herdr is not reachable at {}; live sessions and opening panes will not work",
            paths_socket()
        ));
    }
    if let Some(client) = &client {
        let agents = ctx.block_on(client.agent_list()).unwrap_or_default();
        problems.extend(unlinked_agents(&agents));
    }

    let live = ctx.live(client);
    match refresh(&mut index, &ctx.config, false, live.as_ref()) {
        Ok(report) => {
            for problem in report.errors.iter().take(MAX_ERRORS) {
                problems.push(format!("could not read {problem}"));
            }
            if report.errors.len() > MAX_ERRORS {
                problems.push(format!(
                    "{} more files could not be read",
                    report.errors.len() - MAX_ERRORS
                ));
            }
        }
        Err(e) => problems.push(format!("the index could not be refreshed: {e}")),
    }
    if index.count()? == 0 {
        problems.push(format!(
            "no sessions indexed; the index is {}",
            paths::index_path().display()
        ));
    }

    for problem in &problems {
        println!("{problem}");
    }
    if problems.is_empty() {
        println!("hsm doctor: nothing to report");
    }
    Ok(problems.is_empty())
}

/// A store directory that is not there. Normal on a machine where that harness has
/// never run, so it names the override that would move it rather than claiming a fault.
fn missing_store(kind: &HarnessKind, root: &Path) -> Option<String> {
    if root.exists() {
        return None;
    }
    let env = match kind {
        HarnessKind::Claude => "CLAUDE_CONFIG_DIR",
        _ => "CODEX_HOME",
    };
    Some(format!(
        "no {kind} sessions in {}; nothing has run there, or {env} points elsewhere",
        root.display()
    ))
}

/// herdr knows an agent is in the pane but not which session, which is what the
/// harness integration reports. Without it nothing can be marked live.
fn unlinked_agents(agents: &[AgentInfo]) -> Vec<String> {
    let mut kinds: Vec<&str> = agents
        .iter()
        .filter(|a| a.agent_session.is_none())
        .filter_map(|a| a.agent.as_deref())
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    kinds
        .into_iter()
        .map(|agent| {
            format!(
                "herdr runs {agent} in a pane but reports no session for it; \
                 run: herdr integration install {agent}"
            )
        })
        .collect()
}

fn paths_socket() -> String {
    herdr_client::socket_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "an unknown path".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_store_that_exists_is_not_a_problem() {
        let dir = tempfile::tempdir().expect("tmp");
        assert_eq!(missing_store(&HarnessKind::Claude, dir.path()), None);
    }

    #[test]
    fn a_missing_store_names_its_override() {
        let p = missing_store(&HarnessKind::Claude, Path::new("/nope/projects")).expect("problem");
        assert!(p.contains("CLAUDE_CONFIG_DIR"), "{p}");
        let p = missing_store(&HarnessKind::Codex, Path::new("/nope/sessions")).expect("problem");
        assert!(p.contains("CODEX_HOME"), "{p}");
    }

    fn agent(kind: Option<&str>, linked: bool) -> AgentInfo {
        AgentInfo {
            pane_id: "w1:p1".into(),
            agent: kind.map(str::to_string),
            agent_session: linked.then(|| herdr_client::AgentSessionInfo {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                kind: herdr_client::AgentSessionRefKind::Id,
                value: "01a0b532".into(),
            }),
            ..AgentInfo::default()
        }
    }

    #[test]
    fn an_agent_without_a_session_names_the_integration_to_install() {
        let problems = unlinked_agents(&[agent(Some("codex"), false), agent(Some("codex"), false)]);
        assert_eq!(problems.len(), 1, "one line per harness, not per pane");
        assert!(problems[0].contains("herdr integration install codex"));
    }

    #[test]
    fn linked_agents_are_quiet() {
        assert!(unlinked_agents(&[agent(Some("codex"), true)]).is_empty());
    }
}
