use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }

    pub fn from_name(s: &str) -> Option<Role> {
        match s {
            "user" => Some(Role::User),
            "assistant" | "agent" => Some(Role::Assistant),
            _ => None,
        }
    }
}

/// One searchable turn. Tool calls and tool results are never emitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedMessage {
    pub role: Role,
    pub text: String,
    pub ts: Option<DateTime<Utc>>,
}

/// Prefixes of prompts the harness injects on the user's behalf: slash-command
/// wrappers (`<command-name>`, `<command-message>`, `<command-args>`) and the
/// output the CLI feeds back after running one (`<local-command-stdout>`,
/// `<local-command-caveat>`). None of it is something a human typed, so it is
/// never the "first prompt" and never indexed.
pub const META_PREFIXES: &[&str] = &[
    "<local-command-",
    "<command-",
    "<system-reminder>",
    "<user-prompt-submit-hook>",
    "<bash-stdout>",
    "<bash-stderr>",
];

pub fn is_meta_text(s: &str) -> bool {
    let t = s.trim_start();
    META_PREFIXES.iter().any(|p| t.starts_with(p))
}
