use std::path::PathBuf;

use crate::domain::{HarnessKind, SessionRef};

/// The binary herdr launches for a kind. Borrowed from the argument so
/// `Other(name)` stays addressable instead of collapsing to a placeholder.
pub fn executable(kind: &HarnessKind) -> &str {
    match kind {
        // The only kind whose binary name differs from its herdr kind.
        HarnessKind::Cursor => "cursor-agent",
        other => other.as_str(),
    }
}

/// Resume arguments from docs/harnesses.md. `None` means the harness has no
/// resume command, so the session can only be reopened fresh in its cwd.
pub fn resume_args(kind: &HarnessKind, session: &SessionRef) -> Option<Vec<String>> {
    use HarnessKind::*;
    let v = &session.value;
    let args = match kind {
        Claude => vec!["--resume".into(), v.clone()],
        Codex => vec!["resume".into(), v.clone()],
        Pi => vec!["--session".into(), v.clone()],
        Omp => vec![format!("--resume={v}")],
        Agy => vec!["--conversation".into(), v.clone()],
        Cursor => vec!["--resume".into(), v.clone()],
        Grok => vec!["--resume".into(), v.clone()],
        Copilot => vec![format!("--resume={v}")],
        Devin => vec!["--resume".into(), v.clone()],
        Droid => vec!["--resume".into(), v.clone()],
        Kimi => vec!["--session".into(), v.clone()],
        Qodercli => vec!["--resume".into(), v.clone()],
        Qwen => vec!["--resume".into(), v.clone()],
        Opencode => vec!["--session".into(), v.clone()],
        Kilo => vec!["--session".into(), v.clone()],
        Hermes => vec!["--resume".into(), v.clone()],
        Mastracode => vec!["--thread".into(), v.clone()],
        Gemini | Cline | Amp | Kiro | Maki | Muse | Other(_) => return None,
    };
    Some(args)
}

pub fn is_resumable(kind: &HarnessKind) -> bool {
    let probe = SessionRef::id(kind.clone(), "x");
    resume_args(kind, &probe).is_some()
}

/// Whether the index scans this harness's own transcript store. For every
/// other kind a session is only ever known from a herdr pane ref, so a missing
/// transcript says nothing about whether the session still exists.
pub fn has_transcript_store(kind: &HarnessKind) -> bool {
    matches!(kind, HarnessKind::Claude | HarnessKind::Codex)
}

/// Where a harness keeps its transcripts. Empty for kinds we cannot read.
pub fn transcript_roots(kind: &HarnessKind) -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    match kind {
        HarnessKind::Claude => vec![home.join(".claude/projects")],
        HarnessKind::Codex => vec![home.join(".codex/sessions")],
        _ => Vec::new(),
    }
}

/// Codex's own thread index, read-only. Absent on a fresh install.
pub fn codex_state_db() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".codex/state_5.sqlite"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(kind: HarnessKind, value: &str) -> Option<Vec<String>> {
        let r = SessionRef::id(kind.clone(), value);
        resume_args(&kind, &r)
    }

    #[test]
    fn resumable_table_matches_docs() {
        let expect: &[(HarnessKind, &[&str])] = &[
            (HarnessKind::Claude, &["--resume", "S"]),
            (HarnessKind::Codex, &["resume", "S"]),
            (HarnessKind::Pi, &["--session", "S"]),
            (HarnessKind::Omp, &["--resume=S"]),
            (HarnessKind::Agy, &["--conversation", "S"]),
            (HarnessKind::Cursor, &["--resume", "S"]),
            (HarnessKind::Grok, &["--resume", "S"]),
            (HarnessKind::Copilot, &["--resume=S"]),
            (HarnessKind::Devin, &["--resume", "S"]),
            (HarnessKind::Droid, &["--resume", "S"]),
            (HarnessKind::Kimi, &["--session", "S"]),
            (HarnessKind::Qodercli, &["--resume", "S"]),
            (HarnessKind::Qwen, &["--resume", "S"]),
            (HarnessKind::Opencode, &["--session", "S"]),
            (HarnessKind::Kilo, &["--session", "S"]),
            (HarnessKind::Hermes, &["--resume", "S"]),
            (HarnessKind::Mastracode, &["--thread", "S"]),
        ];
        for (kind, want) in expect {
            let got = args(kind.clone(), "S").unwrap_or_else(|| panic!("{kind} must resume"));
            assert_eq!(got, *want, "{kind}");
        }
    }

    #[test]
    fn detected_but_unresumable_kinds_have_no_args() {
        for kind in [
            HarnessKind::Gemini,
            HarnessKind::Cline,
            HarnessKind::Amp,
            HarnessKind::Kiro,
            HarnessKind::Maki,
            HarnessKind::Muse,
            HarnessKind::Other("brand-new".into()),
        ] {
            assert!(args(kind.clone(), "S").is_none(), "{kind}");
            assert!(!is_resumable(&kind));
        }
    }

    #[test]
    fn executable_defaults_to_the_kind_name() {
        assert_eq!(executable(&HarnessKind::Claude), "claude");
        assert_eq!(executable(&HarnessKind::Cursor), "cursor-agent");
        assert_eq!(executable(&HarnessKind::Other("zed".into())), "zed");
    }

    #[test]
    fn only_claude_and_codex_have_known_stores() {
        for kind in HarnessKind::all() {
            let roots = transcript_roots(&kind);
            let expected = matches!(kind, HarnessKind::Claude | HarnessKind::Codex);
            assert_eq!(has_transcript_store(&kind), expected, "{kind}");
            assert_eq!(
                !roots.is_empty(),
                expected && dirs::home_dir().is_some(),
                "{kind}"
            );
        }
        assert!(!has_transcript_store(&HarnessKind::Other("zed".into())));
    }
}
