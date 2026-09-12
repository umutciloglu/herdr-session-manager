use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The external session provider could not answer. Callers treat this as "no results",
    /// never as a hard failure: agentmail degrades to registry + directory.
    #[error("session provider unavailable: {0}")]
    ProviderUnavailable(String),

    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse: {0}")]
    Parse(String),

    #[error("not found: {0}")]
    NotFound(String),
}

impl Error {
    pub fn parse(msg: impl fmt::Display) -> Self {
        Error::Parse(msg.to_string())
    }

    pub fn not_found(msg: impl fmt::Display) -> Self {
        Error::NotFound(msg.to_string())
    }

    pub fn provider(msg: impl fmt::Display) -> Self {
        Error::ProviderUnavailable(msg.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Parse(e.to_string())
    }
}

impl From<toml::de::Error> for Error {
    fn from(e: toml::de::Error) -> Self {
        Error::Parse(e.to_string())
    }
}
