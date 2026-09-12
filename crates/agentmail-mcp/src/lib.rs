//! agentmail-mcp — one MCP server per agent session.
//!
//! `service` holds every decision and knows nothing about MCP; `server` maps it onto
//! rmcp over stdio; `run` wires a process together: identity, registry row, wake-up
//! socket, deliverer chain and the background tasks that keep them fresh.
//!
//! stdout belongs to the protocol. Everything this crate wants to say goes to `tracing`,
//! which the binary points at stderr.

pub mod identity;
pub mod server;
pub mod service;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

use agentmail_core::{
    Address, CommandSessionProvider, Config, Mailbox, Paths, PokeListener, Resolver,
    SessionProvider, Store,
};
use rmcp::ServiceExt;

pub use identity::{Evidence, Identity};
pub use server::{server_info, AgentmailServer};
pub use service::{ChannelMessage, DirectorySource, Service, ServiceError, INSTRUCTIONS};

/// How often the registry row is refreshed so `live()` keeps trusting it.
const TOUCH_EVERY: Duration = Duration::from_secs(30);
/// How often the live-session set is re-read for `resources/list_changed`.
const RESOURCE_POLL: Duration = Duration::from_secs(5);
/// The session index answers by shelling out and its contents move slowly, so it is
/// checked once every this many resource polls rather than on every one.
const PROVIDER_EVERY: u32 = 6;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("agentmail: {0}")]
    Core(#[from] agentmail_core::Error),

    #[error("mcp: {0}")]
    Mcp(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Who this process belongs to, with every source of evidence consulted — including
/// herdr, which is the only one that can tell a Codex pane apart from the Claude
/// environment it inherited.
pub async fn detect_identity(store: &Store) -> Identity {
    let mut evidence = Evidence::from_process();
    if let Some(pane) = evidence.herdr_pane() {
        evidence = evidence.with_herdr(herdr_pane_identity(&pane).await);
    }
    Identity::from_evidence(&evidence, store)
}

/// Assemble the service the MCP server and the CLI both use: deliverer chain, mailbox,
/// session provider and — when the feature is on and herdr is reachable — its directory.
pub async fn build_service(identity: Identity, store: Arc<Store>, cfg: Config) -> Service {
    let herdr = connect_herdr().await;
    let deliverers =
        agentmail_delivery::default_deliverers(&cfg, Arc::clone(&store), herdr_handle(&herdr));
    let mailbox = Mailbox::new(
        Arc::clone(&store),
        Resolver::new(Arc::clone(&store)),
        deliverers,
    );

    let mut svc = Service::new(identity, Arc::clone(&store), mailbox, cfg.clone());
    if let Some(argv) = cfg.provider_argv() {
        // `hsm` is usually a sibling of this binary rather than something on PATH.
        let argv = agentmail_delivery::resolve_argv(argv);
        let provider: Arc<dyn SessionProvider> = Arc::new(CommandSessionProvider::new(argv));
        svc = svc.with_provider(provider);
    }
    if let Some(dir) = directory(&herdr) {
        svc = svc.with_directory(dir);
    }
    svc
}

/// Serve MCP on stdio until the client disconnects.
pub async fn run() -> Result<(), Error> {
    let paths = Paths::from_env()?;
    let store = Arc::new(Store::open_at(&paths)?);
    let cfg = Config::load(&paths)?;

    // Sessions that ended without deregistering leave rows behind; clearing them here
    // keeps `resources/list` and address resolution honest.
    match store.prune(chrono::Utc::now()) {
        Ok(n) if n > 0 => tracing::info!(pruned = n, "dropped stale registry rows"),
        Ok(_) => {}
        Err(e) => tracing::warn!("prune: {e}"),
    }

    let identity = detect_identity(&store).await;
    if identity.provisional {
        tracing::warn!(
            harness = %identity.harness,
            cwd = %identity.cwd.display(),
            "no session id in the environment and no SessionStart hook row for this directory; \
             registering provisionally, so this session is not addressable by its real id"
        );
    }

    // One ring for "mail arrived" (waiting tool calls) and one for "try to work out who
    // we are again" (the session loop, which owns the socket).
    let wake = Arc::new(Notify::new());
    let repair = Arc::new(Notify::new());
    let svc = Arc::new(
        build_service(identity.clone(), Arc::clone(&store), cfg.clone())
            .await
            .with_signals(Arc::clone(&wake), Arc::clone(&repair))
            .serving_mcp(),
    );

    // The wake-up socket exists only where something acts on a poke. Under Codex there
    // is no channel, and a socket that answers but does nothing would make senders
    // believe the message landed instead of letting them prompt or queue it.
    let poke = match svc.channel_enabled() {
        true => match PokeListener::bind(&identity.registration(None)) {
            Ok(listener) => Some(listener),
            Err(e) => {
                tracing::warn!("could not bind the poke socket: {e}");
                None
            }
        },
        false => None,
    };
    let reg = identity.registration(poke.as_ref().map(|p| p.endpoint().to_string()));
    store.register(&reg)?;

    let running = AgentmailServer::new(Arc::clone(&svc))
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| Error::Mcp(e.to_string()))?;
    let peer = running.peer().clone();

    // Both the poke path and the poll compare against the same last-known state, so a
    // wake-up cannot make the poll announce a change twice.
    let seen = Arc::new(Mutex::new(Fingerprint {
        live: svc.live_fingerprint().await,
        provider: Vec::new(),
    }));
    let tasks = vec![
        tokio::spawn(session_loop(
            Arc::clone(&svc),
            peer.clone(),
            poke,
            Arc::clone(&seen),
            wake,
            Arc::clone(&repair),
        )),
        tokio::spawn(touch_loop(Arc::clone(&svc), Arc::clone(&store), repair)),
        tokio::spawn(resource_loop(Arc::clone(&svc), peer, seen)),
    ];

    let outcome = running
        .waiting()
        .await
        .map_err(|e| Error::Mcp(e.to_string()));
    for task in tasks {
        task.abort();
    }
    // Leaving a dead row behind would make this session look reachable forever.
    store.deregister(&identity.harness, &identity.session_id)?;
    outcome.map(|_| ())
}

/// Owns the wake-up socket, and therefore everything that depends on it: draining the
/// Claude channel, waking blocked waits, and re-binding when the identity changes.
async fn session_loop(
    svc: Arc<Service>,
    peer: rmcp::service::Peer<rmcp::service::RoleServer>,
    mut poke: Option<PokeListener>,
    seen: Arc<Mutex<Fingerprint>>,
    wake: Arc<Notify>,
    repair: Arc<Notify>,
) {
    // What the current socket answers for. `None` means there is no socket, or it is
    // bound to an address we have since stopped being.
    let mut bound = poke
        .as_ref()
        .map(|_| svc.identity().address())
        .filter(|_| !svc.identity().provisional);

    push_pending(&svc, &peer).await;
    settle_identity(&svc, &mut poke, &mut bound);

    loop {
        match poke.as_mut() {
            Some(listener) => {
                tokio::select! {
                    _ = listener.next() => wake.notify_waiters(),
                    _ = repair.notified() => {}
                }
            }
            // No socket (Codex, a failed bind, or an identity we cannot name yet):
            // only a repair request can wake this loop.
            None => repair.notified().await,
        }
        push_pending(&svc, &peer).await;
        settle_identity(&svc, &mut poke, &mut bound);
        // A poke usually means somebody just came or went, so this is the cheapest
        // moment to notice a changed session list.
        notify_changed_resources(&svc, &peer, &seen, false).await;
    }
}

/// Adopts a real session id if one has appeared, then makes the wake-up socket match
/// whatever address we answer to now. Both halves are idempotent, so this can run on
/// every wake-up.
fn settle_identity(svc: &Service, poke: &mut Option<PokeListener>, bound: &mut Option<Address>) {
    if let Some(found) = svc.resolved_identity() {
        let previous = svc.identity();
        svc.adopt_and_move_mail(&previous, found.clone());
        tracing::info!(was = %previous.address(), now = %found.address(), "adopted the real session id");
    }

    let me = svc.identity();
    // Nothing stable to bind to, or nothing that would listen anyway.
    if me.provisional || !svc.channel_enabled() {
        return;
    }
    if bound.as_ref() == Some(&me.address()) {
        return;
    }
    match PokeListener::bind(&me.registration(None)) {
        Ok(listener) => {
            let endpoint = listener.endpoint().to_string();
            *poke = Some(listener);
            *bound = Some(me.address());
            if let Err(e) = svc.store().register(&me.registration(Some(endpoint))) {
                tracing::warn!("could not record the poke socket: {e}");
            }
        }
        Err(e) => tracing::warn!("could not bind the poke socket: {e}"),
    }
}

async fn push_pending(svc: &Service, peer: &rmcp::service::Peer<rmcp::service::RoleServer>) {
    let messages = match svc.drain_channel() {
        Ok(messages) => messages,
        Err(e) => {
            tracing::warn!("channel drain: {e}");
            return;
        }
    };
    for msg in messages {
        if let Err(e) = peer
            .send_notification(server::channel_notification(msg.params()))
            .await
        {
            tracing::warn!("channel notification: {e}");
            return;
        }
    }
}

async fn touch_loop(svc: Arc<Service>, store: Arc<Store>, repair: Arc<Notify>) {
    loop {
        tokio::time::sleep(TOUCH_EVERY).await;
        let me = svc.identity();
        if let Err(e) = store.touch(&me.harness, &me.session_id) {
            tracing::warn!("touch: {e}");
        }
        // Cheap, and the only sweep a long-lived session ever gets.
        if let Err(e) = store.prune(chrono::Utc::now()) {
            tracing::debug!("prune: {e}");
        }
        // Still anonymous? The hook row may exist by now.
        if me.provisional {
            repair.notify_one();
        }
    }
}

async fn resource_loop(
    svc: Arc<Service>,
    peer: rmcp::service::Peer<rmcp::service::RoleServer>,
    seen: Arc<Mutex<Fingerprint>>,
) {
    let mut ticks: u32 = 0;
    loop {
        tokio::time::sleep(RESOURCE_POLL).await;
        ticks = ticks.wrapping_add(1);
        notify_changed_resources(&svc, &peer, &seen, ticks % PROVIDER_EVERY == 0).await;
    }
}

/// What `resources/list` would contain, reduced to something comparable.
#[derive(Default)]
struct Fingerprint {
    /// Our registry plus the agents the multiplexer can see.
    live: Vec<String>,
    /// The session index's recent tail.
    provider: Vec<String>,
}

/// Announces `resources/list_changed` only when the list really moved: a session
/// registered or went away, herdr gained or lost an agent, or the index picked up
/// something new.
async fn notify_changed_resources(
    svc: &Service,
    peer: &rmcp::service::Peer<rmcp::service::RoleServer>,
    seen: &Mutex<Fingerprint>,
    check_provider: bool,
) {
    let live = svc.live_fingerprint().await;
    let provider = match check_provider {
        true => Some(svc.provider_fingerprint().await),
        false => None,
    };

    {
        let mut last = seen.lock().unwrap_or_else(|e| e.into_inner());
        let mut changed = last.live != live;
        last.live = live;
        if let Some(provider) = provider {
            changed |= last.provider != provider;
            last.provider = provider;
        }
        if !changed {
            return;
        }
    }
    let _ = peer.notify_resource_list_changed().await;
}

#[cfg(feature = "herdr")]
mod herdr_adapter {
    use super::*;
    use agentmail_delivery::HerdrDirectory;
    use async_trait::async_trait;
    use herdr_client::HerdrClient;

    #[async_trait]
    impl DirectorySource for HerdrDirectory {
        async fn refresh(&self) {
            if let Err(e) = HerdrDirectory::refresh(self).await {
                tracing::debug!("herdr agent.list: {e}");
            }
        }

        fn as_directory(&self) -> &dyn agentmail_core::Directory {
            self
        }
    }

    /// Only dial herdr when the environment says there is one; a missing socket is
    /// normal and must not slow startup down.
    pub async fn connect() -> Option<HerdrClient> {
        if std::env::var("HERDR_SOCKET_PATH").ok()?.is_empty() {
            return None;
        }
        match HerdrClient::connect().await {
            Ok(client) => Some(client),
            Err(e) => {
                tracing::debug!("herdr unavailable: {e}");
                None
            }
        }
    }
}

#[cfg(feature = "herdr")]
async fn connect_herdr() -> Option<herdr_client::HerdrClient> {
    herdr_adapter::connect().await
}

/// herdr knows which agent owns a pane and which session it reported. Unreachable
/// herdr, unknown pane and an agent that never reported a session ref all answer `None`.
#[cfg(feature = "herdr")]
async fn herdr_pane_identity(pane: &str) -> Option<agentmail_core::Address> {
    let client = connect_herdr().await?;
    let agent = client.agent_get(pane).await.ok()?;
    agentmail_delivery::address_of(&agent)
}

#[cfg(not(feature = "herdr"))]
async fn herdr_pane_identity(_pane: &str) -> Option<agentmail_core::Address> {
    None
}

#[cfg(not(feature = "herdr"))]
async fn connect_herdr() -> Option<std::convert::Infallible> {
    None
}

#[cfg(feature = "herdr")]
fn herdr_handle(
    herdr: &Option<herdr_client::HerdrClient>,
) -> Option<agentmail_delivery::HerdrHandle> {
    herdr.clone()
}

#[cfg(not(feature = "herdr"))]
fn herdr_handle(
    _herdr: &Option<std::convert::Infallible>,
) -> Option<agentmail_delivery::HerdrHandle> {
    None
}

#[cfg(feature = "herdr")]
fn directory(herdr: &Option<herdr_client::HerdrClient>) -> Option<Arc<dyn DirectorySource>> {
    herdr
        .clone()
        .map(|c| Arc::new(agentmail_delivery::HerdrDirectory::new(c)) as Arc<dyn DirectorySource>)
}

#[cfg(not(feature = "herdr"))]
fn directory(_herdr: &Option<std::convert::Infallible>) -> Option<Arc<dyn DirectorySource>> {
    None
}
