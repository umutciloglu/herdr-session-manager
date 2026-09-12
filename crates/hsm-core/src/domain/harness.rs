use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Every agent kind herdr can detect (docs/harnesses.md). `Other` keeps an
/// unknown future kind addressable instead of dropping the session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HarnessKind {
    Claude,
    Codex,
    Pi,
    Omp,
    Agy,
    Cursor,
    Grok,
    Copilot,
    Devin,
    Droid,
    Kimi,
    Qodercli,
    Qwen,
    Opencode,
    Kilo,
    Hermes,
    Mastracode,
    // Detected but not resumable.
    Gemini,
    Cline,
    Amp,
    Kiro,
    Maki,
    Muse,
    Other(String),
}

impl HarnessKind {
    /// herdr's own lowercase kind name; this is what goes into an address and
    /// into `agent.start {kind}`.
    pub fn as_str(&self) -> &str {
        use HarnessKind::*;
        match self {
            Claude => "claude",
            Codex => "codex",
            Pi => "pi",
            Omp => "omp",
            Agy => "agy",
            Cursor => "cursor",
            Grok => "grok",
            Copilot => "copilot",
            Devin => "devin",
            Droid => "droid",
            Kimi => "kimi",
            Qodercli => "qodercli",
            Qwen => "qwen",
            Opencode => "opencode",
            Kilo => "kilo",
            Hermes => "hermes",
            Mastracode => "mastracode",
            Gemini => "gemini",
            Cline => "cline",
            Amp => "amp",
            Kiro => "kiro",
            Maki => "maki",
            Muse => "muse",
            Other(s) => s,
        }
    }

    /// Never fails: an unrecognised name becomes `Other`.
    pub fn from_name(s: &str) -> Self {
        use HarnessKind::*;
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" => Claude,
            "codex" => Codex,
            "pi" => Pi,
            "omp" => Omp,
            "agy" => Agy,
            "cursor" | "cursor-agent" => Cursor,
            "grok" => Grok,
            "copilot" => Copilot,
            "devin" => Devin,
            "droid" => Droid,
            "kimi" => Kimi,
            "qodercli" => Qodercli,
            "qwen" => Qwen,
            "opencode" => Opencode,
            "kilo" => Kilo,
            "hermes" => Hermes,
            "mastracode" => Mastracode,
            "gemini" => Gemini,
            "cline" => Cline,
            "amp" => Amp,
            "kiro" => Kiro,
            "maki" => Maki,
            "muse" => Muse,
            other => Other(other.to_string()),
        }
    }

    /// Every named kind, in table order. Used by the registry tests and by
    /// callers that enumerate harnesses (config validation, filters).
    pub fn all() -> Vec<HarnessKind> {
        use HarnessKind::*;
        vec![
            Claude, Codex, Pi, Omp, Agy, Cursor, Grok, Copilot, Devin, Droid, Kimi, Qodercli, Qwen,
            Opencode, Kilo, Hermes, Mastracode, Gemini, Cline, Amp, Kiro, Maki, Muse,
        ]
    }
}

impl fmt::Display for HarnessKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HarnessKind {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(HarnessKind::from_name(s))
    }
}

impl Serialize for HarnessKind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for HarnessKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(HarnessKind::from_name(&s))
    }
}

/// How a harness names a session on its resume command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefKind {
    Id,
    Path,
}

impl RefKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RefKind::Id => "id",
            RefKind::Path => "path",
        }
    }

    pub fn from_name(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "path" => RefKind::Path,
            _ => RefKind::Id,
        }
    }
}

impl fmt::Display for RefKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RefKind {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(RefKind::from_name(s))
    }
}

/// What herdr stores on a pane (`agent_session {agent, kind, value}`) and what
/// the registry turns into resume args.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRef {
    pub harness: HarnessKind,
    pub kind: RefKind,
    pub value: String,
}

impl SessionRef {
    pub fn id(harness: HarnessKind, value: impl Into<String>) -> Self {
        SessionRef {
            harness,
            kind: RefKind::Id,
            value: value.into(),
        }
    }

    pub fn path(harness: HarnessKind, value: impl Into<String>) -> Self {
        SessionRef {
            harness,
            kind: RefKind::Path,
            value: value.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_known_kind() {
        for k in HarnessKind::all() {
            assert_eq!(HarnessKind::from_name(k.as_str()), k);
        }
    }

    #[test]
    fn unknown_kind_survives_as_other() {
        let k = HarnessKind::from_name("Brand-New");
        assert_eq!(k, HarnessKind::Other("brand-new".into()));
        assert_eq!(k.to_string(), "brand-new");
    }

    #[test]
    fn cursor_agent_binary_name_maps_to_cursor() {
        assert_eq!(HarnessKind::from_name("cursor-agent"), HarnessKind::Cursor);
    }
}
