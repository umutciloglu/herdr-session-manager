//! Wake-up sockets. A poke carries no payload: it only tells a waiting process
//! "check the store". That keeps the transport trivial and the store the single
//! source of truth.

use std::time::Duration;

use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::tokio::{Listener, Stream};
use interprocess::local_socket::{ListenerOptions, Name};

use crate::domain::Registration;
use crate::error::Result;

#[cfg(not(windows))]
use std::path::PathBuf;

#[cfg(not(windows))]
use crate::paths::Paths;

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
    #[cfg(windows)]
    {
        Ok(crate::paths::poke_name(&reg.harness, &reg.session_id))
    }
    #[cfg(not(windows))]
    {
        let paths = Paths::from_env()?;
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
}

#[cfg(all(test, unix))]
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
