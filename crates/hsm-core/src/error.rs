use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

/// One error type for the whole crate: every layer here fails for the same few
/// reasons (bad file, bad json, bad sqlite) and splitting them only forces
/// conversions at every boundary.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("json error in {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("config error in {path}: {source}")]
    Config {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    /// A `PaneOps` implementation (herdr, or a test double) refused the call.
    #[error("pane backend error: {0}")]
    Backend(String),

    #[error("no context pane to open into")]
    NoContextPane,

    #[error("session id prefix {0:?} is ambiguous")]
    AmbiguousId(String),

    #[error("session {0} not found")]
    NotFound(String),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }

    pub fn json(path: impl Into<PathBuf>, source: serde_json::Error) -> Self {
        Error::Json {
            path: path.into(),
            source,
        }
    }
}
