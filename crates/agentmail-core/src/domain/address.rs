use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::harness::Harness;
use crate::error::Error;

/// Shortest id prefix we accept when resolving. Anything shorter is too likely
/// to collide across sessions.
pub const MIN_PREFIX: usize = 8;

/// `<harness>:<session id>`, e.g. `claude:8890a685-...`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Address {
    pub harness: Harness,
    pub id: String,
}

impl Address {
    pub fn new(harness: Harness, id: impl Into<String>) -> Self {
        Address {
            harness,
            id: id.into(),
        }
    }

    /// First 8 characters of the id — what humans and envelopes use.
    pub fn short(&self) -> String {
        self.id.chars().take(MIN_PREFIX).collect()
    }

    /// `claude:8890a685`, the display form used in envelopes.
    pub fn short_display(&self) -> String {
        format!("{}:{}", self.harness, self.short())
    }

    pub fn matches_prefix(&self, prefix: &str) -> bool {
        self.id
            .to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase())
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.harness, self.id)
    }
}

impl FromStr for Address {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        // Split on the first colon only: some harness ids contain colons.
        let (h, id) = s
            .split_once(':')
            .ok_or_else(|| Error::parse(format!("address {s:?} is missing ':'")))?;
        let id = id.trim();
        if id.is_empty() {
            return Err(Error::parse(format!("address {s:?} has an empty id")));
        }
        Ok(Address {
            harness: h.parse()?,
            id: id.to_string(),
        })
    }
}

impl Serialize for Address {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Address {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// What a caller typed into `to`. Anything without a colon is an alias, so
/// `agentmail send reviewer "..."` works without knowing session ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressTarget {
    Address(Address),
    New(Harness),
    Alias(String),
}

impl AddressTarget {
    pub fn harness(&self) -> Option<&Harness> {
        match self {
            AddressTarget::Address(a) => Some(&a.harness),
            AddressTarget::New(h) => Some(h),
            AddressTarget::Alias(_) => None,
        }
    }
}

impl fmt::Display for AddressTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AddressTarget::Address(a) => write!(f, "{a}"),
            AddressTarget::New(h) => write!(f, "{h}:new"),
            AddressTarget::Alias(name) => f.write_str(name),
        }
    }
}

impl FromStr for AddressTarget {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(Error::parse("empty address target"));
        }
        match s.split_once(':') {
            Some((h, id)) if id.trim().eq_ignore_ascii_case("new") => {
                Ok(AddressTarget::New(h.parse()?))
            }
            Some(_) => Ok(AddressTarget::Address(s.parse()?)),
            None => Ok(AddressTarget::Alias(s.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_parse_table() {
        let cases: &[(&str, Option<(Harness, &str)>)] = &[
            ("claude:8890a685", Some((Harness::Claude, "8890a685"))),
            ("CLAUDE:8890a685", Some((Harness::Claude, "8890a685"))),
            ("codex:abc-123", Some((Harness::Codex, "abc-123"))),
            ("cursor:x1", Some((Harness::Other("cursor".into()), "x1"))),
            ("  claude:8890a685  ", Some((Harness::Claude, "8890a685"))),
            // Only the first colon splits, so ids may contain colons.
            (
                "codex:2026-09-12T08:00",
                Some((Harness::Codex, "2026-09-12T08:00")),
            ),
            ("claude", None),
            ("claude:", None),
            (":8890a685", None),
            ("", None),
        ];

        for (input, want) in cases {
            match (input.parse::<Address>(), want) {
                (Ok(got), Some((h, id))) => {
                    assert_eq!(&got.harness, h, "harness for {input:?}");
                    assert_eq!(got.id, *id, "id for {input:?}");
                    assert_eq!(
                        got.to_string(),
                        format!("{h}:{id}"),
                        "display for {input:?}"
                    );
                }
                (Err(_), None) => {}
                (got, want) => panic!("{input:?}: got {got:?}, wanted {want:?}"),
            }
        }
    }

    #[test]
    fn target_parse_table() {
        assert_eq!(
            "claude:new".parse::<AddressTarget>().expect("parse"),
            AddressTarget::New(Harness::Claude)
        );
        assert_eq!(
            "codex:NEW".parse::<AddressTarget>().expect("parse"),
            AddressTarget::New(Harness::Codex)
        );
        assert_eq!(
            "claude:8890a685".parse::<AddressTarget>().expect("parse"),
            AddressTarget::Address(Address::new(Harness::Claude, "8890a685"))
        );
        assert_eq!(
            "reviewer".parse::<AddressTarget>().expect("parse"),
            AddressTarget::Alias("reviewer".into())
        );
        assert!("".parse::<AddressTarget>().is_err());
    }

    #[test]
    fn short_and_prefix() {
        let a = Address::new(Harness::Claude, "8890a685-1234-4f00-9abc-def012345678");
        assert_eq!(a.short(), "8890a685");
        assert_eq!(a.short_display(), "claude:8890a685");
        assert!(a.matches_prefix("8890a685"));
        assert!(a.matches_prefix("8890A685"));
        assert!(!a.matches_prefix("8890a686"));

        // Short ids do not panic on the char take.
        assert_eq!(Address::new(Harness::Codex, "ab").short(), "ab");
    }
}
