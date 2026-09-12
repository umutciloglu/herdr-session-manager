use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SplitDirection {
    #[default]
    Horizontal,
    Vertical,
}

impl SplitDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            SplitDirection::Horizontal => "horizontal",
            SplitDirection::Vertical => "vertical",
        }
    }
}

impl fmt::Display for SplitDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a restored session lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenTarget {
    /// Type the resume command into the pane the user came from.
    Current,
    Split(SplitDirection),
    Tab,
}

impl Default for OpenTarget {
    fn default() -> Self {
        OpenTarget::Split(SplitDirection::Horizontal)
    }
}

impl OpenTarget {
    pub fn as_str(&self) -> &'static str {
        match self {
            OpenTarget::Current => "current",
            OpenTarget::Split(SplitDirection::Horizontal) => "split-horizontal",
            OpenTarget::Split(SplitDirection::Vertical) => "split-vertical",
            OpenTarget::Tab => "tab",
        }
    }
}

impl fmt::Display for OpenTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown open target {0:?}")]
pub struct OpenTargetParseError(pub String);

impl FromStr for OpenTarget {
    type Err = OpenTargetParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "current" => Ok(OpenTarget::Current),
            "split" | "split-horizontal" | "horizontal" => {
                Ok(OpenTarget::Split(SplitDirection::Horizontal))
            }
            "split-vertical" | "vertical" => Ok(OpenTarget::Split(SplitDirection::Vertical)),
            "tab" => Ok(OpenTarget::Tab),
            other => Err(OpenTargetParseError(other.to_string())),
        }
    }
}

impl Serialize for OpenTarget {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for OpenTarget {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_config_spellings() {
        assert_eq!(
            "split".parse::<OpenTarget>().expect("split"),
            OpenTarget::default()
        );
        assert_eq!(
            "split-vertical".parse::<OpenTarget>().expect("v"),
            OpenTarget::Split(SplitDirection::Vertical)
        );
        assert_eq!("tab".parse::<OpenTarget>().expect("tab"), OpenTarget::Tab);
        assert!("popup".parse::<OpenTarget>().is_err());
    }
}
