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
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("agentmail"))
}
