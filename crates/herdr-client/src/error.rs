use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The `error` body herdr puts on a failed response.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct HerdrError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for HerdrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// herdr answered, and the answer was a refusal. Callers branch on [`Error::code`].
    #[error("herdr error: {0}")]
    Api(HerdrError),

    #[error("herdr socket {path}: {source}")]
    Connect {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("herdr socket io: {0}")]
    Io(#[from] std::io::Error),

    #[error("herdr sent malformed json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("herdr closed the connection")]
    Closed,

    #[error("{method} timed out after {timeout:?}")]
    Timeout { method: String, timeout: Duration },

    /// The response arrived but does not carry what this method's schema promises,
    /// usually a herdr new enough to have changed the shape.
    #[error("{method} returned no `{field}` field")]
    UnexpectedResult { method: String, field: String },

    #[error("cannot locate the herdr socket: no HERDR_SOCKET_PATH and no home directory")]
    SocketPathUnknown,
}

impl Error {
    /// herdr's machine-readable code, e.g. `agent_blocked`, `not_found`, `popup_not_open`.
    pub fn code(&self) -> Option<&str> {
        match self {
            Error::Api(e) => Some(e.code.as_str()),
            _ => None,
        }
    }

    pub fn is_code(&self, code: &str) -> bool {
        self.code() == Some(code)
    }
}
