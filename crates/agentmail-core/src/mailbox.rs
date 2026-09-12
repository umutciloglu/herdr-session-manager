//! The service layer: send, reply, wait, inbox.
//!
//! The invariant everything else hangs off: a message is written to the store before
//! anything is resolved or delivered. A crash, a dead peer or a broken adapter can
//! lose a delivery attempt, never a message.

use std::sync::Arc;
use std::time::Duration;

use crate::domain::{Address, AddressTarget, Harness, Message, SessionCard};

// SendMode lives in `domain` so `traits` can name it without depending on this module.
pub use crate::domain::SendMode;
use crate::error::{Error, Result};
use crate::poke::PokeListener;
use crate::resolver::{ResolveCtx, Resolved, Resolver};
use crate::store::Store;
use crate::traits::{Deliverer, DeliveryOutcome, DeliveryRequest};

const WAIT_POLL: Duration = Duration::from_millis(250);
const INBOX_LIMIT: usize = 50;

#[derive(Debug, Clone, Default)]
pub struct SendOpts {
    pub expects_reply: bool,
    pub mode: SendMode,
    pub reply_to: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SendOutcome {
    Pushed,
    Queued,
    Spawned,
    Ambiguous(Vec<SessionCard>),
    NotFound,
    Failed(String),
}

impl SendOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            SendOutcome::Pushed => "pushed",
            SendOutcome::Queued => "queued",
            SendOutcome::Spawned => "spawned",
            SendOutcome::Ambiguous(_) => "ambiguous",
            SendOutcome::NotFound => "not_found",
            SendOutcome::Failed(_) => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SendResult {
    pub message_id: String,
    pub to: Option<Address>,
    pub outcome: SendOutcome,
    /// Only ever set in `Ask` mode, where a one-shot peer answers inline.
    pub reply: Option<String>,
}

pub struct Mailbox {
    store: Arc<Store>,
    resolver: Resolver,
    deliverers: Vec<Box<dyn Deliverer>>,
}

impl Mailbox {
    pub fn new(store: Arc<Store>, resolver: Resolver, deliverers: Vec<Box<dyn Deliverer>>) -> Self {
        Mailbox {
            store,
            resolver,
            deliverers,
        }
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    pub async fn send(
        &self,
        from: &Address,
        target: &AddressTarget,
        text: &str,
        opts: SendOpts,
        ctx: &ResolveCtx<'_>,
    ) -> Result<SendResult> {
        let msg = Message::new(from.clone(), placeholder(target), text)
            .expecting_reply(opts.expects_reply)
            .in_reply_to(opts.reply_to.clone());
        self.store.enqueue(&msg)?;

        let resolved = match self.resolver.resolve(target, ctx) {
            Ok(r) => r,
            Err(e) => {
                let err = e.to_string();
                self.store.mark_failed(&msg.id, &err)?;
                return Ok(SendResult {
                    message_id: msg.id,
                    to: None,
                    outcome: SendOutcome::Failed(err),
                    reply: None,
                });
            }
        };

        match &resolved {
            Resolved::Ambiguous(cards) => {
                // Keep the row so the caller can retry with a precise address.
                self.store.mark_failed(&msg.id, "ambiguous address")?;
                return Ok(SendResult {
                    message_id: msg.id,
                    to: None,
                    outcome: SendOutcome::Ambiguous(cards.clone()),
                    reply: None,
                });
            }
            Resolved::NotFound => {
                self.store.mark_failed(&msg.id, "no such session")?;
                return Ok(SendResult {
                    message_id: msg.id,
                    to: None,
                    outcome: SendOutcome::NotFound,
                    reply: None,
                });
            }
            _ => {}
        }

        let mut msg = msg;
        if let Some(addr) = resolved.address() {
            self.store.retarget(&msg.id, &addr)?;
            msg.to = addr;
        }

        let outcome = self
            .run_deliverers(&DeliveryRequest {
                message: &msg,
                resolved: &resolved,
                mode: opts.mode,
            })
            .await;

        match outcome {
            DeliveryOutcome::Pushed => {
                self.store.mark_delivered(std::slice::from_ref(&msg.id))?;
                Ok(SendResult {
                    message_id: msg.id,
                    to: Some(msg.to),
                    outcome: SendOutcome::Pushed,
                    reply: None,
                })
            }
            DeliveryOutcome::Queued => Ok(SendResult {
                message_id: msg.id,
                to: Some(msg.to),
                outcome: SendOutcome::Queued,
                reply: None,
            }),
            DeliveryOutcome::Spawned(addr) => {
                self.store.retarget(&msg.id, &addr)?;
                self.store.mark_delivered(std::slice::from_ref(&msg.id))?;
                // An ask-mode spawner may answer by enqueuing a reply instead of
                // returning one; pick it up without blocking so the caller sees the
                // answer in the same call.
                let reply = match opts.mode {
                    SendMode::Ask => self.take_reply(from, &msg.id)?,
                    _ => None,
                };
                Ok(SendResult {
                    message_id: msg.id,
                    to: Some(addr),
                    outcome: SendOutcome::Spawned,
                    reply,
                })
            }
            DeliveryOutcome::Replied { reply, from: peer } => {
                // A headless one-shot may leave no resumable session, in which case the
                // row keeps whatever the target resolved to rather than a made-up address.
                if let Some(addr) = peer {
                    self.store.retarget(&msg.id, &addr)?;
                    msg.to = addr;
                }
                self.store.mark_delivered(std::slice::from_ref(&msg.id))?;
                Ok(SendResult {
                    message_id: msg.id,
                    to: Some(msg.to),
                    outcome: SendOutcome::Spawned,
                    reply: Some(reply),
                })
            }
            DeliveryOutcome::Failed(err) => {
                self.store.mark_failed(&msg.id, &err)?;
                Ok(SendResult {
                    message_id: msg.id,
                    to: Some(msg.to),
                    outcome: SendOutcome::Failed(err),
                    reply: None,
                })
            }
        }
    }

    /// Reply to `message_id`, addressed back at whoever sent it.
    pub async fn reply(
        &self,
        from: &Address,
        message_id: &str,
        text: &str,
        ctx: &ResolveCtx<'_>,
    ) -> Result<SendResult> {
        let original = self
            .store
            .get(message_id)?
            .ok_or_else(|| Error::not_found(format!("message {message_id}")))?;

        // Answering a message is the strongest possible signal it was read.
        self.store.mark_read(std::slice::from_ref(&original.id))?;

        let target = AddressTarget::Address(original.from.clone());
        let opts = SendOpts {
            expects_reply: false,
            mode: SendMode::Auto,
            reply_to: Some(original.id),
        };
        self.send(from, &target, text, opts, ctx).await
    }

    /// Blocks until a message for `me` shows up or `timeout` elapses. Polls, and also
    /// wakes on a poke so a live peer gets a reply in milliseconds rather than 250 ms.
    pub async fn wait(
        &self,
        me: &Address,
        timeout: Duration,
        reply_to: Option<&str>,
        poke: Option<&mut PokeListener>,
    ) -> Result<Option<Message>> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut poke = poke;

        loop {
            if let Some(msg) = self.store.next_for(me, reply_to)? {
                self.store.mark_read(std::slice::from_ref(&msg.id))?;
                return Ok(Some(msg));
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(None);
            }

            let step = WAIT_POLL.min(deadline - tokio::time::Instant::now());
            match poke.as_deref_mut() {
                Some(listener) => {
                    tokio::select! {
                        _ = listener.next() => {}
                        _ = tokio::time::sleep(step) => {}
                    }
                }
                None => tokio::time::sleep(step).await,
            }
        }
    }

    pub fn inbox(&self, me: &Address) -> Result<Vec<Message>> {
        self.store.inbox(me, false, INBOX_LIMIT)
    }

    async fn run_deliverers(&self, req: &DeliveryRequest<'_>) -> DeliveryOutcome {
        // Nothing wired up: leave it pending. A Stop hook drains it on the other side.
        if self.deliverers.is_empty() {
            return DeliveryOutcome::Queued;
        }
        let mut last = DeliveryOutcome::Failed("no deliverer accepted the target".into());
        for d in &self.deliverers {
            match d.deliver(req).await {
                DeliveryOutcome::Failed(e) => last = DeliveryOutcome::Failed(e),
                accepted => return accepted,
            }
        }
        last
    }

    fn take_reply(&self, me: &Address, message_id: &str) -> Result<Option<String>> {
        let Some(reply) = self.store.next_for(me, Some(message_id))? else {
            return Ok(None);
        };
        self.store.mark_read(std::slice::from_ref(&reply.id))?;
        Ok(Some(reply.text))
    }
}

/// The `to` written before resolution. `claude:new` is the protocol's own spawn form;
/// `alias:<name>` is not a real harness, so it can never collide with a session.
fn placeholder(target: &AddressTarget) -> Address {
    match target {
        AddressTarget::Address(a) => a.clone(),
        AddressTarget::New(h) => Address::new(h.clone(), "new"),
        AddressTarget::Alias(name) => Address::new(Harness::Other("alias".into()), name.clone()),
    }
}
