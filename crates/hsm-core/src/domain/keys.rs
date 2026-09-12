//! The browser keys the user may rebind.
//!
//! A binding is spelled, not typed: hsm-core knows nothing about crossterm, so
//! the TUI matches a real key event against one of these.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// One keystroke as the config file spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyBinding {
    Enter,
    Tab,
    Char(char),
    /// `alt-<char>`: reaches the action without leaving the search box.
    AltChar(char),
}

impl fmt::Display for KeyBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyBinding::Enter => f.write_str("enter"),
            KeyBinding::Tab => f.write_str("tab"),
            KeyBinding::Char(c) => write!(f, "{c}"),
            KeyBinding::AltChar(c) => write!(f, "alt-{c}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown key {0:?}; use enter, tab, a single character, or alt-<character>")]
pub struct KeyBindingParseError(pub String);

impl FromStr for KeyBinding {
    type Err = KeyBindingParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let lower = s.to_ascii_lowercase();
        match lower.as_str() {
            "enter" => return Ok(KeyBinding::Enter),
            "tab" => return Ok(KeyBinding::Tab),
            _ => {}
        }
        // The prefix is matched case-insensitively but the character is not:
        // `alt-O` and `alt-o` are different keys.
        let (rest, alt) = match lower.starts_with("alt-") || lower.starts_with("alt+") {
            true => (&s[4..], true),
            false => (s, false),
        };
        let mut chars = rest.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if alt => Ok(KeyBinding::AltChar(c)),
            (Some(c), None) => Ok(KeyBinding::Char(c)),
            _ => Err(KeyBindingParseError(s.to_string())),
        }
    }
}

impl Serialize for KeyBinding {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for KeyBinding {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// The two browser actions worth rebinding. Everything else is fixed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Keys {
    /// Focus the pane a live session already runs in, instead of opening a
    /// second copy of it.
    pub jump: KeyBinding,
    pub open_split: KeyBinding,
}

impl Default for Keys {
    fn default() -> Self {
        Keys {
            jump: KeyBinding::Enter,
            open_split: KeyBinding::Char('o'),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_form_round_trips() {
        for (text, binding) in [
            ("enter", KeyBinding::Enter),
            ("tab", KeyBinding::Tab),
            ("o", KeyBinding::Char('o')),
            ("alt-g", KeyBinding::AltChar('g')),
        ] {
            assert_eq!(text.parse::<KeyBinding>().expect(text), binding);
            assert_eq!(binding.to_string(), text);
        }
    }

    #[test]
    fn spellings_are_forgiving_but_junk_is_not() {
        assert_eq!(
            "ENTER".parse::<KeyBinding>().expect("enter"),
            KeyBinding::Enter
        );
        assert_eq!(
            "Alt+G".parse::<KeyBinding>().expect("alt"),
            KeyBinding::AltChar('G')
        );
        assert!("ctrl-shift-banana".parse::<KeyBinding>().is_err());
        assert!("".parse::<KeyBinding>().is_err());
        assert!("alt-".parse::<KeyBinding>().is_err());
    }
}
