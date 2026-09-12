use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::Error;

/// The agent CLI a session belongs to. Anything herdr knows about that we have no
/// special handling for lands in `Other`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Harness {
    Claude,
    Codex,
    Other(String),
}

impl Harness {
    /// Not a CLI. `human:<name>` rows are reply sinks for a person: addressable
    /// forever, never a running process.
    pub const HUMAN: &'static str = "human";

    pub fn is_human(&self) -> bool {
        self.as_str() == Harness::HUMAN
    }

    pub fn as_str(&self) -> &str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Other(s) => s,
        }
    }
}

impl fmt::Display for Harness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Harness {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(Error::parse("empty harness"));
        }
        let lower = s.to_ascii_lowercase();
        Ok(match lower.as_str() {
            "claude" => Harness::Claude,
            "codex" => Harness::Codex,
            _ => Harness::Other(lower),
        })
    }
}

impl Serialize for Harness {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Harness {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}
