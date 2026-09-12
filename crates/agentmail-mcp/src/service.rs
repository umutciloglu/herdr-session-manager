//! Everything the MCP server does, with no MCP types in sight.
//!
//! The transport layer (rmcp, today) only converts shapes: every decision about tools,
//! resources and the Claude channel lives here so it can be tested against an in-memory
//! store without speaking JSON-RPC.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use agentmail_core::{
    Address, AddressTarget, AgentState, Config, Directory, Envelope, Harness, Mailbox, Message,
    Registration, ResolveCtx, Resolved, Resolver, SendMode, SendOpts, SendOutcome, SessionCard,
    SessionProvider, Store,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tokio::sync::Notify;

use crate::identity::Identity;

pub const CHANNEL_METHOD: &str = "notifications/claude/channel";
pub const CHANNEL_CAPABILITY: &str = "claude/channel";
pub const RESOURCE_SCHEME: &str = "agentmail://session/";
pub const TRANSCRIPT_TEMPLATE: &str = "agentmail://session/{address}/transcript";
pub const RESOURCE_MIME: &str = "text/markdown";

const RECENT_RESOURCES: usize = 40;
const MAX_WAIT_S: u64 = 3600;
const DEFAULT_WAIT_S: u64 = 300;
const MAX_FIND_LIMIT: usize = 10;
/// How hard a tool call tries to find out who it belongs to before answering anyway.
const REPAIR_ATTEMPTS: usize = 3;
const REPAIR_PAUSE: Duration = Duration::from_millis(700);

/// Told to the model once, at initialize. Kept short: it competes with the user's
/// own prompt for attention.
pub const INSTRUCTIONS: &str = "\
agentmail delivers messages between agent sessions, across harnesses. An address is \
`<harness>:<session-id>` such as `claude:8890a685` or `codex:01999b0e`; a unique 8+ \
character id prefix or a session alias also resolves, and `claude:new` / `codex:new` \
starts a fresh session. If you are Claude and the peer is a live Claude session, use \
Claude's own session messaging instead — agentmail_send answers `use_native` with the \
peer's name when that is the case. Use these tools for Codex and other harnesses, for \
sessions that are not running, and for spawning one. Keep a message short and \
self-contained: the peer sees one envelope, not your transcript. Set expects_reply when \
you need an answer, and answer mail you receive with agentmail_reply. Use \
agentmail_find_session when you know the topic but not the address.";

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("unknown tool {0}")]
    UnknownTool(String),

    #[error("{0}")]
    BadArguments(String),

    #[error("agentmail: {0}")]
    Core(#[from] agentmail_core::Error),
}

/// A directory that knows how to bring itself up to date. Separate from `Directory`
/// (which is synchronous by design) and not a supertrait of it, so no trait upcasting
/// is needed to hand the resolver a plain `&dyn Directory`.
#[async_trait]
pub trait DirectorySource: Send + Sync {
    async fn refresh(&self);
    fn as_directory(&self) -> &dyn Directory;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceEntry {
    pub uri: String,
    pub name: String,
    pub description: Option<String>,
    pub mime: &'static str,
}

/// One `notifications/claude/channel` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelMessage {
    pub content: String,
    pub from: String,
    pub message_id: String,
    pub expects_reply: bool,
}

impl ChannelMessage {
    pub fn of(msg: &Message) -> ChannelMessage {
        ChannelMessage {
            content: Envelope::render(msg, None),
            from: msg.from.to_string(),
            message_id: msg.id.clone(),
            expects_reply: msg.expects_reply,
        }
    }

    /// `meta` values are strings: the channel carries a flat string map.
    pub fn params(&self) -> Value {
        json!({
            "content": self.content,
            "meta": {
                "from": self.from,
                "message_id": self.message_id,
                "expects_reply": if self.expects_reply { "true" } else { "false" },
            }
        })
    }

    /// The whole JSON-RPC line, for tests and for any transport that wants it raw.
    pub fn notification(&self) -> Value {
        json!({
            "jsonrpc": "2.0",
            "method": CHANNEL_METHOD,
            "params": self.params(),
        })
    }
}

pub fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "agentmail_send",
            description:
                "Send a message to another agent session (address, alias, or <harness>:new).",
            schema: json!({
                "type": "object",
                "properties": {
                    "to": {"type": "string", "description": "<harness>:<id>, an 8+ char id prefix, an alias, or claude:new / codex:new"},
                    "text": {"type": "string", "description": "The message. Short and self-contained."},
                    "expects_reply": {"type": "boolean", "description": "Ask the peer to answer with agentmail_reply.", "default": false},
                    "mode": {"type": "string", "enum": ["auto", "ask", "background", "pane"], "description": "How hard to try when the peer is not running.", "default": "auto"}
                },
                "required": ["to", "text"],
                "additionalProperties": false
            }),
        },
        ToolDef {
            name: "agentmail_reply",
            description: "Reply to a message you received, by its message id.",
            schema: json!({
                "type": "object",
                "properties": {
                    "message_id": {"type": "string", "description": "The id from the envelope header."},
                    "text": {"type": "string"}
                },
                "required": ["message_id", "text"],
                "additionalProperties": false
            }),
        },
        ToolDef {
            name: "agentmail_wait",
            description: "Block until the next message addressed to this session arrives.",
            schema: json!({
                "type": "object",
                "properties": {
                    "timeout_s": {"type": "number", "description": "Seconds to wait.", "default": DEFAULT_WAIT_S, "maximum": MAX_WAIT_S},
                    "reply_to": {"type": "string", "description": "Only accept a reply to this message id."}
                },
                "additionalProperties": false
            }),
        },
        ToolDef {
            name: "agentmail_find_session",
            description:
                "Find agent sessions by topic, project or harness when you do not know the address.",
            schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "harness": {"type": "string", "description": "claude, codex, ..."},
                    "project": {"type": "string"},
                    "limit": {"type": "number", "maximum": MAX_FIND_LIMIT, "default": 5}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        },
    ]
}

pub struct Service {
    /// Mutable because a provisional identity is upgraded in place the moment the
    /// SessionStart hook row it was missing shows up.
    identity: RwLock<Identity>,
    store: Arc<Store>,
    mailbox: Mailbox,
    resolver: Resolver,
    cfg: Config,
    provider: Option<Arc<dyn SessionProvider>>,
    directory: Option<Arc<dyn DirectorySource>>,
    /// Fired by whoever owns the wake-up socket on every poke, so a blocked
    /// `agentmail_wait` answers in milliseconds instead of on the next poll.
    wake: Option<Arc<Notify>>,
    /// Asks that owner to retry the identity lookup: it is the only writer, because it
    /// is the only thing that can re-bind the socket the new address needs.
    repair: Option<Arc<Notify>>,
    /// True only when this service is answering MCP tool calls for an interactive
    /// session. Claude's own session messaging exists there and nowhere else, so only
    /// there may a send be turned down in favour of it.
    serving_mcp: bool,
    /// Rows this process has pushed over the channel and not yet seen acknowledged.
    /// They stay `Pending` in the store: a channel notification is only delivered if
    /// the session was launched with channels enabled, which the server cannot know,
    /// and a row marked delivered that nobody read is a lost message.
    pushed: Mutex<HashSet<String>>,
}

impl Service {
    pub fn new(identity: Identity, store: Arc<Store>, mailbox: Mailbox, cfg: Config) -> Service {
        Service {
            identity: RwLock::new(identity),
            resolver: Resolver::new(Arc::clone(&store)),
            store,
            mailbox,
            cfg,
            provider: None,
            directory: None,
            wake: None,
            repair: None,
            serving_mcp: false,
            pushed: Mutex::new(HashSet::new()),
        }
    }

    /// Marks this service as the one behind the MCP tools. The CLI and the hooks build
    /// the same service and must always deliver.
    pub fn serving_mcp(mut self) -> Self {
        self.serving_mcp = true;
        self
    }

    /// `wake` is rung on every poke; `repair` asks for an identity retry.
    pub fn with_signals(mut self, wake: Arc<Notify>, repair: Arc<Notify>) -> Self {
        self.wake = Some(wake);
        self.repair = Some(repair);
        self
    }

    pub fn with_provider(mut self, provider: Arc<dyn SessionProvider>) -> Self {
        self.provider = Some(provider);
        self
    }

    pub fn with_directory(mut self, directory: Arc<dyn DirectorySource>) -> Self {
        self.directory = Some(directory);
        self
    }

    pub fn identity(&self) -> Identity {
        self.identity
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The identity this process should have adopted, if it can be known yet. The
    /// caller re-binds the wake-up socket and rewrites the registry, then calls
    /// [`Service::adopt_identity`]; doing it here would leave the socket behind.
    pub fn resolved_identity(&self) -> Option<Identity> {
        self.identity().reresolve(&self.store)
    }

    pub fn adopt_identity(&self, next: Identity) {
        *self.identity.write().unwrap_or_else(|e| e.into_inner()) = next;
    }

    /// Nudges the socket owner to retry. Cheap enough to call on every tool call, and
    /// silent once the identity is real.
    pub fn request_repair(&self) {
        if let Some(repair) = &self.repair {
            if self.identity().provisional {
                repair.notify_one();
            }
        }
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    pub fn me(&self) -> Address {
        self.identity().address()
    }

    /// Codex has no channel: its pending mail is handed over by the Stop hook instead.
    pub fn channel_enabled(&self) -> bool {
        self.cfg.claude_channel && self.identity().harness == Harness::Claude
    }

    async fn refresh_directory(&self) {
        if let Some(dir) = &self.directory {
            dir.refresh().await;
        }
    }

    fn ctx<'a>(&'a self, from: &'a Address) -> ResolveCtx<'a> {
        let mut ctx = ResolveCtx::new(from);
        if let Some(dir) = &self.directory {
            ctx = ctx.with_directory(dir.as_directory());
        }
        if let Some(provider) = &self.provider {
            ctx = ctx.with_provider(provider.as_ref());
        }
        ctx
    }

    pub async fn call_tool(&self, name: &str, args: &Value) -> Result<Value, ServiceError> {
        // A tool call proves the model is awake and has its context in front of it, so
        // anything this process pushed over the channel has been seen.
        self.acknowledge_pushed();
        // It is also the last moment to find out who we are before we put an address on
        // outgoing mail, so this waits rather than nudging a background task.
        self.repair_identity_now().await;
        let mut result = match name {
            "agentmail_send" => self.send(args).await,
            "agentmail_reply" => self.reply(args).await,
            "agentmail_wait" => self.wait(args).await,
            "agentmail_find_session" => self.find_session(args).await,
            other => Err(ServiceError::UnknownTool(other.to_string())),
        };
        // Anything sent while still anonymous may never get its answer back, and the
        // model is the only one who can work around that.
        if let (Ok(value), Some(note)) = (&mut result, self.provisional_warning()) {
            if let Some(obj) = value.as_object_mut() {
                obj.insert("warning".into(), json!(note));
            }
        }
        result
    }

    /// The recipient has to be told too: they are about to answer an address that may
    /// not exist, and only they can decide to reply some other way.
    fn annotate(&self, text: &str) -> String {
        match self.provisional_warning() {
            Some(note) => format!("{text}\n\n[agentmail] {note}"),
            None => text.to_string(),
        }
    }

    /// A provisional identity is a guess; a message sent under one is addressed from a
    /// session that will not exist a minute from now.
    fn provisional_warning(&self) -> Option<String> {
        let me = self.identity();
        me.provisional.then(|| {
            format!(
                "this session could not identify itself ({}), so a reply may not reach it",
                me.address()
            )
        })
    }

    /// Retries the identity lookup inline, using herdr's view of our own pane first.
    /// Returns the adopted identity, or `None` when we are still anonymous.
    pub async fn repair_identity_now(&self) -> Option<Identity> {
        if !self.identity().provisional {
            return None;
        }
        for attempt in 0..REPAIR_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(REPAIR_PAUSE).await;
            }
            self.refresh_directory().await;
            let me = self.identity();
            let found = self
                .pane_address()
                .map(|addr| me.adopted(addr))
                .or_else(|| me.reresolve(&self.store));
            if let Some(next) = found {
                self.adopt_and_move_mail(&me, next.clone());
                // The socket still answers on the old address; its owner re-binds it.
                if let Some(repair) = &self.repair {
                    repair.notify_one();
                }
                return Some(next);
            }
        }
        None
    }

    /// The session herdr says is running in our own pane.
    fn pane_address(&self) -> Option<Address> {
        let pane = self.identity().herdr_pane?;
        let dir = self.directory.as_ref()?;
        dir.as_directory()
            .live_agents()
            .into_iter()
            .find(|e| e.pane.as_deref() == Some(pane.as_str()))
            .and_then(|e| e.address)
    }

    /// Adopts `next` and takes the mail with it: rows written under the provisional
    /// address would otherwise be stranded on a session that never existed.
    pub fn adopt_and_move_mail(&self, previous: &Identity, next: Identity) {
        if let Err(e) = self.store.register(&next.registration(None)) {
            tracing::warn!("could not register {}: {e}", next.address());
            return;
        }
        match self
            .store
            .retarget_address(&previous.address(), &next.address())
        {
            Ok(moved) if moved > 0 => {
                tracing::info!(moved, "moved mail to the real session address")
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("could not move mail: {e}"),
        }
        if let Err(e) = self
            .store
            .deregister(&previous.harness, &previous.session_id)
        {
            tracing::warn!("could not drop the provisional row: {e}");
        }
        self.adopt_identity(next);
    }

    /// A tool call means the model has its context; the channel did its job.
    fn acknowledge_pushed(&self) {
        let ids: Vec<String> = {
            let mut pushed = self.pushed.lock().unwrap_or_else(|e| e.into_inner());
            pushed.drain().collect()
        };
        if ids.is_empty() {
            return;
        }
        if let Err(e) = self.store.mark_read(&ids) {
            tracing::warn!("could not mark pushed mail read: {e}");
        }
    }

    async fn send(&self, args: &Value) -> Result<Value, ServiceError> {
        let to = str_arg(args, "to")?;
        let text = str_arg(args, "text")?;
        let expects_reply = args
            .get("expects_reply")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mode = mode_arg(args)?;
        let target: AddressTarget = to
            .parse()
            .map_err(|e| ServiceError::BadArguments(format!("{e}")))?;

        self.refresh_directory().await;
        let me = self.me();

        if let Some(native) = self.native_peer(&target, &me)? {
            return Ok(native);
        }

        let opts = SendOpts {
            expects_reply,
            mode,
            reply_to: None,
        };
        let result = self
            .mailbox
            .send(&me, &target, &self.annotate(&text), opts, &self.ctx(&me))
            .await?;
        Ok(send_json(&result))
    }

    /// Claude can message another live Claude session natively, and that path keeps the
    /// peer's own context; agentmail would only be a slower copy of it. Say so instead
    /// of sending, and name the peer so the model can address it.
    fn native_peer(
        &self,
        target: &AddressTarget,
        me: &Address,
    ) -> Result<Option<Value>, ServiceError> {
        if !self.serving_mcp || self.identity().harness != Harness::Claude {
            return Ok(None);
        }
        let Resolved::Live(reg, entry) = self.resolver.resolve(target, &self.ctx(me))? else {
            return Ok(None);
        };
        if reg.harness != Harness::Claude || reg.address() == *me {
            return Ok(None);
        }
        // A registration nothing can reach — no wake-up socket, not on screen — is not
        // a session the model can message natively either.
        if reg.poke_path.is_none() && entry.is_none() {
            return Ok(None);
        }
        let alias = reg
            .alias
            .clone()
            .or_else(|| entry.as_ref().and_then(|e| e.alias.clone()))
            .unwrap_or_else(|| reg.address().short_display());
        Ok(Some(json!({
            "message_id": Value::Null,
            "to": reg.address().to_string(),
            "outcome": "use_native",
            "peer": alias,
            "hint": "This peer is a live Claude session: use Claude's own session messaging to reach it."
        })))
    }

    async fn reply(&self, args: &Value) -> Result<Value, ServiceError> {
        let message_id = str_arg(args, "message_id")?;
        let text = str_arg(args, "text")?;
        self.refresh_directory().await;
        let me = self.me();
        let result = self
            .mailbox
            .reply(&me, &message_id, &self.annotate(&text), &self.ctx(&me))
            .await?;
        Ok(send_json(&result))
    }

    async fn wait(&self, args: &Value) -> Result<Value, ServiceError> {
        let timeout = args
            .get("timeout_s")
            .and_then(Value::as_f64)
            .map(|s| s.max(0.0) as u64)
            .unwrap_or(DEFAULT_WAIT_S)
            .min(MAX_WAIT_S);
        let reply_to = args
            .get("reply_to")
            .and_then(Value::as_str)
            .map(str::to_string);

        let found = self
            .wait_for(Duration::from_secs(timeout), reply_to.as_deref())
            .await?;
        Ok(match found {
            Some(msg) => json!({"timed_out": false, "message": msg}),
            None => json!({"timed_out": true}),
        })
    }

    /// The task that owns the poke listener cannot lend it out, so a poke arrives here
    /// as a `Notify` instead: every ring restarts the mailbox wait, which checks the
    /// store before it sleeps again. Under Codex nobody rings it and this is the
    /// mailbox's plain 250 ms poll.
    async fn wait_for(
        &self,
        timeout: Duration,
        reply_to: Option<&str>,
    ) -> Result<Option<Message>, ServiceError> {
        let me = self.me();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let found = match &self.wake {
                Some(wake) => tokio::select! {
                    // Cancelling the mailbox wait can only happen while it sleeps: it
                    // marks a message read and returns it without ever yielding between.
                    found = self.mailbox.wait(&me, left, reply_to, None) => found?,
                    _ = wake.notified() => None,
                },
                None => self.mailbox.wait(&me, left, reply_to, None).await?,
            };
            if found.is_some() {
                return Ok(found);
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }

    async fn find_session(&self, args: &Value) -> Result<Value, ServiceError> {
        let query = str_arg(args, "query")?;
        let harness = args
            .get("harness")
            .and_then(Value::as_str)
            .and_then(|h| h.parse::<Harness>().ok());
        let project = args.get("project").and_then(Value::as_str);
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(5)
            .clamp(1, MAX_FIND_LIMIT as u64) as usize;

        self.refresh_directory().await;
        let me = self.me().to_string();
        let mut cards = self.known_sessions();
        // Nobody needs to be told about themselves; a model writing to its own address
        // would only be talking into a mirror.
        cards.retain(|c| c.address != me && matches_query(c, &query, harness.as_ref(), project));

        if let Some(provider) = &self.provider {
            if let Ok(found) = provider.search(&query, harness.as_ref(), project, limit) {
                cards.extend(found.into_iter().filter(|c| c.address != me));
            }
        }
        dedupe(&mut cards);
        cards.truncate(limit);
        Ok(json!({ "sessions": cards }))
    }

    /// Everything we can name without asking an external index: our own registry plus
    /// whatever the multiplexer sees.
    fn known_sessions(&self) -> Vec<SessionCard> {
        let mut cards: Vec<SessionCard> = self
            .store
            .live()
            .unwrap_or_default()
            .iter()
            .map(card_of_registration)
            .collect();
        if let Some(dir) = &self.directory {
            cards.extend(
                dir.as_directory()
                    .live_agents()
                    .iter()
                    .filter_map(card_of_entry),
            );
        }
        dedupe(&mut cards);
        cards
    }

    // ---- resources ---------------------------------------------------------

    pub async fn resources(&self) -> Vec<ResourceEntry> {
        let me = self.me().to_string();
        self.sessions()
            .await
            .iter()
            .filter(|c| c.address != me)
            .map(resource_entry)
            .collect()
    }

    /// Every session we can name: our registry, the multiplexer, and the recent tail of
    /// the external index. Also what `agentmail sessions` prints.
    pub async fn sessions(&self) -> Vec<SessionCard> {
        self.refresh_directory().await;
        let mut cards = self.known_sessions();
        if let Some(provider) = &self.provider {
            if let Ok(recent) = provider.recent(RECENT_RESOURCES) {
                cards.extend(recent);
            }
        }
        dedupe(&mut cards);
        cards
    }

    pub fn resource_templates(&self) -> Vec<ResourceEntry> {
        vec![ResourceEntry {
            uri: TRANSCRIPT_TEMPLATE.to_string(),
            name: "session transcript".to_string(),
            description: Some("Full transcript text for a session, when the index has it.".into()),
            mime: RESOURCE_MIME,
        }]
    }

    pub async fn read_resource(&self, uri: &str) -> Option<String> {
        let (addr, transcript) = parse_resource_uri(uri)?;
        if transcript {
            return self
                .provider
                .as_ref()
                .and_then(|p| p.transcript(&addr).ok().flatten());
        }
        self.refresh_directory().await;
        let mut cards: Vec<SessionCard> = self
            .known_sessions()
            .into_iter()
            .filter(|c| c.parsed_address().as_ref() == Some(&addr))
            .collect();
        // Ask the index about this one address rather than hoping it was in the recent
        // tail: a card is only worth reading if it carries a title and timestamps.
        if let Some(provider) = &self.provider {
            if let Ok(found) = provider.search(&addr.id, Some(&addr.harness), None, 5) {
                cards.extend(
                    found
                        .into_iter()
                        .filter(|c| c.parsed_address().as_ref() == Some(&addr)),
                );
            }
        }
        dedupe(&mut cards);
        let card = cards.into_iter().next().unwrap_or_else(|| SessionCard {
            address: addr.to_string(),
            harness: addr.harness.to_string(),
            ..SessionCard::default()
        });
        Some(card_markdown(&card))
    }

    /// What `resources/list` is made of, cheaply: our own registry plus whatever the
    /// multiplexer can see. Refreshing the directory here is also what keeps the
    /// resolver's copy of it warm between sends.
    pub async fn live_fingerprint(&self) -> Vec<String> {
        self.refresh_directory().await;
        let mut out = self.live_addresses();
        if let Some(dir) = &self.directory {
            out.extend(dir.as_directory().live_agents().iter().map(|e| {
                format!(
                    "{}|{}",
                    e.address
                        .as_ref()
                        .map(|a| a.to_string())
                        .unwrap_or_default(),
                    e.alias.clone().unwrap_or_default()
                )
            }));
        }
        out.sort();
        out
    }

    /// The index's recent tail. Shelling out is slow and its answer changes slowly, so
    /// this is polled on its own, longer cadence — and off the async worker.
    pub async fn provider_fingerprint(&self) -> Vec<String> {
        let Some(provider) = self.provider.clone() else {
            return Vec::new();
        };
        tokio::task::spawn_blocking(move || {
            let mut out: Vec<String> = provider
                .recent(RECENT_RESOURCES)
                .unwrap_or_default()
                .into_iter()
                .map(|c| c.address)
                .collect();
            out.sort();
            out
        })
        .await
        .unwrap_or_default()
    }

    /// The registry half of the fingerprint, and what `doctor` calls live.
    pub fn live_addresses(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .store
            .live()
            .unwrap_or_default()
            .iter()
            .map(|r| r.address().to_string())
            .collect();
        out.sort();
        out
    }

    // ---- channel -----------------------------------------------------------

    /// Everything pending for this session that this process has not already pushed.
    ///
    /// The rows stay `Pending` on purpose. Claude only routes channel notifications to a
    /// server the session was launched with as a channel, and nothing in the protocol
    /// tells the server whether that happened — so a row marked delivered here could be
    /// one nobody ever saw. Instead the attempt is stamped with `pushed_at`: the Stop
    /// hook looks the id up in the transcript and only repeats what never arrived. The
    /// in-process set stops this loop pushing the same row on every poke, and a later
    /// tool call marks them read.
    pub fn drain_channel(&self) -> Result<Vec<ChannelMessage>, ServiceError> {
        if !self.channel_enabled() {
            return Ok(Vec::new());
        }
        let pending = self.store.pending_for(&self.me())?;
        let mut pushed = self.pushed.lock().unwrap_or_else(|e| e.into_inner());
        let fresh: Vec<Message> = pending
            .into_iter()
            .filter(|m| pushed.insert(m.id.clone()))
            .collect();
        drop(pushed);

        // Recorded, not assumed: the Stop hook checks the transcript for these ids
        // before handing them over again, so a session that really did read the
        // notification is never told twice.
        let ids: Vec<String> = fresh.iter().map(|m| m.id.clone()).collect();
        self.store.mark_pushed(&ids)?;
        Ok(fresh.iter().map(ChannelMessage::of).collect())
    }
}

fn str_arg(args: &Value, key: &str) -> Result<String, ServiceError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| ServiceError::BadArguments(format!("{key} is required")))
}

fn mode_arg(args: &Value) -> Result<SendMode, ServiceError> {
    match args.get("mode").and_then(Value::as_str) {
        None | Some("auto") => Ok(SendMode::Auto),
        Some("ask") => Ok(SendMode::Ask),
        Some("background") => Ok(SendMode::Background),
        Some("pane") => Ok(SendMode::Pane),
        Some(other) => Err(ServiceError::BadArguments(format!(
            "unknown mode {other:?}"
        ))),
    }
}

pub fn send_json(result: &agentmail_core::SendResult) -> Value {
    let mut out = json!({
        "message_id": result.message_id,
        "to": result.to.as_ref().map(|a| a.to_string()),
        "outcome": result.outcome.as_str(),
    });
    if let SendOutcome::Ambiguous(cards) = &result.outcome {
        out["candidates"] = json!(cards);
    }
    if let SendOutcome::Failed(err) = &result.outcome {
        out["error"] = json!(err);
    }
    if let Some(reply) = &result.reply {
        out["reply"] = json!(reply);
    }
    out
}

fn matches_query(
    card: &SessionCard,
    query: &str,
    harness: Option<&Harness>,
    project: Option<&str>,
) -> bool {
    if let Some(h) = harness {
        if card.harness != h.as_str() {
            return false;
        }
    }
    if let Some(p) = project {
        let hit = card
            .project
            .as_deref()
            .or(card.cwd.as_deref())
            .is_some_and(|v| v.to_lowercase().contains(&p.to_lowercase()));
        if !hit {
            return false;
        }
    }
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    [
        Some(card.address.as_str()),
        card.title.as_deref(),
        card.project.as_deref(),
        card.cwd.as_deref(),
        card.first_prompt.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|field| field.to_lowercase().contains(&q))
}

/// One card per address, in the order the sources were asked, each filled in from the
/// ones behind it. The registry and the multiplexer know a session is alive and what its
/// pane is called; only the index knows its title, its first prompt and when it last
/// moved. Dropping the duplicates outright would throw one half of that away.
fn dedupe(cards: &mut Vec<SessionCard>) {
    let mut merged: Vec<SessionCard> = Vec::with_capacity(cards.len());
    for card in cards.drain(..) {
        match merged.iter_mut().find(|c| c.address == card.address) {
            Some(existing) => fill_gaps(existing, card),
            None => merged.push(card),
        }
    }
    *cards = merged;
}

fn fill_gaps(into: &mut SessionCard, from: SessionCard) {
    fn keep(slot: &mut Option<String>, value: Option<String>) {
        if slot.as_deref().map(str::trim).unwrap_or("").is_empty() {
            *slot = value;
        }
    }
    keep(&mut into.project, from.project);
    keep(&mut into.cwd, from.cwd);
    keep(&mut into.title, from.title);
    keep(&mut into.started, from.started);
    keep(&mut into.last_active, from.last_active);
    keep(&mut into.state, from.state);
    keep(&mut into.first_prompt, from.first_prompt);
    keep(&mut into.last_user_message, from.last_user_message);
    keep(&mut into.transcript_path, from.transcript_path);
    into.resumable |= from.resumable;
}

pub fn card_of_registration(reg: &Registration) -> SessionCard {
    SessionCard {
        address: reg.address().to_string(),
        harness: reg.harness.to_string(),
        project: project_of(&reg.cwd),
        cwd: Some(reg.cwd.to_string_lossy().into_owned()),
        title: reg.alias.clone(),
        started: Some(reg.started_at.to_rfc3339()),
        last_active: Some(reg.last_seen.to_rfc3339()),
        state: Some("live".into()),
        resumable: true,
        ..SessionCard::default()
    }
}

pub fn card_of_entry(entry: &agentmail_core::DirectoryEntry) -> Option<SessionCard> {
    let addr = entry.address.clone()?;
    Some(SessionCard {
        address: addr.to_string(),
        harness: addr.harness.to_string(),
        project: entry.cwd.as_deref().and_then(project_of),
        cwd: entry.cwd.as_ref().map(|c| c.to_string_lossy().into_owned()),
        title: entry.title.clone().or_else(|| entry.alias.clone()),
        state: Some(state_label(entry.state).to_string()),
        resumable: true,
        ..SessionCard::default()
    })
}

fn state_label(state: AgentState) -> &'static str {
    match state {
        AgentState::Idle => "idle",
        AgentState::Working => "working",
        AgentState::Blocked => "blocked",
        AgentState::Unknown => "unknown",
    }
}

fn project_of(cwd: &Path) -> Option<String> {
    cwd.file_name().map(|n| n.to_string_lossy().into_owned())
}

fn resource_entry(card: &SessionCard) -> ResourceEntry {
    let title = card.label().unwrap_or("(no title)");
    let age = card
        .last_active
        .as_deref()
        .and_then(parse_ts)
        .map(|t| humanize_age(Utc::now() - t))
        .unwrap_or_else(|| "?".to_string());
    ResourceEntry {
        uri: format!("{RESOURCE_SCHEME}{}", card.address),
        name: format!(
            "{} · {} · {} · {}",
            card.harness,
            card.project.as_deref().unwrap_or("-"),
            title,
            age
        ),
        description: card.first_prompt.clone().or_else(|| card.title.clone()),
        mime: RESOURCE_MIME,
    }
}

/// Roughly 100 tokens: enough for a model to decide whether to write to this session.
pub fn card_markdown(card: &SessionCard) -> String {
    let mut out = format!("# {}\n\n", card.address);
    let mut line = |label: &str, value: Option<&str>| {
        if let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) {
            out.push_str(&format!("- {label}: {v}\n"));
        }
    };
    line("harness", Some(&card.harness));
    line("project", card.project.as_deref());
    line("title", card.title.as_deref());
    line("state", card.state.as_deref());
    line("cwd", card.cwd.as_deref());
    line("started", card.started.as_deref());
    line("last active", card.last_active.as_deref());
    if let Some(prompt) = card.first_prompt.as_deref() {
        out.push_str(&format!("\n**first prompt**\n{}\n", truncate(prompt, 280)));
    }
    if let Some(last) = card.last_user_message.as_deref() {
        out.push_str(&format!("\n**last message**\n{}\n", truncate(last, 280)));
    }
    out.push_str(&format!(
        "\nWrite to it with `agentmail_send to=\"{}\"`.\n",
        card.address
    ));
    out
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn humanize_age(d: chrono::Duration) -> String {
    let secs = d.num_seconds().max(0);
    match secs {
        0..=89 => "just now".to_string(),
        90..=5399 => format!("{}m ago", secs / 60),
        5400..=172_799 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

/// `agentmail://session/<harness>:<id>[/transcript]`.
pub fn parse_resource_uri(uri: &str) -> Option<(Address, bool)> {
    let rest = uri.strip_prefix(RESOURCE_SCHEME)?;
    let (addr, transcript) = match rest.strip_suffix("/transcript") {
        Some(a) => (a, true),
        None => (rest, false),
    };
    Some((addr.parse().ok()?, transcript))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_uris_round_trip() {
        let (addr, transcript) =
            parse_resource_uri("agentmail://session/claude:8890a685").expect("uri");
        assert_eq!(addr, Address::new(Harness::Claude, "8890a685"));
        assert!(!transcript);

        let (addr, transcript) =
            parse_resource_uri("agentmail://session/codex:01999b0e/transcript").expect("uri");
        assert_eq!(addr, Address::new(Harness::Codex, "01999b0e"));
        assert!(transcript);

        assert!(parse_resource_uri("https://example.com").is_none());
    }

    #[test]
    fn ages_read_like_a_human_wrote_them() {
        assert_eq!(humanize_age(chrono::Duration::seconds(30)), "just now");
        assert_eq!(humanize_age(chrono::Duration::minutes(20)), "20m ago");
        assert_eq!(humanize_age(chrono::Duration::hours(5)), "5h ago");
        assert_eq!(humanize_age(chrono::Duration::days(3)), "3d ago");
    }
}
