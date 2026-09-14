//! Wake-up sockets. A poke carries no payload: it only tells a waiting process
//! "check the store". That keeps the transport trivial and the store the single
//! source of truth.

use std::time::Duration;

use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::tokio::Listener;
use interprocess::local_socket::{ListenerOptions, Name};

use crate::domain::Registration;
use crate::error::Result;
use crate::paths::Paths;

#[cfg(not(windows))]
use std::path::PathBuf;

#[cfg(not(windows))]
use interprocess::local_socket::tokio::Stream;
#[cfg(not(windows))]
use interprocess::local_socket::GenericFilePath;
#[cfg(windows)]
use interprocess::local_socket::GenericNamespaced;

/// The socket path (Unix) or pipe name (Windows) a registration listens on.
/// An explicit `poke_path` on the registration wins so an adapter can place it anywhere.
pub fn endpoint(reg: &Registration) -> Result<String> {
    if let Some(p) = reg.poke_path.as_deref().filter(|p| !p.is_empty()) {
        return Ok(p.to_string());
    }
    let paths = Paths::from_env()?;
    #[cfg(windows)]
    {
        Ok(paths.poke_pipe(&reg.harness, &reg.session_id))
    }
    #[cfg(not(windows))]
    {
        Ok(paths
            .poke_socket(&reg.harness, &reg.session_id)
            .to_string_lossy()
            .into_owned())
    }
}

fn to_name(s: &str) -> Result<Name<'_>> {
    #[cfg(windows)]
    {
        Ok(s.to_ns_name::<GenericNamespaced>()?)
    }
    #[cfg(not(windows))]
    {
        Ok(s.to_fs_name::<GenericFilePath>()?)
    }
}

pub struct PokeListener {
    listener: Listener,
    endpoint: String,
}

impl PokeListener {
    pub fn bind(reg: &Registration) -> Result<PokeListener> {
        let endpoint = endpoint(reg)?;

        #[cfg(not(windows))]
        {
            let path = PathBuf::from(&endpoint);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // A crashed process leaves its socket file behind and the bind would fail.
            // The file itself is worthless state, so removing it is always safe.
            if path.exists() {
                let _ = std::fs::remove_file(&path);
            }
        }

        let listener = ListenerOptions::new()
            .name(to_name(&endpoint)?)
            .create_tokio()?;

        Ok(PokeListener { listener, endpoint })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Resolves once per incoming poke. The connection is dropped immediately.
    pub async fn next(&mut self) {
        loop {
            match self.listener.accept().await {
                Ok(conn) => {
                    drop(conn);
                    return;
                }
                // A failed accept is not a wake-up; back off so a broken socket
                // cannot spin the caller's loop.
                Err(_) => tokio::time::sleep(Duration::from_millis(250)).await,
            }
        }
    }
}

pub struct Poker;

impl Poker {
    /// `Ok(false)` means nobody is listening — a normal outcome, not an error.
    #[cfg(not(windows))]
    pub async fn poke(reg: &Registration) -> Result<bool> {
        let endpoint = endpoint(reg)?;
        let name = to_name(&endpoint)?;
        match Stream::connect(name).await {
            Ok(stream) => {
                drop(stream);
                Ok(true)
            }
            Err(_) => Ok(false),
        }
    }

    /// `Ok(false)` means nobody is listening — a normal outcome, not an error.
    ///
    /// Opened with tokio rather than interprocess: when every instance of the pipe is
    /// taken, interprocess waits for a free one with no end and ignores any timeout.
    #[cfg(windows)]
    pub async fn poke(reg: &Registration) -> Result<bool> {
        use tokio::net::windows::named_pipe::ClientOptions;
        const ERROR_PIPE_BUSY: i32 = 231;

        let endpoint = endpoint(reg)?;
        // The namespace interprocess puts a namespaced listener name in.
        match ClientOptions::new().open(format!(r"\\.\pipe\{endpoint}")) {
            Ok(client) => {
                drop(client);
                Ok(true)
            }
            // Every instance taken means the owner is alive and reads the store on its
            // next wake-up, which is what a poke asks for.
            Err(e) => Ok(e.raw_os_error() == Some(ERROR_PIPE_BUSY)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Harness;

    fn reg(dir: &std::path::Path, id: &str) -> Registration {
        let mut r = Registration::new(Harness::Claude, id, dir);
        r.poke_path = Some(
            Paths::new(dir)
                .poke_socket(&Harness::Claude, id)
                .to_string_lossy()
                .into_owned(),
        );
        r
    }

    #[tokio::test]
    async fn poke_wakes_the_listener() {
        let dir = tempfile::tempdir().expect("tempdir");
        let r = reg(dir.path(), "sess-1");
        let mut listener = PokeListener::bind(&r).expect("bind");

        let woke = tokio::spawn(async move { listener.next().await });
        assert!(Poker::poke(&r).await.expect("poke"));
        tokio::time::timeout(Duration::from_secs(5), woke)
            .await
            .expect("listener did not wake")
            .expect("join");
    }

    #[tokio::test]
    async fn poke_with_nobody_home_is_false() {
        let dir = tempfile::tempdir().expect("tempdir");
        let r = reg(dir.path(), "sess-absent");
        assert!(!Poker::poke(&r).await.expect("poke"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bind_clears_a_stale_socket_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let r = reg(dir.path(), "sess-stale");
        let path = PathBuf::from(endpoint(&r).expect("endpoint"));
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, b"junk").expect("write stale");

        let _listener = PokeListener::bind(&r).expect("bind over stale file");
    }
}
