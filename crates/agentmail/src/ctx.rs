//! Shared wiring for the CLI subcommands: state dir, store, config and who "I" am.

use std::path::PathBuf;
use std::sync::Arc;

use agentmail_core::{Address, Config, Harness, Paths, Registration, Store};
use agentmail_mcp::{build_service, detect_identity, Identity, Service};

pub struct Ctx {
    pub paths: Paths,
    pub store: Arc<Store>,
    pub cfg: Config,
    pub identity: Identity,
}

impl Ctx {
    /// `from` is the `--from` flag: an explicit address wins over anything detected.
    pub async fn open(from: Option<&str>) -> anyhow::Result<Ctx> {
        let paths = Paths::from_env()?;
        let store = Arc::new(Store::open_at(&paths)?);
        let cfg = Config::load(&paths)?;
        let identity = identity(from, &store).await?;
        // A person at a terminal needs a mailbox too: without a registry row the
        // resolver cannot find `human:<user>`, and every reply to a CLI message would
        // die as `not_found`.
        if identity.harness == Harness::Other("human".into()) {
            // No pid: a person is not a process, and a row carrying this CLI's pid
            // would be swept the moment the command exits.
            store.register(&Registration::new(
                identity.harness.clone(),
                &identity.session_id,
                &identity.cwd,
            ))?;
        }
        Ok(Ctx {
            paths,
            store,
            cfg,
            identity,
        })
    }

    pub fn me(&self) -> Address {
        self.identity.address()
    }

    pub async fn service(&self) -> Service {
        build_service(
            self.identity.clone(),
            Arc::clone(&self.store),
            self.cfg.clone(),
        )
        .await
    }
}

async fn identity(from: Option<&str>, store: &Store) -> anyhow::Result<Identity> {
    let cwd = std::env::current_dir().unwrap_or_default();
    if let Some(raw) = from {
        let addr: Address = raw.parse()?;
        return Ok(Identity {
            harness: addr.harness,
            session_id: addr.id,
            cwd,
            pid: std::process::id(),
            herdr_pane: None,
            provisional: false,
        });
    }

    let detected = detect_identity(store).await;
    if !detected.provisional {
        return Ok(detected);
    }
    // Nothing identified a session, so this is a person at a terminal. `human` is not a
    // harness, so the address can never collide with a real session.
    Ok(Identity {
        harness: Harness::Other("human".into()),
        session_id: username(),
        cwd,
        pid: std::process::id(),
        herdr_pane: None,
        provisional: false,
    })
}

fn username() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| "user".to_string())
}

/// Where the installed binary lives, for the config snippets `setup` writes.
pub fn exe_path() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("agentmail"));
    #[cfg(windows)]
    let exe = without_verbatim_prefix(exe);
    exe
}

/// herdr starts plugin binaries through a canonicalized `\\?\C:\...` path, and the
/// prefix then shows up in `current_exe`. Harness shells take the plain form better,
/// and only a path longer than MAX_PATH needs the prefix to work at all.
#[cfg(any(windows, test))]
fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    const MAX_PATH: usize = 260;
    let plain = path
        .to_str()
        .and_then(|s| s.strip_prefix(r"\\?\"))
        .filter(|rest| rest.len() < MAX_PATH && rest.as_bytes().get(1) == Some(&b':'))
        .map(PathBuf::from);
    plain.unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verbatim_drive_path_loses_its_prefix() {
        assert_eq!(
            without_verbatim_prefix(PathBuf::from(r"\\?\C:\herdr\agentmail.exe")),
            PathBuf::from(r"C:\herdr\agentmail.exe")
        );
    }

    #[test]
    fn a_verbatim_unc_or_overlong_path_keeps_it() {
        let unc = PathBuf::from(r"\\?\UNC\server\share\agentmail.exe");
        assert_eq!(without_verbatim_prefix(unc.clone()), unc);
        let long = PathBuf::from(format!(r"\\?\C:\{}\agentmail.exe", "d".repeat(300)));
        assert_eq!(without_verbatim_prefix(long.clone()), long);
    }
}
