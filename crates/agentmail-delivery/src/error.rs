use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("agentmail: {0}")]
    Core(#[from] agentmail_core::Error),

    /// A hook handed us something that is not the documented JSON shape.
    #[error("hook input: {0}")]
    HookInput(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::HookInput(e.to_string())
    }
}

impl Error {
    pub fn hook(msg: impl fmt::Display) -> Self {
        Error::HookInput(msg.to_string())
    }
}
