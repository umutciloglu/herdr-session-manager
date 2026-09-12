//! Newline-delimited JSON over the herdr local socket.
//!
//! Unix uses a Unix domain socket, Windows a named pipe. Both sides are just an
//! `AsyncRead + AsyncWrite`, so everything above this module is platform-blind.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};

use crate::error::{Error, Result};

#[cfg(unix)]
pub(crate) type Socket = tokio::net::UnixStream;
#[cfg(windows)]
pub(crate) type Socket = tokio::net::windows::named_pipe::NamedPipeClient;

/// Default socket file name inside the herdr config directory.
const SOCKET_FILE: &str = "herdr.sock";

/// Reads an env var, treating empty as unset: herdr exports empty strings for
/// values it has nothing to say about.
fn env_var(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// herdr keeps its config under `~/.config/herdr` on every unix, macOS included,
/// so `dirs::config_dir` (which points at `Library/Application Support` there) is wrong.
pub fn config_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        if let Some(xdg) = env_var("XDG_CONFIG_HOME") {
            return Some(PathBuf::from(xdg).join("herdr"));
        }
        dirs::home_dir().map(|home| home.join(".config").join("herdr"))
    }
    #[cfg(not(unix))]
    {
        dirs::config_dir().map(|dir| dir.join("herdr"))
    }
}

/// Pure form of [`socket_path`], so the resolution order is testable without env mutation.
pub(crate) fn resolve_socket_path(
    socket_env: Option<&str>,
    session_env: Option<&str>,
    config_dir: Option<&Path>,
) -> Result<PathBuf> {
    if let Some(explicit) = socket_env {
        return Ok(PathBuf::from(explicit));
    }
    let dir = config_dir.ok_or(Error::SocketPathUnknown)?;
    Ok(match session_env {
        Some(name) => dir.join("sessions").join(name).join(SOCKET_FILE),
        None => dir.join(SOCKET_FILE),
    })
}

/// `HERDR_SOCKET_PATH`, else `HERDR_SESSION` named session, else the default session socket.
pub fn socket_path() -> Result<PathBuf> {
    resolve_socket_path(
        env_var("HERDR_SOCKET_PATH").as_deref(),
        env_var("HERDR_SESSION").as_deref(),
        config_dir().as_deref(),
    )
}

/// The herdr executable to shell out to. Plugins get `HERDR_BIN_PATH` injected;
/// everyone else falls back to whatever `herdr` resolves to on `PATH`.
pub fn herdr_bin() -> PathBuf {
    env_var("HERDR_BIN_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("herdr"))
}

#[cfg(unix)]
async fn open(path: &Path) -> std::io::Result<Socket> {
    tokio::net::UnixStream::connect(path).await
}

#[cfg(windows)]
async fn open(path: &Path) -> std::io::Result<Socket> {
    use tokio::net::windows::named_pipe::ClientOptions;

    // A named pipe rejects connects while the server is between accepts; that is
    // normal and short-lived, so retry rather than surfacing it.
    const ERROR_PIPE_BUSY: i32 = 231;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match ClientOptions::new().open(path) {
            Ok(client) => return Ok(client),
            Err(e)
                if e.raw_os_error() == Some(ERROR_PIPE_BUSY)
                    && std::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// One socket connection, framed as one JSON value per line.
pub struct Transport {
    reader: BufReader<ReadHalf<Socket>>,
    writer: WriteHalf<Socket>,
    buf: String,
    path: PathBuf,
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transport")
            .field("path", &self.path)
            .finish()
    }
}

impl Transport {
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let socket = open(&path).await.map_err(|source| Error::Connect {
            path: path.display().to_string(),
            source,
        })?;
        let (read, writer) = tokio::io::split(socket);
        tracing::debug!(path = %path.display(), "connected to herdr");
        Ok(Transport {
            reader: BufReader::new(read),
            writer,
            buf: String::new(),
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn write_line(&mut self, value: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(value)?;
        line.push(b'\n');
        self.writer.write_all(&line).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// Next line, or `None` once herdr closes the connection.
    pub async fn read_line(&mut self) -> Result<Option<Value>> {
        loop {
            self.buf.clear();
            if self.reader.read_line(&mut self.buf).await? == 0 {
                return Ok(None);
            }
            let line = self.buf.trim();
            // herdr does not send keepalive blanks today, but a blank line is
            // framing, not a value, so never hand it to serde.
            if line.is_empty() {
                continue;
            }
            return Ok(Some(serde_json::from_str(line)?));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_socket_path_wins() {
        let path = resolve_socket_path(Some("/tmp/x.sock"), Some("work"), Some(Path::new("/cfg")));
        assert_eq!(path.expect("path").to_str(), Some("/tmp/x.sock"));
    }

    #[test]
    fn named_session_gets_its_own_socket() {
        let path = resolve_socket_path(None, Some("work"), Some(Path::new("/cfg")));
        assert_eq!(
            path.expect("path").to_str(),
            Some("/cfg/sessions/work/herdr.sock")
        );
    }

    #[test]
    fn default_session_socket() {
        let path = resolve_socket_path(None, None, Some(Path::new("/cfg")));
        assert_eq!(path.expect("path").to_str(), Some("/cfg/herdr.sock"));
    }

    #[test]
    fn no_config_dir_is_an_error() {
        let err = resolve_socket_path(None, None, None).expect_err("should fail");
        assert!(matches!(err, Error::SocketPathUnknown));
    }
}
